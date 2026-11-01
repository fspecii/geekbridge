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

/// `~/Downloads/GeekBridge` (the XDG download directory on Linux when configured).
pub fn default_save_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "linux")]
    if let Some(dir) = xdg_download_dir(&home) {
        return dir.join("GeekBridge");
    }
    home.join("Downloads").join("GeekBridge")
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
struct Perm {
    receive: bool,
    auto_accept: bool,
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    devices: BTreeMap<String, Perm>,
}

struct Settings {
    path: Option<PathBuf>,
    devices: BTreeMap<String, Perm>,
}

impl Settings {
    fn open(path: Option<PathBuf>) -> Self {
        let devices = path
            .as_deref()
            .and_then(|p| fsutil::read_private_file(p).ok().flatten())
            .and_then(|b| serde_json::from_slice::<SettingsFile>(&b).ok())
            .filter(|f| f.version == 1 && f.devices.len() <= MAX_SETTINGS_DEVICES)
            .map(|f| f.devices)
            .unwrap_or_default();
        Self { path, devices }
    }

    fn persist(&self, next: &BTreeMap<String, Perm>) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&SettingsFile {
            version: 1,
            devices: next.clone(),
        })
        .map_err(|e| e.to_string())?;
        fsutil::atomic_write_private(path, &bytes).map_err(|e| e.to_string())
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
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>()
        };
        let size = size.map_or(String::new(), |n| format!(" ({})", human_size(n)));
        vec![
            "--app-name=GeekBridge".into(),
            "--wait".into(),
            "--action=accept=Accept".into(),
            "--action=decline=Decline".into(),
            "--expire-time=60000".into(),
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
                    _ => {}
                }
            }
        });
    }

    fn received_text(&self, device: &str, is_url: bool) {
        let what = if is_url { "a link" } else { "text" };
        let device: String = device
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        let _ = std::process::Command::new("notify-send")
            .args([
                "--app-name=GeekBridge",
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

struct Transfer {
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
}

pub struct ShareManager {
    opts: ShareOptions,
    inner: Mutex<Inner>,
    settings: Mutex<Settings>,
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
        Arc::new(Self {
            opts,
            inner: Mutex::new(Inner::default()),
            settings: Mutex::new(settings),
            sink: Mutex::new(None),
            notifier: Mutex::new(Arc::new(NoNotifier)),
        })
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
        let mut s = self.settings();
        let mut next = s.devices.clone();
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
        s.persist(&next)
            .map_err(|_| err(ErrorCode::Failed, "settings could not be saved"))?;
        s.devices = next;
        drop(s);
        if receive == Some(false) {
            self.decline_offers_of(device_id);
        }
        Ok(())
    }

    /// The device was unpaired: forget its permissions and stop its transfers.
    pub fn forget_device(&self, device_id: &str) {
        let mut s = self.settings();
        if s.devices.contains_key(device_id) {
            let mut next = s.devices.clone();
            next.remove(device_id);
            if s.persist(&next).is_ok() {
                s.devices = next;
            }
        }
        drop(s);
        let now = Instant::now();
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
        if !perm.receive {
            return Disposition::Rejected;
        }
        let content = match kshare::parse_share(packet, self.opts.max_file_bytes) {
            Ok(c) => c,
            Err(_) => return Disposition::Rejected,
        };
        match content {
            ShareContent::Ignored => Disposition::Handled,
            ShareContent::Text(t) => self.store_text(device_id, device_name, TransferKind::Text, t),
            ShareContent::Url(u) => self.store_text(device_id, device_name, TransferKind::Url, u),
            ShareContent::File(offer) => {
                self.offer_file(device_id, device_name, ctx, offer, perm.auto_accept)
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
    ) -> Disposition {
        let (id, name, size) = {
            let mut inner = self.inner();
            if !self.make_room(&mut inner) {
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
                return Disposition::Rejected;
            }
            inner.next_id += 1;
            let id = inner.next_id;
            let now = Instant::now();
            let (name, size) = (offer.name.clone(), offer.size);
            inner.items.push(Transfer {
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
        let (offer, ctx, bytes, cancel, device_id) = {
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
            (
                offer,
                ctx,
                t.bytes.clone(),
                t.cancel.clone(),
                t.info.device_id.clone(),
            )
        };
        let me = self.clone();
        let dir = self.opts.save_dir.clone();
        let max = self.opts.max_file_bytes;
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
        }
        self.emit(
            device,
            EventKind::ShareFinished {
                transfer_id: id,
                ok,
            },
        );
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
mod tests {
    use super::*;

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
