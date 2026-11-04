// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-neutral description of an audio server's state: cards, devices and streams.
//!
//! These are plain data. The Linux adapter fills them from libpulse (`pipewire-pulse`); tests
//! fill them by hand. Nothing here performs I/O.
//!
//! ## How a phone call looks on PipeWire (1.x, checked against 1.6.9 sources)
//!
//! With the phone as the HFP audio gateway and this computer as the hands-free unit:
//!
//! * the phone's card (`bluez_card.<ADDR>`) has the profile **`audio-gateway`** (plus `off`).
//!   Profile names describe the *remote* device: `audio-gateway` = the remote is a phone.
//!   `headset-head-unit` is what a *headset* gets (the computer is then the gateway) and never
//!   carries a phone call.
//! * while the voice link (SCO/eSCO) is open, the card exposes two **streams**, not devices:
//!   the caller's voice as a playback stream (`Stream/Output/Audio`, a pulse *sink input*,
//!   `bluez_input.<ADDR>.<n>`) and the uplink as a capture stream (`Stream/Input/Audio`, a pulse
//!   *source output*, `bluez_output.<ADDR>.<n>`), both with `api.bluez5.profile =
//!   headset-audio-gateway` and `node.autoconnect = true`. WirePlumber links them to the default
//!   speakers and microphone by itself; GeekBridge moves them to the devices the user picked.
//! * they disappear when the voice link closes. Nothing has to be unloaded afterwards.
//!
//! PulseAudio (no PipeWire) names the same profile `headset_audio_gateway` and exposes a source
//! and sink instead; it also loops them to the defaults itself. GeekBridge detects that shape and
//! reports it, but only routes on PipeWire.

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

    /// Last two octets only, for logs and user-facing lines (`..:EE:FF`).
    pub fn short(&self) -> String {
        format!("..:{:02X}:{:02X}", self.0[4], self.0[5])
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

/// What a card profile or a node's `api.bluez5.profile` says about the *remote* device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Off,
    /// The remote is a phone (HFP/HSP audio gateway): PipeWire card profile `audio-gateway`,
    /// node profile `headset-audio-gateway`, PulseAudio `headset_audio_gateway`. The only kind
    /// that carries a phone call.
    PhoneGateway,
    /// The remote sends media here (a phone playing music): `a2dp-source`. No microphone.
    MediaFromRemote,
    /// The remote plays media (headphones/speaker): `a2dp-sink[-codec]`.
    MediaToRemote,
    /// The remote is a headset (this computer is the gateway): `headset-head-unit[-codec]`.
    /// A Bluetooth headset's microphone profile, never a phone call.
    HeadsetUnit,
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
        } else if [
            "audio-gateway",
            "headset-audio-gateway",
            "headset_audio_gateway",
            "handsfree_audio_gateway",
            "handsfree-audio-gateway",
        ]
        .iter()
        .any(|b| family(n, b))
        {
            Self::PhoneGateway
        } else if family(n, "a2dp-source") || family(n, "a2dp_source") {
            Self::MediaFromRemote
        } else if family(n, "a2dp-sink") || family(n, "a2dp_sink") {
            Self::MediaToRemote
        } else if [
            "headset-head-unit",
            "headset_head_unit",
            "handsfree_head_unit",
            "handsfree-head-unit",
        ]
        .iter()
        .any(|b| family(n, b))
        {
            Self::HeadsetUnit
        } else {
            Self::Other
        }
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
    pub mute: bool,
    pub props: Props,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub card: Option<u32>,
    pub mute: bool,
    /// `Some` for a sink monitor, which is never a microphone.
    pub monitor_of_sink: Option<u32>,
    pub props: Props,
}

/// A playback stream (pulse sink input) or capture stream (pulse source output).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub index: u32,
    pub name: String,
    /// The sink (for a playback stream) or source (capture stream) it is connected to.
    pub device: Option<u32>,
    pub corked: bool,
    pub mute: bool,
    pub props: Props,
}

/// Identifies one running audio-server instance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServerIdentity {
    pub server_name: String,
    pub server_version: String,
    pub host_name: String,
    /// Random per server start in PulseAudio and pipewire-pulse. 0 = not reported.
    pub cookie: u32,
}

impl ServerIdentity {
    /// `pipewire-pulse` reports `PulseAudio (on PipeWire x.y.z)`.
    pub fn is_pipewire(&self) -> bool {
        self.server_name.contains("PipeWire")
    }
}

/// One read of the whole server. Indices are per read; names are the stable handles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerSnapshot {
    pub identity: ServerIdentity,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub cards: Vec<CardInfo>,
    pub sinks: Vec<SinkInfo>,
    pub sources: Vec<SourceInfo>,
    /// Playback streams (pulse sink inputs).
    pub sink_inputs: Vec<StreamInfo>,
    /// Capture streams (pulse source outputs).
    pub source_outputs: Vec<StreamInfo>,
}

pub const PROP_BT_ADDRESS: &str = "api.bluez5.address";
pub const PROP_BT_PROFILE: &str = "api.bluez5.profile";
pub const PROP_BT_CODEC: &str = "api.bluez5.codec";
pub const PROP_DEVICE_API: &str = "device.api";
pub const PROP_DEVICE_STRING: &str = "device.string";
pub const PROP_MEDIA_CLASS: &str = "media.class";
pub const PROP_NODE_NAME: &str = "node.name";
pub const PROP_FORM_FACTOR: &str = "device.form_factor";

/// The Bluetooth address a card or node reports through its properties. Names are never parsed.
pub fn props_address(props: &Props) -> Option<BtAddress> {
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
    /// The phone-gateway profile this card offers, if any (available or not).
    pub fn gateway_profile(&self) -> Option<&CardProfile> {
        self.profiles
            .iter()
            .filter(|p| ProfileKind::classify(&p.name) == ProfileKind::PhoneGateway)
            .max_by_key(|p| p.available)
    }
}

impl StreamInfo {
    pub fn address(&self) -> Option<BtAddress> {
        props_address(&self.props)
    }
    pub fn profile_kind(&self) -> Option<ProfileKind> {
        self.props
            .get(PROP_BT_PROFILE)
            .map(|p| ProfileKind::classify(p))
    }
    /// The HFP codec name PipeWire reports (`cvsd`, `msbc`, `lc3_swb`), if any.
    pub fn codec(&self) -> Option<&str> {
        self.props.get(PROP_BT_CODEC).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_parsing_accepts_pipewire_card_form_and_rejects_partials() {
        let a = BtAddress::parse("aa:BB:cc:00:11:22").unwrap();
        assert_eq!(a.expose(), "AA:BB:CC:00:11:22");
        assert_eq!(a.short(), "..:11:22");
        assert_eq!(BtAddress::parse("AA_BB_CC_00_11_22"), Some(a));
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11"), None);
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11:22:33"), None);
        assert_eq!(BtAddress::parse("AA:BB:CC:00:11:2"), None);
        assert_eq!(BtAddress::parse("GG:BB:CC:00:11:22"), None);
        assert_eq!(format!("{a:?}"), "<redacted>");
    }

    #[test]
    fn profile_names_describe_the_remote_device() {
        use ProfileKind::*;
        // PipeWire card profile and node profile of a phone; PulseAudio's name for the same.
        assert_eq!(ProfileKind::classify("audio-gateway"), PhoneGateway);
        assert_eq!(ProfileKind::classify("headset-audio-gateway"), PhoneGateway);
        assert_eq!(ProfileKind::classify("headset_audio_gateway"), PhoneGateway);
        // A headset's profiles are never a phone call.
        assert_eq!(ProfileKind::classify("headset-head-unit"), HeadsetUnit);
        assert_eq!(ProfileKind::classify("headset-head-unit-msbc"), HeadsetUnit);
        assert_eq!(ProfileKind::classify("handsfree_head_unit"), HeadsetUnit);
        assert_eq!(ProfileKind::classify("a2dp-sink"), MediaToRemote);
        assert_eq!(ProfileKind::classify("a2dp-sink-aac"), MediaToRemote);
        assert_eq!(ProfileKind::classify("a2dp-source"), MediaFromRemote);
        assert_eq!(ProfileKind::classify("off"), Off);
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

    #[test]
    fn gateway_profile_prefers_an_available_one() {
        let card = CardInfo {
            index: 1,
            name: "bluez_card.X".into(),
            props: Props::new(),
            profiles: vec![
                CardProfile {
                    name: "off".into(),
                    available: true,
                },
                CardProfile {
                    name: "audio-gateway".into(),
                    available: true,
                },
            ],
            active_profile: Some("off".into()),
        };
        assert_eq!(card.gateway_profile().unwrap().name, "audio-gateway");
        assert_eq!(card.active_kind(), Some(ProfileKind::Off));
    }
}
