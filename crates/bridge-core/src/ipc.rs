//! Authenticated local control protocol between `geekbridge-service` and its clients.
//!
//! This is *not* the public discovery API ([`crate::fetch_snapshot`]); that one stays read-only and
//! never carries anything but untrusted announcement metadata. The control protocol lives on its
//! own loopback port ([`DEFAULT_CONTROL_PORT`]) and every request must prove knowledge of a per-user
//! random token that the service keeps in its private state directory.
//!
//! # Wire contract (version 1)
//!
//! One request per TCP connection, newline-terminated lines, hard byte limits, and one total
//! deadline per exchange:
//!
//! ```text
//! S -> C   GB-IPC 1 <64 hex nonce>\n            service is ready
//!          GB-IPC 1 unavailable <code> <text>\n protected state could not be opened (then close)
//! C -> S   <64 hex mac> <request json>\n        mac = HMAC-SHA256(token, "geekbridge-ipc-v1 request\0" || nonce || json)
//! S -> C   <64 hex mac> <response json>\n       mac = HMAC-SHA256(token, "geekbridge-ipc-v1 response\0" || nonce || request mac || json)
//!          ERR unauthorized\n                   the request mac was wrong; nothing was executed
//! ```
//!
//! The token itself never crosses the socket, so a process that wins the port race learns nothing
//! reusable, and the client rejects a response whose mac does not verify. Phone content is never
//! stored by the service and no response here carries message bodies, contacts or clipboard text;
//! the types below have no field for them.
//!
//! The secret is read by the caller (use `bridge_kde::fsutil::read_private_file`, which checks
//! owner and mode, on the file at [`token_path`]) and handed to [`Token::parse`]. This crate
//! deliberately does not re-implement platform file-permission checks.

use crate::share::{ShareStatus, ShareTextKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const IPC_VERSION: u32 = 1;
/// Loopback port of the authenticated control API (the public discovery API is `API port`).
pub const DEFAULT_CONTROL_PORT: u16 = crate::DEFAULT_API_PORT + 1;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Total time one client exchange (connect, hello, request, response) may take.
pub const CLIENT_DEADLINE: Duration = Duration::from_secs(4);
pub const TOKEN_FILE_NAME: &str = "ipc-token";
pub const TOKEN_BYTES: usize = 32;
pub const ENV_STATE_DIR: &str = "GEEKBRIDGE_STATE_DIR";
pub const ENV_CONTROL_PORT: &str = "GEEKBRIDGE_CONTROL_PORT";

const REQUEST_DOMAIN: &[u8] = b"geekbridge-ipc-v1 request\0";
const RESPONSE_DOMAIN: &[u8] = b"geekbridge-ipc-v1 response\0";

// ---------------------------------------------------------------------------
// Configuration lookup (no secrets, no file access)
// ---------------------------------------------------------------------------

/// State directory used when none is configured: `$GEEKBRIDGE_STATE_DIR`, else the per-user data
/// directory of the platform (`…/geekbridge/service`). `None` if no home can be determined.
pub fn default_state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ENV_STATE_DIR).map(PathBuf::from)
        && dir.is_absolute()
    {
        return Some(dir);
    }
    platform_data_dir().map(|base| base.join("geekbridge").join("service"))
}

/// Per-user data directory of the platform: `%LOCALAPPDATA%` (Windows), `~/Library/Application
/// Support` (macOS), `$XDG_DATA_HOME` or `~/.local/share` elsewhere. Always absolute.
#[cfg(target_os = "windows")]
pub fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(target_os = "macos")]
pub fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        .filter(|p| p.is_absolute())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .filter(|p| p.is_absolute())
}

/// Where the service keeps the control token inside `state_dir`.
pub fn token_path(state_dir: &Path) -> PathBuf {
    state_dir.join(TOKEN_FILE_NAME)
}

/// `$GEEKBRIDGE_CONTROL_PORT` if it is a valid non-zero port, else [`DEFAULT_CONTROL_PORT`].
pub fn control_port_from_env() -> u16 {
    std::env::var(ENV_CONTROL_PORT)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_CONTROL_PORT)
}

/// The control API only ever listens on IPv4 loopback.
pub fn control_address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

// ---------------------------------------------------------------------------
// Token and MACs
// ---------------------------------------------------------------------------

/// A 256-bit shared secret. `Debug` is redacted and the bytes are wiped on drop. There is
/// intentionally no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; TOKEN_BYTES]);

#[derive(Debug, PartialEq, Eq)]
pub struct TokenFormatError;

impl fmt::Display for TokenFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("control token file is not 64 hexadecimal characters")
    }
}
impl std::error::Error for TokenFormatError {}

impl Token {
    pub fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    /// Parse the contents of the token file: 64 hex characters, surrounding whitespace ignored.
    pub fn parse(contents: &[u8]) -> Result<Self, TokenFormatError> {
        let text = std::str::from_utf8(contents).map_err(|_| TokenFormatError)?;
        decode_hex::<TOKEN_BYTES>(text.trim())
            .map(Self)
            .ok_or(TokenFormatError)
    }

    /// File contents for persisting (hex plus newline). Handle the result like the secret it is.
    pub fn to_file_contents(&self) -> String {
        let mut s = encode_hex(&self.0);
        s.push('\n');
        s
    }

    fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        hmac_sha256(&self.0, parts)
    }

    /// MAC a client sends for `json` on a connection whose hello carried `nonce`.
    pub fn request_mac(&self, nonce: &str, json: &[u8]) -> [u8; 32] {
        self.mac(&[REQUEST_DOMAIN, nonce.as_bytes(), json])
    }

    /// MAC the service sends for `json`, bound to the nonce and to the request it answers.
    pub fn response_mac(&self, nonce: &str, request_mac: &[u8; 32], json: &[u8]) -> [u8; 32] {
        let request_hex = encode_hex(request_mac);
        self.mac(&[
            RESPONSE_DOMAIN,
            nonce.as_bytes(),
            request_hex.as_bytes(),
            json,
        ])
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // SAFETY: `byte` is a valid, exclusive reference; volatile keeps the wipe from being elided.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn encode_hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Strict fixed-length hex decoder (either case).
pub fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut out = [0_u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = nibble(bytes[2 * i])? << 4 | nibble(bytes[2 * i + 1])?;
    }
    Some(out)
}

/// Comparison whose duration does not depend on where the inputs differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// HMAC-SHA256 (RFC 2104) over the concatenation of `parts`.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0_u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

// ---------------------------------------------------------------------------
// Typed requests, responses, status
// ---------------------------------------------------------------------------

/// Phone capabilities a user can allow GeekBridge to *ask* for. Every one starts disabled; none
/// is requested merely because the phone advertises it. `Ping` is the paired-interoperability
/// check and needs no consent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Sms,
    Contacts,
    /// Read-only calendar events (`geekbridge.calendar.*`).
    Calendar,
    /// Creating, changing and deleting events on the phone's calendar (`geekbridge.calendar.write`).
    /// Separate from reading and off by default; nothing is ever written without it.
    CalendarEdit,
    /// Desktop notifications shortly before events from the shared calendar.
    CalendarAlerts,
    Clipboard,
    Notifications,
    Telephony,
}

impl Feature {
    pub const ALL: [Feature; 8] = [
        Feature::Sms,
        Feature::Contacts,
        Feature::Calendar,
        Feature::CalendarEdit,
        Feature::CalendarAlerts,
        Feature::Clipboard,
        Feature::Notifications,
        Feature::Telephony,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairMode {
    /// Send a pair request now; both phone and desktop show the verification key.
    Request,
    /// Send nothing; wait for the phone to ask.
    Await,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub address: std::net::IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Service identity, paired devices, live links, pending challenges and consents.
    Status,
    /// Events newer than `after` (0 for everything retained).
    Events {
        after: u64,
    },
    /// Dial a device. `endpoint` is the user's explicit choice; when omitted the service uses the
    /// address from its current discovery record. Unknown devices end in an unpaired link that can
    /// only be paired or closed.
    Connect {
        device_id: String,
        #[serde(default)]
        endpoint: Option<Endpoint>,
    },
    /// Start pairing on an unpaired link. Produces a challenge that must be approved explicitly.
    Pair {
        device_id: String,
        mode: PairMode,
    },
    Approve {
        challenge_id: String,
    },
    Deny {
        challenge_id: String,
    },
    /// Show-a-QR pairing: issue a one-time token and return the text to encode in the QR code.
    /// Scanning it pins this computer on the phone; the phone still asks its user.
    PairQr,
    /// Revoke every outstanding QR token (the QR dialog was closed).
    PairQrCancel,
    /// "Test connection": look for the usual reasons a phone and this computer cannot talk and
    /// answer in plain language. `device_id` names the phone to probe (its discovered address);
    /// `endpoint` overrides the address.
    TestConnection {
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        endpoint: Option<Endpoint>,
    },
    /// Close the live link; the pairing is kept.
    Disconnect {
        device_id: String,
    },
    /// Forget the device (and tell it, when linked).
    Unpair {
        device_id: String,
    },
    SetConsent {
        device_id: String,
        feature: Feature,
        enabled: bool,
    },
    /// Interoperability check on a paired, connected device. Carries no user text.
    Ping {
        device_id: String,
    },
    /// Read-only probe of the host's Bluetooth calling backend (counts only, no numbers).
    CallingStatus,
    /// The merged call view: ringing and live calls with caller, the controls that really exist
    /// and the audio route. Carries phone numbers and names, so it is only served on this
    /// authenticated control channel and the apps must keep it out of logs.
    Calls,
    /// Answer, decline or end a call. `target` must be one the latest `Calls` view offered.
    CallAction {
        target: crate::calls_view::ActionTarget,
        action: CallAction,
    },
    /// Mute or unmute the computer's microphone for the live call (only where offered).
    CallMute {
        source: String,
        muted: bool,
    },
    /// Ask a source to move the live call's audio to this computer (only where offered).
    CallAudio {
        source: String,
    },
    /// Update-check state (never contacts the network by itself).
    UpdateStatus,
    /// Start a check of the latest GitHub Release now; poll `UpdateStatus` for the result.
    UpdateCheck,
    /// Start downloading and SHA-256-verifying the offered installer; poll `UpdateStatus`.
    UpdateDownload,
    /// Hide the offer for this version (a newer release is offered again).
    UpdateSkip {
        version: String,
    },
    /// Turn the automatic (startup and daily) check on or off. Persisted.
    UpdateSetEnabled {
        enabled: bool,
    },
    /// Clipboard bridge state: backend, per-device opt-in and counters. Never carries clipboard text.
    ClipboardStatus,
    /// Ask a connected phone for its latest eligible clipboard item and place it on this computer's
    /// clipboard ("paste from phone"). Without `device_id` every enabled, connected phone is asked.
    /// The answer arrives asynchronously; see `ClipboardStatus::last_pull`.
    ClipboardPull {
        #[serde(default)]
        device_id: Option<String>,
    },
    /// Send the computer's current clipboard to the phone as its richest item (image, HTML, URL or
    /// text). Without `device_id` every enabled, connected phone receives it.
    ClipboardPush {
        #[serde(default)]
        device_id: Option<String>,
    },
    /// Change clipboard settings. Omitted fields are left alone.
    ClipboardSettings {
        #[serde(default)]
        window_ms: Option<u64>,
        #[serde(default)]
        hotkey: Option<String>,
        #[serde(default)]
        max_image_bytes: Option<u64>,
    },
    /// Share settings, per-device permissions and the transfer list (no file contents).
    ShareStatus,
    /// Send a local file to a connected, paired device. Returns `TransferStarted`.
    ShareSendFile {
        device_id: String,
        path: String,
    },
    /// Send text or a link to a connected, paired device.
    ShareSendText {
        device_id: String,
        text_kind: ShareTextKind,
        text: String,
    },
    /// Answer an incoming file offer.
    ShareDecide {
        transfer_id: u64,
        accept: bool,
    },
    /// Cancel a live transfer (incoming or outgoing).
    ShareCancel {
        transfer_id: u64,
    },
    /// Forget a finished transfer / received text.
    ShareDismiss {
        transfer_id: u64,
    },
    /// Per-device share permissions. Omitted fields are left alone. Both default to off.
    ShareSettings {
        device_id: String,
        #[serde(default)]
        receive: Option<bool>,
        #[serde(default)]
        auto_accept: Option<bool>,
    },
    /// People matching `query` and the events of `day` (`YYYY-MM-DD`, seen at `utc_offset_minutes`)
    /// from every phone whose Contacts / Calendar consent is on. Memory only; nothing is stored.
    AgendaView {
        #[serde(default)]
        query: String,
        day: String,
        utc_offset_minutes: i32,
        /// Number of days shown from `day` (1 = a single day, 7 week, up to 42 for a month grid).
        #[serde(default)]
        days: u32,
        /// How many contacts to list at most (0 = the default of 300, capped at 1000).
        #[serde(default)]
        people_limit: usize,
    },
    /// Create, change or delete one event on a phone's calendar. `op_id` makes a retry idempotent
    /// (the phone applies each id once). Needs the per-device `CalendarEdit` consent and a phone that
    /// declares write support; an explicit `calendar_id` is required to create.
    CalendarEdit {
        device_id: String,
        op_id: String,
        action: crate::phone_view::CalendarEditAction,
        #[serde(default)]
        calendar_id: Option<String>,
        #[serde(default)]
        event_id: Option<String>,
        #[serde(default)]
        draft: Option<Box<crate::phone_view::EventDraft>>,
    },
    /// Send an uncertain or failed edit again under the same `op_id` (safe: the phone answers a
    /// repeated id with its first outcome).
    CalendarEditRetry {
        device_id: String,
        op_id: String,
    },
    /// Ask the phone(s) for fresh contacts and calendar data now.
    AgendaRefresh {
        #[serde(default)]
        device_id: Option<String>,
    },
    /// Conversations of every phone with SMS consent, plus one thread's messages if asked.
    MessagesView {
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        thread_id: Option<i64>,
        /// Filter the list to conversations whose name, number or any held message contains this.
        #[serde(default)]
        query: Option<String>,
    },
    /// Ask for the conversation list, or for older history of one thread.
    MessagesRefresh {
        device_id: String,
        #[serde(default)]
        thread_id: Option<i64>,
    },
    /// Send one SMS through the phone. `submission_id` makes a retry of the same click idempotent.
    MessagesSend {
        device_id: String,
        recipient: String,
        text: String,
        submission_id: String,
        /// SIM subscription id from the phone's list; absent = the phone's default.
        #[serde(default)]
        sub_id: Option<i32>,
    },
    /// Mark a conversation read on this computer only (the phone is not told).
    MessagesMarkRead {
        device_id: String,
        thread_id: i64,
    },
    /// Send an uncertain or failed message again, accepting that the recipient may get it twice.
    MessagesResend {
        device_id: String,
        send_id: u64,
        submission_id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPhase {
    /// TCP/TLS handshake in progress.
    Connecting,
    /// TLS is up but the peer is not paired; nothing can be exchanged except pairing.
    Unpaired,
    /// A pairing attempt is running (see the challenge list).
    Pairing,
    /// Authenticated against the pinned certificate.
    Connected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    Initiator,
    Acceptor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStatus {
    pub device_id: String,
    pub name: String,
    pub phase: LinkPhase,
    pub role: Option<LinkRole>,
    pub connection_id: Option<u64>,
    pub peer_ip: Option<std::net::IpAddr>,
    pub peer_cert_sha256: Option<String>,
    /// Packet types usable on this link: what the peer sends and we declared we accept.
    pub negotiated: Vec<String>,
    pub since_unix_ms: u64,
    pub pings_received: u64,
    pub last_ping_unix_ms: Option<u64>,
    pub rejected_packets: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentEntry {
    pub feature: Feature,
    pub enabled: bool,
    /// False until a protocol adapter for the feature exists; consent is stored but unused.
    pub adapter_integrated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub device_id: String,
    pub name: String,
    pub device_type: String,
    pub cert_sha256: String,
    pub paired_at_unix: i64,
    pub consents: Vec<ConsentEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeDirection {
    /// We asked the phone.
    Outgoing,
    /// The phone asked us.
    Incoming,
}

/// One pending pairing decision. It is bound to a single connection and certificate and dies with
/// either, or when it expires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairChallenge {
    pub challenge_id: String,
    pub device_id: String,
    pub device_name: String,
    pub direction: ChallengeDirection,
    pub connection_id: u64,
    pub peer_cert_sha256: String,
    pub own_cert_sha256: String,
    /// Compare with the code the phone shows before approving.
    pub verification_key: String,
    pub expires_in_ms: u64,
}

/// What the pair-by-QR dialog shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairQr {
    /// The exact text to encode in the QR code (also usable as a pasteable link).
    pub payload: String,
    pub expires_in_ms: u64,
    pub computer_name: String,
    /// The addresses written into the code, for display.
    pub addresses: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// Never checked (or the last answer is unknown).
    Idle,
    Checking,
    UpToDate,
    /// A newer release exists.
    Available,
    Downloading,
    /// The installer is downloaded and its SHA-256 matched the published value.
    Downloaded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRelease {
    pub version: String,
    pub tag: String,
    /// Plain text, control characters removed, length bounded.
    pub notes: String,
    pub release_url: String,
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
    /// The published SHA-256 of the asset; a download is offered only when this is present.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// Automatic checks (startup, then about daily). Manual checks always work.
    pub enabled: bool,
    pub current_version: String,
    pub state: UpdateState,
    pub latest: Option<UpdateRelease>,
    /// `latest` is newer than the running build.
    pub newer: bool,
    /// The user chose "Skip this version" for `latest`.
    pub skipped: bool,
    /// `newer` and an asset for this OS/arch with a published checksum exists.
    pub downloadable: bool,
    pub last_checked_unix: Option<u64>,
    pub downloaded_bytes: u64,
    pub download_total: u64,
    /// Where the verified installer is (state `downloaded`). Nothing is run or replaced.
    pub downloaded_path: Option<String>,
    /// Last check or download problem, short and single-line.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallAction {
    Answer,
    Reject,
    Hangup,
}

/// Answer to [`Request::Calls`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallsStatus {
    pub view: crate::calls_view::CallsView,
    /// The latest thing the user should know about a control request ("The phone has not allowed
    /// GeekBridge to answer calls."). Cleared by the next successful action or a new call.
    pub notice: Option<String>,
    /// One line per call source saying why it is or is not usable, without numbers or addresses.
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallingStatus {
    pub platform: String,
    pub backend: String,
    pub verdict: String,
    pub telephony_service_present: bool,
    pub bluetooth_controllers: Option<usize>,
    pub phones: usize,
    pub calls: usize,
    pub read_only: bool,
}

/// What the desktop clipboard bridge can do on this machine. Contains no clipboard content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardStatus {
    /// Human-readable backend name, e.g. `wl-clipboard`, `xclip`, or `none`.
    pub backend: String,
    /// The OS clipboard can be read and written with this backend right now.
    pub available: bool,
    /// How changes are noticed: `polling`, `event` or `none`.
    pub observe: String,
    /// The watcher is running (it only runs while a paired phone has clipboard sync enabled).
    pub watching: bool,
    /// Short fixed reason when `available` is false or the last read/write failed.
    pub problem: Option<String>,
    pub max_text_bytes: u64,
    /// Largest image accepted or sent (PNG/JPEG, streamed as a payload).
    pub max_image_bytes: u64,
    /// A phone item is only offered if it was copied at most this long ago.
    pub window_ms: u64,
    /// The "paste from phone" hotkey, normalized (e.g. `Ctrl+Shift+V`).
    pub hotkey: String,
    /// Whether the hotkey is bound system-wide: `bound`, `not_bound` or `unsupported`.
    pub hotkey_binding: String,
    /// Outcome of the latest "paste from phone" request.
    pub last_pull: Option<ClipboardPullStatus>,
    pub devices: Vec<ClipboardDeviceStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardPullStatus {
    pub device_id: String,
    /// `waiting`, `delivered`, or `none`.
    pub state: String,
    /// Fixed reason for `none`: `locked`, `expired`, `empty`, `needs_gesture`, `disabled`,
    /// `too_large`, `unsupported`, `sensitive`, `timeout`, `failed`.
    pub reason: Option<String>,
    /// `text`, `url`, `html` or `image` when delivered.
    pub kind: Option<String>,
    pub at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardDeviceStatus {
    pub device_id: String,
    pub name: String,
    /// The user's per-device opt-in (the `Clipboard` consent).
    pub enabled: bool,
    pub connected: bool,
    /// The phone advertised the clipboard packets, so sync can work on the current link.
    pub negotiated: bool,
    pub sent: u64,
    pub received: u64,
    /// Items not sent (sensitive, too large, empty) or refused.
    pub skipped: u64,
    pub last_sent_unix_ms: Option<u64>,
    pub last_received_unix_ms: Option<u64>,
    /// Fixed reason for the most recent skip, e.g. `sensitive`, `too_large`.
    pub last_skip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub ipc_version: u32,
    pub service_version: String,
    pub device_id: String,
    pub device_name: String,
    pub own_cert_sha256: String,
    pub paired: Vec<PairedDevice>,
    pub links: Vec<LinkStatus>,
    pub challenges: Vec<PairChallenge>,
    /// Highest event sequence number so far; pass it back as `Events::after` to stay current.
    pub event_seq: u64,
    pub inbound_listener: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Connecting,
    ConnectFailed {
        reason: String,
    },
    NeedsPairing {
        peer_cert_sha256: String,
    },
    PairChallenge {
        challenge_id: String,
    },
    /// A phone presented a QR token that was refused (expired, already used, unknown, locked out).
    /// `reason` is a plain-language sentence.
    QrRefused {
        reason: String,
    },
    /// The next pairing used a valid QR token, so no code comparison was shown.
    QrAccepted,
    Paired,
    PairFailed {
        reason: String,
    },
    Connected,
    LinkLost {
        reason: String,
    },
    PeerUnpaired,
    PeerRequestedRePair,
    Unpaired,
    Disconnected,
    PingReceived,
    PacketRejected,
    ConsentChanged {
        feature: Feature,
        enabled: bool,
    },
    /// A phone offered a file; answer with `ShareDecide` (or the settings auto-accept it).
    ShareOffered {
        transfer_id: u64,
    },
    /// A transfer reached a final state (`done`, `failed`, `cancelled`, `declined`).
    ShareFinished {
        transfer_id: u64,
        ok: bool,
    },
    /// A phone sent a link or text.
    ShareReceivedText {
        transfer_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub at_unix_ms: u64,
    pub device_id: Option<String>,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<Event>,
    pub next_after: u64,
    /// True if events between `after` and the first returned one were discarded; re-read `Status`.
    pub gap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    NotFound,
    /// A link for this device already exists (one live link per device).
    Busy,
    TooMany,
    InvalidEndpoint,
    NotConnected,
    NotUnpaired,
    ChallengeExpired,
    ChallengeStale,
    Unsupported,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Status(Box<Status>),
    Events(EventPage),
    CallingStatus(CallingStatus),
    Calls(Box<CallsStatus>),
    Update(Box<UpdateStatus>),
    ClipboardStatus(Box<ClipboardStatus>),
    Share(Box<ShareStatus>),
    PairQr(Box<PairQr>),
    ConnectionTest(Box<crate::pairing_help::ConnectionTest>),
    Agenda(Box<crate::phone_view::AgendaView>),
    Messages(Box<crate::phone_view::MessagesView>),
    MessageQueued {
        send_id: u64,
    },
    TransferStarted {
        transfer_id: u64,
    },
    /// The request was accepted. Long operations (connect, pair) continue in the background;
    /// watch `Status` or `Events`.
    Ack,
    Error(ErrorBody),
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    /// The service is up but could not open its private state; protected calls cannot work.
    Unavailable {
        code: String,
        message: String,
    },
    /// The service did not accept our token.
    Unauthorized,
    /// The peer on the port is not a GeekBridge service we share a secret with.
    Protocol(String),
    /// The service understood the request and refused it.
    Service(ErrorBody),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot reach GeekBridge service control port: {e}"),
            Self::Unavailable { code, message } => {
                write!(
                    f,
                    "GeekBridge service has no protected state ({code}): {message}"
                )
            }
            Self::Unauthorized => f.write_str("GeekBridge service rejected the control token"),
            Self::Protocol(m) => write!(f, "unexpected control protocol reply: {m}"),
            Self::Service(e) => write!(
                f,
                "GeekBridge service refused the request ({:?}): {}",
                e.code, e.message
            ),
        }
    }
}
impl std::error::Error for IpcError {}
impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What the unauthenticated hello says about the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hello {
    Ready { nonce: String },
    Unavailable { code: String, message: String },
}

pub fn format_hello(hello: &Hello) -> String {
    match hello {
        Hello::Ready { nonce } => format!("GB-IPC {IPC_VERSION} {nonce}\n"),
        Hello::Unavailable { code, message } => {
            let clean = |s: &str| -> String {
                s.chars()
                    .map(|c| {
                        if c.is_ascii_graphic() || c == ' ' {
                            c
                        } else {
                            '?'
                        }
                    })
                    .take(120)
                    .collect()
            };
            format!(
                "GB-IPC {IPC_VERSION} unavailable {} {}\n",
                clean(code).replace(' ', "_"),
                clean(message)
            )
        }
    }
}

pub fn parse_hello(line: &str) -> Result<Hello, IpcError> {
    let rest = line
        .strip_prefix(&format!("GB-IPC {IPC_VERSION} "))
        .ok_or_else(|| IpcError::Protocol("not a GeekBridge control service".into()))?;
    if let Some(rest) = rest.strip_prefix("unavailable ") {
        let (code, message) = rest.split_once(' ').unwrap_or((rest, ""));
        return Ok(Hello::Unavailable {
            code: code.to_owned(),
            message: message.to_owned(),
        });
    }
    if decode_hex::<32>(rest).is_none() {
        return Err(IpcError::Protocol("malformed nonce".into()));
    }
    Ok(Hello::Ready {
        nonce: rest.to_owned(),
    })
}

struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
    buffer: Vec<u8>,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "control exchange deadline exceeded",
                )
            })
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    /// Next line without its terminator, at most `limit` bytes.
    fn read_line(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                return Ok(line);
            }
            if self.buffer.len() > limit {
                return Err(io::Error::other("control line too long"));
            }
            self.stream.set_read_timeout(Some(self.remaining()?))?;
            let mut chunk = [0_u8; 4096];
            let n = self.stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "service closed the connection",
                ));
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
    }
}

fn open(port: u16) -> io::Result<DeadlineStream> {
    let deadline = Instant::now() + CLIENT_DEADLINE;
    let stream = TcpStream::connect_timeout(&control_address(port), Duration::from_millis(800))?;
    stream.set_nodelay(true)?;
    Ok(DeadlineStream {
        stream,
        deadline,
        buffer: Vec::new(),
    })
}

fn read_hello(conn: &mut DeadlineStream) -> Result<Hello, IpcError> {
    let line = conn.read_line(256)?;
    let line =
        std::str::from_utf8(&line).map_err(|_| IpcError::Protocol("hello is not text".into()))?;
    parse_hello(line)
}

/// Unauthenticated check whether the service has protected state at all. Never sends a request.
pub fn probe_protected(port: u16) -> Result<Hello, IpcError> {
    read_hello(&mut open(port)?)
}

/// Send one authenticated request and verify the answer. A `Response::Error` is returned as
/// `Err(IpcError::Service)` so callers can use `?`.
pub fn call(port: u16, token: &Token, request: &Request) -> Result<Response, IpcError> {
    let mut conn = open(port)?;
    let nonce = match read_hello(&mut conn)? {
        Hello::Ready { nonce } => nonce,
        Hello::Unavailable { code, message } => {
            return Err(IpcError::Unavailable { code, message });
        }
    };
    let json = serde_json::to_vec(request).map_err(|e| IpcError::Protocol(e.to_string()))?;
    if json.len() > MAX_REQUEST_BYTES {
        return Err(IpcError::Protocol("request too large".into()));
    }
    let mac = token.request_mac(&nonce, &json);
    let mut frame = encode_hex(&mac).into_bytes();
    frame.push(b' ');
    frame.extend_from_slice(&json);
    frame.push(b'\n');
    conn.write_all(&frame)?;

    let line = conn.read_line(MAX_RESPONSE_BYTES + 128)?;
    if line == b"ERR unauthorized" {
        return Err(IpcError::Unauthorized);
    }
    let (their_mac, body) =
        split_frame(&line).ok_or_else(|| IpcError::Protocol("malformed response frame".into()))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(IpcError::Protocol("response too large".into()));
    }
    let expected = token.response_mac(&nonce, &mac, body);
    if !constant_time_eq(&expected, &their_mac) {
        return Err(IpcError::Protocol("response authentication failed".into()));
    }
    match serde_json::from_slice::<Response>(body).map_err(|e| IpcError::Protocol(e.to_string()))? {
        Response::Error(e) => Err(IpcError::Service(e)),
        other => Ok(other),
    }
}

/// Split `<64 hex mac> <payload>` into its parts.
pub fn split_frame(line: &[u8]) -> Option<([u8; 32], &[u8])> {
    if line.len() < 66 || line[64] != b' ' {
        return None;
    }
    let mac = decode_hex::<32>(std::str::from_utf8(&line[..64]).ok()?)?;
    Some((mac, &line[65..]))
}

pub fn status(port: u16, token: &Token) -> Result<Status, IpcError> {
    match call(port, token, &Request::Status)? {
        Response::Status(s) => Ok(*s),
        other => Err(IpcError::Protocol(format!("unexpected reply {other:?}"))),
    }
}

pub fn events(port: u16, token: &Token, after: u64) -> Result<EventPage, IpcError> {
    match call(port, token, &Request::Events { after })? {
        Response::Events(p) => Ok(p),
        other => Err(IpcError::Protocol(format!("unexpected reply {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(bytes: &[u8]) -> String {
        encode_hex(bytes)
    }

    #[test]
    fn hmac_matches_rfc4231_vectors() {
        let mac = hmac_sha256(&[0x0b; 20], &[b"Hi ", b"There"]);
        assert_eq!(
            hex_of(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want for nothing?"]);
        assert_eq!(
            hex_of(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Key longer than the block size is hashed first (RFC 4231 case 6).
        let mac = hmac_sha256(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
        );
        assert_eq!(
            hex_of(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn token_parse_is_strict_and_debug_is_redacted() {
        let token = Token::from_bytes([7; 32]);
        let file = token.to_file_contents();
        assert_eq!(Token::parse(file.as_bytes()).unwrap(), token);
        assert!(Token::parse(b"abc").is_err());
        assert!(Token::parse(&[b'g'; 64]).is_err());
        assert!(Token::parse(&[b'a'; 65]).is_err());
        assert!(!format!("{token:?}").contains("07"));
    }

    #[test]
    fn macs_bind_nonce_request_and_body() {
        let t = Token::from_bytes([1; 32]);
        let nonce = "ab".repeat(32);
        let a = t.request_mac(&nonce, b"{}");
        assert_ne!(a, t.request_mac(&"cd".repeat(32), b"{}"));
        assert_ne!(a, t.request_mac(&nonce, b"{ }"));
        let r = t.response_mac(&nonce, &a, b"x");
        assert_ne!(r, t.response_mac(&nonce, &[0; 32], b"x"));
        assert_ne!(r, a);
        assert_ne!(a, Token::from_bytes([2; 32]).request_mac(&nonce, b"{}"));
    }

    #[test]
    fn requests_reject_unknown_ops_and_fields() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"status"}"#).is_ok());
        assert!(serde_json::from_str::<Request>(r#"{"op":"format_disk"}"#).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"op":"ping","device_id":"x","extra":1}"#).is_err()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"op":"set_consent","device_id":"x","feature":"sms","enabled":true}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"op":"set_consent","device_id":"x","feature":"root","enabled":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn hello_round_trips_and_sanitises() {
        let nonce = "0f".repeat(32);
        let line = format_hello(&Hello::Ready {
            nonce: nonce.clone(),
        });
        assert_eq!(
            parse_hello(line.trim_end()).unwrap(),
            Hello::Ready { nonce }
        );
        let line = format_hello(&Hello::Unavailable {
            code: "state insecure".into(),
            message: "bad\nmode".into(),
        });
        assert_eq!(line.matches('\n').count(), 1);
        assert!(matches!(
            parse_hello(line.trim_end()).unwrap(),
            Hello::Unavailable { code, .. } if code == "state_insecure"
        ));
        assert!(parse_hello("HTTP/1.0 200 OK").is_err());
        assert!(parse_hello("GB-IPC 1 zz").is_err());
    }

    #[test]
    fn event_flattens_kind() {
        let e = Event {
            seq: 3,
            at_unix_ms: 9,
            device_id: Some("d".into()),
            kind: EventKind::LinkLost { reason: "x".into() },
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "link_lost");
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), e);
    }

    #[test]
    fn client_reports_unavailable_without_sending_anything() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(
                format_hello(&Hello::Unavailable {
                    code: "state_insecure".into(),
                    message: "mode".into(),
                })
                .as_bytes(),
            )
            .unwrap();
            let mut rest = Vec::new();
            s.set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let _ = s.read_to_end(&mut rest);
            rest
        });
        let err = call(port, &Token::from_bytes([1; 32]), &Request::Status).unwrap_err();
        assert!(matches!(err, IpcError::Unavailable { .. }));
        assert!(
            server.join().unwrap().is_empty(),
            "client leaked bytes to an unavailable service"
        );
    }

    #[test]
    fn client_rejects_forged_responses_and_trickle() {
        // Server that knows no token: replies with a plausible frame with a wrong mac.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let nonce = "11".repeat(32);
            s.write_all(format_hello(&Hello::Ready { nonce }).as_bytes())
                .unwrap();
            let mut buf = [0_u8; 512];
            let _ = s.read(&mut buf).unwrap();
            let forged = format!("{} {{\"kind\":\"ack\"}}\n", "00".repeat(32));
            s.write_all(forged.as_bytes()).unwrap();
        });
        let err = call(port, &Token::from_bytes([1; 32]), &Request::Status).unwrap_err();
        assert!(matches!(err, IpcError::Protocol(_)), "{err}");
        server.join().unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            for _ in 0..200 {
                if s.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let start = Instant::now();
        assert!(probe_protected(port).is_err());
        assert!(start.elapsed() < CLIENT_DEADLINE + Duration::from_secs(1));
        peer.join().unwrap();
    }
}
