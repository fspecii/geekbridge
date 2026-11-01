//! Capability reporting. Events, control, SCO and routing are separate planes and every operation
//! has its own entry; nothing is ever reported `Verified` by this crate (that needs a recorded
//! hardware exercise, see `CallCapabilities::record_verified` in `bridge-core`).
use crate::model::{AgFeatures, BluetoothAuth};
use crate::plan::OpKind;
use crate::route::AudioProof;
use bridge_core::calls::{
    CallBackend, CallCapabilities, CallFeature, HfpEvidence, HfpHost, Support, UnsupportedReason,
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Planes {
    /// Seeing call state (indicators, caller id).
    pub events: Support,
    /// Acting on calls (`IOBluetoothHandsFreeDevice` call-control methods).
    pub control: Support,
    /// Opening/closing the SCO link (`connectSCO`, `transferAudioTo*`).
    pub sco: Support,
    /// Listing CoreAudio endpoints and moving defaults to caller-named ones.
    pub routing: Support,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MacCapabilities {
    /// Feed this to the daemon's `CallState::check_command`.
    pub core: CallCapabilities,
    pub planes: Planes,
    pub ops: BTreeMap<OpKind, Support>,
    pub proof: AudioProof,
    pub ag_features: AgFeatures,
    pub authorization: BluetoothAuth,
}

fn unsupported(reason: UnsupportedReason) -> Support {
    Support::Unsupported { reason }
}

/// Capabilities for a build without a macOS Bluetooth stack: everything explicitly unsupported.
pub fn unsupported_host() -> MacCapabilities {
    let core = CallCapabilities::unsupported(
        CallBackend::BluetoothHfp {
            host: HfpHost::MacOs,
        },
        UnsupportedReason::NoPlatformApi,
    );
    let no = unsupported(UnsupportedReason::NoPlatformApi);
    MacCapabilities {
        core,
        planes: Planes {
            events: no,
            control: no,
            sco: no,
            routing: no,
        },
        ops: OpKind::ALL.into_iter().map(|k| (k, no)).collect(),
        proof: AudioProof::None,
        ag_features: AgFeatures(0),
        authorization: BluetoothAuth::Unknown,
    }
}

/// Everything known before any phone is connected.
pub fn unprobed(authorization: BluetoothAuth) -> MacCapabilities {
    derive(
        &HfpEvidence::default(),
        AgFeatures(0),
        Support::Unknown,
        AudioProof::None,
        authorization,
    )
}

pub fn derive(
    evidence: &HfpEvidence,
    ag: AgFeatures,
    routing: Support,
    proof: AudioProof,
    authorization: BluetoothAuth,
) -> MacCapabilities {
    let mut core = CallCapabilities::from_hfp_evidence(HfpHost::MacOs, evidence);
    // Swap/hold need the gateway's three-way calling; distinguish "not reported" from "absent".
    if ag.reported() && !ag.has(AgFeatures::THREE_WAY_CALLING) {
        core.set(
            CallFeature::SwapCalls,
            unsupported(UnsupportedReason::RemoteDoesNotAdvertise),
        );
    }
    // `setInputMuted` adjusts the HFP microphone gain/mute state; it is not a call-mute that
    // this adapter can honour for audio it does not own.
    core.set(
        CallFeature::MuteMicrophone,
        unsupported(UnsupportedReason::NoPlatformApi),
    );
    let swap = core.support(CallFeature::SwapCalls);
    let ops: BTreeMap<OpKind, Support> = OpKind::ALL
        .into_iter()
        .map(|k| {
            let s = match k {
                OpKind::Dial => core.support(CallFeature::Dial),
                OpKind::Answer => core.support(CallFeature::Answer),
                OpKind::Hangup => core.support(CallFeature::Hangup),
                OpKind::SendTones => core.support(CallFeature::SendTones),
                OpKind::Hold | OpKind::Resume | OpKind::Swap => swap,
                OpKind::ConnectAudio | OpKind::DisconnectAudio => {
                    core.support(CallFeature::AudioLinkControl)
                }
            };
            (k, s)
        })
        .collect();
    let planes = Planes {
        events: core.support(CallFeature::CallStateEvents),
        control: core.support(CallFeature::Dial),
        sco: core.support(CallFeature::AudioLinkControl),
        routing,
    };
    MacCapabilities {
        core,
        planes,
        ops,
        proof,
        ag_features: ag,
        authorization,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked() -> HfpEvidence {
        HfpEvidence {
            service_level_connection: true,
            call_indicators: true,
            caller_id_notification: false,
            three_way_calling: true,
            host_call_control_api: true,
            host_audio_control_api: true,
            audio_codec: None,
        }
    }

    #[test]
    fn host_without_macos_is_explicitly_unsupported_everywhere() {
        let c = unsupported_host();
        assert!(
            c.ops
                .values()
                .all(|s| matches!(s, Support::Unsupported { .. }))
        );
        assert!(matches!(c.planes.sco, Support::Unsupported { .. }));
        for f in CallFeature::ALL {
            assert!(
                matches!(c.core.support(f), Support::Unsupported { .. }),
                "{f:?}"
            );
        }
    }

    #[test]
    fn nothing_is_probed_before_a_link_exists() {
        let c = unprobed(BluetoothAuth::Allowed);
        assert!(c.ops.values().all(|s| *s == Support::Unknown));
        assert_eq!(c.planes.routing, Support::Unknown);
    }

    #[test]
    fn linked_planes_are_declared_never_verified() {
        let c = derive(
            &linked(),
            AgFeatures(AgFeatures::THREE_WAY_CALLING),
            Support::Declared,
            AudioProof::None,
            BluetoothAuth::Allowed,
        );
        assert_eq!(c.planes.events, Support::Declared);
        assert_eq!(c.planes.control, Support::Declared);
        assert_eq!(c.planes.sco, Support::Declared);
        assert!(c.ops.values().all(|s| *s != Support::Verified));
        assert_eq!(c.ops[&OpKind::Hold], Support::Declared);
    }

    #[test]
    fn absent_three_way_makes_hold_unsupported_but_unreported_stays_unknown() {
        let mut ev = linked();
        ev.three_way_calling = false;
        let absent = derive(
            &ev,
            AgFeatures(AgFeatures::EC_NR),
            Support::Unknown,
            AudioProof::None,
            BluetoothAuth::Allowed,
        );
        assert!(matches!(
            absent.ops[&OpKind::Hold],
            Support::Unsupported { .. }
        ));
        assert_eq!(absent.ops[&OpKind::Dial], Support::Declared);
        let unreported = derive(
            &ev,
            AgFeatures(0),
            Support::Unknown,
            AudioProof::None,
            BluetoothAuth::Allowed,
        );
        assert_eq!(unreported.ops[&OpKind::Hold], Support::Unknown);
    }

    #[test]
    fn core_capabilities_round_trip_through_json_for_the_daemon() {
        let c = derive(
            &linked(),
            AgFeatures(AgFeatures::THREE_WAY_CALLING),
            Support::Declared,
            AudioProof::None,
            BluetoothAuth::Allowed,
        );
        let json = serde_json::to_value(&c).unwrap();
        let back: CallCapabilities = serde_json::from_value(json["core"].clone()).unwrap();
        assert_eq!(back, c.core);
    }

    #[test]
    fn microphone_mute_and_ringer_mute_are_unsupported() {
        let c = derive(
            &linked(),
            AgFeatures(1),
            Support::Unknown,
            AudioProof::None,
            BluetoothAuth::Allowed,
        );
        assert!(matches!(
            c.core.support(CallFeature::MuteMicrophone),
            Support::Unsupported { .. }
        ));
        assert!(matches!(
            c.core.support(CallFeature::MuteRinger),
            Support::Unsupported { .. }
        ));
    }
}
