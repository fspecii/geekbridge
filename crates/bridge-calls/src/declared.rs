// SPDX-License-Identifier: GPL-3.0-or-later
//! What the PipeWire telephony D-Bus API *declares*, and what that licenses.
//!
//! Sources, in decreasing order of weight:
//! 1. live bus introspection of the object in front of us (authoritative for this process);
//! 2. the XML compiled into the installed `libspa-bluez5.so` (pipewire 1.6.9 on the development
//!    machine, extracted with `strings`; identical interface set to master);
//! 3. PipeWire `spa/plugins/bluez5/telephony.c` and `backend-native.c` (master, read 2026-11-01).
//!
//! Everything derived here is at most [`Support::Declared`]. `Verified` needs a recorded hardware
//! exercise and is never produced by this crate.

use std::collections::BTreeMap;

use bridge_core::calls::{
    CallBackend, CallCapabilities, CallFeature, HfpHost, Support, UnsupportedReason,
};
use serde::Serialize;

use crate::model::{AudioCodecEvidence, TransportSnapshot, TransportState};

pub const SERVICE: &str = "org.pipewire.Telephony";
pub const MANAGER_PATH: &str = "/org/pipewire/Telephony";
pub const IFACE_AG: &str = "org.pipewire.Telephony.AudioGateway1";
pub const IFACE_TRANSPORT: &str = "org.pipewire.Telephony.AudioGatewayTransport1";
pub const IFACE_CALL: &str = "org.pipewire.Telephony.Call1";
pub const IFACE_OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
pub const ERROR_PREFIX: &str = "org.pipewire.Telephony.Error.";

/// Longest dial string `telephony.c::validate_phone_number` accepts.
pub const PIPEWIRE_MAX_DIAL: usize = 80;
/// This crate's own, stricter DTMF bound (matches `bridge_core::calls::MAX_DIAL_LEN`).
pub const MAX_TONES: usize = bridge_core::calls::MAX_DIAL_LEN;

/// One method of the API with the exact input signature this crate sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MethodSpec {
    pub interface: &'static str,
    pub member: &'static str,
    /// D-Bus input signature (concatenated `in` argument types).
    pub in_signature: &'static str,
}

macro_rules! method {
    ($name:ident, $iface:expr, $member:literal, $sig:literal) => {
        pub const $name: MethodSpec = MethodSpec {
            interface: $iface,
            member: $member,
            in_signature: $sig,
        };
    };
}
method!(DIAL, IFACE_AG, "Dial", "s");
method!(SWAP_CALLS, IFACE_AG, "SwapCalls", "");
method!(RELEASE_AND_ANSWER, IFACE_AG, "ReleaseAndAnswer", "");
method!(RELEASE_AND_SWAP, IFACE_AG, "ReleaseAndSwap", "");
method!(HOLD_AND_ANSWER, IFACE_AG, "HoldAndAnswer", "");
method!(HANGUP_ALL, IFACE_AG, "HangupAll", "");
method!(CREATE_MULTIPARTY, IFACE_AG, "CreateMultiparty", "");
method!(SEND_TONES, IFACE_AG, "SendTones", "s");
method!(ACTIVATE, IFACE_TRANSPORT, "Activate", "");
method!(ANSWER, IFACE_CALL, "Answer", "");
method!(HANGUP, IFACE_CALL, "Hangup", "");

/// Methods and properties (name to signature) of one interface as introspected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DeclaredInterface {
    pub methods: BTreeMap<String, String>,
    pub properties: BTreeMap<String, String>,
}

/// Whether one method is usable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MethodDeclaration {
    /// Present with the signature this crate sends.
    Declared,
    /// The interface or the method is absent (older/newer PipeWire, or not introspected).
    Missing,
    /// Present, but with a different signature. Never called.
    SignatureMismatch { found: String },
}

impl DeclaredInterface {
    pub fn declaration(&self, spec: &MethodSpec) -> MethodDeclaration {
        match self.methods.get(spec.member) {
            None => MethodDeclaration::Missing,
            Some(sig) if sig == spec.in_signature => MethodDeclaration::Declared,
            Some(sig) => MethodDeclaration::SignatureMismatch { found: sig.clone() },
        }
    }
    fn has_property(&self, name: &str, signature: &str) -> bool {
        self.properties.get(name).is_some_and(|s| s == signature)
    }
}

/// The introspected API of one phone's objects.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DeclaredApi {
    /// `AudioGateway1` from the phone object. `None` = interface not declared.
    pub audio_gateway: Option<DeclaredInterface>,
    /// `AudioGatewayTransport1` from the phone object.
    pub transport: Option<DeclaredInterface>,
    /// `Call1`, introspected from a call object. `None` while no call has existed since the
    /// service started: the interface only exists on call objects.
    pub call: Option<DeclaredInterface>,
    /// The phone object implements `org.freedesktop.DBus.ObjectManager` (call add/remove).
    pub object_manager: bool,
}

impl DeclaredApi {
    /// The declaration of `spec` on whichever interface it belongs to.
    pub fn declaration(&self, spec: &MethodSpec) -> MethodDeclaration {
        let iface = match spec.interface {
            IFACE_AG => &self.audio_gateway,
            IFACE_TRANSPORT => &self.transport,
            IFACE_CALL => &self.call,
            _ => &None,
        };
        iface
            .as_ref()
            .map_or(MethodDeclaration::Missing, |i| i.declaration(spec))
    }
}

/// Why a feature has its [`Support`] value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FeatureEvidence {
    pub feature: CallFeature,
    pub support: Support,
    pub basis: String,
}

/// Derive declared capabilities for one phone. Pure; never yields [`Support::Verified`].
///
/// Presence of the phone object is itself evidence (PipeWire registers it only after the HFP
/// service-level connection is configured, `backend-native.c`, master): the *source* says so, it
/// was not observed on hardware here.
pub fn derive_capabilities(
    api: &DeclaredApi,
    transport: Option<&TransportSnapshot>,
) -> (CallCapabilities, Vec<FeatureEvidence>) {
    let mut caps = CallCapabilities::unprobed(CallBackend::BluetoothHfp {
        host: HfpHost::LinuxPipeWire,
    });
    let mut evidence = Vec::new();
    let mut put = |feature: CallFeature, support: Support, basis: String| {
        caps.set(feature, support);
        evidence.push(FeatureEvidence {
            feature,
            support,
            basis,
        });
    };
    let method = |spec: &MethodSpec| -> (Support, String) {
        match api.declaration(spec) {
            MethodDeclaration::Declared => (
                Support::Declared,
                format!(
                    "{}.{}({}) declared by bus introspection; never exercised on hardware",
                    spec.interface, spec.member, spec.in_signature
                ),
            ),
            MethodDeclaration::Missing => (
                Support::Unknown,
                format!(
                    "{}.{} not present in introspection (older/newer service, or no such object yet)",
                    spec.interface, spec.member
                ),
            ),
            MethodDeclaration::SignatureMismatch { found } => (
                Support::Unknown,
                format!(
                    "{}.{} declared with signature ({found}), this crate sends ({}); not called",
                    spec.interface, spec.member, spec.in_signature
                ),
            ),
        }
    };

    // Observation.
    let state_prop = api.call.as_ref().map(|c| c.has_property("State", "s"));
    let (events, basis) = match (api.object_manager, state_prop) {
        (true, Some(true) | None) => (
            Support::Declared,
            "phone object exports ObjectManager (call add/remove) and Call1.State is a string property where introspectable; never observed on a real call".to_string(),
        ),
        (true, Some(false)) => (
            Support::Unknown,
            "Call1.State missing or not a string in introspection".to_string(),
        ),
        (false, _) => (
            Support::Unknown,
            "phone object does not export ObjectManager; call lifecycle cannot be followed".to_string(),
        ),
    };
    put(CallFeature::CallStateEvents, events, basis);
    let (id_support, id_basis) = match api.call.as_ref().map(|c| c.has_property("LineIdentification", "s")) {
        Some(true) => (Support::Declared, "Call1.LineIdentification (s) declared by bus introspection; whether the phone supplies +CLIP data is untested".to_string()),
        Some(false) => (Support::Unknown, "Call1.LineIdentification missing or not a string".to_string()),
        None => (Support::Unknown, "no call object exists to introspect Call1".to_string()),
    };
    put(CallFeature::CallerIdentity, id_support, id_basis);

    // Control.
    for (feature, spec) in [
        (CallFeature::Answer, ANSWER),
        (CallFeature::Hangup, HANGUP),
        (CallFeature::Dial, DIAL),
        (CallFeature::SendTones, SEND_TONES),
    ] {
        let (support, mut basis) = method(&spec);
        if spec.interface == IFACE_CALL && api.call.is_none() {
            basis = "no call object exists to introspect Call1; the method is in PipeWire's source and the installed library XML but not observable on the bus yet".to_string();
        }
        put(feature, support, basis);
    }
    let (support, mut basis) = method(&SWAP_CALLS);
    if support == Support::Declared {
        basis.push_str("; whether the phone supports AT+CHLD 3-way handling is not visible over D-Bus (the service replies NotSupported otherwise)");
    }
    put(CallFeature::SwapCalls, support, basis);
    put(
        CallFeature::MuteRinger,
        Support::Unsupported {
            reason: UnsupportedReason::NotInProtocol,
        },
        "HFP has no ringer-mute procedure in this API".to_string(),
    );
    put(
        CallFeature::MuteMicrophone,
        Support::Unsupported {
            reason: UnsupportedReason::NotImplemented,
        },
        "microphone mute is a host audio-graph operation, not part of this telephony API"
            .to_string(),
    );

    // Audio. The voice path is only claimed when the SCO/eSCO transport reports active with a
    // codec; a declared Activate() alone says nothing about audio flowing.
    let audio_up = transport.is_some_and(|t| {
        t.state == TransportState::Active && !matches!(t.codec_evidence, AudioCodecEvidence::None)
    });
    let (audio, basis) = if audio_up {
        (
            Support::Declared,
            "AudioGatewayTransport1 reports active with a codec; audio samples were not inspected"
                .to_string(),
        )
    } else {
        (
            Support::Unknown,
            "no active voice transport reported".to_string(),
        )
    };
    put(CallFeature::AudioDownlink, audio, basis.clone());
    put(CallFeature::AudioUplink, audio, basis);
    let (support, mut basis) = method(&ACTIVATE);
    if support == Support::Declared {
        basis.push_str("; there is no matching deactivate method, so DisconnectAudio is refused");
    }
    put(CallFeature::AudioLinkControl, support, basis);

    (caps, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(methods: &[(&str, &str)], props: &[(&str, &str)]) -> DeclaredInterface {
        DeclaredInterface {
            methods: methods
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            properties: props
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }
    fn full_api(with_call: bool) -> DeclaredApi {
        DeclaredApi {
            audio_gateway: Some(iface(
                &[
                    ("Dial", "s"),
                    ("SwapCalls", ""),
                    ("ReleaseAndAnswer", ""),
                    ("ReleaseAndSwap", ""),
                    ("HoldAndAnswer", ""),
                    ("HangupAll", ""),
                    ("CreateMultiparty", ""),
                    ("SendTones", "s"),
                ],
                &[("Address", "s")],
            )),
            transport: Some(iface(
                &[("Activate", "")],
                &[("State", "s"), ("Codec", "y")],
            )),
            call: with_call.then(|| {
                iface(
                    &[("Answer", ""), ("Hangup", "")],
                    &[("State", "s"), ("LineIdentification", "s")],
                )
            }),
            object_manager: true,
        }
    }

    fn get(caps: &CallCapabilities, f: CallFeature) -> Support {
        caps.support(f)
    }

    #[test]
    fn declared_never_verified_and_answer_needs_a_call_object() {
        let (caps, evidence) = derive_capabilities(&full_api(false), None);
        for f in CallFeature::ALL {
            assert_ne!(get(&caps, f), Support::Verified, "{f:?}");
        }
        assert_eq!(get(&caps, CallFeature::Dial), Support::Declared);
        assert_eq!(get(&caps, CallFeature::SendTones), Support::Declared);
        assert_eq!(get(&caps, CallFeature::AudioLinkControl), Support::Declared);
        // Call1 only exists on call objects: Answer/Hangup stay Unknown until one is seen.
        assert_eq!(get(&caps, CallFeature::Answer), Support::Unknown);
        assert_eq!(get(&caps, CallFeature::Hangup), Support::Unknown);
        assert_eq!(get(&caps, CallFeature::CallerIdentity), Support::Unknown);
        assert_eq!(get(&caps, CallFeature::AudioDownlink), Support::Unknown);
        assert!(evidence.iter().all(|e| !e.basis.is_empty()));

        let (caps, _) = derive_capabilities(&full_api(true), None);
        assert_eq!(get(&caps, CallFeature::Answer), Support::Declared);
        assert_eq!(get(&caps, CallFeature::CallerIdentity), Support::Declared);
    }

    #[test]
    fn version_differences_degrade_to_unknown_not_declared() {
        let mut api = full_api(true);
        // A future/older service: Dial takes (ss), no Activate, no SendTones.
        api.audio_gateway
            .as_mut()
            .unwrap()
            .methods
            .insert("Dial".into(), "ss".into());
        api.audio_gateway
            .as_mut()
            .unwrap()
            .methods
            .remove("SendTones");
        api.transport = None;
        let (caps, evidence) = derive_capabilities(&api, None);
        assert_eq!(get(&caps, CallFeature::Dial), Support::Unknown);
        assert_eq!(get(&caps, CallFeature::SendTones), Support::Unknown);
        assert_eq!(get(&caps, CallFeature::AudioLinkControl), Support::Unknown);
        let dial = evidence
            .iter()
            .find(|e| e.feature == CallFeature::Dial)
            .unwrap();
        assert!(dial.basis.contains("(ss)"), "{}", dial.basis);
        // Untouched methods remain usable.
        assert_eq!(get(&caps, CallFeature::SwapCalls), Support::Declared);
    }

    #[test]
    fn no_object_manager_means_no_call_events() {
        let mut api = full_api(false);
        api.object_manager = false;
        let (caps, _) = derive_capabilities(&api, None);
        assert_eq!(get(&caps, CallFeature::CallStateEvents), Support::Unknown);
    }

    #[test]
    fn audio_plane_requires_active_transport_with_codec() {
        let mut t = TransportSnapshot {
            state: TransportState::Idle,
            raw_state: "idle".into(),
            codec_id: 2,
            codec_evidence: AudioCodecEvidence::Known(bridge_core::calls::AudioCodec::Msbc),
            reject_sco: Some(false),
        };
        let (caps, _) = derive_capabilities(&full_api(false), Some(&t));
        assert_eq!(get(&caps, CallFeature::AudioUplink), Support::Unknown);
        t.state = TransportState::Active;
        let (caps, _) = derive_capabilities(&full_api(false), Some(&t));
        assert_eq!(get(&caps, CallFeature::AudioUplink), Support::Declared);
        assert_eq!(get(&caps, CallFeature::AudioDownlink), Support::Declared);
        t.codec_evidence = AudioCodecEvidence::None;
        let (caps, _) = derive_capabilities(&full_api(false), Some(&t));
        assert_eq!(get(&caps, CallFeature::AudioUplink), Support::Unknown);
    }

    #[test]
    fn ringer_and_microphone_are_explicitly_unsupported() {
        let (caps, _) = derive_capabilities(&full_api(true), None);
        assert!(matches!(
            get(&caps, CallFeature::MuteRinger),
            Support::Unsupported {
                reason: UnsupportedReason::NotInProtocol
            }
        ));
        assert!(matches!(
            get(&caps, CallFeature::MuteMicrophone),
            Support::Unsupported { .. }
        ));
    }
}
