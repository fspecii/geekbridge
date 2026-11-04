// SPDX-License-Identifier: GPL-3.0-or-later
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
/// Total time one client exchange (connect, hello, request, response) may take. It stays longer than
/// the service's own exchange deadline so a slow request is answered or closed by the service first.
/// Handlers fsync several state files (file lock, rename, directory sync); a busy or slow disk must
/// not turn a half-finished change into a closed connection.
pub const CLIENT_DEADLINE: Duration = Duration::from_secs(10);
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
    /// Debug-build diagnostics from the phone (`geekbridge.debug.*`): its log and a redacted state dump.
    /// Only a debug build of the phone ever advertises it; it is on by default for such a phone.
    Debug,
    /// The phone's battery level and charging state (`kdeconnect.battery`). Off by default; read
    /// only while on, kept in memory for the life of the link.
    Battery,
    /// A short list of the phone's recent photos with small thumbnails, on request (`geekbridge.photos.*`).
    /// Off by default; never a background upload, nothing is read unless this computer asked.
    Photos,
    /// EXPERIMENTAL: listening to a phone call on this computer over Wi-Fi through the phone's ADB helper
    /// (`geekbridge.callaudio.*`). Off by default; the first of three consents (see `bridge-callwifi`).
    CallAudioWifi,
}

impl Feature {
    pub const ALL: [Feature; 12] = [
        Feature::Sms,
        Feature::Contacts,
        Feature::Calendar,
        Feature::CalendarEdit,
        Feature::CalendarAlerts,
        Feature::Clipboard,
        Feature::Notifications,
        Feature::Telephony,
        Feature::Debug,
        Feature::Battery,
        Feature::Photos,
        Feature::CallAudioWifi,
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
    /// Rename this computer. Peers see the new name from their next connection; the service saves
    /// it. The name is validated (no control characters, at most 32 characters) and stored as
    /// the peers will display it.
    SetDeviceName {
        name: String,
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
    /// Ask a debug-build phone for its redacted state dump (`geekbridge.debug.state`). Needs the `debug`
    /// consent and a phone that advertises the debug packets; answered with [`Response::PhoneDebugState`].
    PhoneDebugState {
        device_id: String,
    },
    /// Mirrored phone notifications currently held (memory only), newest first. Carries titles and
    /// texts from the phone, so it is only served on this authenticated control channel.
    Notifications {
        #[serde(default)]
        device_id: Option<String>,
    },
    /// The phone photos currently held (memory only), newest first; no image bytes. Needs the `photos` consent.
    Photos {
        #[serde(default)]
        device_id: Option<String>,
    },
    /// Ask a phone for its newest photos (a list and then thumbnails). Needs the `photos` consent; the
    /// service asks at most once every few seconds per phone.
    PhotosRefresh {
        device_id: String,
    },
    /// One held thumbnail (a small JPEG). Needs the `photos` consent.
    PhotoThumb {
        device_id: String,
        id: String,
    },
    /// Ask the phone to send one photo in full over the share channel. Progress is in `Photos.fetches`.
    PhotoFetch {
        device_id: String,
        id: String,
        mode: crate::photos_view::FetchMode,
    },
    /// Dismiss one mirrored notification: removed here and, when the phone allows it, cancelled there.
    /// Needs the `notifications` consent.
    DismissNotification {
        device_id: String,
        id: String,
    },
    /// Press a button of a mirrored notification on the phone (`action` is one of its listed titles).
    NotificationAction {
        device_id: String,
        id: String,
        action: String,
    },
    /// Read-only probe of the host's Bluetooth calling backend (counts only, no numbers).
    CallingStatus,
    /// The "Call audio on this computer" checklist (adapter, radio, pairing, hands-free, headphones)
    /// with the devices to choose from and the state of the local test. Read-only; carries no phone
    /// numbers. Answered with `Response::CallAudioSetup`.
    CallAudioSetup,
    /// Remember the headphones and microphone to use for call audio (`None` = the system default).
    /// Names must be ones the latest `CallAudioSetup` listed. Answered with `Response::CallAudioSetup`.
    CallAudioChoose {
        output: Option<String>,
        input: Option<String>,
    },
    /// Start the local test: a short tone on the chosen output, then five seconds of microphone
    /// level. No call is involved and nothing is recorded or kept. Answered with `Response::CallAudioSetup`
    /// (poll `CallAudioSetup` for the meter).
    CallAudioTest,
    /// The merged call view: ringing and live calls with caller, the controls that really exist
    /// and the audio route. Carries phone numbers and names, so it is only served on this
    /// authenticated control channel and the apps must keep it out of logs.
    Calls,
    /// Answer, decline or end a call. `target` must be one the latest `Calls` view offered.
    CallAction {
        target: crate::calls_view::ActionTarget,
        action: CallAction,
    },
    /// Place a call to `number` through `source` (the `dial` the latest `Calls` view offered). The
    /// service re-checks that it is still offered and that the number is plain digits. The apps
    /// must have asked the user to confirm first; nothing here dials without that.
    CallDial {
        source: String,
        number: crate::calls::Sensitive,
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
    /// Send the live call's audio back to the phone (Bluetooth hands-free sources only).
    CallAudioPhone {
        source: String,
    },
    /// The "Hear calls on this computer" checklist and the state of the guided setup.
    HearCalls,
    /// Drive the guided call-audio setup (pair the phone as a hands-free device, fix the
    /// WirePlumber configuration). Answers with `HearCalls`.
    CallAudioPair {
        action: CallAudioSetupAction,
    },
    /// List the speakers/headphones and microphones a call can use, with the user's pick. With
    /// `set`, first store `output`/`input` as the pick (`None` = follow the system default).
    CallAudioDevices {
        #[serde(default)]
        set: bool,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        input: Option<String>,
    },
    /// EXPERIMENTAL call audio over Wi-Fi: state, start/stop listening, speak back. Answers with `CallWifi`.
    CallWifi {
        action: crate::call_wifi_view::CallWifiAction,
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
    /// Choose the update channel (`stable` releases or the rolling `dev` build). Persisted; the
    /// cached offer of the other channel is dropped.
    UpdateSetChannel {
        channel: UpdateChannel,
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
        /// How many of the thread's newest messages to return; 0 = the default `MAX_MESSAGES_SHOWN`. Capped by
        /// `MAX_MESSAGES_LIMIT` and by the reply's byte budget.
        #[serde(default)]
        message_limit: usize,
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
    /// Latest battery reading from this phone while the link is up and `Feature::Battery` is on.
    /// Memory only; gone with the link.
    #[serde(default)]
    pub battery: Option<BatteryStatus>,
}

/// One mirrored phone notification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationItem {
    pub device_id: String,
    pub device_name: String,
    pub id: String,
    pub app: String,
    pub title: String,
    pub text: String,
    pub time_ms: Option<u64>,
    pub clearable: bool,
    pub actions: Vec<String>,
}

/// Answer to [`Request::Notifications`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationsView {
    /// Newest first.
    pub items: Vec<NotificationItem>,
    /// Paired phones whose `notifications` consent is on.
    pub enabled_devices: Vec<String>,
}

/// A phone's battery as last reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
    /// The phone flagged its low-battery threshold.
    pub low: bool,
}

impl BatteryStatus {
    /// Short plain text for tooltips and tiles: `Battery 80%` or `Battery 80% (charging)`.
    pub fn text(&self) -> String {
        format!(
            "Battery {}%{}",
            self.percent,
            if self.charging { " (charging)" } else { "" }
        )
    }
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

/// Wording for [`UpdateState::NoPublicRelease`], shared by `geekbridge-ctl` and the desktop UI.
pub const UPDATE_NO_PUBLIC_RELEASE: &str =
    "No public release found: the project repository may be private or has no releases yet.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// Never checked (or the last answer is unknown).
    Idle,
    Checking,
    UpToDate,
    /// The release list answered 404/403: the repository may be private or have no releases yet.
    /// Not "up to date", and not a failure.
    NoPublicRelease,
    /// A newer release exists.
    Available,
    Downloading,
    /// The installer is downloaded and its SHA-256 matched the published value.
    Downloaded,
}

/// Which releases the update check follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Published, non-prerelease GitHub releases (`vX.Y.Z`).
    #[default]
    Stable,
    /// The rolling `dev` prerelease that always carries the newest main build (docs/updates.md).
    Dev,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Dev => "dev",
        }
    }

    pub fn parse(text: &str) -> Option<UpdateChannel> {
        match text.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(UpdateChannel::Stable),
            "dev" => Some(UpdateChannel::Dev),
            _ => None,
        }
    }
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
    /// The channel being followed.
    #[serde(default)]
    pub channel: UpdateChannel,
    /// Source commit of the running build (12 hex digits), when it was built from a git checkout.
    #[serde(default)]
    pub current_commit: Option<String>,
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
    /// Set when calls can be controlled from here but their audio cannot come to this computer
    /// (for example no Bluetooth controller): what works, what stays on the phone, the next step.
    #[serde(default)]
    pub audio_note: Option<String>,
}

/// The opening line of `geekbridge-ctl calling` (see [`HearCalls::banner`]); also used when the
/// service is not running and only the adapter count is known.
pub fn banner_line(no_bluetooth: bool, ready: bool, phone: Option<&str>) -> String {
    if no_bluetooth {
        "Call audio: NO BLUETOOTH. Calls can't carry voice on this computer; add a USB Bluetooth adapter (you can still see and answer calls).".into()
    } else if ready {
        match phone {
            Some(p) => format!("Call audio: ready (phone: {p})."),
            None => "Call audio: ready.".into(),
        }
    } else if phone.is_some() {
        "Call audio: set up, but not working yet. The first FAIL below says what to fix; geekbridge-ctl call setup pairs again.".into()
    } else {
        "Call audio: not set up. One step: press \"Set up call audio\" in Settings, or run geekbridge-ctl call setup.".into()
    }
}

/// What every place that shows calls says when this computer has no Bluetooth adapter.
pub const NO_BLUETOOTH_WARNING: &str = "This computer has no Bluetooth. You can see and answer calls here, but talking needs Bluetooth (add a USB adapter). Messages, contacts and the other features work normally.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum CallAudioSetupAction {
    /// Start pairing. `phone`: the phone's Bluetooth name or part of it (default: the name of
    /// the connected GeekBridge phone, else the first phone that shows up).
    Start {
        #[serde(default)]
        phone: Option<String>,
    },
    /// The user compared the six-digit code with the phone's.
    Confirm {
        accept: bool,
    },
    Cancel,
    /// Write GeekBridge's WirePlumber fragment (no sudo) and restart WirePlumber.
    FixConfig,
    /// Unpair the remembered phone from this computer's Bluetooth and forget it for call audio
    /// (the phone's own Bluetooth entry is the user's to remove).
    Forget,
}

/// The section of `docs/call-audio-troubleshooting.md` that explains a checklist step.
pub fn troubleshooting_anchor(step_id: &str) -> Option<&'static str> {
    Some(match step_id {
        "adapter" => "adapter-not-found",
        "rfkill" => "radio-blocked-rfkill",
        "service" => "bluetooth-service-inactive",
        "powered" => "adapter-powered-off",
        "wireplumber" | "telephony" => "the-wireplumber-drop-in",
        "paired" | "connected" => "pairing-loops",
        "hfp" => "phone-never-connects-hfp",
        "card" => "profile-audio-gateway-missing",
        "devices" => "call-audio-goes-to-the-phone",
        "voice" => "connected-but-silent",
        _ => return None,
    })
}

/// One step of the "Hear calls on this computer" checklist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HearCallsStep {
    /// Stable id: adapter, rfkill, service, powered, wireplumber, telephony, paired, connected,
    /// hfp, card, devices, voice.
    pub id: String,
    pub title: String,
    /// ok, warn, fail or skip.
    pub state: String,
    pub detail: String,
    pub fix: Option<String>,
}

/// The guided setup as it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallAudioSetupState {
    /// idle, working, confirm (show `passkey`), done or failed.
    pub phase: String,
    /// The latest line for the user.
    pub message: String,
    /// Six digits to compare with the phone's screen while `phase` is confirm.
    pub passkey: Option<String>,
    /// The device asking to pair.
    pub device: Option<String>,
    /// Every line of this run.
    pub log: Vec<String>,
    /// The phone set up for call audio (name), remembered across restarts.
    pub phone: Option<String>,
}

/// Can a call's voice play here, and if not what is the next step. Stable for the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HearCalls {
    pub ready: bool,
    /// No Bluetooth adapter at all: show [`NO_BLUETOOTH_WARNING`].
    pub no_bluetooth: bool,
    /// One line: ready, the warning above, or the first open step.
    pub headline: String,
    pub steps: Vec<HearCallsStep>,
    pub setup: CallAudioSetupState,
    /// A Bluetooth adapter appeared since the service started and no phone is set up yet:
    /// offer "Set up call audio".
    #[serde(default)]
    pub offer_setup: bool,
}

impl HearCalls {
    /// One line that opens `geekbridge-ctl calling`: can a call's voice come through this
    /// computer, and the one thing to do next.
    pub fn banner(&self) -> String {
        banner_line(self.no_bluetooth, self.ready, self.setup.phone.as_deref())
    }

    /// The first failing step, else the first warning.
    pub fn first_problem(&self) -> Option<&HearCallsStep> {
        self.steps
            .iter()
            .find(|s| s.state == "fail")
            .or_else(|| self.steps.iter().find(|s| s.state == "warn"))
    }

    /// The checklist as text (`geekbridge-ctl calling`).
    pub fn render(&self) -> String {
        let mut out = String::from("Hear calls on this computer:\n");
        if self.no_bluetooth {
            out.push_str(&format!("  {NO_BLUETOOTH_WARNING}\n"));
        }
        for (n, s) in self.steps.iter().enumerate() {
            let mark = match s.state.as_str() {
                "ok" => "ok  ",
                "warn" => "warn",
                "fail" => "FAIL",
                _ => "--  ",
            };
            out.push_str(&format!(
                "  {:>2}. [{mark}] {}: {}\n",
                n + 1,
                s.title,
                s.detail
            ));
            if let Some(fix) = &s.fix {
                for (i, line) in fix.lines().enumerate() {
                    out.push_str(if i == 0 {
                        "        fix: "
                    } else {
                        "             "
                    });
                    out.push_str(line);
                    out.push('\n');
                }
            }
            if matches!(s.state.as_str(), "fail" | "warn")
                && let Some(a) = troubleshooting_anchor(&s.id)
            {
                out.push_str(&format!(
                    "        more: docs/call-audio-troubleshooting.md#{a}\n"
                ));
            }
        }
        out.push_str(if self.ready {
            "  Result: ready. Answer a call from this computer and the voice comes to the devices above.\n"
        } else {
            "  Result: not ready yet; fix the first FAIL above and run this again.\n"
        });
        out.push_str(
            "  Details and a hardware test script: docs/reports/call-audio-pc-headset.md; every failing step: docs/call-audio-troubleshooting.md\n",
        );
        out
    }
}

/// One speaker/headphone or microphone a call can use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallAudioDevice {
    /// The sound server's device name (what `CallAudioDevices` takes).
    pub name: String,
    pub description: String,
    pub is_default: bool,
    /// Itself a Bluetooth device (a headset), not the phone.
    pub bluetooth: bool,
    pub muted: bool,
}

/// The call-audio device choice: what exists, what the user picked, what a call uses right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallAudioDevices {
    pub outputs: Vec<CallAudioDevice>,
    pub inputs: Vec<CallAudioDevice>,
    /// The user's pick; `None` = the system default.
    pub chosen_output: Option<String>,
    pub chosen_input: Option<String>,
    /// Where a live call's audio is and anything wrong with it (one line each, with a fix).
    #[serde(default)]
    pub call: Vec<String>,
    /// Set when the devices could not be read (no sound server, unsupported platform).
    #[serde(default)]
    pub problem: Option<String>,
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
    /// Stock KDE Connect is running on this computer and may sync the same clipboard to the same
    /// phone (echo or loops are possible). Informational: nothing is switched off.
    #[serde(default)]
    pub other_sync_app: bool,
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
    /// Paired phones this computer is dialing again after the link dropped (shown as "Reconnecting").
    #[serde(default)]
    pub reconnecting: Vec<String>,
    /// For each paired phone without a link: the plain-words reason and a suggested fix.
    #[serde(default)]
    pub disconnected: Vec<crate::why_disconnected::WhyDisconnected>,
}

impl Status {
    /// One calm line when the listener moved off port 1716 (see [`listen_port_note`]).
    pub fn port_note(&self) -> Option<String> {
        listen_port_note(self.inbound_listener, true)
    }
}

/// Why the listener is not on 1716, worded so it does not read as a fault. `None` on 1716, when
/// the address was chosen explicitly (`auto` false) or when there is no listener. KDE Connect
/// reserves 1716-1764 and phones learn the port from our announcement, so any port in the range works.
pub fn listen_port_note(listener: Option<SocketAddr>, auto: bool) -> Option<String> {
    let port = listener?.port();
    (auto && (1717..=1764).contains(&port)).then(|| {
        format!("Using port {port} because 1716 is in use by another program: this is fine")
    })
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
    CallAudioSetup(Box<crate::call_audio::Setup>),
    /// The phone's redacted state dump (multi-line text).
    PhoneDebugState {
        text: String,
    },
    Calls(Box<CallsStatus>),
    CallAudioDevices(Box<CallAudioDevices>),
    HearCalls(Box<HearCalls>),
    CallWifi(Box<crate::call_wifi_view::CallWifiView>),
    Notifications(Box<NotificationsView>),
    Photos(Box<crate::photos_view::PhotosView>),
    PhotoThumb(Box<crate::photos_view::PhotoThumbView>),
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

/// Adding a `Response` variant stops compiling here until `Response::from_json` learns it too.
#[allow(dead_code)]
fn every_response_is_parsed(r: &Response) {
    match r {
        Response::Status(_)
        | Response::Events(_)
        | Response::CallingStatus(_)
        | Response::CallAudioSetup(_)
        | Response::PhoneDebugState { .. }
        | Response::Calls(_)
        | Response::CallAudioDevices(_)
        | Response::HearCalls(_)
        | Response::CallWifi(_)
        | Response::Notifications(_)
        | Response::Photos(_)
        | Response::PhotoThumb(_)
        | Response::Update(_)
        | Response::ClipboardStatus(_)
        | Response::Share(_)
        | Response::PairQr(_)
        | Response::ConnectionTest(_)
        | Response::Agenda(_)
        | Response::Messages(_)
        | Response::MessageQueued { .. }
        | Response::TransferStarted { .. }
        | Response::Ack
        | Response::Error(_) => {}
    }
}

impl Response {
    /// Parse a response body. Same wire format and result as `serde_json::from_slice::<Response>`, but
    /// the `kind` tag is read first and the payload is then deserialized straight into its own type.
    /// The derived impl buffers every reply into serde's `Content` tree and replays it, which
    /// instantiates a second copy of every nested type's deserializer.
    pub fn from_json(json: &[u8]) -> Result<Response, serde_json::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        struct Tag {
            kind: String,
        }
        #[derive(Deserialize)]
        struct Text {
            text: String,
        }
        #[derive(Deserialize)]
        struct SendId {
            send_id: u64,
        }
        #[derive(Deserialize)]
        struct TransferId {
            transfer_id: u64,
        }
        fn p<'a, T: Deserialize<'a>>(json: &'a [u8]) -> Result<T, serde_json::Error> {
            serde_json::from_slice(json)
        }
        let tag: Tag = p(json)?;
        Ok(match tag.kind.as_str() {
            "status" => Response::Status(Box::new(p(json)?)),
            "events" => Response::Events(p(json)?),
            "calling_status" => Response::CallingStatus(p(json)?),
            "call_audio_setup" => Response::CallAudioSetup(Box::new(p(json)?)),
            "phone_debug_state" => Response::PhoneDebugState {
                text: p::<Text>(json)?.text,
            },
            "calls" => Response::Calls(Box::new(p(json)?)),
            "call_audio_devices" => Response::CallAudioDevices(Box::new(p(json)?)),
            "hear_calls" => Response::HearCalls(Box::new(p(json)?)),
            "call_wifi" => Response::CallWifi(Box::new(p(json)?)),
            "notifications" => Response::Notifications(Box::new(p(json)?)),
            "photos" => Response::Photos(Box::new(p(json)?)),
            "photo_thumb" => Response::PhotoThumb(Box::new(p(json)?)),
            "update" => Response::Update(Box::new(p(json)?)),
            "clipboard_status" => Response::ClipboardStatus(Box::new(p(json)?)),
            "share" => Response::Share(Box::new(p(json)?)),
            "pair_qr" => Response::PairQr(Box::new(p(json)?)),
            "connection_test" => Response::ConnectionTest(Box::new(p(json)?)),
            "agenda" => Response::Agenda(Box::new(p(json)?)),
            "messages" => Response::Messages(Box::new(p(json)?)),
            "message_queued" => Response::MessageQueued {
                send_id: p::<SendId>(json)?.send_id,
            },
            "transfer_started" => Response::TransferStarted {
                transfer_id: p::<TransferId>(json)?.transfer_id,
            },
            "ack" => Response::Ack,
            "error" => Response::Error(p(json)?),
            other => {
                return Err(serde_json::Error::custom(format_args!(
                    "unknown variant `{other}`"
                )));
            }
        })
    }
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
    call_with_body(port, token, request).map(|(response, _)| response)
}

/// [`call`], also returning the authenticated reply body exactly as the service wrote it (compact JSON),
/// for callers that print it with [`pretty_json`] instead of serializing the parsed reply again.
pub fn call_with_body(
    port: u16,
    token: &Token,
    request: &Request,
) -> Result<(Response, Vec<u8>), IpcError> {
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
    match Response::from_json(body).map_err(|e| IpcError::Protocol(e.to_string()))? {
        Response::Error(e) => Err(IpcError::Service(e)),
        other => Ok((other, body.to_vec())),
    }
}

/// Re-indent compact JSON the way `serde_json::to_string_pretty` prints it (two spaces, `"key": value`,
/// `{}` and `[]` for empty containers), keeping key order and string escapes byte for byte. Input must be
/// valid JSON (an authenticated service reply that already parsed); it avoids instantiating a second,
/// serializing copy of every reply type in the CLI.
pub fn pretty_json(compact: &[u8]) -> String {
    fn line(out: &mut Vec<u8>, depth: usize) {
        out.push(b'\n');
        out.resize(out.len() + depth * 2, b' ');
    }
    let mut out = Vec::with_capacity(compact.len() * 2);
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let mut i = 0;
    while i < compact.len() {
        let c = compact[i];
        i += 1;
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push(c);
            }
            b'{' | b'[' => {
                out.push(c);
                if compact.get(i).is_some_and(|n| matches!(n, b'}' | b']')) {
                    out.push(compact[i]);
                    i += 1;
                } else {
                    depth += 1;
                    line(&mut out, depth);
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                line(&mut out, depth);
                out.push(c);
            }
            b',' => {
                out.push(c);
                line(&mut out, depth);
            }
            b':' => out.extend_from_slice(b": "),
            _ => out.push(c),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    fn pretty_json_matches_serde_json_pretty() {
        let v = serde_json::json!({
            "kind": "status", "empty_o": {}, "empty_a": [], "n": [1, -2.5, null, true],
            "s": "q\"uote \\ {brace} [x], a:b \n \u{e9}", "nested": {"a": [{"b": []}, {}], "c": {"d": {"e": "f"}}},
            "list": [[], [[]], [{"k": "v"}]]
        });
        let compact = serde_json::to_vec(&v).unwrap();
        assert_eq!(
            pretty_json(&compact),
            serde_json::to_string_pretty(&v).unwrap()
        );
        for s in [r#"{}"#, r#"[]"#, r#""x""#, "12", r#"{"a":"\\"}"#] {
            let v: serde_json::Value = serde_json::from_str(s).unwrap();
            assert_eq!(
                pretty_json(s.as_bytes()),
                serde_json::to_string_pretty(&v).unwrap()
            );
        }
    }

    #[test]
    fn from_json_matches_the_derived_response_parser() {
        let samples = [
            Response::Ack,
            Response::MessageQueued { send_id: 7 },
            Response::TransferStarted { transfer_id: 9 },
            Response::PhoneDebugState {
                text: "a\nb".into(),
            },
            Response::Error(ErrorBody {
                code: ErrorCode::Busy,
                message: "m".into(),
            }),
            Response::Events(EventPage {
                events: vec![Event {
                    seq: 1,
                    at_unix_ms: 2,
                    device_id: None,
                    kind: EventKind::Connected,
                }],
                next_after: 1,
                gap: false,
            }),
        ];
        for r in samples {
            let json = serde_json::to_vec(&r).unwrap();
            assert_eq!(Response::from_json(&json).unwrap(), r);
            assert_eq!(serde_json::from_slice::<Response>(&json).unwrap(), r);
        }
        assert!(Response::from_json(br#"{"kind":"nope"}"#).is_err());
        assert!(Response::from_json(br#"{"x":1}"#).is_err());
        assert!(Response::from_json(br#"{"kind":"message_queued"}"#).is_err());
        assert!(Response::from_json(b"not json").is_err());
    }

    // ---- test-gaps-4: Response::from_json parity with the derived parser, every variant

    /// One representative reply per `Response` variant, as the JSON the service writes. Each is checked
    /// to parse with the derived parser first, so a sample that rots fails loudly instead of testing nothing.
    fn every_response_json() -> Vec<(&'static str, serde_json::Value)> {
        use serde_json::json;
        let view = json!({
            "rows": [{
                "key": "k", "phone": "Pixel", "direction": "incoming", "phase": "incoming",
                "number": "+155", "name": null, "answer": null, "reject": null, "hangup": null,
                "missing_control": "x"
            }],
            "audio": {"route": {"route": "no_call"}, "hint": null, "connect": null},
            "mute": null, "sources_up": 1,
            "dial": {"source": "link:p1", "path": "link", "phone": "Pixel"}
        });
        let status = json!({
            "kind": "status", "ipc_version": 1, "service_version": "0.1.0", "device_id": "me",
            "device_name": "pc", "own_cert_sha256": "", "paired": [{
                "device_id": "p1", "name": "Pixel", "device_type": "phone", "cert_sha256": "ab",
                "paired_at_unix": 5, "consents": []
            }], "links": [], "challenges": [], "event_seq": 3, "inbound_listener": "0.0.0.0:1716",
            "reconnecting": ["p1"], "disconnected": [{
                "device_id": "p1", "code": "gave_up", "sentence": "s", "fix": "f",
                "last_seen_ago_s": 3, "tries": 2
            }]
        });
        let v = vec![
            ("status", status),
            (
                "events",
                json!({"kind": "events", "events": [
                {"seq": 1, "at_unix_ms": 2, "device_id": "p1", "kind": "link_lost", "reason": "x"},
                {"seq": 2, "at_unix_ms": 3, "device_id": null, "kind": "connected"}
            ], "next_after": 2, "gap": true}),
            ),
            (
                "calling_status",
                json!({"kind": "calling_status", "platform": "linux", "backend": "pipewire",
                "verdict": "ready", "telephony_service_present": true, "bluetooth_controllers": 1,
                "phones": 1, "calls": 0, "read_only": false}),
            ),
            (
                "call_audio_setup",
                json!({"kind": "call_audio_setup", "platform": "linux", "verdict": "todo",
                "steps": [{"id": "adapter", "state": "ok"},
                          {"id": "hfp", "state": "todo", "fix": {"kind": "connect", "command": "x"}}],
                "can_test": false}),
            ),
            (
                "phone_debug_state",
                json!({"kind": "phone_debug_state", "text": "a\nb \u{1F600}"}),
            ),
            (
                "calls",
                json!({"kind": "calls", "view": view, "notice": "n", "sources": ["a", "b"], "audio_note": null}),
            ),
            (
                "notifications",
                json!({"kind": "notifications", "items": [{
                "device_id": "p1", "device_name": "Pixel", "id": "gb1", "app": "Chat", "title": "T",
                "text": "x", "time_ms": 5, "clearable": true, "actions": ["A"]
            }], "enabled_devices": ["p1"]}),
            ),
            (
                "photos",
                json!({"kind": "photos", "items": [{
                "device_id": "p1", "id": "p3", "name": "IMG_1.jpg", "taken_ms": 5, "size": 9,
                "width": 4, "height": 3, "has_thumb": true
            }], "devices": [{"device_id": "p1", "name": "Pixel", "partial": false, "listed": true}],
                "offered": [], "fetches": [{"fetch_id": 1, "device_id": "p1", "photo_id": "p3",
                "name": "IMG_1.jpg", "mode": "copy", "state": "done", "path": "/x/y.jpg", "code": null}]}),
            ),
            (
                "photo_thumb",
                json!({"kind": "photo_thumb", "width": 2, "height": 2, "jpeg_base64": "/9j/"}),
            ),
            (
                "update",
                json!({"kind": "update", "enabled": true, "current_version": "0.1.0", "state": "up_to_date",
                "latest": {"version": "0.2.0", "tag": "v0.2.0", "notes": "n", "release_url": "u",
                    "asset_name": null, "asset_size": null, "sha256": null},
                "newer": true, "skipped": false, "downloadable": false, "last_checked_unix": 1,
                "downloaded_bytes": 0, "download_total": 0, "downloaded_path": null, "error": null}),
            ),
            (
                "clipboard_status",
                json!({"kind": "clipboard_status", "backend": "wl", "available": true,
                "observe": "watch", "watching": true, "problem": null, "max_text_bytes": 1, "max_image_bytes": 2,
                "window_ms": 3, "hotkey": "h", "hotkey_binding": "b", "last_pull": {"device_id": "p1",
                "state": "none", "reason": "empty", "kind": "text", "at_unix_ms": 1}, "devices": [],
                "other_sync_app": false}),
            ),
            (
                "share",
                json!({"kind": "share", "save_dir": "/d", "max_file_bytes": 9,
                "devices": [{"device_id": "p1", "receive": true, "auto_accept": false}], "transfers": []}),
            ),
            (
                "pair_qr",
                json!({"kind": "pair_qr", "payload": "gb:x", "expires_in_ms": 5, "computer_name": "pc",
                "addresses": ["192.168.1.2"]}),
            ),
            (
                "connection_test",
                json!({"kind": "connection_test", "ok": false, "headline": "h",
                "steps": [{"label": "l", "ok": true, "detail": "d"}], "advice": ["a"]}),
            ),
            (
                "agenda",
                json!({"kind": "agenda", "devices": [], "people": [], "people_total": 0, "events": []}),
            ),
            (
                "messages",
                json!({"kind": "messages", "devices": [], "conversations": [], "selected": null,
                "sends": []}),
            ),
            (
                "message_queued",
                json!({"kind": "message_queued", "send_id": u64::MAX}),
            ),
            (
                "transfer_started",
                json!({"kind": "transfer_started", "transfer_id": 0}),
            ),
            ("ack", json!({"kind": "ack"})),
            (
                "error",
                json!({"kind": "error", "code": "busy", "message": "m"}),
            ),
        ];
        for (name, j) in &v {
            let derived = serde_json::from_value::<Response>(j.clone());
            assert!(
                derived.is_ok(),
                "sample {name} no longer parses: {derived:?}"
            );
        }
        v
    }

    fn parity(bytes: &[u8]) -> Result<(), String> {
        let a = Response::from_json(bytes);
        let b = serde_json::from_slice::<Response>(bytes);
        match (&a, &b) {
            (Ok(x), Ok(y)) if x == y => Ok(()),
            (Err(_), Err(_)) => Ok(()),
            _ => Err(format!(
                "from_json={:?} derived={:?} for {}",
                a.map(|_| ()),
                b.map(|_| ()),
                String::from_utf8_lossy(bytes)
            )),
        }
    }

    #[test]
    fn from_json_covers_every_response_variant_and_round_trips() {
        let all = every_response_json();
        assert_eq!(all.len(), 20, "one sample per Response variant");
        let mut seen = std::collections::HashSet::new();
        for (name, j) in &all {
            let bytes = serde_json::to_vec(j).unwrap();
            let r = Response::from_json(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                r,
                serde_json::from_slice::<Response>(&bytes).unwrap(),
                "{name}"
            );
            // And what the service really writes (serialising the parsed value) parses back to itself.
            let out = serde_json::to_vec(&r).unwrap();
            assert_eq!(
                Response::from_json(&out).unwrap(),
                r,
                "{name} re-serialised"
            );
            assert_eq!(serde_json::to_value(&r).unwrap()["kind"], *name);
            seen.insert(std::mem::discriminant(&r));
        }
        assert_eq!(seen.len(), 20, "every variant was produced");
    }

    /// Every way to break one value: drop it, or swap it for each JSON type.
    fn damaged(v: &serde_json::Value) -> Vec<serde_json::Value> {
        use serde_json::{Value, json};
        let mut out = vec![
            Value::Null,
            json!(0),
            json!(-1),
            json!(1.5),
            json!(u64::MAX),
            json!(""),
            json!("x"),
            json!(true),
            json!([]),
            json!({}),
        ];
        if let Value::Array(a) = v {
            out.push(Value::Array(a.iter().chain(a.iter()).cloned().collect()));
        }
        out
    }

    /// Visit every object member and array element up to `depth` levels, calling `f` with a copy of the
    /// whole document that has that one place damaged or removed.
    fn mutations(root: &serde_json::Value, depth: usize, f: &mut dyn FnMut(serde_json::Value)) {
        use serde_json::Value;
        fn at<'a>(v: &'a mut Value, path: &[String]) -> &'a mut Value {
            let mut cur = v;
            for p in path {
                cur = match cur {
                    Value::Object(m) => m.get_mut(p).unwrap(),
                    Value::Array(a) => &mut a[p.parse::<usize>().unwrap()],
                    _ => unreachable!(),
                };
            }
            cur
        }
        fn walk(root: &Value, path: &mut Vec<String>, depth: usize, f: &mut dyn FnMut(Value)) {
            let mut copy = root.clone();
            let here = at(&mut copy, path).clone();
            let keys: Vec<String> = match &here {
                Value::Object(m) => m.keys().cloned().collect(),
                Value::Array(a) => (0..a.len()).map(|i| i.to_string()).collect(),
                _ => return,
            };
            // an unknown member
            if matches!(here, Value::Object(_)) {
                let mut c = root.clone();
                at(&mut c, path)
                    .as_object_mut()
                    .unwrap()
                    .insert("zz_unknown".into(), serde_json::json!({"a": [1]}));
                f(c);
            }
            for k in keys {
                // remove it
                let mut c = root.clone();
                match at(&mut c, path) {
                    Value::Object(m) => {
                        m.remove(&k);
                    }
                    Value::Array(a) => {
                        a.remove(k.parse().unwrap());
                    }
                    _ => {}
                }
                f(c);
                // replace it by every other type
                path.push(k.clone());
                let target = at(&mut root.clone(), path).clone();
                for d in damaged(&target) {
                    let mut c = root.clone();
                    *at(&mut c, path) = d;
                    f(c);
                }
                if depth > 1 {
                    walk(root, path, depth - 1, f);
                }
                path.pop();
            }
        }
        walk(root, &mut Vec::new(), depth, f);
    }

    #[test]
    fn from_json_agrees_with_the_derived_parser_on_damaged_replies() {
        let mut cases = 0usize;
        for (name, j) in every_response_json() {
            mutations(&j, 3, &mut |m| {
                cases += 1;
                if let Err(e) = parity(&serde_json::to_vec(&m).unwrap()) {
                    panic!("{name}: {e}");
                }
            });
        }
        assert!(cases > 1500, "{cases} damaged replies were compared");
    }

    #[test]
    fn from_json_agrees_on_the_tag_itself_in_every_shape() {
        use serde_json::json;
        let kinds: Vec<String> = every_response_json()
            .into_iter()
            .map(|(n, _)| n.to_owned())
            .collect();
        // Every sample body under every other kind (wrong payload for the tag), and a bare tag.
        for (name, j) in every_response_json() {
            for k in &kinds {
                let mut m = j.clone();
                m["kind"] = json!(k);
                parity(&serde_json::to_vec(&m).unwrap())
                    .unwrap_or_else(|e| panic!("{name} as {k}: {e}"));
                parity(format!(r#"{{"kind":"{k}"}}"#).as_bytes()).unwrap();
            }
        }
        for raw in [
            // tag shapes
            r#"{}"#,
            r#"{"kind":null}"#,
            r#"{"kind":5}"#,
            r#"{"kind":["ack"]}"#,
            r#"{"kind":{"a":1}}"#,
            r#"{"kind":""}"#,
            r#"{"kind":"ACK"}"#,
            r#"{"kind":"ack "}"#,
            r#"{"kind":"Ack"}"#,
            r#"{"kind":"nope"}"#,
            r#"{"kind":"__proto__"}"#,
            r#"{"Kind":"ack"}"#,
            // escapes, duplicates, order, whitespace
            r#"{"k\u0069nd":"ack"}"#,
            r#"{"kind":"\u0061ck"}"#,
            r#"{"kind":"ack","kind":"ack"}"#,
            r#"{"kind":"ack","kind":"error","code":"busy","message":"m"}"#,
            r#"{"kind":"error","kind":"ack"}"#,
            r#"{"code":"busy","message":"m","kind":"error"}"#,
            r#"{"message":"m","code":"busy","message":"again","kind":"error"}"#,
            "  \n{\"kind\" : \"ack\"}\n",
            r#"{"kind":"ack",}"#,
            r#"{"kind":"ack"} {}"#,
            r#"{"kind":"ack"}x"#,
            // not objects, not JSON, empty
            "",
            " ",
            "null",
            "true",
            "5",
            r#""ack""#,
            "[]",
            r#"[{"kind":"ack"}]"#,
            "{",
            "}",
            r#"{"kind":"ack""#,
            "\u{feff}{\"kind\":\"ack\"}",
            // numbers
            r#"{"kind":"message_queued","send_id":1.0}"#,
            r#"{"kind":"message_queued","send_id":1e3}"#,
            r#"{"kind":"message_queued","send_id":-0}"#,
            r#"{"kind":"message_queued","send_id":18446744073709551616}"#,
            r#"{"kind":"message_queued","send_id":"7"}"#,
            r#"{"kind":"message_queued","send_id":7,"send_id":8}"#,
            r#"{"kind":"transfer_started","transfer_id":null}"#,
            // strings
            r#"{"kind":"phone_debug_state","text":"\ud800"}"#,
            r#"{"kind":"phone_debug_state","text":"\udc00x"}"#,
            r#"{"kind":"phone_debug_state","text":null}"#,
            r#"{"kind":"phone_debug_state","text":"a","text":"b"}"#,
            r#"{"kind":"phone_debug_state","text":"tab	raw"}"#,
        ] {
            parity(raw.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        }
        // invalid UTF-8 and NULs
        for raw in [
            &b"\xff\xfe"[..],
            b"{\"kind\":\"ack\"}\0",
            b"{\"kind\":\"\xff\"}",
            b"{\"kind\":\"phone_debug_state\",\"text\":\"\xc3\"}",
        ] {
            parity(raw).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn from_json_agrees_on_every_truncation_and_single_byte_corruption() {
        for (name, j) in every_response_json() {
            let bytes = serde_json::to_vec(&j).unwrap();
            let step = (bytes.len() / 400).max(1);
            for cut in (0..bytes.len()).step_by(step) {
                parity(&bytes[..cut]).unwrap_or_else(|e| panic!("{name} cut {cut}: {e}"));
            }
            // Deterministic xorshift, no dependency.
            let mut x: u64 = 0x9e37_79b9_7f4a_7c15 ^ bytes.len() as u64;
            for _ in 0..300 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let mut b = bytes.clone();
                let at = (x % b.len() as u64) as usize;
                b[at] = b"{}[]\":,0 -ex\\\xff\n"[(x >> 32) as usize % 15];
                parity(&b).unwrap_or_else(|e| panic!("{name} byte {at}: {e}"));
            }
        }
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
            for _ in 0..(CLIENT_DEADLINE.as_millis() / 40 + 100) {
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

#[cfg(test)]
mod port_note_tests {
    use super::listen_port_note;
    use std::net::SocketAddr;

    #[test]
    fn note_only_for_the_fallback_range() {
        let at = |p: u16| Some(SocketAddr::from(([0, 0, 0, 0], p)));
        assert_eq!(listen_port_note(at(1716), true), None);
        assert_eq!(listen_port_note(None, true), None);
        assert_eq!(listen_port_note(at(1739), false), None);
        assert_eq!(listen_port_note(at(40000), true), None);
        assert_eq!(
            listen_port_note(at(1739), true).as_deref(),
            Some("Using port 1739 because 1716 is in use by another program: this is fine")
        );
        assert!(listen_port_note(at(1764), true).is_some());
    }
}

#[cfg(test)]
mod hear_banner_tests {
    use super::*;

    #[test]
    fn the_calling_banner_names_the_one_next_step() {
        let mut h = HearCalls {
            no_bluetooth: true,
            ..Default::default()
        };
        assert!(h.banner().starts_with("Call audio: NO BLUETOOTH"));
        h.no_bluetooth = false;
        assert!(h.banner().contains("not set up"));
        assert!(h.banner().contains("Set up call audio"));
        h.setup.phone = Some("Fold".into());
        assert!(h.banner().contains("not working yet"));
        h.ready = true;
        assert_eq!(h.banner(), "Call audio: ready (phone: Fold).");
    }
    #[test]
    fn open_steps_point_at_their_troubleshooting_section() {
        let step = |id: &str, state: &str| HearCallsStep {
            id: id.into(),
            title: "t".into(),
            state: state.into(),
            detail: "d".into(),
            fix: Some("do this".into()),
        };
        let h = HearCalls {
            steps: vec![
                step("adapter", "fail"),
                step("card", "warn"),
                step("rfkill", "ok"),
            ],
            ..Default::default()
        };
        let text = h.render();
        assert!(
            text.contains("docs/call-audio-troubleshooting.md#adapter-not-found"),
            "{text}"
        );
        assert!(
            text.contains("docs/call-audio-troubleshooting.md#profile-audio-gateway-missing"),
            "{text}"
        );
        assert!(
            !text.contains("#radio-blocked-rfkill"),
            "an ok step needs no pointer: {text}"
        );
        for id in [
            "adapter",
            "rfkill",
            "service",
            "powered",
            "wireplumber",
            "telephony",
            "paired",
            "connected",
            "hfp",
            "card",
            "devices",
            "voice",
        ] {
            assert!(troubleshooting_anchor(id).is_some(), "{id}");
        }
        // every anchor is a real heading of the page (GitHub style: lowercase, spaces to dashes)
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/call-audio-troubleshooting.md"
        ))
        .unwrap();
        let heads: Vec<String> = doc
            .lines()
            .filter_map(|l| l.strip_prefix("### ").or_else(|| l.strip_prefix("## ")))
            .map(|h| {
                h.to_lowercase()
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
                    .collect::<String>()
                    .replace(' ', "-")
            })
            .collect();
        for id in [
            "adapter",
            "rfkill",
            "service",
            "powered",
            "wireplumber",
            "telephony",
            "paired",
            "connected",
            "hfp",
            "card",
            "devices",
            "voice",
        ] {
            let a = troubleshooting_anchor(id).unwrap();
            assert!(heads.iter().any(|h| h == a), "no heading for {a}");
        }
    }
}
