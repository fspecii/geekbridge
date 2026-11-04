// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure decisions over one [`ServerSnapshot`]: where is the phone's call audio, which local
//! devices did the user pick, and does audio flow. Nothing here touches a server.

use serde::{Deserialize, Serialize};

use crate::audio::model::{
    BtAddress, CardInfo, PROP_FORM_FACTOR, PROP_MEDIA_CLASS, ProfileKind, ServerSnapshot,
    StreamInfo, props_address,
};

/// The phone's side of a call, as the audio server shows it right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneVoice {
    pub address: BtAddress,
    pub card: Option<CardInfo>,
    /// The caller's voice: a playback stream (pulse sink input).
    pub rx: Option<StreamInfo>,
    /// The uplink to the caller: a capture stream (pulse source output).
    pub tx: Option<StreamInfo>,
    /// PulseAudio shape: the card exposes a voice source/sink instead of streams.
    pub legacy_devices: bool,
}

impl PhoneVoice {
    pub fn streams_present(&self) -> bool {
        self.rx.is_some() || self.tx.is_some()
    }
    /// HFP codec (`cvsd` = narrowband 8 kHz, `msbc` = wideband 16 kHz, `lc3_swb` = 32 kHz).
    pub fn codec(&self) -> Option<&str> {
        self.rx
            .as_ref()
            .and_then(StreamInfo::codec)
            .or_else(|| self.tx.as_ref().and_then(StreamInfo::codec))
    }
}

fn is_gateway_stream(s: &StreamInfo, addr: BtAddress, class: &str) -> bool {
    s.address() == Some(addr)
        && s.profile_kind() == Some(ProfileKind::PhoneGateway)
        && s.props
            .get(PROP_MEDIA_CLASS)
            .is_none_or(|c| c.starts_with(class))
}

pub fn phone_voice(snap: &ServerSnapshot, addr: BtAddress) -> PhoneVoice {
    let card = snap
        .cards
        .iter()
        .find(|c| c.address() == Some(addr))
        .cloned();
    let rx = snap
        .sink_inputs
        .iter()
        .find(|s| is_gateway_stream(s, addr, "Stream/Output"))
        .cloned();
    let tx = snap
        .source_outputs
        .iter()
        .find(|s| is_gateway_stream(s, addr, "Stream/Input"))
        .cloned();
    let card_index = card.as_ref().map(|c| c.index);
    let legacy_devices = card_index.is_some()
        && snap.sources.iter().any(|s| {
            s.card == card_index
                && s.props
                    .get(crate::audio::model::PROP_BT_PROFILE)
                    .is_some_and(|p| ProfileKind::classify(p) == ProfileKind::PhoneGateway)
        });
    PhoneVoice {
        address: addr,
        card,
        rx,
        tx,
        legacy_devices,
    }
}

/// Which end of the call a local device serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Output,
    Input,
}

/// One local speaker/headphone (output) or microphone (input).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub is_default: bool,
    /// Set if this device is itself Bluetooth (a headset; never the phone).
    pub bluetooth: bool,
    /// `device.form_factor` (`headset`, `headphone`, `speaker`, `internal`, ...), if reported.
    pub form_factor: Option<String>,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceList {
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    pub default_output: Option<String>,
    pub default_input: Option<String>,
}

/// Every local output and input that can carry call audio: monitors and the phones' own nodes
/// are left out.
pub fn devices(snap: &ServerSnapshot) -> DeviceList {
    let phone_cards: Vec<u32> = snap
        .cards
        .iter()
        .filter(|c| {
            c.profiles
                .iter()
                .any(|p| ProfileKind::classify(&p.name) == ProfileKind::PhoneGateway)
        })
        .map(|c| c.index)
        .collect();
    let on_phone = |card: Option<u32>| card.is_some_and(|c| phone_cards.contains(&c));
    let outputs = snap
        .sinks
        .iter()
        .filter(|s| !on_phone(s.card))
        .map(|s| Device {
            name: s.name.clone(),
            description: s.description.clone(),
            is_default: snap.default_sink.as_deref() == Some(s.name.as_str()),
            bluetooth: props_address(&s.props).is_some(),
            form_factor: s.props.get(PROP_FORM_FACTOR).cloned(),
            muted: s.mute,
        })
        .collect();
    let inputs = snap
        .sources
        .iter()
        .filter(|s| s.monitor_of_sink.is_none() && !on_phone(s.card))
        .map(|s| Device {
            name: s.name.clone(),
            description: s.description.clone(),
            is_default: snap.default_source.as_deref() == Some(s.name.as_str()),
            bluetooth: props_address(&s.props).is_some(),
            form_factor: s.props.get(PROP_FORM_FACTOR).cloned(),
            muted: s.mute,
        })
        .collect();
    DeviceList {
        outputs,
        inputs,
        default_output: snap.default_sink.clone(),
        default_input: snap.default_source.clone(),
    }
}

/// The device a call should use for one role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolved {
    /// The device the user picked.
    Chosen { name: String },
    /// No pick: the system default.
    Default { name: String },
    /// The picked device is not connected; the system default is used instead.
    Fallback { wanted: String, name: String },
}

impl Resolved {
    pub fn name(&self) -> &str {
        match self {
            Self::Chosen { name } | Self::Default { name } | Self::Fallback { name, .. } => name,
        }
    }
}

/// Resolve the user's pick (`None` = system default) against what exists now.
pub fn resolve(snap: &ServerSnapshot, role: Role, choice: Option<&str>) -> Option<Resolved> {
    let list = devices(snap);
    let (pool, default) = match role {
        Role::Output => (&list.outputs, list.default_output.as_deref()),
        Role::Input => (&list.inputs, list.default_input.as_deref()),
    };
    let exists = |n: &str| pool.iter().any(|d| d.name == n);
    let default = default.filter(|d| exists(d)).map(str::to_owned);
    match choice {
        Some(w) if exists(w) => Some(Resolved::Chosen { name: w.to_owned() }),
        Some(w) => default.map(|name| Resolved::Fallback {
            wanted: w.to_owned(),
            name,
        }),
        None => default.map(|name| Resolved::Default { name }),
    }
}

/// Display name of a device, for user-facing lines.
pub fn describe(snap: &ServerSnapshot, role: Role, name: &str) -> String {
    let d = match role {
        Role::Output => snap
            .sinks
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.description.clone()),
        Role::Input => snap
            .sources
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.description.clone()),
    };
    d.filter(|d| !d.is_empty())
        .unwrap_or_else(|| name.to_owned())
}

/// Something that keeps the caller from being heard, or the user from hearing them. Each has a
/// one-line explanation and a concrete fix; none is ever swallowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum FlowProblem {
    /// The call is live and audio was asked for here, but the phone opened no voice link.
    NoVoiceLink,
    /// The phone's voice stream is not connected to any speaker.
    RxUnlinked,
    /// The uplink stream is not connected to any microphone.
    TxUnlinked,
    /// The voice stream is connected but nothing but digital silence arrived.
    RxSilent,
    /// The microphone used for the call is muted, and not by GeekBridge.
    MicMuted { device: String },
    /// The microphone delivers digital silence.
    MicSilent { device: String },
    /// No output device at all.
    NoOutput,
    /// No input device at all.
    NoInput,
    /// The picked device is not connected; the default is used.
    PickedMissing { role: Role, wanted: String },
    /// PulseAudio instead of PipeWire: the server's own policy routes the call.
    NotPipeWire,
}

impl FlowProblem {
    pub fn message(&self) -> String {
        match self {
            Self::NoVoiceLink => "The call audio has not come to this computer: the phone did not open the Bluetooth voice link. Fix: in the phone's call screen tap the audio button and pick this computer (Bluetooth); on Samsung also check Settings > Connections > Bluetooth > this computer (gear icon) > Calls is on. Then run: geekbridge-ctl call audio".into(),
            Self::RxUnlinked => "The caller's voice is not connected to any speaker. Fix: geekbridge-ctl call audio-devices (pick an output), and check WirePlumber runs: systemctl --user status wireplumber".into(),
            Self::TxUnlinked => "Your microphone is not connected to the call. Fix: geekbridge-ctl call audio-devices (pick an input), and check WirePlumber runs: systemctl --user status wireplumber".into(),
            Self::RxSilent => "No sound is arriving from the phone over Bluetooth (digital silence). If you cannot hear the caller, the adapter is not carrying voice (SCO): unplug and replug the dongle, then check journalctl --user -u pipewire -u wireplumber | grep -i -E 'sco|hfp'. Some USB adapters only work with narrowband: set bluez5.enable-msbc = false (see docs/reports/call-audio-pc-headset.md).".into(),
            Self::MicMuted { device } => format!("Your microphone ({device}) is muted, so the caller hears nothing. Fix: geekbridge-ctl call mute off (or unmute it in the system mixer or on the headset)."),
            Self::MicSilent { device } => format!("Your microphone ({device}) sends only silence. Check the headset's own mute switch, that it is switched on, or pick another input: geekbridge-ctl call audio-devices --input NAME"),
            Self::NoOutput => "This computer has no speaker or headphones to play the call on. Connect one, then: geekbridge-ctl call audio".into(),
            Self::NoInput => "This computer has no microphone for the call; the caller will not hear you. Connect one or pick it: geekbridge-ctl call audio-devices --input NAME".into(),
            Self::PickedMissing { role, wanted } => format!(
                "The {} you picked for calls ({wanted}) is not connected; using the system default instead.",
                match role {
                    Role::Output => "output",
                    Role::Input => "microphone",
                }
            ),
            Self::NotPipeWire => "The sound server is not PipeWire: the call goes to the default speakers and microphone and GeekBridge cannot route or mute it.".into(),
        }
    }
}

/// Is the uplink at risk of echo (open speakers with a nearby microphone)? Headsets are not.
pub fn echo_risk(output: &Device) -> bool {
    !matches!(
        output.form_factor.as_deref(),
        Some("headset" | "headphone" | "hands-free" | "handset")
    ) && !output.bluetooth
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::audio::model::{CardProfile, Props, ServerIdentity, SinkInfo, SourceInfo};

    pub const PHONE: &str = "AA:BB:CC:00:11:22";
    pub const HEADSET_OUT: &str =
        "alsa_output.usb-HP__Inc_HyperX_Cloud_II_Core_Wireless-00.analog-stereo";
    pub const HEADSET_IN: &str =
        "alsa_input.usb-HP__Inc_HyperX_Cloud_II_Core_Wireless-00.mono-fallback";
    pub const HDMI_OUT: &str = "alsa_output.pci-0000_03_00.1.hdmi-stereo";

    pub fn props(pairs: &[(&str, &str)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    pub fn phone() -> BtAddress {
        BtAddress::parse(PHONE).unwrap()
    }

    /// The host as it looks on the owner's machine plus a connected phone; `call` adds the two
    /// voice streams PipeWire creates while the SCO link is open.
    pub fn snapshot(card_profile: &str, call: bool) -> ServerSnapshot {
        let card = CardInfo {
            index: 7,
            name: "bluez_card.AA_BB_CC_00_11_22".into(),
            props: props(&[("api.bluez5.address", PHONE), ("device.api", "bluez5")]),
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
            active_profile: Some(card_profile.into()),
        };
        let mut s = ServerSnapshot {
            identity: ServerIdentity {
                server_name: "PulseAudio (on PipeWire 1.6.9)".into(),
                server_version: "15.0.0".into(),
                host_name: "omarchy".into(),
                cookie: 1,
            },
            default_sink: Some(HEADSET_OUT.into()),
            default_source: Some(HEADSET_IN.into()),
            cards: vec![card],
            sinks: vec![
                SinkInfo {
                    index: 65,
                    name: HEADSET_OUT.into(),
                    description: "HyperX Cloud II Core Wireless Analog Stereo".into(),
                    card: Some(74),
                    mute: false,
                    props: props(&[("device.form_factor", "headset")]),
                },
                SinkInfo {
                    index: 79,
                    name: HDMI_OUT.into(),
                    description: "HDMI".into(),
                    card: Some(69),
                    mute: false,
                    props: Props::new(),
                },
            ],
            sources: vec![
                SourceInfo {
                    index: 68,
                    name: HEADSET_IN.into(),
                    description: "HyperX Cloud II Core Wireless Mono".into(),
                    card: Some(74),
                    mute: false,
                    monitor_of_sink: None,
                    props: props(&[("device.form_factor", "headset")]),
                },
                SourceInfo {
                    index: 66,
                    name: format!("{HEADSET_OUT}.monitor"),
                    description: "Monitor".into(),
                    card: Some(74),
                    mute: false,
                    monitor_of_sink: Some(65),
                    props: Props::new(),
                },
            ],
            sink_inputs: vec![],
            source_outputs: vec![],
        };
        if call && card_profile == "audio-gateway" {
            add_call_streams(&mut s, Some(79), Some(68));
        }
        s
    }

    pub fn add_call_streams(s: &mut ServerSnapshot, rx_on: Option<u32>, tx_on: Option<u32>) {
        let bt = |class: &str| {
            props(&[
                ("api.bluez5.address", PHONE),
                ("api.bluez5.profile", "headset-audio-gateway"),
                ("api.bluez5.codec", "msbc"),
                ("device.api", "bluez5"),
                ("media.class", class),
            ])
        };
        s.sink_inputs.push(StreamInfo {
            index: 201,
            name: "bluez_input.AA:BB:CC:00:11:22.0".into(),
            device: rx_on,
            corked: false,
            mute: false,
            props: bt("Stream/Output/Audio"),
        });
        s.source_outputs.push(StreamInfo {
            index: 202,
            name: "bluez_output.AA:BB:CC:00:11:22.1".into(),
            device: tx_on,
            corked: false,
            mute: false,
            props: bt("Stream/Input/Audio"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_phones_voice_is_found_as_two_streams_by_address_and_profile() {
        let s = snapshot("audio-gateway", true);
        let v = phone_voice(&s, phone());
        assert_eq!(v.rx.as_ref().unwrap().index, 201);
        assert_eq!(v.tx.as_ref().unwrap().index, 202);
        assert_eq!(v.codec(), Some("msbc"));
        assert!(!v.legacy_devices);
        // Another phone's streams are not ours.
        let other = BtAddress::parse("11:22:33:44:55:66").unwrap();
        assert!(!phone_voice(&s, other).streams_present());
        // Phone media (A2DP) from the same phone is not a call.
        let mut m = snapshot("audio-gateway", false);
        add_call_streams(&mut m, Some(79), Some(68));
        for st in m.sink_inputs.iter_mut().chain(m.source_outputs.iter_mut()) {
            st.props
                .insert("api.bluez5.profile".into(), "a2dp-source".into());
        }
        assert!(!phone_voice(&m, phone()).streams_present());
    }

    #[test]
    fn devices_leave_out_monitors_and_mark_the_defaults() {
        let l = devices(&snapshot("audio-gateway", false));
        assert_eq!(l.outputs.len(), 2);
        assert_eq!(l.inputs.len(), 1, "the monitor is not a microphone");
        assert!(
            l.outputs
                .iter()
                .any(|d| d.name == HEADSET_OUT && d.is_default)
        );
        assert!(!echo_risk(&l.outputs[0]), "a headset has no echo path");
        assert!(echo_risk(&l.outputs[1]));
    }

    #[test]
    fn a_pick_wins_a_missing_pick_falls_back_and_no_pick_is_the_default() {
        let s = snapshot("audio-gateway", false);
        assert_eq!(
            resolve(&s, Role::Output, Some(HDMI_OUT)),
            Some(Resolved::Chosen {
                name: HDMI_OUT.into()
            })
        );
        assert_eq!(
            resolve(&s, Role::Output, Some("gone")),
            Some(Resolved::Fallback {
                wanted: "gone".into(),
                name: HEADSET_OUT.into()
            })
        );
        assert_eq!(
            resolve(&s, Role::Input, None),
            Some(Resolved::Default {
                name: HEADSET_IN.into()
            })
        );
        // A monitor can never be picked as the microphone.
        let monitor = format!("{HEADSET_OUT}.monitor");
        assert!(matches!(
            resolve(&s, Role::Input, Some(&monitor)),
            Some(Resolved::Fallback { .. })
        ));
        let mut none = s.clone();
        none.default_source = None;
        assert_eq!(resolve(&none, Role::Input, None), None);
    }

    #[test]
    fn every_problem_explains_itself_with_a_fix() {
        for p in [
            FlowProblem::NoVoiceLink,
            FlowProblem::RxUnlinked,
            FlowProblem::TxUnlinked,
            FlowProblem::RxSilent,
            FlowProblem::MicMuted { device: "m".into() },
            FlowProblem::MicSilent { device: "m".into() },
            FlowProblem::NoOutput,
            FlowProblem::NoInput,
            FlowProblem::PickedMissing {
                role: Role::Input,
                wanted: "x".into(),
            },
            FlowProblem::NotPipeWire,
        ] {
            let m = p.message();
            assert!(m.len() > 40, "{m}");
        }
        assert!(FlowProblem::NoVoiceLink.message().contains("Samsung"));
    }
}
