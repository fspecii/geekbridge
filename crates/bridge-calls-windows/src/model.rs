//! Platform-neutral facts and identifiers observed through the Windows phone-line APIs.
//!
//! Everything here is plain data so the engine, the projection to `bridge-core` and the tests run
//! on any host. Values that identify a person or a device (numbers, names, Bluetooth addresses,
//! device interface paths) are wrapped so `Debug` and the default reports never print them.
use bridge_core::calls::Sensitive;
use serde::Serialize;
use std::fmt;

/// Most Bluetooth phone transports tracked at once.
pub const MAX_DEVICES: usize = 8;
/// Most phone lines read in one refresh.
pub const MAX_LINES: usize = 16;
/// Most active calls kept per device (same bound as `bridge-core`).
pub const MAX_CALLS_PER_DEVICE: usize = bridge_core::calls::MAX_CALLS;
/// Longest accepted device interface id or call id, in bytes.
pub const MAX_ID_LEN: usize = 1024;

/// Windows build number that identifies Windows 11 (the version still reports 10.0).
pub const WINDOWS_11_FIRST_BUILD: u32 = 22000;

// ------------------------------------------------------------------ identifiers

/// Why an identifier was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdError {
    Empty,
    TooLong,
    ControlCharacter,
}

fn check_id(raw: &str) -> Result<(), IdError> {
    if raw.is_empty() {
        Err(IdError::Empty)
    } else if raw.len() > MAX_ID_LEN {
        Err(IdError::TooLong)
    } else if raw.chars().any(char::is_control) {
        Err(IdError::ControlCharacter)
    } else {
        Ok(())
    }
}

/// 64-bit FNV-1a, used only to give identifiers a short non-reversible label for logs and CLI
/// selection. It is not a security primitive.
pub(crate) fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (hash >> 32) as u32 ^ hash as u32)
}

/// The device interface id Windows uses for one Bluetooth phone transport
/// (`PhoneLineTransportDevice.DeviceId`, equal to `PhoneLine.TransportDeviceId`).
///
/// Device interface paths are case-insensitive, so equality and lookup use [`Self::key`]. The
/// value embeds the phone's Bluetooth address, so `Debug` prints only a fingerprint.
#[derive(Clone)]
pub struct TransportDeviceId {
    raw: String,
    key: String,
}
impl TransportDeviceId {
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into();
        check_id(&raw)?;
        let key = raw.to_ascii_lowercase();
        Ok(Self { raw, key })
    }
    /// Exact text, to pass back to Windows. Do not log.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
    /// Case-folded form used for comparison and map keys.
    pub fn key(&self) -> &str {
        &self.key
    }
    /// Short stable label safe for logs.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key)
    }
}
impl PartialEq for TransportDeviceId {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for TransportDeviceId {}
impl PartialOrd for TransportDeviceId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TransportDeviceId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}
impl fmt::Debug for TransportDeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TransportDeviceId(#{})", self.fingerprint())
    }
}

/// Identifier of a `PhoneLine` (a GUID, lower-case hyphenated).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(String);
impl LineId {
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into().to_ascii_lowercase();
        check_id(&raw)?;
        Ok(Self(raw))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for LineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LineId(#{})", fingerprint(&self.0))
    }
}

/// `PhoneCall.CallId` as reported by Windows.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativeCallId(String);
impl NativeCallId {
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into();
        check_id(&raw)?;
        Ok(Self(raw))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.0)
    }
}
impl fmt::Debug for NativeCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NativeCallId(#{})", self.fingerprint())
    }
}

// ------------------------------------------------------------------ native vocabulary

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTransport {
    Cellular,
    VoipApp,
    Bluetooth,
    Other(i32),
}

/// `PhoneCallStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCallStatus {
    Lost,
    Incoming,
    Dialing,
    Talking,
    Held,
    Ended,
    Other(i32),
}
impl NativeCallStatus {
    pub fn is_ringing(self) -> bool {
        self == Self::Incoming
    }
    /// A call that Windows still considers part of the line.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Incoming | Self::Dialing | Self::Talking | Self::Held
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDirection {
    Unknown,
    Incoming,
    Outgoing,
}

/// `PhoneCallAudioDevice`: where the call audio is routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDevice {
    Unknown,
    /// This computer (the hands-free unit).
    Local,
    /// The phone (Bluetooth HFP remote).
    Remote,
}

/// `TransportDeviceAudioRoutingStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioRouting {
    Unknown,
    CanRouteToLocal,
    CannotRouteToLocal,
}

/// `PhoneCallOperationStatus`: Windows' verdict on a call operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Succeeded,
    OtherFailure,
    TimedOut,
    ConnectionLost,
    InvalidCallState,
    Other(i32),
}

/// `DeviceAccessStatus` from `PhoneLineTransportDevice.RequestAccessAsync`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessStatus {
    Unspecified,
    Allowed,
    DeniedByUser,
    DeniedBySystem,
}

// ------------------------------------------------------------------ errors from the port

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformErrorKind {
    /// Not Windows, or the API/contract is not present on this build.
    Unsupported,
    /// `E_ACCESSDENIED`: missing capability, package identity, or privacy setting. Never worked
    /// around.
    AccessDenied,
    /// The object (device, line, call) does not exist (any more).
    NotFound,
    /// The deadline passed before Windows answered. For a mutating operation the effect is
    /// unknown.
    TimedOut,
    /// The asynchronous operation was cancelled.
    Canceled,
    /// The API returned no object (for example another hands-free stack owns the phone).
    NotAvailable,
    /// The caller passed a value Windows or this crate refuses.
    InvalidArgument,
    /// Any other operating-system failure; the HRESULT is kept.
    Os,
}

/// A failure reported by the platform port, with enough context to diagnose without leaking
/// private values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PlatformError {
    pub kind: PlatformErrorKind,
    pub hresult: Option<i32>,
    /// Static description of the API step, for example `"PhoneLineTransportDevice.FromId"`.
    pub context: &'static str,
}
impl PlatformError {
    pub fn new(kind: PlatformErrorKind, context: &'static str) -> Self {
        Self {
            kind,
            hresult: None,
            context,
        }
    }
    pub fn with_hresult(mut self, hresult: i32) -> Self {
        self.hresult = Some(hresult);
        self
    }
    pub fn is_timeout(&self) -> bool {
        self.kind == PlatformErrorKind::TimedOut
    }
}
impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.context, self.kind)?;
        if let Some(code) = self.hresult {
            write!(f, " (HRESULT 0x{:08X})", code as u32)?;
        }
        Ok(())
    }
}
impl std::error::Error for PlatformError {}

/// Result of one read performed by the port.
pub type Step<T> = Result<T, PlatformError>;

// ------------------------------------------------------------------ environment facts

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OsPlatform {
    NotWindows,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OsFacts {
    pub platform: OsPlatform,
    /// Build number when the kernel reported it.
    pub build: Option<u32>,
}
impl OsFacts {
    pub fn not_windows() -> Self {
        Self {
            platform: OsPlatform::NotWindows,
            build: None,
        }
    }
    pub fn is_windows_11(&self) -> bool {
        self.platform == OsPlatform::Windows
            && self.build.is_some_and(|b| b >= WINDOWS_11_FIRST_BUILD)
    }
}

/// Presence of `Windows.ApplicationModel.Calls.CallsPhoneContract` major versions. `None` means
/// the question itself could not be asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ContractFacts {
    /// Version 5: Bluetooth phone-line transport and dial.
    pub v5: Option<bool>,
    /// Version 6: in-call control (`PhoneCall` operations, `ChangeAudioDeviceAsync`).
    pub v6: Option<bool>,
    /// Version 7: OS-reported `AudioRoutingStatus`.
    pub v7: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageIdentity {
    /// The process runs with package identity (MSIX or sparse package).
    Present { family_name: String },
    /// `APPMODEL_ERROR_NO_PACKAGE`.
    Absent,
    /// The query failed for another reason.
    Unknown { code: u32 },
}

/// The Phone calls privacy capability as reported by `AppCapability("phoneCall").CheckAccess()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyAccess {
    Allowed,
    DeniedByUser,
    DeniedBySystem,
    UserPromptRequired,
    /// The package manifest does not declare the capability.
    NotDeclaredByApp,
    /// Windows could not answer (not an assertion that access is granted).
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentFacts {
    pub os: OsFacts,
    pub contract: ContractFacts,
    pub package: PackageIdentity,
    pub privacy: PrivacyAccess,
}
impl EnvironmentFacts {
    pub fn not_windows() -> Self {
        Self {
            os: OsFacts::not_windows(),
            contract: ContractFacts::default(),
            package: PackageIdentity::Unknown { code: 0 },
            privacy: PrivacyAccess::Unavailable,
        }
    }
}

// ------------------------------------------------------------------ device, line and call records

/// A phone offered by Windows as a Bluetooth phone-line transport (from device enumeration).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportRecord {
    pub id: TransportDeviceId,
    pub name: Option<Sensitive>,
    /// Bluetooth address when Windows supplied it.
    pub address: Option<Sensitive>,
    pub enabled: bool,
}

/// State read from one `PhoneLineTransportDevice`. Each member is its own step because Windows
/// builds expose different subsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportInfo {
    pub transport: Step<NativeTransport>,
    pub registered: Step<bool>,
    pub audio_routing: Step<AudioRouting>,
    pub in_band_ringing: Step<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineRecord {
    pub id: LineId,
    pub transport: NativeTransport,
    pub transport_device_id: Option<TransportDeviceId>,
    pub can_dial: Option<bool>,
    pub name: Option<Sensitive>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    pub id: NativeCallId,
    pub status: NativeCallStatus,
    pub direction: NativeDirection,
    pub audio_device: AudioDevice,
    pub muted: Option<bool>,
    pub number: Option<Sensitive>,
    pub name: Option<Sensitive>,
    pub hold_supported: Option<bool>,
}

/// Active calls of one line plus Windows' own verdict on the listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallListing {
    pub calls: Vec<CallRecord>,
    /// More calls than [`MAX_CALLS_PER_DEVICE`] were reported; the rest were dropped.
    pub truncated: bool,
}

/// Result of a dial request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialResult {
    pub status: OperationStatus,
    pub call: Option<CallRecord>,
}

// ------------------------------------------------------------------ commands at the port

/// DTMF keys Windows can send (`DtmfKey`). `A`-`D` are not representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtmfTone {
    Digit(u8),
    Star,
    Pound,
}
impl DtmfTone {
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            '0'..='9' => Some(Self::Digit(c as u8 - b'0')),
            '*' => Some(Self::Star),
            '#' => Some(Self::Pound),
            _ => None,
        }
    }
}

/// One in-call operation on an existing call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOp {
    Accept,
    Reject,
    End,
    Hold,
    Resume,
    Mute,
    Unmute,
    Dtmf(DtmfTone),
    Route(AudioDevice),
}
impl CallOp {
    pub fn name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::End => "end",
            Self::Hold => "hold",
            Self::Resume => "resume",
            Self::Mute => "mute",
            Self::Unmute => "unmute",
            Self::Dtmf(_) => "dtmf",
            Self::Route(_) => "route_audio",
        }
    }
}
