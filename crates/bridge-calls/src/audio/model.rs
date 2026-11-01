//! Platform-neutral description of an audio server's state: cards, sinks, sources and modules.
//!
//! These are plain data. The Linux adapter fills them from libpulse; tests fill them by hand.
//! Nothing here performs I/O.

use std::collections::BTreeMap;

use bridge_core::calls::Sensitive;
use serde::Serialize;

pub type Props = BTreeMap<String, String>;

/// A Bluetooth device address. `Debug` is redacted like [`Sensitive`]; serialization is the
/// canonical `AA:BB:CC:DD:EE:FF` string (authenticated local APIs only).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BtAddress([u8; 6]);

impl BtAddress {
    /// Accepts six hex octets separated by `:`, `_` or `-` (PipeWire card names use `_`).
    pub fn parse(s: &str) -> Option<Self> {
        let mut out = [0u8; 6];
        let mut parts = s.split([':', '_', '-']);
        for slot in &mut out {
            let part = parts.next()?;
            if part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            *slot = u8::from_str_radix(part, 16).ok()?;
        }
        parts.next().is_none().then_some(Self(out))
    }

    pub fn from_sensitive(s: &Sensitive) -> Option<Self> {
        Self::parse(s.expose())
    }

    /// Explicit access, so reading the content is visible at the call site.
    pub fn expose(&self) -> String {
        let b = &self.0;
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl std::fmt::Debug for BtAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Serialize for BtAddress {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.expose())
    }
}

/// Codec implied by a Bluetooth voice profile *name* (`headset-head-unit-msbc`). It is a hint
/// from the card, not evidence about samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceCodec {
    Cvsd,
    Msbc,
    Lc3,
    /// The profile name does not say (`headset-head-unit`).
    Unspecified,
}

/// What a card profile or node `api.bluez5.profile` value means for routing.
///
/// A2DP is one-directional media (the phone plays *to* this computer, or the reverse) and has no
/// microphone. Only [`ProfileKind::Voice`] (HSP/HFP hands-free unit, an SCO/eSCO link) carries
/// duplex call audio. BlueZ and PipeWire name HSP and HFP profiles alike, so the two cannot be
/// told apart here and both are `Voice`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "codec", rename_all = "snake_case")]
pub enum ProfileKind {
    Off,
    /// This computer is the A2DP sink: phone media playing here. Not a call path.
    A2dpSink,
    A2dpSource,
    /// This computer is the hands-free unit of an HSP/HFP link.
    Voice(VoiceCodec),
    /// This computer would be the audio gateway: the wrong role for a phone.
    AudioGateway,
    Other,
}

fn family(name: &str, base: &str) -> bool {
    name == base
        || name
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with(['-', '_']))
}

impl ProfileKind {
    pub fn classify(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        let n = n.as_str();
        if n == "off" {
            Self::Off
        } else if family(n, "a2dp-sink") || family(n, "a2dp_sink") {
            Self::A2dpSink
        } else if family(n, "a2dp-source") || family(n, "a2dp_source") {
            Self::A2dpSource
        } else if family(n, "headset-audio-gateway")
            || family(n, "headset_audio_gateway")
            || family(n, "handsfree-audio-gateway")
            || family(n, "handsfree_audio_gateway")
        {
            Self::AudioGateway
        } else if [
            "headset-head-unit",
            "headset_head_unit",
            "handsfree_head_unit",
            "handsfree-head-unit",
        ]
        .iter()
        .any(|b| family(n, b))
        {
            Self::Voice(if n.contains("msbc") {
                VoiceCodec::Msbc
            } else if n.contains("cvsd") {
                VoiceCodec::Cvsd
            } else if n.contains("lc3") {
                VoiceCodec::Lc3
            } else {
                VoiceCodec::Unspecified
            })
        } else {
            Self::Other
        }
    }

    pub fn is_voice(&self) -> bool {
        matches!(self, Self::Voice(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardProfile {
    pub name: String,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardInfo {
    pub index: u32,
    pub name: String,
    pub props: Props,
    pub profiles: Vec<CardProfile>,
    pub active_profile: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub card: Option<u32>,
    pub owner_module: Option<u32>,
    pub props: Props,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub card: Option<u32>,
    pub owner_module: Option<u32>,
    /// `Some` for a sink monitor, which is never a microphone.
    pub monitor_of_sink: Option<u32>,
    pub props: Props,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    pub index: u32,
    pub name: String,
    /// The argument string the module was loaded with. Some servers do not report it.
    pub argument: Option<String>,
}

/// Identifies one running audio-server instance. Module indices are only meaningful for the
/// instance that issued them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServerIdentity {
    pub server_name: String,
    pub server_version: String,
    pub host_name: String,
    /// Random per server start in PulseAudio. 0 means the server does not report one, in which
    /// case a restart cannot be told from a reconnect and module indices are never trusted
    /// across a reconnect.
    pub cookie: u32,
}

impl ServerIdentity {
    pub fn proves_same_instance(&self, other: &Self) -> bool {
        self.cookie != 0 && self == other
    }
}

/// One read of the whole server. Entities are numbered per read; they are not stable handles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerSnapshot {
    pub identity: ServerIdentity,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub cards: Vec<CardInfo>,
    pub sinks: Vec<SinkInfo>,
    pub sources: Vec<SourceInfo>,
    pub modules: Vec<ModuleInfo>,
}

/// A sink or source pinned by index *and* exact name. Both must still match at start time.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct EndpointId {
    pub index: u32,
    pub name: String,
}

pub(crate) const PROP_BT_ADDRESS: &str = "api.bluez5.address";
pub(crate) const PROP_BT_PROFILE: &str = "api.bluez5.profile";
pub(crate) const PROP_DEVICE_API: &str = "device.api";
pub(crate) const PROP_DEVICE_STRING: &str = "device.string";

/// The Bluetooth address a card or node reports through its properties. Names are never parsed.
pub(crate) fn props_address(props: &Props) -> Option<BtAddress> {
    if let Some(a) = props.get(PROP_BT_ADDRESS) {
        return BtAddress::parse(a);
    }
    // `device.string` carries the address for bluez5 devices; accept it only when the device
    // says it is a bluez5 one.
    if props.get(PROP_DEVICE_API).map(String::as_str) == Some("bluez5") {
        return props
            .get(PROP_DEVICE_STRING)
            .and_then(|s| BtAddress::parse(s));
    }
    None
}

impl CardInfo {
    pub fn address(&self) -> Option<BtAddress> {
        props_address(&self.props)
    }
    pub fn active_kind(&self) -> Option<ProfileKind> {
        self.active_profile.as_deref().map(ProfileKind::classify)
    }
    pub fn voice_profile_available(&self) -> bool {
        self.profiles
            .iter()
            .any(|p| p.available && ProfileKind::classify(&p.name).is_voice())
    }
}

impl SinkInfo {
    pub fn id(&self) -> EndpointId {
        EndpointId {
            index: self.index,
            name: self.name.clone(),
        }
    }
}
impl SourceInfo {
    pub fn id(&self) -> EndpointId {
        EndpointId {
            index: self.index,
            name: self.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_parsing_accepts_pipewire_card_form_and_rejects_partials() {
        let a = BtAddress::parse("aa:BB:cc:00:11:22").unwrap();
        assert_eq!(a.expose(), "AA:BB:CC:00:11:22");
        assert_eq!(BtAddress::parse("AA_BB_CC_00_11_22"), Some(a));
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11"), None);
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11:22:33"), None);
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11:2"), None);
        assert_eq!(BtAddress::parse("GG:BB:CC:00:11:22"), None);
        assert_eq!(format!("{a:?}"), "<redacted>");
    }

    #[test]
    fn profile_classification_separates_a2dp_from_duplex_voice() {
        use ProfileKind::*;
        assert_eq!(ProfileKind::classify("a2dp-sink"), A2dpSink);
        assert_eq!(ProfileKind::classify("a2dp-sink-aac"), A2dpSink);
        assert_eq!(ProfileKind::classify("a2dp_sink"), A2dpSink);
        assert_eq!(ProfileKind::classify("a2dp-source"), A2dpSource);
        assert_eq!(
            ProfileKind::classify("headset-head-unit"),
            Voice(VoiceCodec::Unspecified)
        );
        assert_eq!(
            ProfileKind::classify("headset-head-unit-msbc"),
            Voice(VoiceCodec::Msbc)
        );
        assert_eq!(
            ProfileKind::classify("headset-head-unit-cvsd"),
            Voice(VoiceCodec::Cvsd)
        );
        assert_eq!(
            ProfileKind::classify("handsfree_head_unit"),
            Voice(VoiceCodec::Unspecified)
        );
        assert_eq!(ProfileKind::classify("headset-audio-gateway"), AudioGateway);
        assert_eq!(ProfileKind::classify("off"), Off);
        // A name that merely starts with a known family prefix is not that family.
        assert_eq!(ProfileKind::classify("a2dp-sinkhole"), Other);
        assert_eq!(ProfileKind::classify("pro-audio"), Other);
    }

    #[test]
    fn device_string_is_only_trusted_for_bluez_devices() {
        let mut p = Props::new();
        p.insert(PROP_DEVICE_STRING.into(), "AA:BB:CC:00:11:22".into());
        assert_eq!(props_address(&p), None);
        p.insert(PROP_DEVICE_API.into(), "bluez5".into());
        assert!(props_address(&p).is_some());
        p.insert(PROP_BT_ADDRESS.into(), "11:22:33:44:55:66".into());
        assert_eq!(props_address(&p), BtAddress::parse("11:22:33:44:55:66"));
    }
}
