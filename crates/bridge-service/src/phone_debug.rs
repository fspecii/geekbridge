// SPDX-License-Identifier: GPL-3.0-or-later
//! Debug diagnostics from a debug-build phone: its log lines and a redacted state dump.
//!
//! * **Only from a paired, negotiated phone.** The caller (`daemon::trusted_loop`) routes here only the
//!   packets of a trusted link whose peer declared `geekbridge.debug.*`; a release phone and stock KDE
//!   Connect never do.
//! * **Own consent.** `Feature::Debug` (default on for a phone that advertises it, see
//!   `ConsentStore::default_debug_on`). With it off, packets are dropped unread and nothing is asked.
//! * **Bounded.** Oversize, malformed or too-frequent packets are `Invalid` (the daemon counts them
//!   towards closing the link); nothing is stored for them.
//! * **Files.** Lines go to `<state dir>/phone-logs/<device id>.log`, owner-only (0600) in a private
//!   directory, rotated at [`ROTATE_BYTES`] keeping [`KEEP_ROTATED`] older files (`.1` newest).
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bridge_core::debug_link::{self, DebugError};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
pub const KEEP_ROTATED: usize = 3;
/// Sustained log packets per second accepted from one phone; the phone sends at most one per 500 ms.
const RATE_PER_SEC: f64 = 5.0;
const RATE_BURST: f64 = 10.0;
/// How long `phone-state` waits for the phone.
pub const STATE_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Accepted,
    /// Deliberately ignored (consent off, replay); not misbehaviour.
    Dropped,
    /// Malformed, oversize, unsolicited or too frequent.
    Invalid,
}

/// A packet for the phone.
pub struct Out {
    pub kind: String,
    pub body: Map<String, Value>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn take(&mut self) -> bool {
        let now = Instant::now();
        let gained = now.saturating_duration_since(self.at).as_secs_f64() * RATE_PER_SEC;
        self.at = now;
        self.tokens = (self.tokens + gained).min(RATE_BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Link {
    generation: u64,
    tx: mpsc::Sender<Out>,
    /// The phone declared `geekbridge.debug.state_request`.
    accepts: bool,
    last_seq: Option<u64>,
    bucket: Bucket,
    lines_received: u64,
}

struct Waiter {
    request_id: String,
    done: oneshot::Sender<String>,
}

#[derive(Default)]
struct Inner {
    links: HashMap<String, Link>,
    waiting: HashMap<String, Waiter>,
    next_generation: u64,
    next_request: u64,
}

pub struct PhoneDebug {
    /// None when the private state directory is unknown: nothing is stored.
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AskError {
    /// The phone is not linked now.
    NotConnected,
    /// Linked, but the phone did not declare the debug packets (a release build, or stock KDE Connect).
    NotDebugBuild,
    ConsentOff,
    Busy,
    Timeout,
}

impl AskError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotConnected => "the phone is not connected",
            Self::NotDebugBuild => {
                "the phone does not offer debug diagnostics (it is not a GeekBridge debug build)"
            }
            Self::ConsentOff => {
                "debug diagnostics are off for this phone: geekbridge-ctl consent DEVICE_ID debug on"
            }
            Self::Busy => "a state request to this phone is already waiting",
            Self::Timeout => {
                "the phone did not answer in time (is it a debug build with 'Send debug log to computer' on?)"
            }
        }
    }
}

impl PhoneDebug {
    pub fn new(state_dir: Option<&Path>) -> Self {
        Self {
            dir: state_dir.map(log_dir),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A trusted link is up. `accepts_request` = the phone declared `geekbridge.debug.state_request`.
    pub fn link_up(&self, device: &str, accepts_request: bool) -> (mpsc::Receiver<Out>, u64) {
        let (tx, rx) = mpsc::channel(4);
        let mut g = self.lock();
        g.next_generation += 1;
        let generation = g.next_generation;
        g.links.insert(
            device.to_owned(),
            Link {
                generation,
                tx,
                accepts: accepts_request,
                last_seq: None,
                bucket: Bucket {
                    tokens: RATE_BURST,
                    at: Instant::now(),
                },
                lines_received: 0,
            },
        );
        (rx, generation)
    }

    pub fn link_down(&self, device: &str, generation: u64) {
        let mut g = self.lock();
        if g.links
            .get(device)
            .is_some_and(|l| l.generation == generation)
        {
            g.links.remove(device);
            g.waiting.remove(device);
        }
    }

    /// A `geekbridge.debug.log` packet. `consent` = the device's `debug` consent.
    pub fn on_log(&self, device: &str, consent: bool, body: &Map<String, Value>) -> Disposition {
        if !consent {
            return Disposition::Dropped;
        }
        let batch = match debug_link::parse_log(body) {
            Ok(b) => b,
            Err(DebugError::Malformed(_) | DebugError::TooLarge) => return Disposition::Invalid,
        };
        {
            let mut g = self.lock();
            let Some(link) = g.links.get_mut(device) else {
                // No trusted link registered: nothing negotiated, nothing accepted.
                return Disposition::Dropped;
            };
            if !link.bucket.take() {
                return Disposition::Invalid;
            }
            if link.last_seq.is_some_and(|s| batch.seq <= s) {
                return Disposition::Dropped;
            }
            link.last_seq = Some(batch.seq);
            link.lines_received += batch.lines.len() as u64;
        }
        self.store(device, &batch)
    }

    fn store(&self, device: &str, batch: &debug_link::LogBatch) -> Disposition {
        let Some(dir) = &self.dir else {
            return Disposition::Dropped;
        };
        match append_lines(dir, device, &batch.lines) {
            Ok(()) => Disposition::Accepted,
            // A full disk is not the phone's fault.
            Err(_) => Disposition::Dropped,
        }
    }

    /// A `geekbridge.debug.state` packet: complete the waiting `phone-state`, if any.
    pub fn on_state(&self, device: &str, consent: bool, body: &Map<String, Value>) -> Disposition {
        if !consent {
            return Disposition::Dropped;
        }
        let Ok(reply) = debug_link::parse_state(body) else {
            return Disposition::Invalid;
        };
        let mut g = self.lock();
        match g.waiting.remove(device) {
            Some(w) if w.request_id == reply.request_id => {
                let _ = w.done.send(reply.text);
                Disposition::Accepted
            }
            // Unsolicited or late: ignore rather than treat as abuse (a timed-out request).
            Some(w) => {
                g.waiting.insert(device.to_owned(), w);
                Disposition::Dropped
            }
            None => Disposition::Dropped,
        }
    }

    /// Send a state request and return the receiver the answer arrives on.
    pub fn request_state(
        &self,
        device: &str,
        consent: bool,
        connected: bool,
    ) -> Result<(String, oneshot::Receiver<String>), AskError> {
        if !connected {
            return Err(AskError::NotConnected);
        }
        if !consent {
            return Err(AskError::ConsentOff);
        }
        let mut g = self.lock();
        g.next_request += 1;
        let request_id = format!("st{}-{}", g.next_request, now_ms());
        if !g.links.get(device).is_some_and(|l| l.accepts) {
            return Err(AskError::NotDebugBuild);
        }
        if g.waiting.contains_key(device) {
            return Err(AskError::Busy);
        }
        let tx = g.links[device].tx.clone();
        let out = Out {
            kind: debug_link::STATE_REQUEST.to_owned(),
            body: debug_link::state_request_body(&request_id),
        };
        if tx.try_send(out).is_err() {
            return Err(AskError::Busy);
        }
        let (done, rx) = oneshot::channel();
        g.waiting.insert(
            device.to_owned(),
            Waiter {
                request_id: request_id.clone(),
                done,
            },
        );
        Ok((request_id, rx))
    }

    pub fn cancel_wait(&self, device: &str, request_id: &str) {
        let mut g = self.lock();
        if g.waiting
            .get(device)
            .is_some_and(|w| w.request_id == request_id)
        {
            g.waiting.remove(device);
        }
    }

    /// Whether the phone declared the debug packets on its current link.
    pub fn offers(&self, device: &str) -> bool {
        self.lock().links.get(device).is_some_and(|l| l.accepts)
    }

    /// Lines received on the current link, for `diagnose`; None when no link is registered.
    pub fn lines_received(&self, device: &str) -> Option<u64> {
        self.lock().links.get(device).map(|l| l.lines_received)
    }

    /// Delete everything kept for this phone: the live log, the rotated logs, and a state request still
    /// waiting. Called on unpair, forget and when the `debug` consent is withdrawn. A file that is
    /// already gone is fine; a failure is returned after trying every file.
    pub fn erase(&self, device: &str) -> std::io::Result<()> {
        self.lock().waiting.remove(device);
        match &self.dir {
            Some(dir) => erase_files(dir, device),
            None => Ok(()),
        }
    }

    pub fn log_path(&self, device: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{device}.log")))
    }
}

pub fn log_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("phone-logs")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn is_plain_id(device: &str) -> bool {
    !device.is_empty()
        && device.len() <= 64
        && device
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(unix)]
fn open_private(path: &Path, append: bool) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = OpenOptions::new();
    o.create(true).mode(0o600).custom_flags(libc_nofollow());
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    o.open(path)
}

#[cfg(unix)]
fn libc_nofollow() -> i32 {
    // O_NOFOLLOW: a planted symlink in the log directory is never followed.
    #[cfg(target_os = "linux")]
    const FLAG: i32 = 0o400_000;
    #[cfg(not(target_os = "linux"))]
    const FLAG: i32 = 0x0100;
    FLAG
}

#[cfg(not(unix))]
fn open_private(path: &Path, append: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    o.open(path)
}

#[cfg(unix)]
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other("phone-logs is not a directory"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

fn erase_files(dir: &Path, device: &str) -> std::io::Result<()> {
    if !is_plain_id(device) {
        return Ok(());
    }
    let mut paths = vec![dir.join(format!("{device}.log"))];
    paths.extend((1..=KEEP_ROTATED).map(|n| rotated(dir, device, n)));
    let mut first_error = None;
    for p in paths {
        match fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn rotated(dir: &Path, device: &str, n: usize) -> PathBuf {
    dir.join(format!("{device}.log.{n}"))
}

/// Shift `x.log` to `x.log.1`, `.1` to `.2`, ... dropping the oldest beyond [`KEEP_ROTATED`].
pub fn rotate(dir: &Path, device: &str) -> std::io::Result<()> {
    let _ = fs::remove_file(rotated(dir, device, KEEP_ROTATED));
    for n in (1..KEEP_ROTATED).rev() {
        let from = rotated(dir, device, n);
        if from.exists() {
            fs::rename(&from, rotated(dir, device, n + 1))?;
        }
    }
    let live = dir.join(format!("{device}.log"));
    if live.exists() {
        fs::rename(&live, rotated(dir, device, 1))?;
    }
    Ok(())
}

pub fn append_lines(dir: &Path, device: &str, lines: &[String]) -> std::io::Result<()> {
    if !is_plain_id(device) {
        return Err(std::io::Error::other("bad device id"));
    }
    ensure_dir(dir)?;
    let live = dir.join(format!("{device}.log"));
    if let Ok(m) = fs::symlink_metadata(&live) {
        if m.file_type().is_symlink() {
            return Err(std::io::Error::other("log file is a symlink"));
        }
        if m.len() >= ROTATE_BYTES {
            rotate(dir, device)?;
        }
    }
    let mut f = open_private(&live, true)?;
    let mut text = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    f.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn hub(dir: &Path) -> PhoneDebug {
        let h = PhoneDebug::new(Some(dir));
        std::mem::forget(h.link_up(DEV, true));
        h
    }

    fn read(h: &PhoneDebug) -> String {
        fs::read_to_string(h.log_path(DEV).unwrap()).unwrap_or_default()
    }

    #[test]
    fn lines_are_stored_in_order_owner_only() {
        let t = tempfile::tempdir().unwrap();
        let h = hub(t.path());
        let d = h.on_log(DEV, true, &obj(json!({"seq": 1, "lines": ["a", "b"]})));
        assert_eq!(d, Disposition::Accepted);
        h.on_log(DEV, true, &obj(json!({"seq": 2, "lines": ["c"]})));
        assert_eq!(read(&h), "a\nb\nc\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(h.log_path(DEV).unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dm = fs::metadata(log_dir(t.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dm & 0o777, 0o700);
        }
    }

    #[test]
    fn packets_from_a_phone_without_a_registered_trusted_link_store_nothing_even_with_consent() {
        // An unpaired or never-negotiated peer has no link in the hub (only `trusted_loop` registers one).
        let dir = tempfile::tempdir().unwrap();
        let h = hub(dir.path());
        let body = obj(serde_json::json!({"seq": 1, "lines": ["intruder"]}));
        assert_eq!(h.on_log("stranger", true, &body), Disposition::Dropped);
        let state = obj(serde_json::json!({"requestId": "r-1", "text": "x"}));
        assert_eq!(h.on_state("stranger", true, &state), Disposition::Dropped);
        assert!(!h.offers("stranger"));
        assert!(!log_dir(dir.path()).join("stranger.log").exists());
        // A link that went down is as good as none.
        let (_rx, generation) = h.link_up("p1", true);
        h.link_down("p1", generation);
        assert_eq!(h.on_log("p1", true, &body), Disposition::Dropped);
        assert!(!log_dir(dir.path()).join("p1.log").exists());
    }

    #[test]
    fn a_state_request_needs_consent_and_a_phone_that_declared_the_packets() {
        let dir = tempfile::tempdir().unwrap();
        let h = hub(dir.path());
        let (_rx, _g) = h.link_up("rel", false); // a release phone: linked, declared nothing
        assert_eq!(
            h.request_state("rel", true, true).err(),
            Some(AskError::NotDebugBuild)
        );
        assert_eq!(
            h.request_state("rel", false, true).err(),
            Some(AskError::ConsentOff)
        );
        assert_eq!(
            h.request_state("gone", true, false).err(),
            Some(AskError::NotConnected)
        );
    }

    #[test]
    fn consent_off_and_replays_store_nothing() {
        let t = tempfile::tempdir().unwrap();
        let h = hub(t.path());
        assert_eq!(
            h.on_log(DEV, false, &obj(json!({"seq": 1, "lines": ["x"]}))),
            Disposition::Dropped
        );
        assert_eq!(read(&h), "");
        h.on_log(DEV, true, &obj(json!({"seq": 5, "lines": ["one"]})));
        assert_eq!(
            h.on_log(DEV, true, &obj(json!({"seq": 5, "lines": ["dup"]}))),
            Disposition::Dropped
        );
        assert_eq!(read(&h), "one\n");
    }

    #[test]
    fn abuse_is_invalid_and_stores_nothing() {
        let t = tempfile::tempdir().unwrap();
        let h = hub(t.path());
        let big = vec!["x".repeat(900); 100];
        assert_eq!(
            h.on_log(DEV, true, &obj(json!({"seq": 1, "lines": big}))),
            Disposition::Invalid
        );
        let many = vec!["a"; debug_link::MAX_LINES + 1];
        assert_eq!(
            h.on_log(DEV, true, &obj(json!({"seq": 2, "lines": many}))),
            Disposition::Invalid
        );
        assert_eq!(
            h.on_log(DEV, true, &obj(json!({"lines": ["no seq"]}))),
            Disposition::Invalid
        );
        assert_eq!(read(&h), "");
        // A flood beyond the burst is refused.
        let mut invalid = 0;
        for seq in 10..60 {
            if h.on_log(DEV, true, &obj(json!({"seq": seq, "lines": ["f"]})))
                == Disposition::Invalid
            {
                invalid += 1;
            }
        }
        assert!(invalid >= 35, "{invalid}");
    }

    #[test]
    fn device_ids_cannot_escape_the_directory() {
        let t = tempfile::tempdir().unwrap();
        for bad in ["../x", "a/b", "", "a b", ".."] {
            assert!(
                append_lines(&log_dir(t.path()), bad, &["x".into()]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn erase_removes_live_and_rotated_logs_of_that_phone_only() {
        let t = tempfile::tempdir().unwrap();
        let h = hub(t.path());
        let dir = log_dir(t.path());
        append_lines(&dir, DEV, &["a".into()]).unwrap();
        rotate(&dir, DEV).unwrap();
        rotate(&dir, DEV).unwrap();
        append_lines(&dir, DEV, &["b".into()]).unwrap();
        append_lines(&dir, "other", &["keep".into()]).unwrap();
        h.erase(DEV).unwrap();
        for n in ["", ".1", ".2", ".3"] {
            assert!(!dir.join(format!("{DEV}.log{n}")).exists(), "{n}");
        }
        assert!(dir.join("other.log").exists());
        h.erase(DEV).unwrap(); // already gone: fine
    }

    #[test]
    fn rotation_keeps_three_older_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = log_dir(t.path());
        ensure_dir(&dir).unwrap();
        for round in 0..6 {
            let live = dir.join(format!("{DEV}.log"));
            fs::write(&live, format!("round {round}\n")).unwrap();
            rotate(&dir, DEV).unwrap();
        }
        assert!(!dir.join(format!("{DEV}.log")).exists());
        let read = |n: usize| fs::read_to_string(rotated(&dir, DEV, n)).unwrap();
        assert_eq!(
            (read(1), read(2), read(3)),
            ("round 5\n".into(), "round 4\n".into(), "round 3\n".into())
        );
        assert!(!rotated(&dir, DEV, 4).exists());
    }

    #[test]
    fn a_live_file_over_the_limit_rotates_on_the_next_append() {
        let t = tempfile::tempdir().unwrap();
        let dir = log_dir(t.path());
        ensure_dir(&dir).unwrap();
        let live = dir.join(format!("{DEV}.log"));
        let f = File::create(&live).unwrap();
        f.set_len(ROTATE_BYTES).unwrap();
        append_lines(&dir, DEV, &["fresh".into()]).unwrap();
        assert_eq!(fs::read_to_string(&live).unwrap(), "fresh\n");
        assert_eq!(
            fs::metadata(rotated(&dir, DEV, 1)).unwrap().len(),
            ROTATE_BYTES
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_log_file_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let dir = log_dir(t.path());
        ensure_dir(&dir).unwrap();
        let target = t.path().join("victim");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(format!("{DEV}.log"))).unwrap();
        assert!(append_lines(&dir, DEV, &["x".into()]).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    }

    /// A hostile phone with consent on sends maximum-size packets as fast as it likes: what lands on disk is
    /// capped by the rate limit and the rotation, and nothing but the newest files is kept.
    #[test]
    fn a_flood_of_maximum_packets_keeps_disk_use_bounded() {
        let t = tempfile::tempdir().unwrap();
        let h = hub(t.path());
        let line = "x".repeat(debug_link::MAX_LINE_CHARS);
        let lines: Vec<String> = vec![line; 60]; // just under MAX_LOG_BYTES
        let mut accepted = 0u64;
        for seq in 1..=2000u64 {
            let body = debug_link::log_body(seq, &lines);
            if h.on_log(DEV, true, &body) == Disposition::Accepted {
                accepted += 1;
            }
        }
        // Burst plus whatever refilled while the loop ran; far below the 2000 sent.
        assert!(accepted <= 10 + 200, "accepted {accepted}");
        let dir = log_dir(t.path());
        let total: u64 = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= (KEEP_ROTATED as u64 + 1) * (ROTATE_BYTES + 64 * 1024));
        assert!(fs::read_dir(&dir).unwrap().count() <= KEEP_ROTATED + 1);
    }

    #[tokio::test]
    async fn state_requests_are_answered_once_and_checked() {
        let t = tempfile::tempdir().unwrap();
        let h = PhoneDebug::new(Some(t.path()));
        assert_eq!(
            h.request_state(DEV, true, false).err(),
            Some(AskError::NotConnected)
        );
        // Connected but the phone never declared the debug packets.
        let (_rx0, g0) = h.link_up(DEV, false);
        assert_eq!(
            h.request_state(DEV, true, true).err(),
            Some(AskError::NotDebugBuild)
        );
        h.link_down(DEV, g0);
        let (mut rx, _g) = h.link_up(DEV, true);
        assert_eq!(
            h.request_state(DEV, false, true).err(),
            Some(AskError::ConsentOff)
        );
        let (id, answer) = h.request_state(DEV, true, true).unwrap();
        let sent = rx.recv().await.unwrap();
        assert_eq!(sent.kind, debug_link::STATE_REQUEST);
        assert_eq!(sent.body["requestId"], id.as_str());
        assert_eq!(h.request_state(DEV, true, true).err(), Some(AskError::Busy));
        // A reply with the wrong id, or unsolicited, is ignored.
        let wrong = obj(json!({"requestId": "other", "text": "no"}));
        assert_eq!(h.on_state(DEV, true, &wrong), Disposition::Dropped);
        let good = obj(json!({"requestId": id, "text": "sms: not offered: x"}));
        assert_eq!(h.on_state(DEV, true, &good), Disposition::Accepted);
        assert_eq!(answer.await.unwrap(), "sms: not offered: x");
        assert_eq!(h.on_state(DEV, true, &good), Disposition::Dropped);
        assert_eq!(
            h.on_state(DEV, true, &obj(json!({"text": "x"}))),
            Disposition::Invalid
        );
    }
}
