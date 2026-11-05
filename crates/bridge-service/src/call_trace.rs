// SPDX-License-Identifier: GPL-3.0-or-later
//! A small, local, redacted trace of what the call machinery did: call-state transitions, card
//! launches and kills, answer/decline/hang-up requests and their results, the audio route, and the
//! Bluetooth and Wi-Fi hub state. It exists so a call that "did nothing" can be understood after the
//! fact (`geekbridge-ctl calls --trace`), which the service log cannot do.
//!
//! What it never holds: names, full phone numbers, message text. Calls and devices appear as a
//! short hash salted per process, numbers as their last two digits. It is capped (about 128 KiB in
//! memory, 256 KiB in the private `call-trace.log` in the state folder), lives only on this
//! computer, and is emptied when the service starts or on `calls --trace --clear`. Recording is a
//! lock and a `VecDeque` push at the moment something changes; the file append is handed to a writer
//! thread that only exists while there is something to write (no timer, no idle wakeups).

use bridge_core::ipc::TraceEvent;
use std::collections::{BTreeMap, VecDeque};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE: &str = "call-trace.log";
/// Memory cap (sum of line sizes) and file cap. The file is rewritten from memory when it passes its
/// cap, so it shrinks to at most the memory cap and grows again.
const MEMORY_CAP: usize = 128 * 1024;
const FILE_CAP: usize = 256 * 1024;
const MAX_DETAIL: usize = 240;
/// What `calls --trace` returns at most (the newest lines), well under the IPC response cap.
const SNAPSHOT_CAP: usize = 64 * 1024;

/// Control characters (newline, return, tab, escapes) become spaces, so recorded text cannot start
/// a forged line.
fn flatten(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Replace Bluetooth addresses (`AA:BB:CC:DD:EE:FF`, `AA-BB-...`, and BlueZ's `dev_AA_BB_...`) by
/// `<mac>`.
fn redact_macs(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if mac_at(&c, i) {
            out.push_str("<mac>");
            i += 17;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// A MAC address starts at `i` (six hex pairs with one repeated separator, not part of a longer
/// hex run).
fn mac_at(c: &[char], i: usize) -> bool {
    let Some(w) = c.get(i..i + 17) else {
        return false;
    };
    let sep = w[2];
    if !matches!(sep, ':' | '-' | '_') {
        return false;
    }
    for (k, ch) in w.iter().enumerate() {
        let ok = if k % 3 == 2 {
            *ch == sep
        } else {
            ch.is_ascii_hexdigit()
        };
        if !ok {
            return false;
        }
    }
    let before = i.checked_sub(1).and_then(|p| c.get(p));
    let after = c.get(i + 17);
    let hexish = |x: Option<&char>| x.is_some_and(|x| x.is_ascii_hexdigit());
    !hexish(before) && !hexish(after)
}

fn line(e: &TraceEvent) -> String {
    format!("{}\t{}\t{}\n", e.ms, e.kind, e.detail)
}

/// Most queued file operations before the oldest append is dropped (the memory ring keeps it).
const FILE_QUEUE: usize = 256;

enum Op {
    Append(String),
    /// Reopen the file empty and write this (a rewrite after the cap, or just a clear).
    Replace(String),
}

struct Queue {
    ops: VecDeque<Op>,
    running: bool,
    dropped: usize,
}

/// Hands file writes to one writer thread, started when there is something to write and gone when
/// the queue is empty: the caller (an async task, with the hub lock held) never touches the disk.
struct Sink {
    queue: Mutex<Queue>,
    file: Mutex<Option<std::fs::File>>,
    path: PathBuf,
    max_ops: usize,
}

impl Sink {
    fn new(path: PathBuf, file: Option<std::fs::File>, max_ops: usize) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue {
                ops: VecDeque::new(),
                running: false,
                dropped: 0,
            }),
            file: Mutex::new(file),
            path,
            max_ops,
        })
    }

    fn enqueue(self: &Arc<Self>, op: Op) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        match op {
            // Supersedes everything before it.
            Op::Replace(_) => q.ops.clear(),
            Op::Append(_) if q.ops.len() >= self.max_ops => {
                let at = q.ops.iter().position(|o| matches!(o, Op::Append(_)));
                let _ = q.ops.remove(at.unwrap_or(0));
                q.dropped += 1;
            }
            Op::Append(_) => {}
        }
        q.ops.push_back(op);
        if !q.running {
            q.running = true;
            let me = Arc::clone(self);
            if std::thread::Builder::new()
                .name("call-trace".into())
                .spawn(move || me.drain())
                .is_err()
            {
                q.running = false;
                q.ops.clear();
            }
        }
    }

    fn drain(&self) {
        loop {
            let op = {
                let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                match q.ops.pop_front() {
                    Some(op) => op,
                    None => {
                        q.running = false;
                        return;
                    }
                }
            };
            let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
            match op {
                Op::Append(text) => {
                    if let Some(f) = file.as_mut() {
                        let _ = f.write_all(text.as_bytes());
                    }
                }
                Op::Replace(text) => {
                    *file = open_private(&self.path);
                    if let Some(f) = file.as_mut() {
                        let _ = f.write_all(text.as_bytes());
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn wait_idle(&self) {
        loop {
            {
                let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                if q.ops.is_empty() && !q.running {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// The ring itself; the global one is [`record`]'s target. Separate so tests can use their own.
pub struct Trace {
    events: VecDeque<TraceEvent>,
    bytes: usize,
    sink: Option<Arc<Sink>>,
    file_bytes: usize,
    memory_cap: usize,
    file_cap: usize,
}

impl Trace {
    pub const fn new(memory_cap: usize, file_cap: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            sink: None,
            file_bytes: 0,
            memory_cap,
            file_cap,
        }
    }

    /// Start on a fresh, private file in `dir` (an old one is emptied: a restart clears the trace).
    /// Opened here, once, at start-up; every later write goes through the writer thread.
    pub fn attach(&mut self, dir: &std::path::Path) {
        self.events.clear();
        self.bytes = 0;
        self.file_bytes = 0;
        let path = dir.join(FILE);
        let file = open_private(&path);
        self.sink = Some(Sink::new(path, file, FILE_QUEUE));
    }

    #[cfg(test)]
    fn flush(&self) {
        if let Some(s) = &self.sink {
            s.wait_idle();
        }
    }

    pub fn push(&mut self, ms: u64, kind: &str, detail: &str) {
        let kind = flatten(kind);
        let mut detail = redact_macs(&flatten(detail));
        if detail.len() > MAX_DETAIL {
            let mut cut = MAX_DETAIL;
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
        }
        let e = TraceEvent { ms, kind, detail };
        let text = line(&e);
        self.bytes += text.len();
        self.events.push_back(e);
        while self.bytes > self.memory_cap {
            match self.events.pop_front() {
                Some(old) => self.bytes -= line(&old).len(),
                None => break,
            }
        }
        if let Some(sink) = self.sink.clone() {
            if self.file_bytes + text.len() > self.file_cap {
                let all: String = self.events.iter().map(line).collect();
                self.file_bytes = all.len();
                sink.enqueue(Op::Replace(all));
            } else {
                self.file_bytes += text.len();
                sink.enqueue(Op::Append(text));
            }
        }
    }

    /// The newest events whose lines add up to at most [`SNAPSHOT_CAP`] bytes, oldest first.
    pub fn snapshot(&self) -> Vec<TraceEvent> {
        let mut size = 0;
        let mut out: Vec<TraceEvent> = self
            .events
            .iter()
            .rev()
            .take_while(|e| {
                size += line(e).len();
                size <= SNAPSHOT_CAP
            })
            .cloned()
            .collect();
        out.reverse();
        out
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
        self.file_bytes = 0;
        if let Some(sink) = self.sink.clone() {
            sink.enqueue(Op::Replace(String::new()));
        }
    }
}

/// Create/truncate `path` as 0600. `None` when it cannot (the in-memory trace still works).
fn open_private(path: &std::path::Path) -> Option<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    Some(f)
}

static TRACE: Mutex<Trace> = Mutex::new(Trace::new(MEMORY_CAP, FILE_CAP));

fn global() -> std::sync::MutexGuard<'static, Trace> {
    TRACE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Give the global trace its file in the state folder (and empty it). Call once at start.
pub fn init(state_dir: &std::path::Path) {
    global().attach(state_dir);
    record("service", "trace started");
}

pub fn record(kind: &str, detail: &str) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    global().push(ms, kind, detail);
}

pub fn snapshot() -> Vec<TraceEvent> {
    global().snapshot()
}

pub fn clear() {
    global().clear();
}

// ------------------------------------------------------------------------------------ redaction

fn salt() -> &'static RandomState {
    static SALT: OnceLock<RandomState> = OnceLock::new();
    SALT.get_or_init(RandomState::new)
}

/// A short stable-in-this-run label for a call, phone or source id. Not reversible, and different
/// after a restart.
pub fn hash(id: &str) -> String {
    let mut h = salt().build_hasher();
    h.write(id.as_bytes());
    format!("{:06x}", h.finish() & 0xff_ffff)
}

/// The last two digits of a number, nothing else.
pub fn tail(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    match digits.len() {
        0 => "-".into(),
        n => format!(
            "..{}",
            digits[n.saturating_sub(2)..].iter().collect::<String>()
        ),
    }
}

/// `link:<device>|<call>` style row keys: the source kind in clear, the whole key hashed.
fn source_kind(key: &str) -> &str {
    key.split([':', '|']).next().unwrap_or("?")
}

/// What the last recorded view looked like, so only changes are written.
#[derive(Default)]
pub struct ViewMemo {
    rows: BTreeMap<String, String>,
    route: Option<String>,
    mute: Option<bool>,
}

impl ViewMemo {
    pub fn observe(&mut self, view: &bridge_core::calls_view::CallsView) {
        let mut now = BTreeMap::new();
        for r in &view.rows {
            let ctl = format!(
                "{}{}{}",
                if r.answer.is_some() { 'A' } else { '-' },
                if r.reject.is_some() { 'R' } else { '-' },
                if r.hangup.is_some() { 'H' } else { '-' }
            );
            let state = format!(
                "{:?} {:?} controls={ctl} number={} named={}",
                r.phase,
                r.direction,
                r.number
                    .as_ref()
                    .map_or("-".to_owned(), |n| tail(n.expose())),
                r.name.is_some()
            )
            .to_lowercase();
            now.insert(r.key.clone(), (state, r.missing_control.is_some()));
        }
        for (key, (state, _)) in &now {
            if self.rows.get(key) != Some(state) {
                let from = self.rows.get(key).map_or("new", |_| "changed");
                record(
                    "call",
                    &format!("{from} src={} id={} {state}", source_kind(key), hash(key)),
                );
            }
        }
        for key in self.rows.keys().filter(|k| !now.contains_key(*k)) {
            record(
                "call",
                &format!("gone src={} id={}", source_kind(key), hash(key)),
            );
        }
        self.rows = now.into_iter().map(|(k, (s, _))| (k, s)).collect();
        let route = format!("{:?}", view.audio.route).to_lowercase();
        if self.route.as_deref() != Some(route.as_str()) {
            record(
                "audio",
                &format!(
                    "route={route} connect_offered={} hint={}",
                    view.audio.connect.is_some(),
                    view.audio.hint.is_some()
                ),
            );
            self.route = Some(route);
        }
        let muted = view.mute.as_ref().and_then(|m| m.muted);
        if view.mute.is_some() && self.mute != muted {
            record("audio", &format!("mute={muted:?}"));
        }
        self.mute = muted;
    }
}

// --------------------------------------------------------------------------------- text output

/// `2026-11-05 12:00:00.123Z` for `ms` since the epoch (UTC; the trace is for reading next to the
/// service log, not for showing times of day).
pub fn clock(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

pub fn render(events: &[TraceEvent]) -> String {
    if events.is_empty() {
        return "no call events recorded since the service started\n".into();
    }
    events
        .iter()
        .map(|e| format!("{}  {:<8} {}\n", clock(e.ms), e.kind, e.detail))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_two_digits_only() {
        assert_eq!(tail("+1 (555) 010-1234"), "..34");
        assert_eq!(tail("7"), "..7");
        assert_eq!(tail("Mum"), "-");
    }

    #[test]
    fn hash_is_stable_short_and_hides_the_input() {
        let h = hash("link:phone-1|c1");
        assert_eq!(h, hash("link:phone-1|c1"));
        assert_eq!(h.len(), 6);
        assert!(!h.contains("phone"));
        assert_ne!(h, hash("link:phone-1|c2"));
    }

    #[test]
    fn ring_is_bounded_by_bytes_and_keeps_the_newest() {
        let mut t = Trace::new(1000, 4000);
        for i in 0..500 {
            t.push(i, "call", &format!("event number {i}"));
        }
        let s = t.snapshot();
        assert!(t.bytes <= 1000);
        assert!(s.len() < 100);
        assert_eq!(s.last().unwrap().ms, 499);
        assert!(s.windows(2).all(|w| w[0].ms < w[1].ms));
    }

    #[test]
    fn long_details_are_cut_on_a_char_boundary() {
        let mut t = Trace::new(10_000, 10_000);
        t.push(1, "x", &"é".repeat(500));
        assert!(t.snapshot()[0].detail.len() <= MAX_DETAIL);
    }

    #[test]
    fn file_is_private_capped_and_cleared() {
        let dir = std::env::temp_dir().join(format!("gb-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "old secret\n").unwrap();
        let mut t = Trace::new(2000, 5000);
        t.attach(&dir);
        t.flush();
        let path = dir.join(FILE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "",
            "restart clears"
        );
        for i in 0..400 {
            t.push(i, "call", &format!("event number {i}"));
            t.flush();
            assert!(std::fs::metadata(&path).unwrap().len() <= 5000);
        }
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.trim_end().ends_with("event number 399"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        t.clear();
        t.flush();
        assert!(t.snapshot().is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sink_dir(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("gb-trace-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        (dir, path)
    }

    #[test]
    fn file_appends_keep_their_order() {
        let (dir, path) = sink_dir("order");
        let sink = Sink::new(path.clone(), open_private(&path), 1000);
        for i in 0..300 {
            sink.enqueue(Op::Append(format!("{i}\n")));
        }
        sink.wait_idle();
        let want: String = (0..300).map(|i| format!("{i}\n")).collect();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_queue_drops_the_oldest_appends_not_the_newest() {
        let (dir, path) = sink_dir("full");
        let sink = Sink::new(path.clone(), open_private(&path), 4);
        {
            // The writer cannot make progress while the file is held.
            let _held = sink.file.lock().unwrap();
            for i in 0..20 {
                sink.enqueue(Op::Append(format!("{i}\n")));
            }
        }
        sink.wait_idle();
        let text = std::fs::read_to_string(&path).unwrap();
        let got: Vec<u32> = text.lines().map(|l| l.parse().unwrap()).collect();
        assert!(got.len() <= 5, "{got:?}");
        assert_eq!(*got.last().unwrap(), 19);
        assert!(got.windows(2).all(|w| w[0] < w[1]));
        let dropped = sink.queue.lock().unwrap().dropped;
        assert_eq!(dropped + got.len(), 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_thread_exits_when_idle_and_restarts() {
        let (dir, path) = sink_dir("idle");
        let sink = Sink::new(path.clone(), open_private(&path), 10);
        sink.enqueue(Op::Append("a\n".into()));
        sink.wait_idle();
        assert!(!sink.queue.lock().unwrap().running);
        sink.enqueue(Op::Replace("b\n".into()));
        sink.wait_idle();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_characters_cannot_forge_lines() {
        let mut t = Trace::new(10_000, 10_000);
        t.push(1, "call\n", "a\nb\r\tc\u{1b}[0m\u{7f}");
        let e = &t.snapshot()[0];
        assert_eq!(e.kind, "call ");
        assert!(!e.detail.chars().any(char::is_control), "{:?}", e.detail);
        assert_eq!(render(&t.snapshot()).matches('\n').count(), 1);
    }

    #[test]
    fn bluetooth_addresses_are_redacted() {
        let mut t = Trace::new(10_000, 10_000);
        let raw = "no /org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/fd1 or 11:22:33:44:55:66 or aa-bb-cc-dd-ee-ff";
        t.push(1, "x", raw);
        let d = &t.snapshot()[0].detail;
        assert_eq!(d, "no /org/bluez/hci0/dev_<mac>/fd1 or <mac> or <mac>");
        let plain = "12:34:56 and 0123456789abcdef0123 stay";
        t.push(2, "x", plain);
        assert_eq!(t.snapshot()[1].detail, plain);
    }

    #[test]
    fn snapshot_returns_only_the_newest_lines_within_the_cap() {
        let mut t = Trace::new(10 * 1024 * 1024, 10 * 1024 * 1024);
        for i in 0..5000 {
            t.push(i, "call", &"y".repeat(200));
        }
        let s = t.snapshot();
        assert!(s.len() < 5000);
        assert!(s.iter().map(|e| line(e).len()).sum::<usize>() <= SNAPSHOT_CAP);
        assert_eq!(s.last().unwrap().ms, 4999);
        assert!(s.windows(2).all(|w| w[0].ms < w[1].ms));
    }

    #[test]
    fn clock_formats_utc() {
        assert_eq!(clock(0), "1970-01-01 00:00:00.000Z");
        assert_eq!(clock(1_793_880_000_123), "2026-11-05 12:00:00.123Z");
    }
}
