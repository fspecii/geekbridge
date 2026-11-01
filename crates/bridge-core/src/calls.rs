//! Platform-independent call state and call-control capability model.
//!
//! This module performs no I/O. It describes what a backend claims it can do
//! ([`CallCapabilities`]), what a backend observed ([`CallEvent`]), what a caller wants
//! done ([`CallCommand`]), and folds events into a deterministic [`CallState`].
//!
//! Three planes are kept apart on purpose, because supporting one says nothing about the others:
//! *observation* (seeing call state), *control* (answering, hanging up, muting) and *audio*
//! (carrying the voice path). KDE Connect telephony, for example, can only observe and mute the
//! phone's ringer; it carries no call audio. A Bluetooth HFP link with a negotiated codec proves
//! an audio transport exists, not that calls can be controlled.
//!
//! Support is conservative: every feature starts [`Support::Unknown`] and only an explicit
//! [`Support::Verified`] (recorded after a real exercise on hardware) counts as proven. Backends
//! derive at most [`Support::Declared`] from advertised metadata. The module registers no
//! backend and has no side effects; backends for Linux, Windows and macOS translate their native
//! signals into [`CallEvent`] and execute accepted [`CallCommand`]s elsewhere.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// Maximum simultaneously tracked calls (HFP phones rarely exceed an active, held and waiting call).
pub const MAX_CALLS: usize = 8;
/// Maximum finished calls retained in [`CallState::recent_ended`].
pub const MAX_ENDED: usize = 8;
/// Maximum byte length of an id, number or display name accepted from a backend.
pub const MAX_FIELD_LEN: usize = 128;
/// Maximum length of a dialed number or DTMF sequence in a command.
pub const MAX_DIAL_LEN: usize = 32;
/// Call id used by backends (such as KDE Connect) that only ever report one current call.
pub const KDE_CURRENT_CALL_ID: &str = "kde-current";
/// KDE Connect packet type for call events from the phone.
pub const KDE_PACKET_TELEPHONY: &str = "kdeconnect.telephony";
/// KDE Connect packet type asking the phone to silence its ringer.
pub const KDE_PACKET_REQUEST_MUTE: &str = "kdeconnect.telephony.request_mute";

/// A string that must not reach logs: phone numbers, contact names. `Debug` is redacted;
/// serialization is transparent so the value still crosses authenticated local APIs.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sensitive(String);
impl Sensitive {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Explicit access, so reading the content is visible at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Backend-assigned call identifier, unique among live calls on one device link.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(String);
impl CallId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------- capabilities

/// Which mechanism supplies call information and control. New OS backends add variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallBackend {
    /// KDE Connect `kdeconnect.telephony` packets over the paired LAN link.
    KdeConnect,
    /// Bluetooth Hands-Free Profile, with this computer as the hands-free unit.
    BluetoothHfp { host: HfpHost },
    /// GeekBridge's own call-control packets over the paired LAN link (`calls_link`). Carries state
    /// and answer/reject/end requests only; call audio never travels on it.
    GeekBridgeLink,
}

/// The host stack that would implement the HFP hands-free role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HfpHost {
    LinuxPipeWire,
    LinuxOfono,
    Windows,
    MacOs,
}

/// The plane a feature belongs to. Support in one plane never implies another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPlane {
    Observation,
    Control,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallFeature {
    // Observation: seeing what the phone is doing.
    CallStateEvents,
    CallerIdentity,
    // Control: changing what the phone or host is doing.
    Answer,
    Hangup,
    Dial,
    SwapCalls,
    SendTones,
    /// Silence the phone's ringer. Does not affect an established call.
    MuteRinger,
    /// Mute the host microphone for an established call.
    MuteMicrophone,
    // Audio: carrying the voice path.
    /// Phone to host speech.
    AudioDownlink,
    /// Host microphone to phone.
    AudioUplink,
    /// Open or close the audio transport itself.
    AudioLinkControl,
}
impl CallFeature {
    pub const ALL: [CallFeature; 12] = [
        Self::CallStateEvents,
        Self::CallerIdentity,
        Self::Answer,
        Self::Hangup,
        Self::Dial,
        Self::SwapCalls,
        Self::SendTones,
        Self::MuteRinger,
        Self::MuteMicrophone,
        Self::AudioDownlink,
        Self::AudioUplink,
        Self::AudioLinkControl,
    ];
    pub fn plane(self) -> CapabilityPlane {
        match self {
            Self::CallStateEvents | Self::CallerIdentity => CapabilityPlane::Observation,
            Self::AudioDownlink | Self::AudioUplink | Self::AudioLinkControl => {
                CapabilityPlane::Audio
            }
            _ => CapabilityPlane::Control,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnsupportedReason {
    /// The protocol or profile has no such operation.
    NotInProtocol,
    /// The remote did not advertise the operation.
    RemoteDoesNotAdvertise,
    /// No usable public API on this OS or host stack.
    NoPlatformApi,
    /// Nothing is implemented for this feature in this build.
    NotImplemented,
}

/// How much is known about one feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Support {
    /// Not probed. The default; treated as unusable.
    Unknown,
    Unsupported {
        reason: UnsupportedReason,
    },
    /// Advertised or inferred from metadata; never exercised. Attempting is allowed, trusting is not.
    Declared,
    /// Exercised end to end on this backend and device, and recorded explicitly.
    Verified,
}
impl Support {
    /// True when a command may be attempted (declared or verified).
    pub fn may_attempt(self) -> bool {
        matches!(self, Self::Declared | Self::Verified)
    }
}

/// Per-backend, per-device feature support. Missing features read as [`Support::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallCapabilities {
    pub backend: CallBackend,
    features: BTreeMap<CallFeature, Support>,
}
impl CallCapabilities {
    /// Nothing probed yet: every feature is [`Support::Unknown`].
    pub fn unprobed(backend: CallBackend) -> Self {
        Self {
            backend,
            features: BTreeMap::new(),
        }
    }

    /// Every feature explicitly unsupported, for a platform with no usable backend.
    pub fn unsupported(backend: CallBackend, reason: UnsupportedReason) -> Self {
        let features = CallFeature::ALL
            .into_iter()
            .map(|f| (f, Support::Unsupported { reason }))
            .collect();
        Self { backend, features }
    }

    pub fn support(&self, feature: CallFeature) -> Support {
        self.features
            .get(&feature)
            .copied()
            .unwrap_or(Support::Unknown)
    }
    pub fn set(&mut self, feature: CallFeature, support: Support) {
        self.features.insert(feature, support);
    }

    /// Promote a feature to [`Support::Verified`] after a recorded hardware exercise.
    /// Refuses (returns false) to override an explicit [`Support::Unsupported`].
    pub fn record_verified(&mut self, feature: CallFeature) -> bool {
        if matches!(self.support(feature), Support::Unsupported { .. }) {
            return false;
        }
        self.features.insert(feature, Support::Verified);
        true
    }

    pub fn features_in(
        &self,
        plane: CapabilityPlane,
    ) -> impl Iterator<Item = (CallFeature, Support)> + '_ {
        CallFeature::ALL
            .into_iter()
            .filter(move |f| f.plane() == plane)
            .map(|f| (f, self.support(f)))
    }

    /// KDE Connect: derived from the remote's packet lists (declared only, never verified).
    /// `remote_outgoing` is what the phone sends; `remote_incoming` what it accepts. The protocol
    /// has no answer, hang-up, dial, hold or audio operations, so those are unsupported.
    pub fn from_kde_packet_types<S: AsRef<str>>(
        remote_outgoing: impl IntoIterator<Item = S>,
        remote_incoming: impl IntoIterator<Item = S>,
    ) -> Self {
        let sends_events = remote_outgoing
            .into_iter()
            .any(|t| t.as_ref() == KDE_PACKET_TELEPHONY);
        let accepts_mute = remote_incoming
            .into_iter()
            .any(|t| t.as_ref() == KDE_PACKET_REQUEST_MUTE);
        let mut caps = Self::unsupported(CallBackend::KdeConnect, UnsupportedReason::NotInProtocol);
        let advertised = |present: bool| {
            if present {
                Support::Declared
            } else {
                Support::Unsupported {
                    reason: UnsupportedReason::RemoteDoesNotAdvertise,
                }
            }
        };
        caps.set(CallFeature::CallStateEvents, advertised(sends_events));
        caps.set(CallFeature::CallerIdentity, advertised(sends_events));
        caps.set(CallFeature::MuteRinger, advertised(accepts_mute));
        caps
    }

    /// Bluetooth HFP hands-free role: map observed link facts to declared support.
    /// Never returns `Verified`. Notably, a negotiated codec marks only the audio plane; call
    /// control additionally needs the host stack to expose it.
    pub fn from_hfp_evidence(host: HfpHost, e: &HfpEvidence) -> Self {
        let mut caps = Self::unprobed(CallBackend::BluetoothHfp { host });
        let declared = |ok: bool| {
            if ok {
                Support::Declared
            } else {
                Support::Unknown
            }
        };
        let slc = e.service_level_connection;
        let control = slc && e.host_call_control_api;
        let audio = e.audio_codec.is_some();
        caps.set(
            CallFeature::CallStateEvents,
            declared(slc && e.call_indicators),
        );
        caps.set(
            CallFeature::CallerIdentity,
            declared(slc && e.caller_id_notification),
        );
        for f in [
            CallFeature::Answer,
            CallFeature::Hangup,
            CallFeature::Dial,
            CallFeature::SendTones,
        ] {
            caps.set(f, declared(control));
        }
        caps.set(
            CallFeature::SwapCalls,
            declared(control && e.three_way_calling),
        );
        caps.set(
            CallFeature::MuteRinger,
            Support::Unsupported {
                reason: UnsupportedReason::NotInProtocol,
            },
        );
        caps.set(CallFeature::MuteMicrophone, declared(audio));
        caps.set(CallFeature::AudioDownlink, declared(audio));
        caps.set(CallFeature::AudioUplink, declared(audio));
        caps.set(
            CallFeature::AudioLinkControl,
            declared(slc && e.host_audio_control_api),
        );
        caps
    }

    /// Check a command against capabilities and argument rules (state preconditions are
    /// checked by [`CallState::check_command`]).
    pub fn authorize(&self, command: &CallCommand) -> Result<(), CommandRejection> {
        match self.support(command.feature()) {
            Support::Unknown => return Err(CommandRejection::NotProbed),
            Support::Unsupported { reason } => {
                return Err(CommandRejection::Unsupported { reason });
            }
            Support::Declared | Support::Verified => {}
        }
        command.validate()
    }
}

/// Facts observed on a Bluetooth HFP link, gathered by a backend. Each is a claim about the
/// link or host stack, not proof that a call round-trips.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HfpEvidence {
    /// RFCOMM service-level connection completed.
    pub service_level_connection: bool,
    /// The phone reports call, call-setup and held indicators.
    pub call_indicators: bool,
    /// Calling-line identification notification is enabled.
    pub caller_id_notification: bool,
    /// Held/swap (3-way) call handling was advertised.
    pub three_way_calling: bool,
    /// The host stack exposes answer/hang-up/dial to applications.
    pub host_call_control_api: bool,
    /// The host stack exposes opening the audio transport to applications.
    pub host_audio_control_api: bool,
    /// `Some` when a synchronous audio link is up. Says nothing about call control.
    pub audio_codec: Option<AudioCodec>,
}

// ---------------------------------------------------------------- commands

/// A request for a backend to act. Constructing one has no effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallCommand {
    Answer {
        call: CallId,
    },
    Hangup {
        call: CallId,
    },
    /// Decline a ringing call. Needs the same support as [`CallFeature::Hangup`] (every backend
    /// declines through its hang-up operation) but is only valid while the call is ringing, so it
    /// can never end a call in progress.
    Reject {
        call: CallId,
    },
    Dial {
        number: Sensitive,
    },
    SwapCalls,
    SendTones {
        tones: Sensitive,
    },
    MuteRinger,
    SetMicrophoneMute {
        muted: bool,
    },
    ConnectAudio,
    DisconnectAudio,
}
impl CallCommand {
    pub fn feature(&self) -> CallFeature {
        match self {
            Self::Answer { .. } => CallFeature::Answer,
            Self::Hangup { .. } | Self::Reject { .. } => CallFeature::Hangup,
            Self::Dial { .. } => CallFeature::Dial,
            Self::SwapCalls => CallFeature::SwapCalls,
            Self::SendTones { .. } => CallFeature::SendTones,
            Self::MuteRinger => CallFeature::MuteRinger,
            Self::SetMicrophoneMute { .. } => CallFeature::MuteMicrophone,
            Self::ConnectAudio | Self::DisconnectAudio => CallFeature::AudioLinkControl,
        }
    }

    /// Argument checks that need no state.
    pub fn validate(&self) -> Result<(), CommandRejection> {
        let ok = |s: &str, allowed: &str| {
            !s.is_empty() && s.len() <= MAX_DIAL_LEN && s.chars().all(|c| allowed.contains(c))
        };
        match self {
            Self::Dial { number } if !ok(number.expose(), "0123456789+*#") => {
                Err(CommandRejection::InvalidArgument)
            }
            Self::SendTones { tones } if !ok(tones.expose(), "0123456789*#ABCD") => {
                Err(CommandRejection::InvalidArgument)
            }
            Self::Answer { call } | Self::Hangup { call } | Self::Reject { call }
                if call.as_str().is_empty() || call.as_str().len() > MAX_FIELD_LEN =>
            {
                Err(CommandRejection::InvalidArgument)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandRejection {
    /// Feature was never probed.
    NotProbed,
    Unsupported {
        reason: UnsupportedReason,
    },
    InvalidArgument,
    LinkDown,
    NoSuchCall,
    /// The command does not fit the current call or audio state.
    InvalidState,
}

// ---------------------------------------------------------------- state and events

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    Incoming,
    Outgoing,
    #[default]
    Unknown,
}

/// Live call phases (mirrors HFP call status). A finished call is reported with [`CallEvent::CallEnded`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPhase {
    /// Ringing, no other call.
    Incoming,
    /// Ringing while another call is live.
    Waiting,
    /// Outgoing call being set up.
    Dialing,
    /// Outgoing call, remote end alerting.
    Alerting,
    Active,
    Held,
}
impl CallPhase {
    pub fn is_ringing(self) -> bool {
        matches!(self, Self::Incoming | Self::Waiting)
    }
    fn implied_direction(self) -> CallDirection {
        match self {
            Self::Incoming | Self::Waiting => CallDirection::Incoming,
            Self::Dialing | Self::Alerting => CallDirection::Outgoing,
            Self::Active | Self::Held => CallDirection::Unknown,
        }
    }
    fn may_become(self, to: CallPhase) -> bool {
        use CallPhase::*;
        self == to
            || matches!(
                (self, to),
                (Incoming | Waiting, Active)
                    | (Waiting, Incoming)
                    | (Dialing, Alerting | Active)
                    | (Alerting, Active)
                    | (Active, Held)
                    | (Held, Active)
            )
    }
}

/// Why a call ended. A backend that cannot tell sends [`EndReason::Unspecified`] and the reducer
/// derives a conservative reason from the last phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// Was active or held, then ended. Does not say which side hung up.
    Completed,
    /// Ringing call ended without being answered. Missed and declined are not distinguished
    /// (KDE Connect's Android plugin reports both as `missedCall`).
    NotAnswered,
    /// Outgoing call ended before it was answered.
    OutgoingNotConnected,
    /// The backend link dropped; the real outcome is unknown.
    LinkLost,
    Unspecified,
}

/// Remote party. Both fields are best-effort; a name may merely repeat the number
/// (KDE Connect's phone falls back to the number when it cannot read contacts).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallParty {
    #[serde(default)]
    pub number: Option<Sensitive>,
    #[serde(default)]
    pub name: Option<Sensitive>,
}
impl std::fmt::Debug for CallParty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallParty")
            .field("number", &self.number.as_ref().map(|_| "<redacted>"))
            .field("name", &self.name.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}
impl CallParty {
    fn within_bounds(&self) -> bool {
        [&self.number, &self.name]
            .into_iter()
            .flatten()
            .all(|s| s.expose().len() <= MAX_FIELD_LEN)
    }
    /// Newer values replace older ones; absent values never erase known ones.
    fn merge(&mut self, update: &CallParty) {
        if update.number.is_some() {
            self.number = update.number.clone();
        }
        if update.name.is_some() {
            self.name = update.name.clone();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CallInfo {
    pub id: CallId,
    pub direction: CallDirection,
    pub phase: CallPhase,
    pub party: CallParty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndedCall {
    pub id: CallId,
    pub direction: CallDirection,
    pub reason: EndReason,
    pub party: CallParty,
}

/// Backend session state. `Up` means the backend can currently report and (if declared) act;
/// it does not mean calls work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    #[default]
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AudioCodec {
    /// Connected but codec not reported.
    Unknown,
    Cvsd,
    Msbc,
    Lc3Swb,
}

/// State of the voice transport, independent of call state (it may be up with no call, or a
/// call may exist with no audio).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioLinkState {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        codec: AudioCodec,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuteScope {
    /// The phone's ringer (KDE Connect `request_mute`: silences the ring stream only).
    PhoneRinger,
    /// The host microphone during a call.
    HostMicrophone,
}

/// Typed observation from a backend, applied with [`CallState::apply`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallEvent {
    Link {
        state: LinkState,
    },
    /// Create or update a live call. `direction: Unknown` keeps a previously known direction.
    CallUpdated {
        id: CallId,
        direction: CallDirection,
        phase: CallPhase,
        #[serde(default)]
        party: CallParty,
    },
    CallEnded {
        id: CallId,
        reason: EndReason,
    },
    MuteChanged {
        scope: MuteScope,
        muted: bool,
    },
    Audio {
        state: AudioLinkState,
    },
}

impl CallEvent {
    /// Translate a KDE Connect `kdeconnect.telephony` packet into an event, or `None` for packets
    /// that add nothing. KDE reports a single current call, so [`KDE_CURRENT_CALL_ID`] is used.
    ///
    /// Android re-sends the previous packet with `isCancel` when the phone goes idle, then sends
    /// `missedCall` if that packet was `ringing`; the cancel carries the end, so `missedCall` and
    /// the obsolete `sms` event map to `None`. Callers parse `isCancel` (sent as the string
    /// `"true"`) themselves. A `talking` packet has no direction or reliable number.
    pub fn from_kde_telephony(
        event: &str,
        is_cancel: bool,
        number: Option<&str>,
        name: Option<&str>,
    ) -> Option<Self> {
        let id = CallId::new(KDE_CURRENT_CALL_ID);
        if is_cancel {
            return Some(Self::CallEnded {
                id,
                reason: EndReason::Unspecified,
            });
        }
        let party = CallParty {
            number: number.map(Sensitive::new),
            name: name.map(Sensitive::new),
        };
        match event {
            "ringing" => Some(Self::CallUpdated {
                id,
                direction: CallDirection::Incoming,
                phase: CallPhase::Incoming,
                party,
            }),
            "talking" => Some(Self::CallUpdated {
                id,
                direction: CallDirection::Unknown,
                phase: CallPhase::Active,
                party,
            }),
            _ => None,
        }
    }
}

/// Why [`CallState::apply`] refused an event. The state is left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventRejection {
    LinkDown,
    FieldTooLong,
    TooManyCalls,
    UnknownCall,
    InvalidTransition,
    InconsistentDirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Unchanged,
    Rejected(EventRejection),
}

/// Aggregate call state for one device link. Mutated only through [`CallState::apply`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CallState {
    pub link: LinkState,
    pub calls: BTreeMap<CallId, CallInfo>,
    pub recent_ended: VecDeque<EndedCall>,
    pub audio: AudioLinkState,
    /// `None` when unknown (never reported, or the link dropped).
    pub ringer_muted: Option<bool>,
    pub microphone_muted: Option<bool>,
}

impl CallState {
    /// Fold one event into the state. Pure and deterministic; invalid events are rejected whole.
    pub fn apply(&mut self, event: CallEvent) -> Applied {
        use Applied::*;
        use EventRejection::*;
        match event {
            CallEvent::Link {
                state: LinkState::Up,
            } => {
                if self.link == LinkState::Up {
                    return Unchanged;
                }
                self.link = LinkState::Up;
                Changed
            }
            CallEvent::Link {
                state: LinkState::Down,
            } => self.link_down(),
            _ if self.link == LinkState::Down => Rejected(LinkDown),
            CallEvent::CallUpdated {
                id,
                direction,
                phase,
                party,
            } => self.update_call(id, direction, phase, party),
            CallEvent::CallEnded { id, reason } => {
                let Some(call) = self.calls.remove(&id) else {
                    return Rejected(UnknownCall);
                };
                self.finish(call, reason);
                Changed
            }
            CallEvent::MuteChanged { scope, muted } => {
                let slot = match scope {
                    MuteScope::PhoneRinger => &mut self.ringer_muted,
                    MuteScope::HostMicrophone => &mut self.microphone_muted,
                };
                if *slot == Some(muted) {
                    return Unchanged;
                }
                *slot = Some(muted);
                Changed
            }
            CallEvent::Audio { state } => {
                if self.audio == state {
                    return Unchanged;
                }
                self.audio = state;
                Changed
            }
        }
    }

    fn link_down(&mut self) -> Applied {
        if self.link == LinkState::Down
            && self.calls.is_empty()
            && self.audio == AudioLinkState::Disconnected
            && self.ringer_muted.is_none()
            && self.microphone_muted.is_none()
        {
            return Applied::Unchanged;
        }
        self.link = LinkState::Down;
        self.audio = AudioLinkState::Disconnected;
        self.ringer_muted = None;
        self.microphone_muted = None;
        for call in std::mem::take(&mut self.calls).into_values() {
            self.finish(call, EndReason::LinkLost);
        }
        Applied::Changed
    }

    fn update_call(
        &mut self,
        id: CallId,
        direction: CallDirection,
        phase: CallPhase,
        party: CallParty,
    ) -> Applied {
        use Applied::*;
        use EventRejection::*;
        if id.as_str().is_empty() || id.as_str().len() > MAX_FIELD_LEN || !party.within_bounds() {
            return Rejected(FieldTooLong);
        }
        let existing = self.calls.get(&id);
        if existing.is_none() && self.calls.len() >= MAX_CALLS {
            return Rejected(TooManyCalls);
        }
        if existing.is_some_and(|call| !call.phase.may_become(phase)) {
            return Rejected(InvalidTransition);
        }
        let known = existing.map_or(CallDirection::Unknown, |c| c.direction);
        let Some(direction) = resolve_direction(known, direction, phase) else {
            return Rejected(InconsistentDirection);
        };
        match self.calls.get_mut(&id) {
            Some(call) => {
                let mut merged = call.party.clone();
                merged.merge(&party);
                if call.phase == phase && call.direction == direction && call.party == merged {
                    return Unchanged;
                }
                call.phase = phase;
                call.direction = direction;
                call.party = merged;
            }
            None => {
                self.calls.insert(
                    id.clone(),
                    CallInfo {
                        id,
                        direction,
                        phase,
                        party,
                    },
                );
            }
        }
        Changed
    }

    fn finish(&mut self, call: CallInfo, reason: EndReason) {
        let reason = match (reason, call.phase) {
            (EndReason::Unspecified, p) if p.is_ringing() => EndReason::NotAnswered,
            (EndReason::Unspecified, CallPhase::Dialing | CallPhase::Alerting) => {
                EndReason::OutgoingNotConnected
            }
            (EndReason::Unspecified, _) => EndReason::Completed,
            (explicit, _) => explicit,
        };
        if self.recent_ended.len() >= MAX_ENDED {
            self.recent_ended.pop_front();
        }
        self.recent_ended.push_back(EndedCall {
            id: call.id,
            direction: call.direction,
            reason,
            party: call.party,
        });
    }

    /// Full pre-flight for a command: capabilities, arguments, link and call state. Executing
    /// the command, and confirming its effect through later events, is the backend's job.
    pub fn check_command(
        &self,
        caps: &CallCapabilities,
        command: &CallCommand,
    ) -> Result<(), CommandRejection> {
        use CommandRejection::*;
        caps.authorize(command)?;
        if self.link == LinkState::Down {
            return Err(LinkDown);
        }
        let any = |f: fn(&CallInfo) -> bool| self.calls.values().any(f);
        match command {
            CallCommand::Answer { call } => match self.calls.get(call) {
                None => Err(NoSuchCall),
                Some(c) if c.phase.is_ringing() => Ok(()),
                Some(_) => Err(InvalidState),
            },
            CallCommand::Hangup { call } => self
                .calls
                .contains_key(call)
                .then_some(())
                .ok_or(NoSuchCall),
            CallCommand::Reject { call } => match self.calls.get(call) {
                None => Err(NoSuchCall),
                Some(c) if c.phase.is_ringing() => Ok(()),
                Some(_) => Err(InvalidState),
            },
            CallCommand::SwapCalls if !any(|c| c.phase == CallPhase::Held) => Err(InvalidState),
            CallCommand::SendTones { .. } if !any(|c| c.phase == CallPhase::Active) => {
                Err(InvalidState)
            }
            CallCommand::MuteRinger if !any(|c| c.phase.is_ringing()) => Err(InvalidState),
            CallCommand::ConnectAudio if self.audio != AudioLinkState::Disconnected => {
                Err(InvalidState)
            }
            CallCommand::DisconnectAudio if self.audio == AudioLinkState::Disconnected => {
                Err(InvalidState)
            }
            _ => Ok(()),
        }
    }
}

/// Merge a previously known direction with an event's and the phase's implied direction.
fn resolve_direction(
    known: CallDirection,
    reported: CallDirection,
    phase: CallPhase,
) -> Option<CallDirection> {
    let mut result = CallDirection::Unknown;
    for d in [known, reported, phase.implied_direction()] {
        match (result, d) {
            (_, CallDirection::Unknown) => {}
            (CallDirection::Unknown, d) => result = d,
            (a, b) if a != b => return None,
            _ => {}
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> CallId {
        CallId::new(s)
    }
    fn up() -> CallState {
        let mut s = CallState::default();
        s.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        s
    }
    fn update(s: &mut CallState, call: &str, dir: CallDirection, phase: CallPhase) -> Applied {
        s.apply(CallEvent::CallUpdated {
            id: id(call),
            direction: dir,
            phase,
            party: CallParty::default(),
        })
    }
    fn kde(s: &mut CallState, event: &str, cancel: bool, number: Option<&str>) -> Option<Applied> {
        CallEvent::from_kde_telephony(event, cancel, number, None).map(|e| s.apply(e))
    }

    #[test]
    fn unprobed_features_are_unknown_and_rejected() {
        let caps = CallCapabilities::unprobed(CallBackend::BluetoothHfp {
            host: HfpHost::Windows,
        });
        for f in CallFeature::ALL {
            assert_eq!(caps.support(f), Support::Unknown);
        }
        assert_eq!(
            caps.authorize(&CallCommand::Answer { call: id("1") }),
            Err(CommandRejection::NotProbed)
        );
    }

    #[test]
    fn kde_declares_events_and_ringer_mute_only() {
        let caps = CallCapabilities::from_kde_packet_types(
            [KDE_PACKET_TELEPHONY],
            [KDE_PACKET_REQUEST_MUTE],
        );
        assert_eq!(
            caps.support(CallFeature::CallStateEvents),
            Support::Declared
        );
        assert_eq!(caps.support(CallFeature::MuteRinger), Support::Declared);
        for f in [CallFeature::Answer, CallFeature::Hangup, CallFeature::Dial] {
            assert_eq!(
                caps.support(f),
                Support::Unsupported {
                    reason: UnsupportedReason::NotInProtocol
                }
            );
        }
        assert!(
            caps.features_in(CapabilityPlane::Audio)
                .all(|(_, s)| matches!(s, Support::Unsupported { .. }))
        );
        assert!(
            caps.features_in(CapabilityPlane::Control)
                .any(|(_, s)| s == Support::Declared)
        );
        let silent =
            CallCapabilities::from_kde_packet_types(Vec::<&str>::new(), Vec::<&str>::new());
        assert_eq!(
            silent.support(CallFeature::MuteRinger),
            Support::Unsupported {
                reason: UnsupportedReason::RemoteDoesNotAdvertise
            }
        );
    }

    #[test]
    fn hfp_audio_codec_does_not_imply_call_control() {
        let audio_only = HfpEvidence {
            service_level_connection: true,
            audio_codec: Some(AudioCodec::Msbc),
            ..Default::default()
        };
        let caps = CallCapabilities::from_hfp_evidence(HfpHost::LinuxPipeWire, &audio_only);
        assert_eq!(caps.support(CallFeature::AudioDownlink), Support::Declared);
        for f in [
            CallFeature::Answer,
            CallFeature::Hangup,
            CallFeature::Dial,
            CallFeature::CallStateEvents,
        ] {
            assert_eq!(caps.support(f), Support::Unknown, "{f:?}");
        }
        let full = HfpEvidence {
            call_indicators: true,
            host_call_control_api: true,
            three_way_calling: true,
            ..audio_only
        };
        let caps = CallCapabilities::from_hfp_evidence(HfpHost::LinuxPipeWire, &full);
        assert_eq!(caps.support(CallFeature::Answer), Support::Declared);
        assert!(
            CallFeature::ALL
                .into_iter()
                .all(|f| caps.support(f) != Support::Verified)
        );
        assert_eq!(
            caps.support(CallFeature::MuteRinger),
            Support::Unsupported {
                reason: UnsupportedReason::NotInProtocol
            }
        );
    }

    #[test]
    fn verification_is_explicit_and_cannot_override_unsupported() {
        let mut caps =
            CallCapabilities::from_kde_packet_types([KDE_PACKET_TELEPHONY], Vec::<&str>::new());
        assert!(caps.record_verified(CallFeature::CallStateEvents));
        assert_eq!(
            caps.support(CallFeature::CallStateEvents),
            Support::Verified
        );
        assert!(!caps.record_verified(CallFeature::Answer));
        assert!(matches!(
            caps.support(CallFeature::Answer),
            Support::Unsupported { .. }
        ));
    }

    #[test]
    fn incoming_answered_call_completes() {
        let mut s = up();
        assert_eq!(
            update(&mut s, "a", CallDirection::Incoming, CallPhase::Incoming),
            Applied::Changed
        );
        assert_eq!(
            update(&mut s, "a", CallDirection::Unknown, CallPhase::Active),
            Applied::Changed
        );
        assert_eq!(s.calls[&id("a")].direction, CallDirection::Incoming);
        assert_eq!(
            s.apply(CallEvent::CallEnded {
                id: id("a"),
                reason: EndReason::Unspecified
            }),
            Applied::Changed
        );
        assert!(s.calls.is_empty());
        assert_eq!(s.recent_ended[0].reason, EndReason::Completed);
    }

    #[test]
    fn kde_unanswered_sequence_is_not_answered_and_missed_call_packet_is_redundant() {
        let mut s = up();
        assert_eq!(
            kde(&mut s, "ringing", false, Some("+100")),
            Some(Applied::Changed)
        );
        assert_eq!(
            kde(&mut s, "ringing", true, Some("+100")),
            Some(Applied::Changed)
        );
        assert_eq!(kde(&mut s, "missedCall", false, Some("+100")), None);
        assert!(s.calls.is_empty());
        assert_eq!(s.recent_ended[0].reason, EndReason::NotAnswered);
        assert_eq!(
            s.recent_ended[0].party.number.as_ref().unwrap().expose(),
            "+100"
        );
    }

    #[test]
    fn kde_talking_keeps_incoming_direction_and_number() {
        let mut s = up();
        kde(&mut s, "ringing", false, Some("+100"));
        kde(&mut s, "talking", false, None);
        let call = &s.calls[&id(KDE_CURRENT_CALL_ID)];
        assert_eq!(
            (call.phase, call.direction),
            (CallPhase::Active, CallDirection::Incoming)
        );
        assert_eq!(call.party.number.as_ref().unwrap().expose(), "+100");
        kde(&mut s, "talking", true, None);
        assert_eq!(s.recent_ended[0].reason, EndReason::Completed);
        assert_eq!(
            CallEvent::from_kde_telephony("sms", false, None, None),
            None
        );
    }

    #[test]
    fn invalid_events_are_rejected_without_changing_state() {
        let mut s = up();
        update(&mut s, "a", CallDirection::Incoming, CallPhase::Incoming);
        update(&mut s, "a", CallDirection::Unknown, CallPhase::Active);
        let before = s.clone();
        assert_eq!(
            update(&mut s, "a", CallDirection::Unknown, CallPhase::Dialing),
            Applied::Rejected(EventRejection::InvalidTransition)
        );
        assert_eq!(
            update(&mut s, "b", CallDirection::Incoming, CallPhase::Dialing),
            Applied::Rejected(EventRejection::InconsistentDirection)
        );
        assert_eq!(
            s.apply(CallEvent::CallEnded {
                id: id("zzz"),
                reason: EndReason::Unspecified
            }),
            Applied::Rejected(EventRejection::UnknownCall)
        );
        assert_eq!(s, before);
    }

    #[test]
    fn events_require_link_and_link_loss_resets_volatile_state() {
        let mut s = CallState::default();
        assert_eq!(
            update(&mut s, "a", CallDirection::Incoming, CallPhase::Incoming),
            Applied::Rejected(EventRejection::LinkDown)
        );
        s.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        update(&mut s, "a", CallDirection::Outgoing, CallPhase::Dialing);
        s.apply(CallEvent::Audio {
            state: AudioLinkState::Connected {
                codec: AudioCodec::Cvsd,
            },
        });
        s.apply(CallEvent::MuteChanged {
            scope: MuteScope::PhoneRinger,
            muted: true,
        });
        assert_eq!(
            s.apply(CallEvent::Link {
                state: LinkState::Down
            }),
            Applied::Changed
        );
        assert!(s.calls.is_empty());
        assert_eq!(
            (s.audio, s.ringer_muted, s.link),
            (AudioLinkState::Disconnected, None, LinkState::Down)
        );
        assert_eq!(s.recent_ended[0].reason, EndReason::LinkLost);
    }

    #[test]
    fn inputs_are_bounded() {
        let mut s = up();
        for i in 0..MAX_CALLS {
            update(
                &mut s,
                &format!("c{i}"),
                CallDirection::Incoming,
                CallPhase::Waiting,
            );
        }
        assert_eq!(
            update(&mut s, "extra", CallDirection::Incoming, CallPhase::Waiting),
            Applied::Rejected(EventRejection::TooManyCalls)
        );
        let long = "9".repeat(MAX_FIELD_LEN + 1);
        let event = CallEvent::CallUpdated {
            id: id("c0"),
            direction: CallDirection::Incoming,
            phase: CallPhase::Waiting,
            party: CallParty {
                number: Some(Sensitive::new(long)),
                name: None,
            },
        };
        assert_eq!(
            s.apply(event),
            Applied::Rejected(EventRejection::FieldTooLong)
        );
        for i in 0..MAX_CALLS + 3 {
            s.apply(CallEvent::CallEnded {
                id: id(&format!("c{}", i % MAX_CALLS)),
                reason: EndReason::Unspecified,
            });
            update(
                &mut s,
                &format!("c{}", i % MAX_CALLS),
                CallDirection::Incoming,
                CallPhase::Incoming,
            );
        }
        assert!(s.recent_ended.len() <= MAX_ENDED);
    }

    #[test]
    fn commands_check_capabilities_arguments_and_state() {
        let caps = CallCapabilities::from_kde_packet_types(
            [KDE_PACKET_TELEPHONY],
            [KDE_PACKET_REQUEST_MUTE],
        );
        let mut s = up();
        assert_eq!(
            s.check_command(&caps, &CallCommand::MuteRinger),
            Err(CommandRejection::InvalidState)
        );
        kde(&mut s, "ringing", false, None);
        assert_eq!(s.check_command(&caps, &CallCommand::MuteRinger), Ok(()));
        let answer = CallCommand::Answer {
            call: id(KDE_CURRENT_CALL_ID),
        };
        assert_eq!(
            s.check_command(&caps, &answer),
            Err(CommandRejection::Unsupported {
                reason: UnsupportedReason::NotInProtocol
            })
        );
        let mut hfp = CallCapabilities::unprobed(CallBackend::BluetoothHfp {
            host: HfpHost::LinuxPipeWire,
        });
        hfp.set(CallFeature::Answer, Support::Declared);
        hfp.set(CallFeature::Dial, Support::Declared);
        assert_eq!(s.check_command(&hfp, &answer), Ok(()));
        assert_eq!(
            s.check_command(&hfp, &CallCommand::Answer { call: id("nope") }),
            Err(CommandRejection::NoSuchCall)
        );
        assert_eq!(
            hfp.authorize(&CallCommand::Dial {
                number: Sensitive::new("12; rm")
            }),
            Err(CommandRejection::InvalidArgument)
        );
        assert_eq!(
            hfp.authorize(&CallCommand::Dial {
                number: Sensitive::new("+1 555")
            }),
            Err(CommandRejection::InvalidArgument)
        );
        assert_eq!(
            hfp.authorize(&CallCommand::Dial {
                number: Sensitive::new("+15551234")
            }),
            Ok(())
        );
        s.apply(CallEvent::Link {
            state: LinkState::Down,
        });
        assert_eq!(
            s.check_command(
                &hfp,
                &CallCommand::Dial {
                    number: Sensitive::new("1")
                }
            ),
            Err(CommandRejection::LinkDown)
        );
    }

    #[test]
    fn reject_needs_hangup_support_and_a_ringing_call() {
        let mut caps = CallCapabilities::unprobed(CallBackend::GeekBridgeLink);
        let reject = CallCommand::Reject { call: id("a") };
        assert_eq!(reject.feature(), CallFeature::Hangup);
        assert_eq!(caps.authorize(&reject), Err(CommandRejection::NotProbed));
        caps.set(CallFeature::Hangup, Support::Declared);
        let mut s = up();
        assert_eq!(
            s.check_command(&caps, &reject),
            Err(CommandRejection::NoSuchCall)
        );
        update(&mut s, "a", CallDirection::Incoming, CallPhase::Incoming);
        assert_eq!(s.check_command(&caps, &reject), Ok(()));
        update(&mut s, "a", CallDirection::Unknown, CallPhase::Active);
        // A call in progress is ended with Hangup, never with Reject.
        assert_eq!(
            s.check_command(&caps, &reject),
            Err(CommandRejection::InvalidState)
        );
        assert_eq!(
            s.check_command(&caps, &CallCommand::Hangup { call: id("a") }),
            Ok(())
        );
        assert_eq!(
            CallCommand::Reject { call: id("") }.validate(),
            Err(CommandRejection::InvalidArgument)
        );
    }

    #[test]
    fn serde_round_trips_and_debug_redacts() {
        let event = CallEvent::CallUpdated {
            id: id("a"),
            direction: CallDirection::Incoming,
            phase: CallPhase::Incoming,
            party: CallParty {
                number: Some(Sensitive::new("+15551234")),
                name: Some(Sensitive::new("Ada")),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"call_updated\"") && json.contains("+15551234"));
        assert_eq!(serde_json::from_str::<CallEvent>(&json).unwrap(), event);
        let debug = format!(
            "{event:?} {:?}",
            CallCommand::Dial {
                number: Sensitive::new("+15551234")
            }
        );
        assert!(!debug.contains("5551234") && !debug.contains("Ada"));
        let caps = CallCapabilities::from_hfp_evidence(HfpHost::MacOs, &HfpEvidence::default());
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(
            serde_json::from_str::<CallCapabilities>(&json).unwrap(),
            caps
        );
        let state_json = serde_json::to_string(&up()).unwrap();
        assert!(state_json.contains("\"link\":\"up\""));
    }

    #[test]
    fn replay_is_deterministic() {
        let script = [("ringing", false), ("talking", false), ("talking", true)];
        let run = || {
            let mut s = up();
            for (e, c) in script {
                kde(&mut s, e, c, Some("1"));
            }
            s
        };
        assert_eq!(run(), run());
    }
}
