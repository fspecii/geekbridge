// SPDX-License-Identifier: GPL-3.0-or-later
//! File, link and text sharing with paired phones (KDE Connect `share` plugin).
//!
//! * **Receiving** needs the per-device `receive` permission (default off). A file then becomes an
//!   *offer* that waits for Accept / Decline; the per-device `auto_accept` switch (default off)
//!   skips the prompt for that phone only. Text and links are held for the user (never opened,
//!   never put on the clipboard automatically).
//! * **Sending** is always an explicit user action (`geekbridge-ctl send-file`, the Share window).
//! * Bytes move on payload sockets pinned to the paired device's certificate (`bridge_kde::payload`);
//!   file names are reduced to one safe component and destinations are created exclusively, so an
//!   existing file is never overwritten (`bridge_kde::share`).
//! * Bounded everywhere: transfers listed, concurrent transfers (total and per device), pending
//!   offers, offer lifetime, file size, payload idle time.

use bridge_core::ipc::{ErrorBody, ErrorCode, EventKind};
use bridge_core::photos_link::MAX_PHOTO_BYTES;
use bridge_core::photos_view::{FetchInfo, FetchMode, FetchState};
use bridge_core::share::{
    MAX_SEND_PATH_BYTES, MAX_SEND_TEXT_BYTES, ShareDeviceSettings, ShareStatus, ShareTextKind,
    TransferDirection, TransferInfo, TransferKind, TransferState,
};
use bridge_kde::fsutil;
use bridge_kde::share::{self as kshare, FileOffer, ShareContent};
use bridge_kde::{CancelToken, Packet, PayloadContext, TrustedLink};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

pub const SHARE_REQUEST: &str = kshare::SHARE_REQUEST;
pub const MAX_TRANSFERS: usize = 64;
pub const MAX_ACTIVE: usize = 3;
pub const MAX_ACTIVE_PER_DEVICE: usize = 2;
pub const MAX_OFFERED: usize = 8;
/// One paired phone cannot hold more than this many of the [`MAX_OFFERED`] slots.
pub const MAX_OFFERED_PER_DEVICE: usize = 3;
pub const MAX_RECEIVED_TEXT: usize = kshare::MAX_TEXT_BYTES;
/// How long an unfetched outgoing file survives its link before it is given up (see `link_lost`).
const LINK_LOST_GRACE: Duration = Duration::from_secs(10);
/// A photo the user asked for is welcome this long; it then needs the phone to have offered it.
pub const FETCH_WAIT: Duration = Duration::from_secs(120);
/// Photo requests waiting for their file, in all and per phone; and finished ones remembered.
pub const MAX_FETCHES_WAITING: usize = 8;
pub const MAX_FETCHES_WAITING_PER_DEVICE: usize = 4;
pub const MAX_FETCHES_KEPT: usize = 16;
/// Scratch folder (inside the save folder) for photos that are only going to the clipboard.
pub const COPY_DIR: &str = ".geekbridge-photo-copy";
const COPY_KEEP: Duration = Duration::from_secs(10 * 60);
pub const SETTINGS_FILE: &str = "share.json";
const MAX_SETTINGS_DEVICES: usize = 1024;

type EventSink = Arc<dyn Fn(Option<&str>, EventKind) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ShareOptions {
    pub save_dir: PathBuf,
    pub max_file_bytes: u64,
    /// Where per-device permissions persist; `None` keeps them in memory (tests).
    pub settings_path: Option<PathBuf>,
    pub offer_ttl: Duration,
    pub keep_finished: Duration,
}

impl Default for ShareOptions {
    fn default() -> Self {
        Self {
            save_dir: default_save_dir(),
            max_file_bytes: kshare::DEFAULT_MAX_FILE_BYTES,
            settings_path: None,
            offer_ttl: Duration::from_secs(120),
            keep_finished: Duration::from_secs(600),
        }
    }
}

/// `~/Downloads/GeekBridge` (`%USERPROFILE%\\Downloads\\GeekBridge` on Windows; the XDG download
/// directory on Linux when configured).
pub fn default_save_dir() -> PathBuf {
    let home = home_dir(
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        cfg!(windows),
    );
    #[cfg(target_os = "linux")]
    if let Some(dir) = xdg_download_dir(&home) {
        return dir.join("GeekBridge");
    }
    home.join("Downloads").join("GeekBridge")
}

/// The user's home. On Windows `USERPROFILE` wins: `HOME` is often set by Git Bash or MSYS to a
/// POSIX-style path (`/c/Users/x`) that Win32 file APIs resolve to the wrong place. Only absolute
/// values count; with none, the current directory.
fn home_dir(
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    windows: bool,
) -> PathBuf {
    let order = if windows {
        [userprofile, home]
    } else {
        [home, userprofile]
    };
    order
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .find(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(target_os = "linux")]
fn xdg_download_dir(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok()?;
    parse_xdg_download(&text, home)
}

#[cfg(any(target_os = "linux", test))]
fn parse_xdg_download(text: &str, home: &Path) -> Option<PathBuf> {
    let line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("XDG_DOWNLOAD_DIR="))?;
    let value = line.trim().trim_matches('"');
    let rel = value
        .strip_prefix("$HOME/")
        .or(value.strip_prefix("${HOME}/"));
    let path = match rel {
        Some(r) if !r.is_empty() && !r.split('/').any(|c| c == "..") => home.join(r),
        _ => return None,
    };
    Some(path)
}

// ---------------------------------------------------------------------- settings

#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(default)]
struct Perm {
    receive: bool,
    auto_accept: bool,
}

/// `share.json`: `{"version": 1, "devices": {"<device id>": {"receive": bool, "auto_accept": bool}}}`.
/// Unknown fields (top level and per device) are kept on rewrite; a file from a newer build is
/// read but never rewritten; a damaged file without backup means "nothing allowed" (opt-in).
pub static SETTINGS_SPEC: bridge_core::statefile::Spec = bridge_core::statefile::Spec {
    name: SETTINGS_FILE,
    current: 1,
    migrations: &[],
    validate: Some(validate_settings),
    on_corrupt: bridge_core::statefile::Corrupt::Reset,
};

fn validate_settings(body: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    match body.get("devices") {
        Some(serde_json::Value::Object(m)) if m.len() <= MAX_SETTINGS_DEVICES => Ok(()),
        Some(serde_json::Value::Object(_)) => Err("too many entries".into()),
        _ => Err("no devices object".into()),
    }
}

struct Settings {
    file: Option<Arc<fsutil::PrivateStateFile>>,
    devices: BTreeMap<String, Perm>,
    /// Format of a newer build's file, which must not be rewritten.
    newer: Option<u32>,
}

impl Settings {
    fn open(path: Option<PathBuf>) -> Self {
        use bridge_core::statefile::Status;
        let file = path.map(|p| {
            Arc::new(fsutil::PrivateStateFile::new(
                fsutil::PrivateIo,
                p,
                &SETTINGS_SPEC,
            ))
        });
        let mut devices = BTreeMap::new();
        let mut newer = None;
        if let Some(f) = &file {
            match f.load() {
                Ok(l) => {
                    if let Status::Newer { found } = l.status {
                        newer = Some(found);
                    }
                    if let Some(serde_json::Value::Object(m)) = l.body.get("devices") {
                        for (id, v) in m.iter().take(MAX_SETTINGS_DEVICES) {
                            if let Ok(p) = serde_json::from_value::<Perm>(v.clone()) {
                                devices.insert(id.clone(), p);
                            }
                        }
                    }
                }
                // Loose permissions and the like: nothing is allowed, and the file is left alone.
                Err(e) => bridge_core::statefile::note(format!("{SETTINGS_FILE}: {e}")),
            }
        }
        Self {
            file,
            devices,
            newer,
        }
    }

    /// A handle that can write the file without holding the `Settings` mutex (file lock + fsync can take
    /// seconds on a busy disk, and `status` reads the settings).
    fn writer(&self) -> SettingsWriter {
        SettingsWriter {
            file: self.file.clone(),
            newer: self.newer,
        }
    }

    #[cfg(all(test, unix))]
    fn persist(&self, next: &BTreeMap<String, Perm>) -> Result<(), String> {
        self.writer().persist(next)
    }
}

struct SettingsWriter {
    file: Option<Arc<fsutil::PrivateStateFile>>,
    newer: Option<u32>,
}

impl SettingsWriter {
    fn persist(&self, next: &BTreeMap<String, Perm>) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(found) = self.newer {
            return Err(format!(
                "{SETTINGS_FILE} is format {found}, written by a newer version; not changed"
            ));
        }
        let locked = file.lock().map_err(|e| e.to_string())?;
        let loaded = locked.load().map_err(|e| e.to_string())?;
        let mut body = loaded.body;
        let old = match body.remove("devices") {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        let mut devices = serde_json::Map::new();
        for (id, perm) in next {
            let mut entry = match old.get(id) {
                Some(serde_json::Value::Object(m)) => m.clone(),
                _ => serde_json::Map::new(),
            };
            if let Ok(serde_json::Value::Object(known)) = serde_json::to_value(perm) {
                entry.extend(known);
            }
            devices.insert(id.clone(), serde_json::Value::Object(entry));
        }
        body.insert("devices".into(), serde_json::Value::Object(devices));
        locked.save(&body).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------- notifications

/// What the desktop shows when an offer arrives while no Share window may be open. The reply is
/// `true` for Accept, `false` for Decline; never replying leaves the offer for the window.
pub trait Notifier: Send + Sync {
    fn offered(
        &self,
        device_name: &str,
        file_name: &str,
        size: Option<u64>,
        reply: Box<dyn FnOnce(bool) + Send>,
    );
    fn received_text(&self, _device_name: &str, _is_url: bool) {}
    /// A file finished arriving and was saved at `path`.
    fn received_file(&self, _path: &std::path::Path) {}
}

pub struct NoNotifier;
impl Notifier for NoNotifier {
    fn offered(&self, _: &str, _: &str, _: Option<u64>, _: Box<dyn FnOnce(bool) + Send>) {}
}

/// Best-effort freedesktop notification through `notify-send` (libnotify >= 0.8 for actions).
/// Missing tool or no action support just means no notification; the Share window still works.
pub struct NotifySend;

impl NotifySend {
    pub fn args(device: &str, file: &str, size: Option<u64>) -> Vec<String> {
        let clean = |s: &str| crate::notify_text::plain(s, 80);
        let size = size.map_or(String::new(), |n| format!(" ({})", human_size(n)));
        vec![
            "--app-name=GeekBridge".into(),
            "--wait".into(),
            "--action=accept=Accept".into(),
            "--action=decline=Decline".into(),
            // A click on the body (daemons that draw no buttons) opens the Share window; it never accepts.
            "--action=default=Open".into(),
            "--expire-time=60000".into(),
            "--".into(),
            format!("{} wants to send a file", clean(device)),
            format!("{}{size}", clean(file)),
        ]
    }
}

impl Notifier for NotifySend {
    fn offered(
        &self,
        device: &str,
        file: &str,
        size: Option<u64>,
        reply: Box<dyn FnOnce(bool) + Send>,
    ) {
        let args = Self::args(device, file, size);
        std::thread::spawn(move || {
            let out = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            if let Ok(out) = out {
                match String::from_utf8_lossy(&out.stdout).trim() {
                    "accept" => reply(true),
                    "decline" => reply(false),
                    "default" => crate::desktop_notify::open_app("geekbridge-share"),
                    _ => {}
                }
            }
        });
    }

    fn received_file(&self, path: &std::path::Path) {
        crate::desktop_notify::file_received(path);
    }

    fn received_text(&self, device: &str, is_url: bool) {
        let what = if is_url { "a link" } else { "text" };
        let device = crate::notify_text::plain(device, 80);
        let _ = std::process::Command::new("notify-send")
            .args([
                "--app-name=GeekBridge",
                "--",
                &format!("{device} sent {what}"),
                "Open the Share window to view it.",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|mut c| {
                std::thread::spawn(move || {
                    let _ = c.wait();
                })
            });
    }
}

/// Create `dir` (and parents) readable by the owner only where the platform has such a thing.
/// Makes sure `dir` is a real directory (not a link someone planted) that only this user can enter. False if it
/// cannot be made so; nothing should then be written there.
fn make_private_dir(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_symlink() => {
                // Remove only the link itself, never what it points at.
                if std::fs::remove_file(dir).is_err() {
                    return false;
                }
            }
            Ok(m) if !m.is_dir() => return false,
            _ => {}
        }
        if std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .is_err()
        {
            return false;
        }
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                m.permissions().mode() & 0o077 == 0
                    || std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        // Per-user folder on Windows; refuse a reparse point (link or junction).
        if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir()) {
            return false;
        }
        std::fs::create_dir_all(dir).is_ok()
    }
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

// ---------------------------------------------------------------------- manager

/// A full-size photo this computer asked the phone for. The phone's file offer that matches (same phone, same
/// name, within [`FETCH_WAIT`]) is taken without the phone having the general `receive` permission.
struct Fetch {
    info: FetchInfo,
    until: Instant,
    transfer_id: Option<u64>,
}

struct Transfer {
    fetch: Option<u64>,
    info: TransferInfo,
    bytes: Arc<AtomicU64>,
    cancel: CancelToken,
    offer: Option<(FileOffer, PayloadContext)>,
    created: Instant,
    finished: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    items: Vec<Transfer>,
    next_fetch: u64,
    fetches: Vec<Fetch>,
}

pub struct ShareManager {
    opts: ShareOptions,
    inner: Mutex<Inner>,
    settings: Mutex<Settings>,
    /// Serialises settings writers; held across the file lock and fsync, never by readers.
    settings_writer: Mutex<()>,
    sink: Mutex<Option<EventSink>>,
    notifier: Mutex<Arc<dyn Notifier>>,
}

fn err(code: ErrorCode, message: &str) -> ErrorBody {
    ErrorBody {
        code,
        message: message.to_owned(),
    }
}

fn now_ms() -> u64 {
    crate::now_ms()
}

/// What the daemon should do with a packet.
pub enum Disposition {
    /// Not a share packet.
    NotShare,
    /// Share packet, consumed (accepted, stored or refused by policy).
    Handled,
    /// Share packet that was refused or malformed; counts against the link's misbehaviour budget.
    Rejected,
}

/// A job for the link task that owns the `TrustedLink`.
pub enum ShareJob {
    File { transfer_id: u64, path: PathBuf },
    Text { kind: ShareTextKind, text: String },
}

impl ShareManager {
    pub fn new(opts: ShareOptions) -> Arc<Self> {
        let settings = Settings::open(opts.settings_path.clone());
        // A previous run may have died mid-download; clear what it left in the save directory.
        kshare::sweep_partials(&opts.save_dir, Duration::from_secs(600));
        let me = Arc::new(Self {
            opts,
            inner: Mutex::new(Inner::default()),
            settings: Mutex::new(settings),
            settings_writer: Mutex::new(()),
            sink: Mutex::new(None),
            notifier: Mutex::new(Arc::new(NoNotifier)),
        });
        // Nothing in the scratch folder can be in use at start: whatever a crashed run left goes now.
        me.sweep_copy_dir_older_than(Duration::ZERO);
        me
    }

    pub fn set_event_sink(&self, sink: EventSink) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    pub fn set_notifier(&self, n: Arc<dyn Notifier>) {
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = n;
    }

    pub fn options(&self) -> &ShareOptions {
        &self.opts
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, device: &str, kind: EventKind) {
        let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(sink) = sink {
            sink(Some(device), kind);
        }
    }

    // ------------------------------------------------------------ settings

    fn perm(&self, device_id: &str) -> Perm {
        self.settings()
            .devices
            .get(device_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn set_settings(
        &self,
        device_id: &str,
        receive: Option<bool>,
        auto_accept: Option<bool>,
    ) -> Result<(), ErrorBody> {
        let _writer = self
            .settings_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut next, io) = {
            let s = self.settings();
            (s.devices.clone(), s.writer())
        };
        let p = next.entry(device_id.to_owned()).or_default();
        if let Some(r) = receive {
            p.receive = r;
            if !r {
                p.auto_accept = false;
            }
        }
        if let Some(a) = auto_accept {
            // Auto-accept without the receive permission would be meaningless; turn both on.
            p.auto_accept = a;
            if a {
                p.receive = true;
            }
        }
        if *p == Perm::default() {
            next.remove(device_id);
        }
        if next.len() > MAX_SETTINGS_DEVICES {
            return Err(err(ErrorCode::TooMany, "too many devices"));
        }
        io.persist(&next)
            .map_err(|_| err(ErrorCode::Failed, "settings could not be saved"))?;
        self.settings().devices = next;
        if receive == Some(false) {
            self.decline_offers_of(device_id);
        }
        Ok(())
    }

    /// The device was unpaired: forget its permissions and stop its transfers.
    pub fn forget_device(&self, device_id: &str) {
        let _writer = self
            .settings_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (known, mut next, io) = {
            let s = self.settings();
            (
                s.devices.contains_key(device_id),
                s.devices.clone(),
                s.writer(),
            )
        };
        if known {
            next.remove(device_id);
            if io.persist(&next).is_ok() {
                self.settings().devices = next;
            }
        }
        drop(_writer);
        let now = Instant::now();
        self.inner()
            .fetches
            .retain(|f| f.info.device_id != device_id);
        for t in self.inner().items.iter_mut() {
            if t.info.device_id == device_id && t.info.state.is_live() {
                t.cancel.cancel();
                t.info.state = TransferState::Cancelled;
                t.offer = None;
                t.finished = Some(now);
            }
        }
    }

    fn decline_offers_of(&self, device_id: &str) {
        let now = Instant::now();
        for t in self.inner().items.iter_mut() {
            if t.info.device_id == device_id && t.info.state == TransferState::Offered {
                t.info.state = TransferState::Declined;
                t.offer = None;
                t.finished = Some(now);
            }
        }
    }

    // ------------------------------------------------------------ queries

    pub fn status(&self, paired: &[String]) -> ShareStatus {
        self.expire();
        let settings = self.settings();
        let devices = paired
            .iter()
            .map(|id| {
                let p = settings.devices.get(id).copied().unwrap_or_default();
                ShareDeviceSettings {
                    device_id: id.clone(),
                    receive: p.receive,
                    auto_accept: p.auto_accept,
                }
            })
            .collect();
        drop(settings);
        let now = Instant::now();
        let transfers = self
            .inner()
            .items
            .iter()
            .map(|t| {
                let mut i = t.info.clone();
                i.bytes = t.bytes.load(Ordering::Relaxed);
                if i.state == TransferState::Waiting && i.bytes > 0 {
                    i.state = TransferState::Active;
                }
                if i.state == TransferState::Offered {
                    i.expires_in_ms = Some(
                        self.opts
                            .offer_ttl
                            .saturating_sub(now.saturating_duration_since(t.created))
                            .as_millis() as u64,
                    );
                }
                i
            })
            .collect();
        ShareStatus {
            save_dir: self.opts.save_dir.display().to_string(),
            max_file_bytes: self.opts.max_file_bytes,
            devices,
            transfers,
        }
    }

    /// Expire stale offers and forget old finished entries.
    fn expire(&self) {
        let now = Instant::now();
        let mut inner = self.inner();
        for t in inner.items.iter_mut() {
            if t.info.state == TransferState::Offered
                && now.saturating_duration_since(t.created) > self.opts.offer_ttl
            {
                t.info.state = TransferState::Failed;
                t.info.code = Some("expired".into());
                t.offer = None;
                t.finished = Some(now);
            }
        }
        let keep = self.opts.keep_finished;
        inner.items.retain(|t| {
            t.finished
                .is_none_or(|f| now.saturating_duration_since(f) <= keep)
        });
    }

    /// Make room for one more entry by dropping the oldest finished one.
    fn make_room(&self, inner: &mut Inner) -> bool {
        while inner.items.len() >= MAX_TRANSFERS {
            match inner.items.iter().position(|t| !t.info.state.is_live()) {
                Some(i) => {
                    inner.items.remove(i);
                }
                None => return false,
            }
        }
        true
    }

    fn active_counts(inner: &Inner, device: &str) -> (usize, usize) {
        let live =
            |t: &&Transfer| matches!(t.info.state, TransferState::Waiting | TransferState::Active);
        (
            inner.items.iter().filter(live).count(),
            inner
                .items
                .iter()
                .filter(live)
                .filter(|t| t.info.device_id == device)
                .count(),
        )
    }

    // ------------------------------------------------------------ photos asked for

    /// The scratch folder for photos that only go to the clipboard.
    pub fn copy_dir(&self) -> PathBuf {
        self.opts.save_dir.join(COPY_DIR)
    }

    /// Delete what is left in the scratch folder from earlier copies (older than ten minutes).
    pub fn sweep_copy_dir(&self) {
        self.sweep_copy_dir_older_than(COPY_KEEP);
    }

    /// Delete the scratch folder's files that are at least `min_age` old (zero: all of them). Links are removed as
    /// links; nothing outside the folder is touched.
    fn sweep_copy_dir_older_than(&self, min_age: Duration) {
        let dir = self.copy_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten() {
            let old = e
                .metadata()
                .ok()
                .filter(|m| m.is_file() || m.file_type().is_symlink())
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age >= min_age);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Remember that the user asked `device_id` for the photo `photo_id` (display name `name`). The phone's
    /// matching file offer will be accepted by [`Self::on_packet`]. The caller then sends the request.
    pub fn expect_photo(
        &self,
        device_id: &str,
        photo_id: &str,
        name: &str,
        mode: FetchMode,
    ) -> Result<u64, ErrorBody> {
        self.sweep_copy_dir();
        let name = kshare::sanitize_filename(name);
        let now = Instant::now();
        let mut inner = self.inner();
        Self::lapse_fetches(&mut inner, now);
        let waiting = |f: &&Fetch| f.info.state == FetchState::Waiting;
        if inner.fetches.iter().filter(waiting).count() >= MAX_FETCHES_WAITING
            || inner
                .fetches
                .iter()
                .filter(waiting)
                .filter(|f| f.info.device_id == device_id)
                .count()
                >= MAX_FETCHES_WAITING_PER_DEVICE
        {
            return Err(err(ErrorCode::Busy, "too many photo requests are waiting"));
        }
        if inner.fetches.iter().any(|f| {
            f.info.state == FetchState::Waiting
                && f.info.device_id == device_id
                && f.info.name == name
        }) {
            return Err(err(ErrorCode::Busy, "that photo was already asked for"));
        }
        inner.next_fetch += 1;
        let fetch_id = inner.next_fetch;
        inner.fetches.push(Fetch {
            info: FetchInfo {
                fetch_id,
                device_id: device_id.to_owned(),
                photo_id: photo_id.to_owned(),
                name,
                mode,
                state: FetchState::Waiting,
                path: None,
                code: None,
            },
            until: now + FETCH_WAIT,
            transfer_id: None,
        });
        while inner.fetches.len() > MAX_FETCHES_KEPT {
            let Some(i) = inner
                .fetches
                .iter()
                .position(|f| !matches!(f.info.state, FetchState::Waiting | FetchState::Receiving))
            else {
                break;
            };
            inner.fetches.remove(i);
        }
        Ok(fetch_id)
    }

    /// `device_id`'s consent was withdrawn or its link ended: every photo still waited for is forgotten, a photo
    /// already arriving is cancelled (its partial file is removed), and a finished clipboard copy is erased from the
    /// scratch folder. Saved photos stay: the user asked for them.
    pub fn drop_expectations(&self, device_id: &str) {
        let mut cancelled = Vec::new();
        let mut erase = Vec::new();
        {
            let mut inner = self.inner();
            let Inner { items, fetches, .. } = &mut *inner;
            for f in fetches.iter().filter(|f| f.info.device_id == device_id) {
                match f.info.state {
                    FetchState::Receiving => {
                        if let Some(t) = items.iter_mut().find(|t| {
                            Some(t.info.transfer_id) == f.transfer_id && t.info.state.is_live()
                        }) {
                            t.cancel.cancel();
                            t.info.state = TransferState::Cancelled;
                            t.info.code = Some("cancelled".into());
                            t.offer = None;
                            t.finished = Some(Instant::now());
                            cancelled.push(t.info.transfer_id);
                        }
                    }
                    FetchState::Done if f.info.mode == FetchMode::Copy => {
                        erase.extend(f.info.path.clone());
                    }
                    _ => {}
                }
            }
            fetches.retain(|f| {
                f.info.device_id != device_id
                    || !matches!(f.info.state, FetchState::Waiting | FetchState::Receiving)
            });
        }
        let scratch = self.copy_dir();
        for path in erase {
            let path = PathBuf::from(path);
            if path.parent() == Some(scratch.as_path()) {
                let _ = std::fs::remove_file(path);
            }
        }
        for transfer_id in cancelled {
            self.emit(
                device_id,
                EventKind::ShareFinished {
                    transfer_id,
                    ok: false,
                },
            );
        }
    }

    /// The request could not be sent: forget the expectation.
    pub fn cancel_expectation(&self, fetch_id: u64) {
        self.inner().fetches.retain(|f| f.info.fetch_id != fetch_id);
    }

    fn lapse_fetches(inner: &mut Inner, now: Instant) {
        let Inner { items, fetches, .. } = inner;
        for f in fetches.iter_mut() {
            if f.info.state == FetchState::Waiting && now >= f.until {
                f.info.state = FetchState::Failed;
                f.info.code = Some("expired".to_owned());
            }
            if f.info.state == FetchState::Receiving && f.transfer_id.is_some() {
                // The transfer ended some way other than finishing (cancelled, expired, forgotten).
                let t = items
                    .iter()
                    .find(|t| Some(t.info.transfer_id) == f.transfer_id);
                match t {
                    Some(t) if t.info.state.is_live() || t.info.state == TransferState::Done => {}
                    Some(t) => {
                        f.info.state = FetchState::Failed;
                        f.info.code = Some(t.info.code.clone().unwrap_or_else(|| "failed".into()));
                    }
                    None => {
                        f.info.state = FetchState::Failed;
                        f.info.code = Some("lost".to_owned());
                    }
                }
            }
        }
    }

    /// A file offer from `device_id` named `name` of `size` bytes: claims the fetch it answers and returns its id.
    /// The claim is made under the same lock that finds it (state `Receiving`), so two offers arriving together
    /// cannot both be taken for one request. An offer that declares no size, or more than a listed photo can be
    /// ([`MAX_PHOTO_BYTES`]), fails the request instead of filling the disk.
    fn claim_expected(&self, device_id: &str, name: &str, size: Option<u64>) -> Option<u64> {
        let name = kshare::sanitize_filename(name);
        let now = Instant::now();
        let mut inner = self.inner();
        Self::lapse_fetches(&mut inner, now);
        let f = inner.fetches.iter_mut().find(|f| {
            f.info.state == FetchState::Waiting
                && f.info.device_id == device_id
                && f.info.name == name
        })?;
        if !size.is_some_and(|n| n <= MAX_PHOTO_BYTES) {
            f.info.state = FetchState::Failed;
            f.info.code = Some("too_large".to_owned());
            return None;
        }
        f.info.state = FetchState::Receiving;
        Some(f.info.fetch_id)
    }

    /// A claimed fetch whose transfer could not be created.
    fn release_fetch(inner: &mut Inner, fetch: Option<u64>, code: &str) {
        if let Some(f) = inner
            .fetches
            .iter_mut()
            .find(|r| Some(r.info.fetch_id) == fetch && r.transfer_id.is_none())
        {
            f.info.state = FetchState::Failed;
            f.info.code = Some(code.to_owned());
        }
    }

    /// Photo requests, newest last (for the control API).
    pub fn fetches(&self) -> Vec<FetchInfo> {
        let mut inner = self.inner();
        Self::lapse_fetches(&mut inner, Instant::now());
        inner.fetches.iter().map(|f| f.info.clone()).collect()
    }

    // ------------------------------------------------------------ receiving

    pub fn is_share_packet(kind: &str) -> bool {
        kind == kshare::SHARE_REQUEST || kind == kshare::SHARE_REQUEST_UPDATE
    }

    /// A share packet from a trusted, negotiated peer.
    pub fn on_packet(
        self: &Arc<Self>,
        device_id: &str,
        device_name: &str,
        ctx: PayloadContext,
        packet: &Packet,
    ) -> Disposition {
        if !Self::is_share_packet(&packet.kind) {
            return Disposition::NotShare;
        }
        let perm = self.perm(device_id);
        let content = match kshare::parse_share(packet, self.opts.max_file_bytes) {
            Ok(c) => c,
            Err(_) => return Disposition::Rejected,
        };
        // A photo this computer asked the phone for is taken without the general `receive` permission: the
        // user's click on "Save" or "Copy" was the consent. Anything else still needs `receive`.
        if let ShareContent::File(offer) = &content
            && let Some(fetch) = self.claim_expected(device_id, &offer.name, offer.size)
        {
            let ShareContent::File(offer) = content else {
                unreachable!()
            };
            return self.offer_file(device_id, device_name, ctx, offer, true, Some(fetch));
        }
        if !perm.receive {
            return Disposition::Rejected;
        }
        match content {
            ShareContent::Ignored => Disposition::Handled,
            ShareContent::Text(t) => self.store_text(device_id, device_name, TransferKind::Text, t),
            ShareContent::Url(u) => self.store_text(device_id, device_name, TransferKind::Url, u),
            ShareContent::File(offer) => {
                self.offer_file(device_id, device_name, ctx, offer, perm.auto_accept, None)
            }
        }
    }

    fn store_text(
        &self,
        device_id: &str,
        device_name: &str,
        kind: TransferKind,
        text: String,
    ) -> Disposition {
        if text.len() > MAX_RECEIVED_TEXT {
            return Disposition::Rejected;
        }
        let id = {
            let mut inner = self.inner();
            if !self.make_room(&mut inner) {
                return Disposition::Rejected;
            }
            inner.next_id += 1;
            let id = inner.next_id;
            let now = Instant::now();
            inner.items.push(Transfer {
                fetch: None,
                info: TransferInfo {
                    transfer_id: id,
                    device_id: device_id.to_owned(),
                    device_name: device_name.to_owned(),
                    direction: TransferDirection::Incoming,
                    kind,
                    name: String::new(),
                    size: Some(text.len() as u64),
                    bytes: text.len() as u64,
                    state: TransferState::Done,
                    code: None,
                    path: None,
                    text: Some(text),
                    expires_in_ms: None,
                    at_unix_ms: now_ms(),
                },
                bytes: Arc::new(AtomicU64::new(0)),
                cancel: CancelToken::new(),
                offer: None,
                created: now,
                finished: Some(now),
            });
            id
        };
        self.emit(device_id, EventKind::ShareReceivedText { transfer_id: id });
        let notifier = self
            .notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        notifier.received_text(device_name, kind == TransferKind::Url);
        Disposition::Handled
    }

    fn offer_file(
        self: &Arc<Self>,
        device_id: &str,
        device_name: &str,
        ctx: PayloadContext,
        offer: FileOffer,
        auto_accept: bool,
        fetch: Option<u64>,
    ) -> Disposition {
        let (id, name, size) = {
            let mut inner = self.inner();
            if !self.make_room(&mut inner) {
                Self::release_fetch(&mut inner, fetch, "busy");
                return Disposition::Rejected;
            }
            let offered = |t: &&Transfer| t.info.state == TransferState::Offered;
            if inner.items.iter().filter(offered).count() >= MAX_OFFERED
                || inner
                    .items
                    .iter()
                    .filter(offered)
                    .filter(|t| t.info.device_id == device_id)
                    .count()
                    >= MAX_OFFERED_PER_DEVICE
            {
                Self::release_fetch(&mut inner, fetch, "busy");
                return Disposition::Rejected;
            }
            inner.next_id += 1;
            let id = inner.next_id;
            let now = Instant::now();
            let (name, size) = (offer.name.clone(), offer.size);
            inner.items.push(Transfer {
                fetch,
                info: TransferInfo {
                    transfer_id: id,
                    device_id: device_id.to_owned(),
                    device_name: device_name.to_owned(),
                    direction: TransferDirection::Incoming,
                    kind: TransferKind::File,
                    name: name.clone(),
                    size,
                    bytes: 0,
                    state: TransferState::Offered,
                    code: None,
                    path: None,
                    text: None,
                    expires_in_ms: None,
                    at_unix_ms: now_ms(),
                },
                bytes: Arc::new(AtomicU64::new(0)),
                cancel: CancelToken::new(),
                offer: Some((offer, ctx)),
                created: now,
                finished: None,
            });
            if let Some(f) = fetch
                && let Some(rec) = inner.fetches.iter_mut().find(|r| r.info.fetch_id == f)
            {
                rec.transfer_id = Some(id);
                rec.info.state = FetchState::Receiving;
            }
            (id, name, size)
        };
        self.emit(device_id, EventKind::ShareOffered { transfer_id: id });
        // Lapse the offer even if nobody looks at the status again.
        let weak = Arc::downgrade(self);
        let ttl = self.opts.offer_ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl + Duration::from_millis(50)).await;
            if let Some(m) = weak.upgrade() {
                m.expire();
            }
        });
        if auto_accept {
            // Over the concurrency limit it simply stays offered for a manual answer.
            let _ = self.decide(id, true);
        } else {
            let weak = Arc::downgrade(self);
            let notifier = self
                .notifier
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            notifier.offered(
                device_name,
                &name,
                size,
                Box::new(move |accept| {
                    if let Some(m) = weak.upgrade() {
                        let _ = m.decide(id, accept);
                    }
                }),
            );
        }
        Disposition::Handled
    }

    /// Accept or decline an offer. Accepting starts the download in the background.
    pub fn decide(self: &Arc<Self>, transfer_id: u64, accept: bool) -> Result<(), ErrorBody> {
        self.expire();
        let (offer, ctx, bytes, cancel, device_id, copy_only, is_fetch) = {
            let mut inner = self.inner();
            let idx = inner
                .items
                .iter()
                .position(|t| t.info.transfer_id == transfer_id)
                .ok_or_else(|| err(ErrorCode::NotFound, "no such transfer"))?;
            let (total, per_dev) = {
                let dev = inner.items[idx].info.device_id.clone();
                Self::active_counts(&inner, &dev)
            };
            let t = &mut inner.items[idx];
            if t.info.state != TransferState::Offered {
                return Err(err(
                    ErrorCode::Busy,
                    "the offer was already answered or has expired",
                ));
            }
            if !accept {
                t.info.state = TransferState::Declined;
                t.offer = None;
                t.finished = Some(Instant::now());
                let device = t.info.device_id.clone();
                drop(inner);
                self.emit(
                    &device,
                    EventKind::ShareFinished {
                        transfer_id,
                        ok: false,
                    },
                );
                return Ok(());
            }
            if total >= MAX_ACTIVE || per_dev >= MAX_ACTIVE_PER_DEVICE {
                return Err(err(
                    ErrorCode::Busy,
                    "too many transfers are running; try again shortly",
                ));
            }
            let (offer, ctx) = t
                .offer
                .take()
                .ok_or_else(|| err(ErrorCode::Busy, "the offer is no longer available"))?;
            t.info.state = TransferState::Active;
            crate::tray::poke();
            let parts = (
                offer,
                ctx,
                t.bytes.clone(),
                t.cancel.clone(),
                t.info.device_id.clone(),
            );
            let fetch = t.fetch;
            let copy_only = fetch.is_some_and(|f| {
                inner
                    .fetches
                    .iter()
                    .any(|r| r.info.fetch_id == f && r.info.mode == FetchMode::Copy)
            });
            (
                parts.0,
                parts.1,
                parts.2,
                parts.3,
                parts.4,
                copy_only,
                fetch.is_some(),
            )
        };
        let me = self.clone();
        let dir = if copy_only {
            let dir = self.copy_dir();
            if !make_private_dir(&dir) {
                // A planted link or a folder we cannot make private: nothing is written there.
                let id = transfer_id;
                self.finish(id, &device_id, Err("scratch_unsafe"));
                return Ok(());
            }
            dir
        } else {
            self.opts.save_dir.clone()
        };
        // A requested photo is a photo: never more than a listed one can be, whatever the general limit is.
        let max = if is_fetch {
            self.opts.max_file_bytes.min(MAX_PHOTO_BYTES)
        } else {
            self.opts.max_file_bytes
        };
        tokio::spawn(async move {
            let counter = bytes.clone();
            let result = kshare::receive_file(
                &ctx,
                &offer,
                &dir,
                max,
                &mut |n| counter.store(n, Ordering::Relaxed),
                &cancel,
            )
            .await;
            match result {
                Ok(path) => me.finish(transfer_id, &device_id, Ok(Some(path))),
                Err(e) => me.finish(transfer_id, &device_id, Err(e.code())),
            }
        });
        Ok(())
    }

    fn finish(&self, id: u64, device: &str, result: Result<Option<PathBuf>, &'static str>) {
        let ok = result.is_ok();
        let received = result.as_ref().ok().and_then(Clone::clone);
        {
            let mut inner = self.inner();
            let Some(t) = inner.items.iter_mut().find(|t| t.info.transfer_id == id) else {
                return;
            };
            if !t.info.state.is_live() {
                return; // cancelled while the task was finishing
            }
            match result {
                Ok(path) => {
                    t.info.state = TransferState::Done;
                    t.info.bytes = t
                        .info
                        .size
                        .unwrap_or_else(|| t.bytes.load(Ordering::Relaxed));
                    t.bytes.store(t.info.bytes, Ordering::Relaxed);
                    t.info.path = path.map(|p| p.display().to_string());
                }
                Err(code) => {
                    t.info.state = if code == "cancelled" {
                        TransferState::Cancelled
                    } else {
                        TransferState::Failed
                    };
                    t.info.code = Some(code.to_owned());
                }
            }
            t.finished = Some(Instant::now());
            let (state, path, code) = (t.info.state, t.info.path.clone(), t.info.code.clone());
            if let Some(rec) = inner.fetches.iter_mut().find(|r| r.transfer_id == Some(id)) {
                if state == TransferState::Done {
                    rec.info.state = FetchState::Done;
                    rec.info.path = path;
                } else {
                    rec.info.state = FetchState::Failed;
                    rec.info.code = code;
                }
            }
        }
        self.emit(
            device,
            EventKind::ShareFinished {
                transfer_id: id,
                ok,
            },
        );
        if let Some(path) = received.filter(|p| !p.starts_with(self.copy_dir())) {
            let notifier = self
                .notifier
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            notifier.received_file(&path);
        }
    }

    pub fn cancel(&self, transfer_id: u64) -> Result<(), ErrorBody> {
        let device = {
            let mut inner = self.inner();
            let t = inner
                .items
                .iter_mut()
                .find(|t| t.info.transfer_id == transfer_id)
                .ok_or_else(|| err(ErrorCode::NotFound, "no such transfer"))?;
            if !t.info.state.is_live() {
                return Err(err(ErrorCode::Busy, "the transfer already finished"));
            }
            t.cancel.cancel();
            t.info.state = TransferState::Cancelled;
            t.info.code = Some("cancelled".into());
            t.offer = None;
            t.finished = Some(Instant::now());
            t.info.device_id.clone()
        };
        self.emit(
            &device,
            EventKind::ShareFinished {
                transfer_id,
                ok: false,
            },
        );
        Ok(())
    }

    pub fn dismiss(&self, transfer_id: u64) -> Result<(), ErrorBody> {
        let mut inner = self.inner();
        let i = inner
            .items
            .iter()
            .position(|t| t.info.transfer_id == transfer_id)
            .ok_or_else(|| err(ErrorCode::NotFound, "no such transfer"))?;
        if inner.items[i].info.state.is_live() {
            return Err(err(ErrorCode::Busy, "the transfer is still in progress"));
        }
        inner.items.remove(i);
        Ok(())
    }

    // ------------------------------------------------------------ sending

    /// Validate and register an outgoing file; the link task then runs [`Self::run_job`].
    pub fn begin_send_file(
        &self,
        device_id: &str,
        device_name: &str,
        path: &str,
    ) -> Result<u64, ErrorBody> {
        if path.is_empty() || path.len() > MAX_SEND_PATH_BYTES || path.contains('\0') {
            return Err(err(ErrorCode::BadRequest, "invalid path"));
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(err(ErrorCode::BadRequest, "the path must be absolute"));
        }
        let meta = std::fs::metadata(&path)
            .map_err(|_| err(ErrorCode::NotFound, "the file cannot be read"))?;
        if !meta.is_file() {
            return Err(err(ErrorCode::BadRequest, "only regular files can be sent"));
        }
        if meta.len() == 0 {
            return Err(err(ErrorCode::BadRequest, "empty files cannot be sent"));
        }
        if meta.len() > self.opts.max_file_bytes {
            return Err(err(
                ErrorCode::BadRequest,
                "the file is larger than the transfer limit",
            ));
        }
        let name = path
            .file_name()
            .map(|n| kshare::sanitize_filename(&n.to_string_lossy()))
            .unwrap_or_else(|| "file".into());
        self.expire();
        let mut inner = self.inner();
        if !self.make_room(&mut inner) {
            return Err(err(ErrorCode::TooMany, "too many transfers"));
        }
        let (total, per_dev) = Self::active_counts(&inner, device_id);
        if total >= MAX_ACTIVE || per_dev >= MAX_ACTIVE_PER_DEVICE {
            return Err(err(ErrorCode::Busy, "too many transfers are running"));
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let now = Instant::now();
        inner.items.push(Transfer {
            fetch: None,
            info: TransferInfo {
                transfer_id: id,
                device_id: device_id.to_owned(),
                device_name: device_name.to_owned(),
                direction: TransferDirection::Outgoing,
                kind: TransferKind::File,
                name,
                size: Some(meta.len()),
                bytes: 0,
                state: TransferState::Waiting,
                code: None,
                path: Some(path.display().to_string()),
                text: None,
                expires_in_ms: None,
                at_unix_ms: now_ms(),
            },
            bytes: Arc::new(AtomicU64::new(0)),
            cancel: CancelToken::new(),
            offer: None,
            created: now,
            finished: None,
        });
        crate::tray::poke();
        Ok(id)
    }

    /// The link task ended with `job` still queued: it will never run, so fail it instead of leaving
    /// a `Waiting` entry that holds a transfer slot forever.
    pub fn drop_job(&self, job: ShareJob) {
        if let ShareJob::File { transfer_id, .. } = job {
            let device = self
                .inner()
                .items
                .iter()
                .find(|t| t.info.transfer_id == transfer_id)
                .map(|t| t.info.device_id.clone());
            if let Some(device) = device {
                self.finish(transfer_id, &device, Err("link_lost"));
            }
        }
    }

    /// The control link to `device_id` ended. An outgoing file whose payload listener is still waiting
    /// for a phone that never connected would hold its port, file handle and transfer slot for the
    /// whole accept timeout (120 s); after a short grace period for the phone to reconnect to the
    /// payload port, give those up. Transfers that have started streaming are left to finish.
    pub fn link_lost(self: &Arc<Self>, device_id: &str) {
        let (me, device) = (self.clone(), device_id.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(LINK_LOST_GRACE).await;
            let ids: Vec<u64> = me
                .inner()
                .items
                .iter()
                .filter(|t| {
                    t.info.device_id == device
                        && t.info.direction == TransferDirection::Outgoing
                        && t.info.state.is_live()
                        && t.bytes.load(Ordering::Relaxed) == 0
                })
                .map(|t| t.info.transfer_id)
                .collect();
            for id in ids {
                if let Some(t) = me.inner().items.iter().find(|t| t.info.transfer_id == id) {
                    t.cancel.cancel();
                }
                me.finish(id, &device, Err("link_lost"));
            }
        });
    }

    /// A job could not be queued after `begin_send_file`.
    pub fn abandon(&self, transfer_id: u64) {
        self.inner()
            .items
            .retain(|t| t.info.transfer_id != transfer_id);
    }

    pub fn validate_text(kind: ShareTextKind, text: &str) -> Result<(), ErrorBody> {
        let max = match kind {
            ShareTextKind::Text => MAX_SEND_TEXT_BYTES,
            ShareTextKind::Url => kshare::MAX_URL_BYTES,
        };
        if text.is_empty() || text.len() > max {
            return Err(err(ErrorCode::BadRequest, "text is empty or too long"));
        }
        if kind == ShareTextKind::Url && !kshare::is_openable_url(text) {
            return Err(err(
                ErrorCode::BadRequest,
                "only http and https links can be shared",
            ));
        }
        Ok(())
    }

    /// Run a job on the task that owns `link`. Returns false if the link itself failed.
    pub async fn run_job(self: &Arc<Self>, link: &mut TrustedLink, job: ShareJob) -> bool {
        match job {
            ShareJob::Text { kind, text } => {
                let packet = match kind {
                    ShareTextKind::Text => kshare::text_packet(&text),
                    ShareTextKind::Url => kshare::url_packet(&text),
                };
                link.send(&packet).await.is_ok()
            }
            ShareJob::File { transfer_id, path } => self.run_file(link, transfer_id, &path).await,
        }
    }

    async fn run_file(self: &Arc<Self>, link: &mut TrustedLink, id: u64, path: &Path) -> bool {
        let (device, bytes, cancel) = {
            let inner = self.inner();
            match inner
                .items
                .iter()
                .find(|t| t.info.transfer_id == id && t.info.state.is_live())
            {
                Some(t) => (t.info.device_id.clone(), t.bytes.clone(), t.cancel.clone()),
                None => return true, // cancelled before it started
            }
        };
        let out = match kshare::open_outgoing(path, self.opts.max_file_bytes).await {
            Ok(o) => o,
            Err(_) => {
                self.finish(id, &device, Err("unreadable"));
                return true;
            }
        };
        let ctx = link.payload_context();
        let (packet, listener) = match kshare::prepare_send(&ctx, &out).await {
            Ok(x) => x,
            Err(e) => {
                self.finish(id, &device, Err(e.code()));
                return true;
            }
        };
        if link.send_with_payload(&packet).await.is_err() {
            self.finish(id, &device, Err("io_error"));
            return false;
        }
        let me = self.clone();
        let size = out.size;
        tokio::spawn(async move {
            let result = listener
                .send(
                    out.file,
                    size,
                    &mut |n| bytes.store(n, Ordering::Relaxed),
                    &cancel,
                )
                .await;
            match result {
                Ok(_) => me.finish(id, &device, Ok(None)),
                Err(e) => me.finish(id, &device, Err(e.code())),
            }
        });
        true
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;

    fn abs(p: &str) -> std::ffi::OsString {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        format!("{root}{p}").into()
    }

    #[test]
    fn windows_prefers_userprofile_over_home() {
        let (win, other) = if cfg!(windows) {
            ("C:\\Users\\ada", "C:\\msys\\home\\ada")
        } else {
            ("/users/ada", "/msys/home/ada")
        };
        assert_eq!(
            home_dir(Some(other.into()), Some(win.into()), true),
            PathBuf::from(win)
        );
        // A POSIX-style HOME that is not absolute for Win32 is skipped even without USERPROFILE.
        assert_eq!(home_dir(Some("rel".into()), None, true), PathBuf::from("."));
    }

    #[test]
    fn unix_prefers_home_and_ignores_relative_values() {
        assert_eq!(
            home_dir(Some(abs("home/a")), Some(abs("other")), false),
            PathBuf::from(abs("home/a"))
        );
        assert_eq!(
            home_dir(Some("rel".into()), Some("also/rel".into()), false),
            PathBuf::from(".")
        );
        assert_eq!(home_dir(None, None, true), PathBuf::from("."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(save: &Path) -> Arc<ShareManager> {
        ShareManager::new(ShareOptions {
            save_dir: save.to_path_buf(),
            max_file_bytes: 8 << 30,
            settings_path: None,
            offer_ttl: Duration::from_secs(60),
            keep_finished: Duration::from_secs(60),
        })
    }

    #[test]
    fn an_expected_offer_is_claimed_once_and_only_when_it_is_the_size_of_a_photo() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let dev = "a".repeat(32);
        m.expect_photo(&dev, "p1", "IMG_1.jpg", FetchMode::Save)
            .unwrap();
        // The claim itself marks the request used: a second offer of the same name (even at the same instant on
        // another connection) finds nothing.
        assert!(m.claim_expected(&dev, "IMG_1.jpg", Some(1000)).is_some());
        assert!(m.claim_expected(&dev, "IMG_1.jpg", Some(1000)).is_none());
        // Another phone never matches.
        m.expect_photo(&dev, "p2", "IMG_2.jpg", FetchMode::Save)
            .unwrap();
        assert!(
            m.claim_expected(&"b".repeat(32), "IMG_2.jpg", Some(10))
                .is_none()
        );
        // No declared size, or more than a listed photo can be: the request fails and nothing is claimed.
        assert!(
            m.claim_expected(&dev, "IMG_2.jpg", Some(MAX_PHOTO_BYTES + 1))
                .is_none()
        );
        let f = m
            .fetches()
            .into_iter()
            .find(|f| f.photo_id == "p2")
            .unwrap();
        assert_eq!(
            (f.state, f.code.as_deref()),
            (FetchState::Failed, Some("too_large"))
        );
        m.expect_photo(&dev, "p3", "IMG_3.jpg", FetchMode::Save)
            .unwrap();
        assert!(m.claim_expected(&dev, "IMG_3.jpg", None).is_none());
        // A claimed request is not "lost" just because its transfer has not been created yet.
        let f = m
            .fetches()
            .into_iter()
            .find(|f| f.photo_id == "p1")
            .unwrap();
        assert_eq!(f.state, FetchState::Receiving);
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_is_a_private_real_folder_never_a_planted_link() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("scratch");
        // Left over with open permissions: tightened.
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(make_private_dir(&scratch));
        assert_eq!(
            std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o077,
            0
        );
        // A link to somewhere else is replaced by a real folder; the target is left alone.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(make_private_dir(&link));
        let m = std::fs::symlink_metadata(&link).unwrap();
        assert!(m.is_dir() && !m.file_type().is_symlink());
        assert_eq!(m.permissions().mode() & 0o077, 0);
        assert_eq!(
            std::fs::metadata(&elsewhere).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // A plain file where the folder should be: refused.
        let file = tmp.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(!make_private_dir(&file));
    }

    #[test]
    fn a_new_manager_empties_what_a_crashed_run_left_in_the_scratch_folder() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join(COPY_DIR);
        std::fs::create_dir(&scratch).unwrap();
        std::fs::write(scratch.join("IMG_1.jpg"), b"fresh").unwrap();
        std::fs::write(dir.path().join("keep.jpg"), b"saved").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("keep.jpg"), scratch.join("l.jpg")).unwrap();
        let _m = manager(dir.path());
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        assert_eq!(
            std::fs::read(dir.path().join("keep.jpg")).unwrap(),
            b"saved"
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_file_formats_load_keep_unknown_fields_and_respect_newer_versions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("s");
        fsutil::ensure_private_dir(&state).unwrap();
        let path = state.join(SETTINGS_FILE);
        let put = |bytes: &[u8]| {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        // The golden format-1 file.
        let golden = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/state-v1/share.json"),
        )
        .unwrap();
        put(&golden);
        let s = Settings::open(Some(path.clone()));
        let id = "b2".repeat(16);
        assert_eq!(
            s.devices.get(&id),
            Some(&Perm {
                receive: true,
                auto_accept: false
            })
        );
        // An unknown field of a newer minor build survives our rewrite.
        let mut v: serde_json::Value = serde_json::from_slice(&golden).unwrap();
        v["devices"][&id]["mute_until"] = 5.into();
        put(&serde_json::to_vec(&v).unwrap());
        let mut next = s.devices.clone();
        next.get_mut(&id).unwrap().auto_accept = true;
        s.persist(&next).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["devices"][&id]["mute_until"], 5);
        assert_eq!(v["devices"][&id]["auto_accept"], true);
        // A newer format is read but never written.
        v.as_object().unwrap();
        let mut newer = v.clone();
        newer["version"] = 3.into();
        put(&serde_json::to_vec(&newer).unwrap());
        let before = std::fs::read(&path).unwrap();
        let s = Settings::open(Some(path.clone()));
        assert!(s.devices.get(&id).unwrap().receive);
        assert!(s.persist(&BTreeMap::new()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Damage with no backup: nothing is allowed, and the service still starts.
        let _ = std::fs::remove_file(path.with_extension("json.bak"));
        put(b"{ torn");
        assert!(Settings::open(Some(path)).devices.is_empty());
    }

    #[test]
    fn xdg_download_dir_parsing() {
        let home = Path::new("/home/u");
        assert_eq!(
            parse_xdg_download(
                "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\n",
                home
            ),
            Some(PathBuf::from("/home/u/Téléchargements"))
        );
        for bad in [
            "XDG_DOWNLOAD_DIR=\"/etc\"",
            "XDG_DOWNLOAD_DIR=\"$HOME/../x\"",
            "XDG_DOWNLOAD_DIR=\"$HOME/\"",
            "nothing",
        ] {
            assert_eq!(parse_xdg_download(bad, home), None, "{bad}");
        }
    }

    #[test]
    fn a_device_name_cannot_become_an_option_or_markup() {
        // The title starts with the peer's chosen device name (S2-3 in docs/reports/security-review-2.md).
        let a = NotifySend::args("--action=accept=Open invoice", "<b>x</b>.txt", None);
        let dashdash = a.iter().position(|s| s == "--").expect("positional marker");
        assert!(a[..dashdash].iter().all(|s| !s.contains("Open invoice")));
        assert!(a[dashdash + 1..].iter().all(|s| !s.contains(['<', '>'])));
    }

    #[test]
    fn notify_args_are_clean() {
        let a = NotifySend::args("Ph\u{1b}one", "a\nb.txt", Some(1536));
        assert!(a.iter().all(|s| !s.chars().any(|c| c.is_control())));
        assert!(a.last().unwrap().contains("1.5 KB"));
    }

    #[test]
    fn settings_default_off_and_auto_implies_receive() {
        let m = ShareManager::new(ShareOptions::default());
        assert_eq!(m.perm("d"), Perm::default());
        m.set_settings("d", None, Some(true)).unwrap();
        assert_eq!(
            m.perm("d"),
            Perm {
                receive: true,
                auto_accept: true
            }
        );
        m.set_settings("d", Some(false), None).unwrap();
        assert_eq!(m.perm("d"), Perm::default());
    }

    #[test]
    fn text_validation() {
        assert!(ShareManager::validate_text(ShareTextKind::Url, "https://a.b/c").is_ok());
        assert!(ShareManager::validate_text(ShareTextKind::Url, "file:///etc/passwd").is_err());
        assert!(ShareManager::validate_text(ShareTextKind::Text, "").is_err());
        let big = "x".repeat(MAX_SEND_TEXT_BYTES + 1);
        assert!(ShareManager::validate_text(ShareTextKind::Text, &big).is_err());
    }

    #[test]
    fn text_the_phone_may_send_is_not_refused_as_misbehaviour() {
        // The phone's own limit is 48 KiB (`kshare::MAX_TEXT_BYTES`); the desktop must hold that much,
        // or each long share counts against the link's rejected-packet budget.
        let m = ShareManager::new(ShareOptions::default());
        let text = "x".repeat(kshare::MAX_TEXT_BYTES);
        assert!(matches!(
            m.store_text("d", "Phone", TransferKind::Text, text),
            Disposition::Handled
        ));
    }

    #[test]
    fn a_queued_job_dropped_with_its_link_frees_the_slot() {
        let m = ShareManager::new(ShareOptions::default());
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hi").unwrap();
        let path = f.display().to_string();
        let a = m.begin_send_file("d", "Phone", &path).unwrap();
        let b = m.begin_send_file("d", "Phone", &path).unwrap();
        assert!(m.begin_send_file("d", "Phone", &path).is_err());
        // The link task ended before it ran these jobs.
        for id in [a, b] {
            m.drop_job(ShareJob::File {
                transfer_id: id,
                path: f.clone(),
            });
        }
        m.drop_job(ShareJob::Text {
            kind: ShareTextKind::Text,
            text: "x".into(),
        });
        assert!(m.begin_send_file("d", "Phone", &path).is_ok());
        let st = m.status(&["d".to_owned()]);
        assert!(
            st.transfers
                .iter()
                .any(|t| t.transfer_id == a && t.state == TransferState::Failed)
        );
    }

    #[test]
    fn send_requires_a_real_absolute_file() {
        let m = ShareManager::new(ShareOptions::default());
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hi").unwrap();
        assert!(m.begin_send_file("d", "Phone", "rel.txt").is_err());
        assert!(
            m.begin_send_file("d", "Phone", &dir.path().display().to_string())
                .is_err()
        );
        assert!(
            m.begin_send_file("d", "Phone", &dir.path().join("nope").display().to_string())
                .is_err()
        );
        let empty = dir.path().join("e");
        std::fs::write(&empty, b"").unwrap();
        assert!(
            m.begin_send_file("d", "Phone", &empty.display().to_string())
                .is_err()
        );
        let id = m
            .begin_send_file("d", "Phone", &f.display().to_string())
            .unwrap();
        // Per-device concurrency is bounded.
        let _ = m
            .begin_send_file("d", "Phone", &f.display().to_string())
            .unwrap();
        assert!(
            m.begin_send_file("d", "Phone", &f.display().to_string())
                .is_err()
        );
        m.cancel(id).unwrap();
        assert!(
            m.begin_send_file("d", "Phone", &f.display().to_string())
                .is_ok()
        );
    }
}
