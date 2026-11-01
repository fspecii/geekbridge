//! Platform-neutral snapshot and control types. Nothing here touches a bus.

use bridge_core::calls::{
    AudioCodec, AudioLinkState, CallCapabilities, CallDirection, CallId, CallParty, CallPhase,
    Sensitive,
};
use serde::Serialize;

use crate::declared::{DeclaredApi, FeatureEvidence};

/// Whether the telephony service is on the bus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ServiceStatus {
    /// Nothing owns the service name. Every earlier handle is void.
    Absent,
    /// `owner` is the unique bus name of the owning process. Handles are bound to it: a service
    /// restart yields a new owner, which makes every older handle fail with
    /// [`crate::ControlError::ServiceRestarted`] instead of reaching an unrelated object.
    Present { owner: String },
}

/// Identifies one connected phone (an HFP audio gateway) at one point in time.
///
/// The object path (`/org/pipewire/Telephony/ag<N>`) is *reused* by PipeWire for later phones, so
/// the Bluetooth address and the service's unique bus name are part of the identity and are
/// re-checked on every control call.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DeviceRef {
    pub owner: String,
    pub path: String,
    pub address: Sensitive,
}

/// Identifies one call on one phone. Call object paths (`.../call<M>`) are reused too, so the
/// caller-identity fingerprint taken when the handle was issued is compared again before any
/// answer/hang-up is sent.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CallRef {
    pub device: DeviceRef,
    pub id: CallId,
    pub path: String,
    pub fingerprint: CallFingerprint,
}

/// Properties that do not change during a call's life (`Call1` has no way to change them).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CallFingerprint {
    pub line_identification: Option<Sensitive>,
    pub incoming_line: Option<Sensitive>,
    pub name: Option<Sensitive>,
}

/// `AudioGatewayTransport1.State` (`telephony.c::transport_state_to_string`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportState {
    Error,
    Idle,
    Pending,
    Active,
    /// A string this crate does not know (kept in `raw_state`); treated as not connected.
    Unrecognized,
}

impl TransportState {
    pub fn from_dbus(s: &str) -> Self {
        match s {
            "error" => Self::Error,
            "idle" => Self::Idle,
            "pending" => Self::Pending,
            "active" => Self::Active,
            _ => Self::Unrecognized,
        }
    }
}

/// What the `Codec` byte tells us. 0 means none negotiated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AudioCodecEvidence {
    None,
    Known(AudioCodec),
    /// A nonzero id this crate cannot name.
    Unrecognized(u8),
}

impl AudioCodecEvidence {
    /// HFP codec ids: 1 CVSD, 2 mSBC, 3 LC3-SWB (HFP 1.9 numbering; PipeWire's `defs.h` could not
    /// be fetched to confirm the constant, so unknown ids stay `Unrecognized`).
    pub fn from_id(id: u8) -> Self {
        match id {
            0 => Self::None,
            1 => Self::Known(AudioCodec::Cvsd),
            2 => Self::Known(AudioCodec::Msbc),
            3 => Self::Known(AudioCodec::Lc3Swb),
            other => Self::Unrecognized(other),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransportSnapshot {
    pub state: TransportState,
    pub raw_state: String,
    pub codec_id: u8,
    pub codec_evidence: AudioCodecEvidence,
    pub reject_sco: Option<bool>,
}

impl TransportSnapshot {
    /// Map to the neutral audio model. Unknown states count as disconnected.
    pub fn audio_link(&self) -> AudioLinkState {
        match self.state {
            TransportState::Active => AudioLinkState::Connected {
                codec: match self.codec_evidence {
                    AudioCodecEvidence::Known(c) => c,
                    _ => AudioCodec::Unknown,
                },
            },
            TransportState::Pending => AudioLinkState::Connecting,
            _ => AudioLinkState::Disconnected,
        }
    }
}

/// `Call1.State`, mapped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CallObservation {
    Live(CallPhase),
    /// `disconnected`: the call is over (PipeWire removes the object right after).
    Disconnected,
    /// A state string this crate does not know. The call is shown but never acted on.
    Unrecognized(String),
}

impl CallObservation {
    pub fn from_dbus(s: &str) -> Self {
        match s {
            "active" => Self::Live(CallPhase::Active),
            "held" => Self::Live(CallPhase::Held),
            "dialing" => Self::Live(CallPhase::Dialing),
            "alerting" => Self::Live(CallPhase::Alerting),
            "incoming" => Self::Live(CallPhase::Incoming),
            "waiting" => Self::Live(CallPhase::Waiting),
            "disconnected" => Self::Disconnected,
            other => Self::Unrecognized(other.to_string()),
        }
    }
    pub fn phase(&self) -> Option<CallPhase> {
        match self {
            Self::Live(p) => Some(*p),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CallSnapshot {
    /// Path tail (`call3`), unique among live calls on one phone.
    pub id: CallId,
    pub path: String,
    pub raw_state: String,
    pub observation: CallObservation,
    pub fingerprint: CallFingerprint,
    pub multiparty: bool,
}

impl CallSnapshot {
    pub fn direction(&self) -> CallDirection {
        match self.observation.phase() {
            Some(CallPhase::Incoming | CallPhase::Waiting) => CallDirection::Incoming,
            Some(CallPhase::Dialing | CallPhase::Alerting) => CallDirection::Outgoing,
            _ => CallDirection::Unknown,
        }
    }
    pub fn party(&self) -> CallParty {
        let fp = &self.fingerprint;
        // `LineIdentification` is the remote number; for incoming calls `IncomingLine` is the
        // number that was dialed (second line), so it is not used as the party.
        CallParty {
            number: fp.line_identification.clone(),
            name: fp.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PhoneSnapshot {
    pub device: DeviceRef,
    /// Volume scale is 0-15 (HFP); `None` when the property was not readable.
    pub speaker_volume: Option<u8>,
    pub microphone_volume: Option<u8>,
    pub transport: Option<TransportSnapshot>,
    pub calls: Vec<CallSnapshot>,
    pub declared_api: DeclaredApi,
    /// Declared support only; see [`FeatureEvidence`] for the basis of each entry.
    pub capabilities: CallCapabilities,
    pub evidence: Vec<FeatureEvidence>,
}

impl PhoneSnapshot {
    pub fn call(&self, id: &CallId) -> Option<&CallSnapshot> {
        self.calls.iter().find(|c| &c.id == id)
    }
    pub fn call_ref(&self, id: &CallId) -> Option<CallRef> {
        self.call(id).map(|c| CallRef {
            device: self.device.clone(),
            id: c.id.clone(),
            path: c.path.clone(),
            fingerprint: c.fingerprint.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub service: ServiceStatus,
    pub phones: Vec<PhoneSnapshot>,
    /// Non-fatal oddities (a phone that vanished mid-read, an unreadable property).
    pub warnings: Vec<String>,
}

impl Snapshot {
    pub fn absent() -> Self {
        Self {
            service: ServiceStatus::Absent,
            phones: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// An explicitly requested change. Constructing an `Action` has no effect; only
/// [`crate::CallBackend::invoke`] sends anything, and only the method the variant names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `AudioGateway1.Dial(s)`. The call object appears later through `InterfacesAdded`.
    Dial {
        device: DeviceRef,
        number: Sensitive,
    },
    /// `Call1.Answer()`; only for a call in `incoming`.
    Answer {
        call: CallRef,
    },
    /// `Call1.Hangup()` on a call in `incoming` or `waiting`. PipeWire has no separate reject
    /// method; hang-up of a ringing call is how HFP rejects it. Refuses calls that are not ringing.
    Reject {
        call: CallRef,
    },
    /// `Call1.Hangup()` on any live call.
    Hangup {
        call: CallRef,
    },
    /// `AudioGateway1.HangupAll()`.
    HangupAll {
        device: DeviceRef,
    },
    SwapCalls {
        device: DeviceRef,
    },
    ReleaseAndAnswer {
        device: DeviceRef,
    },
    ReleaseAndSwap {
        device: DeviceRef,
    },
    HoldAndAnswer {
        device: DeviceRef,
    },
    CreateMultiparty {
        device: DeviceRef,
    },
    /// `AudioGateway1.SendTones(s)` (DTMF) during an active call.
    SendTones {
        device: DeviceRef,
        tones: Sensitive,
    },
    /// `AudioGatewayTransport1.Activate()`: ask for the SCO/eSCO voice link.
    ActivateAudio {
        device: DeviceRef,
    },
}

impl Action {
    pub fn device(&self) -> &DeviceRef {
        match self {
            Self::Dial { device, .. }
            | Self::HangupAll { device }
            | Self::SwapCalls { device }
            | Self::ReleaseAndAnswer { device }
            | Self::ReleaseAndSwap { device }
            | Self::HoldAndAnswer { device }
            | Self::CreateMultiparty { device }
            | Self::SendTones { device, .. }
            | Self::ActivateAudio { device } => device,
            Self::Answer { call } | Self::Reject { call } | Self::Hangup { call } => &call.device,
        }
    }
    pub fn call(&self) -> Option<&CallRef> {
        match self {
            Self::Answer { call } | Self::Reject { call } | Self::Hangup { call } => Some(call),
            _ => None,
        }
    }
}
