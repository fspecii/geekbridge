//! Desktop clipboard bridge between this computer and paired phones.
//!
//! * **Computer -> phone** is automatic: the OS clipboard is watched and every change is sent as a stock
//!   `kdeconnect.clipboard` packet to each opted-in, connected device (text and URLs; images and HTML
//!   only on request, see [`ClipboardService::push_now`]).
//! * **Phone -> computer** is *pulled*: the hotkey or `geekbridge-ctl clipboard paste-from-phone` asks
//!   the phone for its latest item ([`ClipboardService::pull`]). The phone answers only with an item
//!   copied within the freshness window while it is unlocked, so stale or locked-phone clipboard
//!   content never arrives. Images (PNG/JPEG) come as a KDE payload, never inline.
//!
//! Properties that hold throughout:
//!
//! * **Opt-in.** Nothing is read until at least one paired device has the `Clipboard` consent on and
//!   its link is up; the watcher stops when the last one goes. Content that was on the clipboard
//!   *before* a device was enabled is never sent (the reducer baseline is primed with it).
//! * **Policy.** Every peer has a [`ClipboardSync`]: echo/loop suppression, size limit, sensitive-item
//!   skip (password-manager hint where the platform exposes one). Text applied from one phone is not
//!   relayed to another.
//! * **Memory only.** The current clipboard text is held in RAM while sync is active and dropped when it
//!   stops. Nothing is persisted or logged; the status carries counters, never content. Only the
//!   freshness window, hotkey and image limit are stored.
//! * **Platform.** Linux uses `wl-clipboard` (Wayland, needs the compositor's data-control protocol) or
//!   `xclip` (X11); macOS uses `pbpaste`/`pbcopy`; Windows uses PowerShell. Wayland uses
//!   `wl-paste --watch` and X11 uses `clipnotify` when installed as change events (a slow safety
//!   poll remains); everything else is polled every 1.5 s. Images and HTML need `wl-clipboard`/`xclip`. See
//!   `docs/clipboard-sync.md`.
use crate::{now_ms, private::random_id};
use bridge_core::{
    clipboard::{
        ApplyRequest, ClipboardCapabilities, ClipboardPolicy, ClipboardSync,
        DEFAULT_MAX_TEXT_BYTES, LocalObservation, ObserveMode, ReadAccess, ReadContext,
        ReceiveMode, RejectReason, RemoteDecision, SendDecision, SendMode, Sensitivity, SkipReason,
        TextError, kde_connect_should_apply, message_from_kde_packet,
    },
    clipboard_wire::{
        self, CLIPBOARD, CLIPBOARD_CONNECT, DEFAULT_MAX_IMAGE_BYTES, DEFAULT_WINDOW_MS, ITEM, Item,
        ItemKind, Limits, NONE, NoneReason, PULL, RemoteClip,
    },
    hotkey::{self, DEFAULT_HOTKEY},
    ipc::{ClipboardDeviceStatus, ClipboardPullStatus, ClipboardStatus},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, MutexGuard, mpsc as std_mpsc},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

/// How often the OS clipboard is polled while sync is active and the platform has no change event.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1_500);
/// Safety-net poll while an event source is running (events can be missed after a tool restart).
const EVENT_SAFETY_POLL: Duration = Duration::from_secs(30);
/// Ceiling for one clipboard tool invocation.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);
/// After applying remote text, reads that still show the old clipboard are ignored for this long.
const WRITE_SETTLE_MS: u64 = 2_000;
/// A pull may need the user to tap the phone; give up after this long.
const PULL_TIMEOUT_MS: u64 = 90_000;
const MAX_PENDING_PULLS: usize = 4;
const OUTBOX_DEPTH: usize = 8;
const MAX_STATS: usize = 64;
pub const SETTINGS_FILE: &str = "clipboard-settings.json";

// ---------------------------------------------------------------------------- backend

/// Fixed, content-free failure codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// No clipboard tool/session is available.
    Unavailable,
    /// Reading or writing failed or timed out.
    Failed,
    TooLarge,
    NotUtf8,
    /// This backend cannot do that (e.g. images on `pbpaste`).
    Unsupported,
}
impl BackendError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "clipboard_unavailable",
            Self::Failed => "clipboard_io_failed",
            Self::TooLarge => "too_large",
            Self::NotUtf8 => "not_utf8",
            Self::Unsupported => "unsupported",
        }
    }
}

/// An image read from the clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct ClipImage {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}
impl std::fmt::Debug for ClipImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClipImage({}, {} bytes)", self.mime, self.bytes.len())
    }
}

/// An OS clipboard. Calls block (they run on tokio's blocking pool).
pub trait ClipboardBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ClipboardCapabilities;
    /// Current plain text, `Ok(None)` when the clipboard is empty or holds something else.
    fn read_text(&self, max_bytes: usize) -> Result<Option<String>, BackendError>;
    /// Sensitivity hint for the current clipboard contents.
    fn sensitivity(&self) -> Sensitivity;
    fn write_text(&self, text: &str) -> Result<(), BackendError>;
    /// `text/html` of the current clipboard, if offered.
    fn read_html(&self, _max_bytes: usize) -> Result<Option<String>, BackendError> {
        Ok(None)
    }
    /// PNG/JPEG of the current clipboard, if offered.
    fn read_image(&self, _max_bytes: usize) -> Result<Option<ClipImage>, BackendError> {
        Ok(None)
    }
    fn write_image(&self, _image: &ClipImage) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }
    /// Start a clipboard-change event source, if the platform has one. `None` means poll.
    fn change_events(&self) -> Option<ChangeStream> {
        None
    }
}

/// A running event source: one message per clipboard change. Dropping it stops the helper process.
pub struct ChangeStream {
    rx: mpsc::Receiver<()>,
    child: std::process::Child,
}
impl ChangeStream {
    /// Run `program args...` and treat each line of its stdout as one change.
    fn spawn(program: &str, args: &[&str]) -> Option<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let (tx, rx) = mpsc::channel(1);
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines() {
                if line.is_err() {
                    break;
                }
                // A full channel already holds a pending wakeup: coalesce.
                if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(()) {
                    break;
                }
            }
        });
        Some(Self { rx, child })
    }
    /// Wait for the next change; `None` when the helper exited.
    async fn changed(&mut self) -> Option<()> {
        self.rx.recv().await
    }
}
impl Drop for ChangeStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// In-memory clipboard for tests and `GEEKBRIDGE_CLIPBOARD=memory`.
#[derive(Default)]
pub struct MemoryClipboard {
    state: Mutex<MemoryState>,
}
#[derive(Default)]
struct MemoryState {
    text: Option<String>,
    html: Option<String>,
    image: Option<ClipImage>,
    sensitive: bool,
    writes: usize,
    fail_reads: bool,
}
impl MemoryClipboard {
    fn state(&self) -> MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Simulate the user copying something locally.
    pub fn user_copy(&self, text: &str, sensitive: bool) {
        let mut s = self.state();
        s.text = Some(text.to_owned());
        s.html = None;
        s.image = None;
        s.sensitive = sensitive;
    }
    pub fn user_copy_html(&self, text: &str, html: &str) {
        let mut s = self.state();
        s.text = Some(text.to_owned());
        s.html = Some(html.to_owned());
        s.image = None;
        s.sensitive = false;
    }
    pub fn user_copy_image(&self, image: ClipImage) {
        let mut s = self.state();
        s.text = None;
        s.html = None;
        s.image = Some(image);
        s.sensitive = false;
    }
    pub fn text(&self) -> Option<String> {
        self.state().text.clone()
    }
    pub fn image(&self) -> Option<ClipImage> {
        self.state().image.clone()
    }
    /// How many times the service wrote to it.
    pub fn writes(&self) -> usize {
        self.state().writes
    }
    pub fn set_fail_reads(&self, fail: bool) {
        self.state().fail_reads = fail;
    }
}
impl ClipboardBackend for MemoryClipboard {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn capabilities(&self) -> ClipboardCapabilities {
        ClipboardCapabilities {
            read: ReadAccess::Background,
            write: true,
            observe: ObserveMode::Polling,
            reports_sensitivity: true,
            can_mark_written_sensitive: false,
        }
    }
    fn read_text(&self, max_bytes: usize) -> Result<Option<String>, BackendError> {
        let s = self.state();
        if s.fail_reads {
            return Err(BackendError::Failed);
        }
        match &s.text {
            Some(t) if t.len() > max_bytes => Err(BackendError::TooLarge),
            other => Ok(other.clone()),
        }
    }
    fn sensitivity(&self) -> Sensitivity {
        if self.state().sensitive {
            Sensitivity::Sensitive
        } else {
            Sensitivity::Normal
        }
    }
    fn write_text(&self, text: &str) -> Result<(), BackendError> {
        let mut s = self.state();
        s.text = Some(text.to_owned());
        s.html = None;
        s.image = None;
        s.sensitive = false;
        s.writes += 1;
        Ok(())
    }
    fn read_html(&self, _max: usize) -> Result<Option<String>, BackendError> {
        Ok(self.state().html.clone())
    }
    fn read_image(&self, max_bytes: usize) -> Result<Option<ClipImage>, BackendError> {
        match self.state().image.clone() {
            Some(i) if i.bytes.len() > max_bytes => Err(BackendError::TooLarge),
            other => Ok(other),
        }
    }
    fn write_image(&self, image: &ClipImage) -> Result<(), BackendError> {
        let mut s = self.state();
        s.text = None;
        s.html = None;
        s.image = Some(image.clone());
        s.writes += 1;
        Ok(())
    }
}

/// Clipboard through command-line tools. The tool set is chosen once by [`detect_backend`].
pub struct ToolClipboard {
    tools: Tools,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tools {
    WlClipboard,
    Xclip,
    MacOs,
    PowerShell,
}

/// Password managers mark secrets with this target/type (KeePassXC, Bitwarden, Plasma's Klipper).
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

struct Captured {
    ok: bool,
    bytes: Vec<u8>,
    truncated: bool,
}

/// Run `program`, optionally feeding `stdin`, keeping at most `limit` bytes of stdout. Output is
/// bounded and the child is killed after [`TOOL_TIMEOUT`].
fn run_tool(
    program: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    limit: usize,
) -> Result<Captured, BackendError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| BackendError::Unavailable)?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // Tools read stdin to EOF before they daemonize; a broken pipe means the tool died.
        if pipe.write_all(data).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BackendError::Failed);
        }
    }
    let mut out = child.stdout.take().ok_or(BackendError::Failed)?;
    let (tx, rx) = std_mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut out).take(limit as u64 + 1).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    // Large images need longer than the 2 s used for text.
    let wait = TOOL_TIMEOUT + Duration::from_millis((limit / 4096) as u64);
    let bytes = match rx.recv_timeout(wait) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BackendError::Failed);
        }
    };
    let truncated = bytes.len() > limit;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BackendError::Failed);
            }
        }
    };
    Ok(Captured {
        ok: status.success(),
        bytes: bytes.into_iter().take(limit).collect(),
        truncated,
    })
}

fn utf8(c: Captured) -> Result<Option<String>, BackendError> {
    if c.truncated {
        return Err(BackendError::TooLarge);
    }
    if !c.ok || c.bytes.is_empty() {
        return Ok(None);
    }
    String::from_utf8(c.bytes)
        .map(Some)
        .map_err(|_| BackendError::NotUtf8)
}

fn lists(types: &[u8], wanted: &str) -> bool {
    String::from_utf8_lossy(types)
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case(wanted))
}

impl ToolClipboard {
    fn list_types(&self) -> Option<Vec<u8>> {
        let c = match self.tools {
            Tools::WlClipboard => run_tool("wl-paste", &["--list-types"], None, 4096),
            Tools::Xclip => run_tool(
                "xclip",
                &["-selection", "clipboard", "-o", "-t", "TARGETS"],
                None,
                4096,
            ),
            Tools::MacOs | Tools::PowerShell => return None,
        };
        c.ok().filter(|c| c.ok).map(|c| c.bytes)
    }

    fn read_type(&self, mime: &str, max: usize) -> Result<Captured, BackendError> {
        match self.tools {
            Tools::WlClipboard => {
                run_tool("wl-paste", &["--no-newline", "--type", mime], None, max)
            }
            Tools::Xclip => run_tool(
                "xclip",
                &["-selection", "clipboard", "-o", "-t", mime],
                None,
                max,
            ),
            Tools::MacOs | Tools::PowerShell => Err(BackendError::Unsupported),
        }
    }
}

impl ToolClipboard {
    fn has_events(&self) -> bool {
        match self.tools {
            Tools::WlClipboard => true,
            // `clipnotify` blocks on XFixes selection notify; no XFixes binding is linked here.
            Tools::Xclip => on_path("clipnotify"),
            _ => false,
        }
    }
}

impl ClipboardBackend for ToolClipboard {
    fn change_events(&self) -> Option<ChangeStream> {
        match self.tools {
            // Runs `echo` once per change (and once at start): one stdout line per event.
            Tools::WlClipboard => ChangeStream::spawn("wl-paste", &["--watch", "echo"]),
            Tools::Xclip if on_path("clipnotify") => {
                // clipnotify exits after one change; loop it in a shell so one process serves all.
                ChangeStream::spawn("sh", &["-c", "while clipnotify; do echo; done"])
            }
            _ => None,
        }
    }
    fn name(&self) -> &'static str {
        match self.tools {
            Tools::WlClipboard => "wl-clipboard",
            Tools::Xclip => "xclip",
            Tools::MacOs => "pbpaste/pbcopy",
            Tools::PowerShell => "PowerShell",
        }
    }
    fn capabilities(&self) -> ClipboardCapabilities {
        ClipboardCapabilities {
            read: ReadAccess::Background,
            write: true,
            observe: if self.has_events() {
                ObserveMode::Event
            } else {
                ObserveMode::Polling
            },
            reports_sensitivity: matches!(self.tools, Tools::WlClipboard | Tools::Xclip),
            can_mark_written_sensitive: false,
        }
    }
    fn read_text(&self, max_bytes: usize) -> Result<Option<String>, BackendError> {
        match self.tools {
            Tools::WlClipboard => utf8(run_tool(
                "wl-paste",
                &["--no-newline", "--type", "text"],
                None,
                max_bytes,
            )?),
            Tools::Xclip => {
                for target in ["UTF8_STRING", "STRING"] {
                    let c = self.read_type(target, max_bytes)?;
                    if c.ok || c.truncated {
                        return utf8(c);
                    }
                }
                Ok(None)
            }
            Tools::MacOs => utf8(run_tool("pbpaste", &[], None, max_bytes)?),
            Tools::PowerShell => utf8(run_tool(
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
                     $t=Get-Clipboard -Raw; if($t){[Console]::Out.Write($t)}",
                ],
                None,
                max_bytes,
            )?),
        }
    }
    fn sensitivity(&self) -> Sensitivity {
        match self.list_types() {
            Some(t) if lists(&t, PASSWORD_HINT) => Sensitivity::Sensitive,
            Some(_) => Sensitivity::Normal,
            None => Sensitivity::Unknown,
        }
    }
    fn write_text(&self, text: &str) -> Result<(), BackendError> {
        let ok = |c: Captured| {
            if c.ok {
                Ok(())
            } else {
                Err(BackendError::Failed)
            }
        };
        match self.tools {
            Tools::WlClipboard => ok(run_tool("wl-copy", &[], Some(text.as_bytes()), 16)?),
            Tools::Xclip => ok(run_tool(
                "xclip",
                &["-selection", "clipboard", "-i"],
                Some(text.as_bytes()),
                16,
            )?),
            Tools::MacOs => ok(run_tool("pbcopy", &[], Some(text.as_bytes()), 16)?),
            Tools::PowerShell => ok(run_tool(
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::InputEncoding=[Text.Encoding]::UTF8;\
                     Set-Clipboard -Value ([Console]::In.ReadToEnd())",
                ],
                Some(text.as_bytes()),
                16,
            )?),
        }
    }
    fn read_html(&self, max_bytes: usize) -> Result<Option<String>, BackendError> {
        match self.list_types() {
            Some(t) if lists(&t, "text/html") => utf8(self.read_type("text/html", max_bytes)?),
            _ => Ok(None),
        }
    }
    fn read_image(&self, max_bytes: usize) -> Result<Option<ClipImage>, BackendError> {
        let Some(types) = self.list_types() else {
            return Ok(None);
        };
        for mime in ["image/png", "image/jpeg"] {
            if !lists(&types, mime) {
                continue;
            }
            let c = self.read_type(mime, max_bytes)?;
            if c.truncated {
                return Err(BackendError::TooLarge);
            }
            // The magic bytes, not the advertised type, decide what the data is.
            if let (true, Some(real)) = (c.ok, clipboard_wire::sniff_image(&c.bytes)) {
                return Ok(Some(ClipImage {
                    mime: real,
                    bytes: c.bytes,
                }));
            }
        }
        Ok(None)
    }
    fn write_image(&self, image: &ClipImage) -> Result<(), BackendError> {
        let ok = |c: Captured| {
            if c.ok {
                Ok(())
            } else {
                Err(BackendError::Failed)
            }
        };
        match self.tools {
            Tools::WlClipboard => ok(run_tool(
                "wl-copy",
                &["--type", image.mime],
                Some(&image.bytes),
                16,
            )?),
            Tools::Xclip => ok(run_tool(
                "xclip",
                &["-selection", "clipboard", "-t", image.mime, "-i"],
                Some(&image.bytes),
                16,
            )?),
            Tools::MacOs | Tools::PowerShell => Err(BackendError::Unsupported),
        }
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(program);
        p.is_file() || p.with_extension("exe").is_file()
    })
}

/// Pick a clipboard backend for this session, or `None` (clipboard sync then reports "unavailable").
/// `GEEKBRIDGE_CLIPBOARD=off` disables it; `memory` selects an in-memory clipboard (development).
pub fn detect_backend() -> Option<Arc<dyn ClipboardBackend>> {
    match std::env::var("GEEKBRIDGE_CLIPBOARD").as_deref() {
        Ok("off") => return None,
        Ok("memory") => return Some(Arc::new(MemoryClipboard::default())),
        _ => {}
    }
    let env_set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let tools = if cfg!(target_os = "macos") {
        Tools::MacOs
    } else if cfg!(target_os = "windows") {
        Tools::PowerShell
    } else if env_set("WAYLAND_DISPLAY") && on_path("wl-paste") && on_path("wl-copy") {
        Tools::WlClipboard
    } else if env_set("DISPLAY") && on_path("xclip") {
        Tools::Xclip
    } else {
        return None;
    };
    Some(Arc::new(ToolClipboard { tools }))
}

// ---------------------------------------------------------------------------- settings

/// The only clipboard state that is stored on disk. Never any content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window_ms: u64,
    pub hotkey: String,
    pub max_image_bytes: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_WINDOW_MS,
            hotkey: DEFAULT_HOTKEY.to_owned(),
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        }
    }
}
impl Settings {
    /// Clamp everything to its permitted range; an unusable hotkey falls back to the default.
    fn sanitized(mut self) -> Self {
        self.window_ms = clipboard_wire::clamp_window_ms(self.window_ms);
        self.max_image_bytes = clipboard_wire::clamp_image_bytes(self.max_image_bytes);
        self.hotkey = hotkey::normalize(&self.hotkey).unwrap_or_else(|_| DEFAULT_HOTKEY.to_owned());
        self
    }
}

// ---------------------------------------------------------------------------- service

/// One outgoing item for the daemon to put on a link.
#[derive(Debug)]
pub enum ClipOut {
    Packet {
        kind: &'static str,
        body: Map<String, Value>,
    },
    /// Announce with a payload and stream `bytes` after the packet.
    Image {
        body: Map<String, Value>,
        bytes: Vec<u8>,
    },
}

/// What the link negotiated for clipboard use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkCaps {
    /// The peer accepts stock `kdeconnect.clipboard`.
    pub stock: bool,
    /// The peer speaks the pull/item protocol in both directions.
    pub pull: bool,
}

impl LinkCaps {
    /// From the packet types negotiated for `outgoing` (we send) and `incoming` (we receive).
    pub fn from_negotiated(outgoing: &[String], incoming: &[String]) -> Self {
        let has = |v: &[String], k: &str| v.iter().any(|x| x == k);
        Self {
            stock: has(outgoing, CLIPBOARD),
            pull: has(outgoing, PULL)
                && has(outgoing, ITEM)
                && has(incoming, ITEM)
                && has(incoming, NONE),
        }
    }
}

#[derive(Default, Clone)]
struct Stats {
    sent: u64,
    received: u64,
    skipped: u64,
    last_sent: Option<u64>,
    last_received: Option<u64>,
    last_skip: Option<&'static str>,
}

struct Peer {
    /// Which `link_up` created this entry, so a displaced session cannot remove its successor.
    generation: u64,
    sync: Option<ClipboardSync>,
    tx: mpsc::Sender<ClipOut>,
    caps: LinkCaps,
    name: String,
}

struct PendingPull {
    device_id: String,
    deadline_ms: u64,
}

/// A phone is sending an image: the daemon opens the payload socket and calls back.
#[derive(Clone, Debug)]
pub struct ImageTicket {
    pub device_id: String,
    pub request_id: Option<String>,
    pub mime: &'static str,
    pub size: u64,
    pub max: u64,
}

/// What `on_packet` asks the daemon to do next.
#[derive(Debug)]
pub enum Next {
    Nothing,
    ReceiveImage(ImageTicket),
}

struct Inner {
    next_generation: u64,
    peers: HashMap<String, Peer>,
    stats: HashMap<String, Stats>,
    pulls: HashMap<String, PendingPull>,
    last_pull: Option<ClipboardPullStatus>,
    settings: Settings,
    /// Current clipboard text, only while at least one peer is enabled.
    last_text: Option<String>,
    /// The watcher has taken its first reading since it (re)started.
    primed: bool,
    /// Wall time of the last change we observed or applied; service start until one happens.
    local_update_ms: i64,
    /// Whether `local_update_ms` really reflects a clipboard change (not just the start time).
    update_known: bool,
    watching: bool,
    /// Text we just wrote and the monotonic time until which stale reads are ignored.
    expected: Option<(String, u64)>,
    problem: Option<&'static str>,
    oversize_reported: bool,
    binding: &'static str,
}

pub struct ClipboardService {
    backend: Arc<dyn ClipboardBackend>,
    local_device: String,
    started: Instant,
    poll: Duration,
    /// Wakes the watcher when sync is switched off so it stops (and drops its event source) at once.
    wake: tokio::sync::Notify,
    settings_path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

fn policy_for(caps: &ClipboardCapabilities) -> ClipboardPolicy {
    ClipboardPolicy {
        send: if caps.supports_automatic_send() {
            SendMode::Automatic
        } else {
            SendMode::Off
        },
        receive: if caps.write {
            ReceiveMode::Apply
        } else {
            ReceiveMode::Off
        },
        ..ClipboardPolicy::default()
    }
}

fn skip_label(reason: SkipReason) -> Option<&'static str> {
    match reason {
        SkipReason::Sensitive => Some("sensitive"),
        SkipReason::Invalid(TextError::TooLarge) => Some("too_large"),
        SkipReason::Invalid(TextError::Empty) => None,
        SkipReason::Invalid(_) => Some("invalid_text"),
        SkipReason::SendDisabled | SkipReason::ReadNotPermitted => Some("send_unavailable"),
        // Unchanged / echo are the loop-suppression working, not a user-visible skip.
        _ => None,
    }
}

fn reject_label(reason: RejectReason) -> Option<&'static str> {
    match reason {
        RejectReason::Duplicate | RejectReason::OwnOrigin => None,
        RejectReason::Invalid(TextError::TooLarge) => Some("too_large"),
        RejectReason::Sensitive => Some("sensitive"),
        _ => Some("rejected"),
    }
}

fn none_label(reason: NoneReason) -> &'static str {
    reason.as_str()
}

impl ClipboardService {
    pub fn new(backend: Arc<dyn ClipboardBackend>, local_device: &str) -> Arc<Self> {
        Self::build(backend, local_device, POLL_INTERVAL, None)
    }

    pub fn with_poll(
        backend: Arc<dyn ClipboardBackend>,
        local_device: &str,
        poll: Duration,
    ) -> Arc<Self> {
        Self::build(backend, local_device, poll, None)
    }

    /// Like [`new`](Self::new) but loads and saves the settings file at `path`.
    pub fn with_settings_file(
        backend: Arc<dyn ClipboardBackend>,
        local_device: &str,
        path: PathBuf,
    ) -> Arc<Self> {
        Self::build(backend, local_device, POLL_INTERVAL, Some(path))
    }

    fn build(
        backend: Arc<dyn ClipboardBackend>,
        local_device: &str,
        poll: Duration,
        settings_path: Option<PathBuf>,
    ) -> Arc<Self> {
        let settings = settings_path
            .as_deref()
            .and_then(|p| bridge_kde::fsutil::read_private_file(p).ok().flatten())
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default()
            .sanitized();
        Arc::new(Self {
            backend,
            local_device: local_device.to_owned(),
            started: Instant::now(),
            poll,
            wake: tokio::sync::Notify::new(),
            settings_path,
            inner: Mutex::new(Inner {
                next_generation: 0,
                peers: HashMap::new(),
                stats: HashMap::new(),
                pulls: HashMap::new(),
                last_pull: None,
                settings,
                last_text: None,
                primed: false,
                local_update_ms: now_ms() as i64,
                update_known: false,
                watching: false,
                expected: None,
                problem: None,
                oversize_reported: false,
                binding: "unsupported",
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Monotonic milliseconds for the reducers.
    fn mono(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn max_bytes(&self) -> usize {
        policy_for(&self.backend.capabilities()).max_text_bytes
    }

    fn limits(&self) -> Limits {
        Limits {
            max_text_bytes: self.max_bytes(),
            max_image_bytes: self.lock().settings.max_image_bytes,
        }
    }

    fn new_sync(&self, device_id: &str) -> Option<ClipboardSync> {
        let caps = self.backend.capabilities();
        ClipboardSync::new(&self.local_device, device_id, policy_for(&caps), caps).ok()
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// Apply a settings change. Fields are validated (hotkey) or clamped (window, image size);
    /// the result is stored in the settings file when one is configured.
    pub fn update_settings(
        &self,
        window_ms: Option<u64>,
        hotkey_text: Option<&str>,
        max_image_bytes: Option<u64>,
    ) -> Result<Settings, hotkey::HotkeyError> {
        let new_hotkey = hotkey_text.map(hotkey::normalize).transpose()?;
        let snapshot = {
            let mut g = self.lock();
            if let Some(w) = window_ms {
                g.settings.window_ms = clipboard_wire::clamp_window_ms(w);
            }
            if let Some(h) = new_hotkey {
                g.settings.hotkey = h;
            }
            if let Some(m) = max_image_bytes {
                g.settings.max_image_bytes = clipboard_wire::clamp_image_bytes(m);
            }
            g.settings.clone()
        };
        if let Some(path) = &self.settings_path
            && let Ok(bytes) = serde_json::to_vec(&snapshot)
        {
            let _ = bridge_kde::fsutil::atomic_write_private(path, &bytes);
        }
        Ok(snapshot)
    }

    pub fn set_binding_state(&self, state: &'static str) {
        self.lock().binding = state;
    }

    /// Is any device opted in with a live link? (The hotkey is only bound while this holds.)
    pub fn any_enabled(&self) -> bool {
        self.lock().peers.values().any(|p| p.sync.is_some())
    }

    /// A trusted link to `device_id` is up. Returns the receiver the link task must drain into the
    /// socket.
    pub fn link_up(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        caps: LinkCaps,
        enabled: bool,
    ) -> mpsc::Receiver<ClipOut> {
        self.link_up_generation(device_id, name, caps, enabled).0
    }

    /// [`Self::link_up`], also returning the generation to pass to [`Self::link_down_generation`].
    pub fn link_up_generation(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        caps: LinkCaps,
        enabled: bool,
    ) -> (mpsc::Receiver<ClipOut>, u64) {
        let (tx, rx) = mpsc::channel(OUTBOX_DEPTH);
        let generation;
        {
            let mut g = self.lock();
            g.next_generation += 1;
            generation = g.next_generation;
            g.peers.insert(
                device_id.to_owned(),
                Peer {
                    generation,
                    sync: None,
                    tx,
                    caps,
                    name: name.to_owned(),
                },
            );
            if g.stats.len() >= MAX_STATS && !g.stats.contains_key(device_id) {
                g.stats.clear();
            }
            g.stats.entry(device_id.to_owned()).or_default();
        }
        self.set_enabled(device_id, enabled);
        (rx, generation)
    }

    /// Remove `device_id` only if it is still the link `generation` created. A session displaced by a
    /// reconnect ends after its successor registered, and must leave the successor alone.
    pub fn link_down_generation(&self, device_id: &str, generation: u64) {
        self.remove_peer(device_id, Some(generation));
    }

    pub fn link_down(&self, device_id: &str) {
        self.remove_peer(device_id, None);
    }

    fn remove_peer(&self, device_id: &str, only_generation: Option<u64>) {
        let mut g = self.lock();
        if let (Some(want), Some(p)) = (only_generation, g.peers.get(device_id))
            && p.generation != want
        {
            return;
        }
        g.peers.remove(device_id);
        g.pulls.retain(|_, p| p.device_id != device_id);
        Self::forget_text_if_idle(&mut g);
    }

    /// The device was unpaired: also drop its counters.
    pub fn forget(&self, device_id: &str) {
        self.link_down(device_id);
        self.lock().stats.remove(device_id);
    }

    fn forget_text_if_idle(g: &mut Inner) {
        if !g.peers.values().any(|p| p.sync.is_some()) {
            g.last_text = None;
            g.primed = false;
            g.expected = None;
            g.oversize_reported = false;
            g.update_known = false;
        }
    }

    /// The user's per-device opt-in changed (or the link just came up).
    pub fn set_enabled(self: &Arc<Self>, device_id: &str, enabled: bool) {
        let sync = if enabled {
            self.new_sync(device_id)
        } else {
            None
        };
        let (start_watcher, announce) = {
            let mut g = self.lock();
            let baseline = g.last_text.clone();
            let known = g.update_known;
            let stamp = g.local_update_ms;
            let Some(peer) = g.peers.get_mut(device_id) else {
                return;
            };
            peer.sync = sync;
            if let (Some(sync), Some(text)) = (peer.sync.as_mut(), baseline.as_deref()) {
                // Whatever is on the clipboard now was copied before opt-in: never send it.
                sync.set_baseline(text);
            }
            let sendable = peer.sync.is_some() && peer.caps.stock;
            // Stock behaviour: tell a peer our state when the link comes up, but only content we
            // saw change since sync started, so pre-existing clipboard text is not announced.
            let announce = sendable
                .then_some((known, baseline))
                .and_then(|(k, t)| t.filter(|_| k))
                .map(|t| (peer.tx.clone(), t, stamp));
            let start = peer.sync.is_some() && !g.watching;
            if start {
                g.watching = true;
            }
            if !enabled {
                g.pulls.retain(|_, p| p.device_id != device_id);
                Self::forget_text_if_idle(&mut g);
            }
            (start, announce)
        };
        if let Some((tx, text, stamp)) = announce {
            let _ = tx.try_send(ClipOut::Packet {
                kind: CLIPBOARD_CONNECT,
                body: clipboard_wire::connect_body(&text, stamp),
            });
        }
        if !enabled {
            self.wake.notify_one();
        }
        if start_watcher {
            let me = self.clone();
            tokio::spawn(async move { me.watch().await });
        }
    }

    // ------------------------------------------------------------ phone -> computer

    /// Ask enabled, connected phones (or just `device_id`) for their latest eligible item.
    /// Returns how many requests were sent.
    pub fn pull(&self, device_id: Option<&str>) -> Result<usize, &'static str> {
        let now = self.mono();
        let mut g = self.lock();
        g.pulls.retain(|_, p| p.deadline_ms > now);
        let window = g.settings.window_ms;
        let targets: Vec<String> = g
            .peers
            .iter()
            .filter(|(id, p)| p.sync.is_some() && device_id.is_none_or(|want| want == id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        if targets.is_empty() {
            return Err(if device_id.is_some() {
                "device_not_enabled_or_connected"
            } else {
                "no_enabled_phone_connected"
            });
        }
        let mut sent = 0;
        let mut unsupported = false;
        for id in targets {
            let peer = &g.peers[&id];
            if !peer.caps.pull {
                unsupported = true;
                continue;
            }
            if g.pulls.len() >= MAX_PENDING_PULLS {
                return Err("too_many_requests");
            }
            let request_id = random_id();
            let out = ClipOut::Packet {
                kind: PULL,
                body: clipboard_wire::pull_body(&request_id, window),
            };
            if peer.tx.try_send(out).is_err() {
                continue;
            }
            g.pulls.insert(
                request_id,
                PendingPull {
                    device_id: id.clone(),
                    deadline_ms: now + PULL_TIMEOUT_MS,
                },
            );
            g.last_pull = Some(ClipboardPullStatus {
                device_id: id,
                state: "waiting".into(),
                reason: None,
                kind: None,
                at_unix_ms: now_ms(),
            });
            sent += 1;
        }
        match (sent, unsupported) {
            (0, true) => Err("phone_app_cannot_be_asked"),
            (0, false) => Err("link_busy"),
            _ => Ok(sent),
        }
    }

    fn finish_pull(g: &mut Inner, device_id: &str, outcome: Result<ItemKind, &'static str>) {
        let (state, reason, kind) = match outcome {
            Ok(k) => ("delivered", None, Some(k.as_str().to_owned())),
            Err(r) => ("none", Some(r.to_owned()), None),
        };
        g.last_pull = Some(ClipboardPullStatus {
            device_id: device_id.to_owned(),
            state: state.into(),
            reason,
            kind,
            at_unix_ms: now_ms(),
        });
    }

    /// A clipboard packet arrived on `device_id`'s trusted link. `payload_port` is set when the
    /// packet announced a payload.
    pub fn on_packet(
        self: &Arc<Self>,
        device_id: &str,
        kind: &str,
        body: &Map<String, Value>,
        payload_port: Option<u16>,
    ) -> Next {
        match kind {
            ITEM => self.on_item(device_id, body, payload_port),
            NONE => {
                self.on_none(device_id, body);
                Next::Nothing
            }
            _ => {
                self.on_stock(device_id, kind, body);
                Next::Nothing
            }
        }
    }

    fn on_none(&self, device_id: &str, body: &Map<String, Value>) {
        let Ok((request_id, reason)) = clipboard_wire::parse_none(body) else {
            return;
        };
        let mut g = self.lock();
        if g.pulls
            .get(&request_id)
            .is_some_and(|p| p.device_id == device_id)
        {
            g.pulls.remove(&request_id);
            Self::finish_pull(&mut g, device_id, Err(none_label(reason)));
        }
    }

    fn on_item(
        self: &Arc<Self>,
        device_id: &str,
        body: &Map<String, Value>,
        payload_port: Option<u16>,
    ) -> Next {
        let limits = self.limits();
        let Ok(parsed) = clipboard_wire::parse_item(body, &limits) else {
            let mut g = self.lock();
            if let Some(st) = g.stats.get_mut(device_id) {
                st.skipped += 1;
                st.last_skip = Some("rejected");
            }
            return Next::Nothing;
        };
        {
            let mut g = self.lock();
            let enabled = g.peers.get(device_id).is_some_and(|p| p.sync.is_some());
            if !enabled {
                return Next::Nothing;
            }
            if let Some(req) = &parsed.request_id {
                // A response must answer one of our own requests, from the device we asked.
                let ours = g.pulls.get(req).is_some_and(|p| p.device_id == device_id);
                if !ours {
                    return Next::Nothing;
                }
                if !clipboard_wire::within_window(parsed.age_ms, g.settings.window_ms) {
                    g.pulls.remove(req);
                    Self::finish_pull(&mut g, device_id, Err("expired"));
                    return Next::Nothing;
                }
            }
        }
        match parsed.item {
            Item::Text { kind, text, .. } => {
                let applied = self.apply_text(device_id, text);
                let mut g = self.lock();
                if let Some(req) = &parsed.request_id {
                    g.pulls.remove(req);
                    Self::finish_pull(&mut g, device_id, applied.then_some(kind).ok_or("failed"));
                }
                Next::Nothing
            }
            Item::Image { mime, size } => {
                let Some(port) = payload_port else {
                    let mut g = self.lock();
                    if let Some(req) = &parsed.request_id {
                        g.pulls.remove(req);
                        Self::finish_pull(&mut g, device_id, Err("failed"));
                    }
                    return Next::Nothing;
                };
                let _ = port;
                Next::ReceiveImage(ImageTicket {
                    device_id: device_id.to_owned(),
                    request_id: parsed.request_id,
                    mime,
                    size,
                    max: limits.max_image_bytes,
                })
            }
        }
    }

    /// The daemon received the image bytes of `ticket`.
    pub fn image_received(self: &Arc<Self>, ticket: &ImageTicket, bytes: Vec<u8>) {
        let valid = clipboard_wire::sniff_image(&bytes) == Some(ticket.mime)
            && bytes.len() as u64 == ticket.size;
        let wall = now_ms();
        let backend = self.backend.clone();
        let me = self.clone();
        let ticket = ticket.clone();
        if !valid {
            self.image_failed(&ticket, "rejected");
            return;
        }
        tokio::task::spawn_blocking(move || {
            let result = backend.write_image(&ClipImage {
                mime: ticket.mime,
                bytes,
            });
            let mut g = me.lock();
            match result {
                Ok(()) => {
                    g.problem = None;
                    // The text watcher must not mistake the image for "clipboard emptied".
                    g.last_text = None;
                    g.local_update_ms = wall as i64;
                    g.update_known = true;
                    let st = g.stats.entry(ticket.device_id.clone()).or_default();
                    st.received += 1;
                    st.last_received = Some(wall);
                    if let Some(req) = &ticket.request_id {
                        g.pulls.remove(req);
                    }
                    if ticket.request_id.is_some() {
                        Self::finish_pull(&mut g, &ticket.device_id, Ok(ItemKind::Image));
                    }
                }
                Err(e) => {
                    g.problem = Some(e.code());
                    drop(g);
                    me.image_failed(&ticket, "failed");
                }
            }
        });
    }

    /// The payload transfer failed or was refused.
    pub fn image_failed(&self, ticket: &ImageTicket, reason: &'static str) {
        let mut g = self.lock();
        let st = g.stats.entry(ticket.device_id.clone()).or_default();
        st.skipped += 1;
        st.last_skip = Some(reason);
        if let Some(req) = &ticket.request_id {
            g.pulls.remove(req);
            Self::finish_pull(&mut g, &ticket.device_id, Err(reason));
        }
    }

    /// Run `text` through the peer's reducer and write it to the OS clipboard. True if applied.
    fn apply_text(self: &Arc<Self>, device_id: &str, text: String) -> bool {
        let now = self.mono();
        let wall = now_ms();
        let apply = {
            let mut g = self.lock();
            let Some(peer) = g.peers.get_mut(device_id) else {
                return false;
            };
            let Some(sync) = peer.sync.as_mut() else {
                return false;
            };
            let policy = sync.policy().clone();
            let msg = message_from_kde_packet(text, device_id, random_id(), &policy);
            match sync.remote_received(msg, now) {
                RemoteDecision::Apply(req) => {
                    for (id, other) in g.peers.iter_mut() {
                        if id != device_id
                            && let Some(s) = other.sync.as_mut()
                        {
                            s.note_applied_elsewhere(&req.text, now);
                        }
                    }
                    g.last_text = Some(req.text.clone());
                    g.local_update_ms = wall as i64;
                    g.update_known = true;
                    g.expected = Some((req.text.clone(), now + WRITE_SETTLE_MS));
                    let st = g.stats.entry(device_id.to_owned()).or_default();
                    st.received += 1;
                    st.last_received = Some(wall);
                    Some(req)
                }
                RemoteDecision::Reject(reason) => {
                    if let Some(label) = reject_label(reason) {
                        let st = g.stats.entry(device_id.to_owned()).or_default();
                        st.skipped += 1;
                        st.last_skip = Some(label);
                    }
                    None
                }
                RemoteDecision::Staged { .. } => None,
            }
        };
        match apply {
            Some(req) => {
                self.write_in_background(req);
                true
            }
            None => false,
        }
    }

    /// Stock `kdeconnect.clipboard` / `.connect`.
    fn on_stock(self: &Arc<Self>, device_id: &str, kind: &str, body: &Map<String, Value>) {
        let max = self.max_bytes();
        let local_update = {
            let g = self.lock();
            if g.peers.get(device_id).is_none_or(|p| p.sync.is_none()) {
                return; // not opted in: ignored, not counted, never read
            }
            g.local_update_ms
        };
        let content = match clipboard_wire::parse(kind, body, max) {
            Ok(RemoteClip::Update { content }) => content,
            Ok(RemoteClip::Connect {
                content,
                timestamp_ms,
            }) => {
                if !kde_connect_should_apply(timestamp_ms, local_update) {
                    return; // older than our clipboard: expected, silent
                }
                content
            }
            Err(e) => {
                let mut g = self.lock();
                let st = g.stats.entry(device_id.to_owned()).or_default();
                st.skipped += 1;
                st.last_skip = Some(if e == clipboard_wire::WireError::TooLarge {
                    "too_large"
                } else {
                    "rejected"
                });
                return;
            }
        };
        self.apply_text(device_id, content);
    }

    fn write_in_background(self: &Arc<Self>, req: ApplyRequest) {
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = me.backend.write_text(&req.text);
            let mut g = me.lock();
            match result {
                Ok(()) => g.problem = None,
                Err(e) => {
                    g.problem = Some(e.code());
                    g.expected = None;
                }
            }
        });
    }

    // ------------------------------------------------------------ computer -> phone

    /// Send what is on the clipboard right now, as the richest item the phone understands: an
    /// image (streamed), else HTML with its plain text, else text. This is the explicit action
    /// behind `geekbridge-ctl clipboard push-to-phone`; unlike the automatic path it also covers
    /// images and HTML. Sensitive clipboard contents are never sent. Returns devices sent to.
    pub async fn push_now(
        self: &Arc<Self>,
        device_id: Option<&str>,
    ) -> Result<usize, &'static str> {
        let targets: Vec<(String, mpsc::Sender<ClipOut>, LinkCaps)> = {
            let g = self.lock();
            g.peers
                .iter()
                .filter(|(id, p)| {
                    p.sync.is_some() && device_id.is_none_or(|want| want == id.as_str())
                })
                .map(|(id, p)| (id.clone(), p.tx.clone(), p.caps))
                .collect()
        };
        if targets.is_empty() {
            return Err("no_enabled_phone_connected");
        }
        let backend = self.backend.clone();
        let limits = self.limits();
        let read = tokio::task::spawn_blocking(move || {
            if backend.sensitivity() == Sensitivity::Sensitive {
                return Err("sensitive");
            }
            if let Ok(Some(img)) = backend.read_image(limits.max_image_bytes as usize) {
                return Ok(Pushed::Image(img));
            }
            let text = match backend.read_text(limits.max_text_bytes) {
                Ok(Some(t)) if !t.is_empty() && !t.contains('\0') => t,
                Err(BackendError::TooLarge) => return Err("too_large"),
                _ => return Err("empty"),
            };
            let html = backend
                .read_html(limits.max_text_bytes)
                .ok()
                .flatten()
                .filter(|h| h.len() <= limits.max_text_bytes);
            Ok(Pushed::Text(text, html))
        })
        .await
        .map_err(|_| "failed")??;
        enum Pushed {
            Image(ClipImage),
            Text(String, Option<String>),
        }
        let mut sent = 0;
        for (id, tx, caps) in targets {
            let out = match &read {
                Pushed::Image(img) if caps.pull => ClipOut::Image {
                    body: clipboard_wire::image_item_body(
                        img.mime,
                        img.bytes.len() as u64,
                        None,
                        0,
                    ),
                    bytes: img.bytes.clone(),
                },
                Pushed::Text(text, Some(html)) if caps.pull => ClipOut::Packet {
                    kind: ITEM,
                    body: clipboard_wire::text_item_body(ItemKind::Html, text, Some(html), None, 0),
                },
                Pushed::Text(text, None) if caps.pull => ClipOut::Packet {
                    kind: ITEM,
                    body: clipboard_wire::text_item_body(
                        clipboard_wire::text_kind(text),
                        text,
                        None,
                        None,
                        0,
                    ),
                },
                Pushed::Text(text, _) if caps.stock => ClipOut::Packet {
                    kind: CLIPBOARD,
                    body: clipboard_wire::update_body(text),
                },
                _ => continue,
            };
            if tx.try_send(out).is_ok() {
                sent += 1;
                let mut g = self.lock();
                let st = g.stats.entry(id).or_default();
                st.sent += 1;
                st.last_sent = Some(now_ms());
            }
        }
        if sent == 0 {
            Err("phone_cannot_receive_this")
        } else {
            Ok(sent)
        }
    }

    async fn watch(self: Arc<Self>) {
        let backend = self.backend.clone();
        let mut events = tokio::task::spawn_blocking(move || backend.change_events())
            .await
            .ok()
            .flatten();
        loop {
            let interval = if events.is_some() {
                EVENT_SAFETY_POLL.max(self.poll)
            } else {
                self.poll
            };
            let mut stream_ended = false;
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = self.wake.notified() => {}
                ev = async { events.as_mut().unwrap().changed().await }, if events.is_some() => {
                    stream_ended = ev.is_none();
                }
            }
            if stream_ended {
                events = None; // helper died: fall back to polling
            }
            {
                let mut g = self.lock();
                if !g.peers.values().any(|p| p.sync.is_some()) {
                    g.watching = false;
                    return;
                }
            }
            let backend = self.backend.clone();
            let max = self.max_bytes();
            let read = tokio::task::spawn_blocking(move || backend.read_text(max)).await;
            let read = match read {
                Ok(r) => r,
                Err(_) => Err(BackendError::Failed),
            };
            self.observe(read).await;
        }
    }

    async fn observe(self: &Arc<Self>, read: Result<Option<String>, BackendError>) {
        let now = self.mono();
        // Decide, under the lock, whether this reading is a candidate change.
        let text = {
            let mut g = self.lock();
            match read {
                Err(BackendError::TooLarge) => {
                    g.problem = None;
                    let first = !g.primed;
                    g.primed = true;
                    if !g.oversize_reported && !first {
                        g.oversize_reported = true;
                        let ids: Vec<String> = g
                            .peers
                            .iter()
                            .filter(|(_, p)| p.sync.is_some())
                            .map(|(id, _)| id.clone())
                            .collect();
                        for peer in g.peers.values_mut() {
                            if let Some(sync) = peer.sync.as_mut() {
                                sync.forget_observed();
                            }
                        }
                        for id in ids {
                            let st = g.stats.entry(id).or_default();
                            st.skipped += 1;
                            st.last_skip = Some("too_large");
                        }
                    }
                    g.last_text = None;
                    return;
                }
                Err(e) => {
                    g.problem = Some(e.code());
                    return;
                }
                Ok(None) => {
                    g.problem = None;
                    g.primed = true; // an empty clipboard is a baseline too
                    g.oversize_reported = false;
                    if g.last_text.take().is_some() {
                        for peer in g.peers.values_mut() {
                            if let Some(sync) = peer.sync.as_mut() {
                                sync.forget_observed();
                            }
                        }
                    }
                    return;
                }
                Ok(Some(text)) => {
                    g.problem = None;
                    g.oversize_reported = false;
                    if let Some((expected, until)) = g.expected.clone() {
                        if text == expected {
                            g.expected = None;
                            g.last_text = Some(text);
                            return;
                        }
                        if now < until {
                            return; // our write has not landed yet; this is the stale clipboard
                        }
                        g.expected = None;
                    }
                    if g.last_text.as_deref() == Some(text.as_str()) {
                        return;
                    }
                    if !g.primed {
                        // First reading since sync started: the baseline, not a change.
                        g.primed = true;
                        g.last_text = Some(text.clone());
                        for peer in g.peers.values_mut() {
                            if let Some(sync) = peer.sync.as_mut() {
                                sync.set_baseline(&text);
                            }
                        }
                        return;
                    }
                    text
                }
            }
        };
        let backend = self.backend.clone();
        let sensitivity = tokio::task::spawn_blocking(move || backend.sensitivity())
            .await
            .unwrap_or(Sensitivity::Unknown);
        let wall = now_ms();
        let mut g = self.lock();
        g.last_text = Some(text.clone());
        g.local_update_ms = wall as i64;
        g.update_known = true;
        let ids: Vec<String> = g.peers.keys().cloned().collect();
        for id in ids {
            let Some(peer) = g.peers.get_mut(&id) else {
                continue;
            };
            let stock = peer.caps.stock;
            let tx = peer.tx.clone();
            let Some(sync) = peer.sync.as_mut() else {
                continue;
            };
            let obs = LocalObservation {
                text: text.clone(),
                sensitivity,
                context: ReadContext::Background,
            };
            let decision = sync.local_changed(obs, now, random_id);
            let st = g.stats.entry(id).or_default();
            match decision {
                SendDecision::Send(_) if !stock => {
                    st.skipped += 1;
                    st.last_skip = Some("phone_not_accepting");
                }
                SendDecision::Send(msg) => {
                    let out = ClipOut::Packet {
                        kind: CLIPBOARD,
                        body: clipboard_wire::update_body(&msg.text),
                    };
                    if tx.try_send(out).is_ok() {
                        st.sent += 1;
                        st.last_sent = Some(wall);
                    } else {
                        st.skipped += 1;
                        st.last_skip = Some("link_busy");
                    }
                }
                SendDecision::Skip(reason) => {
                    if let Some(label) = skip_label(reason) {
                        st.skipped += 1;
                        st.last_skip = Some(label);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ status

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Counters for one device, merged by the daemon with its pairing/consent view.
    pub fn device_status(
        &self,
        device_id: &str,
        name: &str,
        enabled: bool,
        connected: bool,
    ) -> ClipboardDeviceStatus {
        let g = self.lock();
        let st = g.stats.get(device_id).cloned().unwrap_or_default();
        let peer = g.peers.get(device_id);
        ClipboardDeviceStatus {
            device_id: device_id.to_owned(),
            name: peer.map_or(name, |p| p.name.as_str()).to_owned(),
            enabled,
            connected,
            negotiated: peer.is_some_and(|p| p.caps.stock || p.caps.pull),
            sent: st.sent,
            received: st.received,
            skipped: st.skipped,
            last_sent_unix_ms: st.last_sent,
            last_received_unix_ms: st.last_received,
            last_skip: st.last_skip.map(str::to_owned),
        }
    }

    pub fn status(&self, devices: Vec<ClipboardDeviceStatus>) -> ClipboardStatus {
        let caps = self.backend.capabilities();
        let now = self.mono();
        let mut g = self.lock();
        let expired: Vec<String> = g
            .pulls
            .iter()
            .filter(|(_, p)| p.deadline_ms <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(p) = g.pulls.remove(&id) {
                Self::finish_pull(&mut g, &p.device_id, Err("timeout"));
            }
        }
        ClipboardStatus {
            backend: self.backend.name().to_owned(),
            available: caps.read != ReadAccess::Unsupported && caps.write,
            observe: match caps.observe {
                ObserveMode::None => "none",
                ObserveMode::Polling => "polling",
                ObserveMode::Event => "event",
            }
            .to_owned(),
            watching: g.watching,
            problem: g.problem.map(str::to_owned),
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES as u64,
            max_image_bytes: g.settings.max_image_bytes,
            window_ms: g.settings.window_ms,
            hotkey: g.settings.hotkey.clone(),
            hotkey_binding: g.binding.to_owned(),
            last_pull: g.last_pull.clone(),
            devices,
        }
    }
}

/// Status when the service has no clipboard backend at all.
pub fn unavailable_status(devices: Vec<ClipboardDeviceStatus>) -> ClipboardStatus {
    let s = Settings::default();
    ClipboardStatus {
        backend: "none".to_owned(),
        available: false,
        observe: "none".to_owned(),
        watching: false,
        problem: Some(if cfg!(target_os = "linux") {
            "no_clipboard_tool".to_owned()
        } else {
            "clipboard_disabled".to_owned()
        }),
        max_text_bytes: DEFAULT_MAX_TEXT_BYTES as u64,
        max_image_bytes: s.max_image_bytes,
        window_ms: s.window_ms,
        hotkey: s.hotkey,
        hotkey_binding: "unsupported".to_owned(),
        last_pull: None,
        devices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "phone-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PHONE2: &str = "phone-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const FAST: Duration = Duration::from_millis(10);
    const FULL: LinkCaps = LinkCaps {
        stock: true,
        pull: true,
    };
    const STOCK_ONLY: LinkCaps = LinkCaps {
        stock: true,
        pull: false,
    };
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-file";

    /// `fsutil` insists on a private (0700) directory, which `tempdir` does not guarantee.
    fn private_tempdir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn rig() -> (Arc<MemoryClipboard>, Arc<ClipboardService>) {
        let mem = Arc::new(MemoryClipboard::default());
        let svc = ClipboardService::with_poll(mem.clone(), "desktop-1", FAST);
        (mem, svc)
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    fn drain(rx: &mut mpsc::Receiver<ClipOut>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        while let Ok(p) = rx.try_recv() {
            out.push(match p {
                ClipOut::Packet { kind, body } => (
                    kind.to_owned(),
                    body.get("content")
                        .or_else(|| body.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                ),
                ClipOut::Image { .. } => ("image".to_owned(), String::new()),
            });
        }
        out
    }

    fn update(text: &str) -> Map<String, Value> {
        clipboard_wire::update_body(text)
    }

    fn request_id_of(rx: &mut mpsc::Receiver<ClipOut>) -> String {
        match rx.try_recv().expect("pull packet") {
            ClipOut::Packet { kind, body } => {
                assert_eq!(kind, PULL);
                body["request_id"].as_str().unwrap().to_owned()
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_displaced_session_does_not_remove_its_successor() {
        let (_mem, svc) = rig();
        let (_old_rx, old) = svc.link_up_generation(PHONE, "Phone", FULL, true);
        // The phone reconnects: the new session registers before the old one is torn down.
        let (_new_rx, _new) = svc.link_up_generation(PHONE, "Phone", FULL, true);
        svc.link_down_generation(PHONE, old);
        assert!(svc.any_enabled(), "the new link must still be registered");
        assert_eq!(svc.pull(Some(PHONE)).map(|_| ()), Ok(()));
    }

    #[tokio::test]
    async fn copy_on_computer_reaches_enabled_phone_only() {
        let (mem, svc) = rig();
        mem.user_copy("before opt-in", false);
        let mut on = svc.link_up(PHONE, "P", FULL, true);
        let mut off = svc.link_up(PHONE2, "Q", FULL, false);
        settle().await;
        // Pre-existing text is the baseline, not a change.
        assert!(drain(&mut on).is_empty());
        mem.user_copy("hello phone", false);
        settle().await;
        assert_eq!(
            drain(&mut on),
            vec![(CLIPBOARD.to_owned(), "hello phone".to_owned())]
        );
        assert!(drain(&mut off).is_empty());
        // Same text polled again is not resent.
        settle().await;
        assert!(drain(&mut on).is_empty());
        let st = svc.device_status(PHONE, "Phone", true, true);
        assert_eq!((st.sent, st.received), (1, 0));
        assert!(st.last_sent_unix_ms.is_some());
    }

    #[tokio::test]
    async fn stock_phone_copy_is_applied_and_not_echoed_back() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", STOCK_ONLY, true);
        settle().await;
        svc.on_packet(PHONE, CLIPBOARD, &update("from phone"), None);
        settle().await;
        assert_eq!(mem.text().as_deref(), Some("from phone"));
        assert_eq!(mem.writes(), 1);
        assert!(drain(&mut rx).is_empty(), "echo must be suppressed");
        let st = svc.device_status(PHONE, "Phone", true, true);
        assert_eq!((st.sent, st.received), (0, 1));
    }

    #[tokio::test]
    async fn disabled_device_is_ignored_and_never_written() {
        let (mem, svc) = rig();
        let _rx = svc.link_up(PHONE, "P", FULL, false);
        svc.on_packet(PHONE, CLIPBOARD, &update("nope"), None);
        settle().await;
        assert_eq!(mem.text(), None);
        assert_eq!(mem.writes(), 0);
        assert!(!svc.status(vec![]).watching, "no watcher without opt-in");
        assert_eq!(
            svc.pull(Some(PHONE)),
            Err("device_not_enabled_or_connected")
        );
    }

    #[tokio::test]
    async fn sensitive_and_oversized_items_are_skipped_and_counted() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        settle().await;
        mem.user_copy("hunter2", true);
        settle().await;
        assert!(drain(&mut rx).is_empty());
        let st = svc.device_status(PHONE, "Phone", true, true);
        assert_eq!(st.last_skip.as_deref(), Some("sensitive"));
        assert_eq!(st.skipped, 1);
        mem.user_copy(&"x".repeat(DEFAULT_MAX_TEXT_BYTES + 1), false);
        settle().await;
        settle().await;
        assert!(drain(&mut rx).is_empty());
        let st = svc.device_status(PHONE, "Phone", true, true);
        assert_eq!(st.last_skip.as_deref(), Some("too_large"));
        assert_eq!(
            st.skipped, 2,
            "an oversized item is counted once, not per poll"
        );
        svc.on_packet(
            PHONE,
            CLIPBOARD,
            &update(&"y".repeat(DEFAULT_MAX_TEXT_BYTES + 1)),
            None,
        );
        assert_eq!(mem.writes(), 0);
        mem.user_copy("fine", false);
        settle().await;
        assert_eq!(drain(&mut rx), vec![(CLIPBOARD.to_owned(), "fine".into())]);
    }

    #[tokio::test]
    async fn applied_text_is_not_relayed_to_another_phone() {
        let (mem, svc) = rig();
        let mut a = svc.link_up(PHONE, "A", FULL, true);
        let mut b = svc.link_up(PHONE2, "B", FULL, true);
        settle().await;
        svc.on_packet(PHONE, CLIPBOARD, &update("from A"), None);
        settle().await;
        assert_eq!(mem.text().as_deref(), Some("from A"));
        assert!(drain(&mut a).is_empty());
        assert!(drain(&mut b).is_empty(), "never relayed between peers");
        mem.user_copy("local", false);
        settle().await;
        assert_eq!(drain(&mut a).len(), 1);
        assert_eq!(drain(&mut b).len(), 1);
    }

    #[tokio::test]
    async fn connect_packets_follow_the_stock_timestamp_rule() {
        let (mem, svc) = rig();
        let _rx = svc.link_up(PHONE, "P", FULL, true);
        settle().await;
        let body = |ts: i64| clipboard_wire::connect_body("old phone state", ts);
        svc.on_packet(PHONE, CLIPBOARD_CONNECT, &body(1_000), None);
        svc.on_packet(PHONE, CLIPBOARD_CONNECT, &body(0), None);
        settle().await;
        assert_eq!(mem.writes(), 0);
        svc.on_packet(
            PHONE,
            CLIPBOARD_CONNECT,
            &body(now_ms() as i64 + 1_000),
            None,
        );
        settle().await;
        assert_eq!(mem.text().as_deref(), Some("old phone state"));
    }

    #[tokio::test]
    async fn link_up_announces_only_content_copied_since_sync_started() {
        let (mem, svc) = rig();
        mem.user_copy("secret from yesterday", false);
        let mut first = svc.link_up(PHONE, "P", FULL, true);
        settle().await;
        assert!(drain(&mut first).is_empty());
        mem.user_copy("fresh", false);
        settle().await;
        assert_eq!(drain(&mut first).len(), 1);
        let mut late = svc.link_up(PHONE2, "Q", FULL, true);
        let got = late.try_recv().expect("connect announcement");
        let ClipOut::Packet { kind, body } = got else {
            panic!()
        };
        assert_eq!(kind, CLIPBOARD_CONNECT);
        assert_eq!(body["content"], "fresh");
        assert!(body["timestamp"].as_i64().unwrap() > 0);
        settle().await;
        assert!(drain(&mut late).is_empty());
    }

    #[tokio::test]
    async fn change_stream_reports_a_change_and_ends_with_the_helper() {
        let mut ev = ChangeStream::spawn("sh", &["-c", "echo; sleep 0.1"]).expect("sh");
        assert!(ev.changed().await.is_some());
        assert!(ev.changed().await.is_none(), "helper exited");
        assert!(ChangeStream::spawn("geekbridge-no-such-tool", &[]).is_none());
    }

    #[tokio::test]
    async fn watcher_stops_and_forgets_text_when_last_device_is_disabled() {
        let (mem, svc) = rig();
        let _rx = svc.link_up(PHONE, "P", FULL, true);
        settle().await;
        mem.user_copy("temp", false);
        settle().await;
        assert!(svc.status(vec![]).watching);
        svc.set_enabled(PHONE, false);
        settle().await;
        assert!(!svc.status(vec![]).watching);
        assert!(svc.lock().last_text.is_none());
    }

    #[tokio::test]
    async fn phone_that_does_not_accept_clipboard_gets_nothing() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", LinkCaps::default(), true);
        settle().await;
        mem.user_copy("x", false);
        settle().await;
        assert!(drain(&mut rx).is_empty());
        assert_eq!(
            svc.device_status(PHONE, "P", true, true)
                .last_skip
                .as_deref(),
            Some("phone_not_accepting")
        );
        assert_eq!(svc.pull(Some(PHONE)), Err("phone_app_cannot_be_asked"));
    }

    #[tokio::test]
    async fn backend_failure_is_reported_and_recovers() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        settle().await;
        mem.set_fail_reads(true);
        settle().await;
        assert_eq!(
            svc.status(vec![]).problem.as_deref(),
            Some("clipboard_io_failed")
        );
        mem.set_fail_reads(false);
        mem.user_copy("back", false);
        settle().await;
        assert!(svc.status(vec![]).problem.is_none());
        assert_eq!(drain(&mut rx).len(), 1);
    }

    // ---- pull

    #[tokio::test]
    async fn pull_delivers_a_fresh_text_item() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        assert_eq!(svc.pull(None), Ok(1));
        let req = request_id_of(&mut rx);
        assert_eq!(svc.status(vec![]).last_pull.unwrap().state, "waiting");
        let body = clipboard_wire::text_item_body(
            ItemKind::Url,
            "https://example.org/x",
            None,
            Some(&req),
            4_000,
        );
        assert!(matches!(
            svc.on_packet(PHONE, ITEM, &body, None),
            Next::Nothing
        ));
        settle().await;
        assert_eq!(mem.text().as_deref(), Some("https://example.org/x"));
        let lp = svc.status(vec![]).last_pull.unwrap();
        assert_eq!(
            (lp.state.as_str(), lp.kind.as_deref()),
            ("delivered", Some("url"))
        );
        // The answer was consumed: replaying it does nothing.
        let writes = mem.writes();
        svc.on_packet(PHONE, ITEM, &body, None);
        settle().await;
        assert_eq!(mem.writes(), writes);
    }

    #[tokio::test]
    async fn pull_refuses_stale_items_even_if_the_phone_sends_them() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        svc.update_settings(Some(10_000), None, None).unwrap();
        svc.pull(None).unwrap();
        let req = request_id_of(&mut rx);
        let body = clipboard_wire::text_item_body(ItemKind::Text, "old", None, Some(&req), 10_001);
        svc.on_packet(PHONE, ITEM, &body, None);
        settle().await;
        assert_eq!(mem.writes(), 0);
        let lp = svc.status(vec![]).last_pull.unwrap();
        assert_eq!(
            (lp.state.as_str(), lp.reason.as_deref()),
            ("none", Some("expired"))
        );
    }

    #[tokio::test]
    async fn pull_none_reason_is_reported_and_unknown_requests_are_ignored() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        let _rx2 = svc.link_up(PHONE2, "Q", FULL, true);
        svc.pull(Some(PHONE)).unwrap();
        let req = request_id_of(&mut rx);
        // Another device cannot answer for this request.
        svc.on_packet(
            PHONE2,
            NONE,
            &clipboard_wire::none_body(&req, NoneReason::Empty),
            None,
        );
        assert_eq!(svc.status(vec![]).last_pull.unwrap().state, "waiting");
        let item = clipboard_wire::text_item_body(ItemKind::Text, "spoof", None, Some(&req), 0);
        svc.on_packet(PHONE2, ITEM, &item, None);
        // An answer to a request we never made is ignored.
        let ghost = clipboard_wire::text_item_body(ItemKind::Text, "ghost", None, Some("nope"), 0);
        svc.on_packet(PHONE, ITEM, &ghost, None);
        settle().await;
        assert_eq!(mem.writes(), 0);
        svc.on_packet(
            PHONE,
            NONE,
            &clipboard_wire::none_body(&req, NoneReason::Locked),
            None,
        );
        let lp = svc.status(vec![]).last_pull.unwrap();
        assert_eq!(
            (lp.state.as_str(), lp.reason.as_deref()),
            ("none", Some("locked"))
        );
    }

    #[tokio::test]
    async fn pull_image_goes_through_a_ticket_and_is_validated() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        svc.pull(None).unwrap();
        let req = request_id_of(&mut rx);
        let body = clipboard_wire::image_item_body("image/png", PNG.len() as u64, Some(&req), 100);
        // No payload announced: refused.
        assert!(matches!(
            svc.on_packet(PHONE, ITEM, &body, None),
            Next::Nothing
        ));
        assert_eq!(svc.status(vec![]).last_pull.unwrap().state, "none");
        svc.pull(None).unwrap();
        let req = request_id_of(&mut rx);
        let body = clipboard_wire::image_item_body("image/png", PNG.len() as u64, Some(&req), 100);
        let Next::ReceiveImage(ticket) = svc.on_packet(PHONE, ITEM, &body, Some(1740)) else {
            panic!("expected ticket")
        };
        assert_eq!((ticket.size, ticket.mime), (PNG.len() as u64, "image/png"));
        // Bytes that are not a PNG are refused whatever was announced.
        svc.image_received(&ticket, b"not a png at all!!".to_vec());
        settle().await;
        assert!(mem.image().is_none());
        assert_eq!(svc.status(vec![]).last_pull.unwrap().state, "none");
        svc.pull(None).unwrap();
        let req = request_id_of(&mut rx);
        let body = clipboard_wire::image_item_body("image/png", PNG.len() as u64, Some(&req), 100);
        let Next::ReceiveImage(ticket) = svc.on_packet(PHONE, ITEM, &body, Some(1740)) else {
            panic!()
        };
        svc.image_received(&ticket, PNG.to_vec());
        settle().await;
        assert_eq!(mem.image().unwrap().bytes, PNG);
        let lp = svc.status(vec![]).last_pull.unwrap();
        assert_eq!(
            (lp.state.as_str(), lp.kind.as_deref()),
            ("delivered", Some("image"))
        );
    }

    #[tokio::test]
    async fn oversized_image_announcement_is_refused_before_any_transfer() {
        let (_mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        svc.update_settings(None, None, Some(100 * 1024)).unwrap();
        svc.pull(None).unwrap();
        let req = request_id_of(&mut rx);
        let body = clipboard_wire::image_item_body("image/png", 200 * 1024, Some(&req), 0);
        assert!(matches!(
            svc.on_packet(PHONE, ITEM, &body, Some(1740)),
            Next::Nothing
        ));
    }

    #[tokio::test]
    async fn unsolicited_item_is_an_explicit_send_from_the_phone() {
        let (mem, svc) = rig();
        let _rx = svc.link_up(PHONE, "P", FULL, true);
        let body = clipboard_wire::text_item_body(ItemKind::Text, "tapped send", None, None, 0);
        svc.on_packet(PHONE, ITEM, &body, None);
        settle().await;
        assert_eq!(mem.text().as_deref(), Some("tapped send"));
    }

    #[tokio::test]
    async fn pending_pull_times_out() {
        let (_mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        svc.pull(None).unwrap();
        let _ = request_id_of(&mut rx);
        for p in svc.lock().pulls.values_mut() {
            p.deadline_ms = 0;
        }
        let lp = svc.status(vec![]).last_pull.unwrap();
        assert_eq!(
            (lp.state.as_str(), lp.reason.as_deref()),
            ("none", Some("timeout"))
        );
    }

    // ---- push on demand

    #[tokio::test]
    async fn push_now_sends_image_html_or_text_and_never_secrets() {
        let (mem, svc) = rig();
        let mut rx = svc.link_up(PHONE, "P", FULL, true);
        mem.user_copy_image(ClipImage {
            mime: "image/png",
            bytes: PNG.to_vec(),
        });
        assert_eq!(svc.push_now(None).await, Ok(1));
        assert!(matches!(rx.try_recv().unwrap(), ClipOut::Image { .. }));
        mem.user_copy_html("plain", "<b>plain</b>");
        assert_eq!(svc.push_now(None).await, Ok(1));
        let ClipOut::Packet { kind, body } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!((kind, body["kind"].as_str()), (ITEM, Some("html")));
        mem.user_copy("https://example.org/", false);
        svc.push_now(None).await.unwrap();
        let ClipOut::Packet { body, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(body["kind"], "url");
        mem.user_copy("hunter2", true);
        assert_eq!(svc.push_now(None).await, Err("sensitive"));
        // A stock-only phone gets plain text through the stock packet and no images.
        let mut stock = svc.link_up(PHONE2, "Q", STOCK_ONLY, true);
        mem.user_copy("plain text", false);
        svc.push_now(Some(PHONE2)).await.unwrap();
        let ClipOut::Packet { kind, .. } = stock.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(kind, CLIPBOARD);
        mem.user_copy_image(ClipImage {
            mime: "image/png",
            bytes: PNG.to_vec(),
        });
        assert_eq!(
            svc.push_now(Some(PHONE2)).await,
            Err("phone_cannot_receive_this")
        );
    }

    // ---- settings

    #[tokio::test]
    async fn settings_are_validated_clamped_and_persisted() {
        let dir = private_tempdir();
        let path = dir.path().join(SETTINGS_FILE);
        let mem = Arc::new(MemoryClipboard::default());
        let svc = ClipboardService::with_settings_file(mem.clone(), "d", path.clone());
        assert_eq!(svc.settings(), Settings::default());
        assert!(svc.update_settings(None, Some("V"), None).is_err());
        assert_eq!(svc.settings().hotkey, DEFAULT_HOTKEY);
        let s = svc
            .update_settings(Some(1), Some("ctrl+alt+p"), Some(u64::MAX))
            .unwrap();
        assert_eq!(s.window_ms, clipboard_wire::MIN_WINDOW_MS);
        assert_eq!(s.hotkey, "Ctrl+Alt+P");
        assert_eq!(s.max_image_bytes, clipboard_wire::HARD_MAX_IMAGE_BYTES);
        // A new service reads them back; the file holds no clipboard content.
        let again = ClipboardService::with_settings_file(mem, "d", path.clone());
        assert_eq!(again.settings(), s);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("text") && raw.contains("hotkey"));
    }

    #[test]
    fn corrupt_settings_fall_back_to_defaults() {
        let dir = private_tempdir();
        let path = dir.path().join(SETTINGS_FILE);
        bridge_kde::fsutil::atomic_write_private(
            &path,
            br#"{"window_ms":99999999999,"hotkey":"nonsense"}"#,
        )
        .unwrap();
        let svc =
            ClipboardService::with_settings_file(Arc::new(MemoryClipboard::default()), "d", path);
        let s = svc.settings();
        assert_eq!(s.window_ms, clipboard_wire::MAX_WINDOW_MS);
        assert_eq!(s.hotkey, DEFAULT_HOTKEY);
    }

    #[test]
    fn link_caps_need_both_directions_of_the_pull_protocol() {
        let v = |k: &[&str]| k.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let c = LinkCaps::from_negotiated(&v(&[CLIPBOARD, PULL, ITEM]), &v(&[ITEM, NONE]));
        assert!(c.stock && c.pull);
        let c = LinkCaps::from_negotiated(&v(&[CLIPBOARD, PULL, ITEM]), &v(&[ITEM]));
        assert!(c.stock && !c.pull, "phone cannot say 'none'");
        let c = LinkCaps::from_negotiated(&v(&[]), &v(&[]));
        assert_eq!(c, LinkCaps::default());
    }

    #[test]
    fn password_hint_detection() {
        assert!(lists(
            b"text/plain\nx-kde-passwordManagerHint\n",
            PASSWORD_HINT
        ));
        assert!(!lists(b"text/plain\nUTF8_STRING\n", PASSWORD_HINT));
    }

    #[test]
    fn missing_tool_is_reported_not_panicked() {
        assert_eq!(
            run_tool("/nonexistent/geekbridge-tool", &[], None, 8).err(),
            Some(BackendError::Unavailable)
        );
    }

    #[test]
    fn tool_output_is_bounded_and_timeouts_kill_the_child() {
        let c = run_tool("sh", &["-c", "printf 0123456789"], None, 4).unwrap();
        assert!(c.truncated && c.bytes == b"0123");
        assert_eq!(utf8(c).unwrap_err(), BackendError::TooLarge);
        let c = run_tool("sh", &["-c", "cat"], Some(b"echo"), 16).unwrap();
        assert_eq!(utf8(c).unwrap().as_deref(), Some("echo"));
        let c = run_tool("sh", &["-c", "exit 1"], None, 16).unwrap();
        assert_eq!(utf8(c).unwrap(), None);
    }
}
