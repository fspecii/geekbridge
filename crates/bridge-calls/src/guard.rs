// SPDX-License-Identifier: GPL-3.0-or-later
//! Pre-dispatch checks, pure over a *fresh* [`PhoneSnapshot`].
//!
//! A control call is only sent after the target phone and call have been re-read from the bus and
//! compared with the handle the caller holds. This turns "answer call3" issued against a stale
//! view into [`ControlError::StaleCall`] rather than answering whatever call3 has become. The
//! check cannot close the window between the re-read and the method call (the phone's state can
//! still change in between); the service's own state checks (`InvalidState`) are the backstop,
//! and their errors are surfaced as [`ControlError::Remote`].
//!
//! State preconditions mirror `backend-native.c` (master) so that doomed requests are refused
//! locally with a precise reason instead of becoming AT commands.

use bridge_core::calls::CallPhase;

use crate::declared::{self, MethodDeclaration, MethodSpec, PIPEWIRE_MAX_DIAL};
use crate::error::ControlError;
use crate::model::{
    Action, CallObservation, CallRef, CallSnapshot, DeviceRef, PhoneSnapshot, TransportState,
};

/// The single D-Bus method call an [`Action`] has been reduced to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCall {
    pub spec: MethodSpec,
    /// Object path to call (phone or call object).
    pub path: String,
    /// The one string argument, for `Dial` and `SendTones`.
    pub arg: Option<String>,
}

/// Dial strings accepted by `telephony.c::validate_phone_number`.
pub fn validate_dial_string(s: &str) -> Result<(), ControlError> {
    let ok = !s.is_empty()
        && s.len() <= PIPEWIRE_MAX_DIAL
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'A'..='D' | '#' | '*' | '+' | ','));
    ok.then_some(()).ok_or(ControlError::InvalidArgument(
        "dial string must be 1-80 characters of 0-9 A-D # * + ,",
    ))
}

/// DTMF strings: `telephony.c::validate_tones` additionally allows an empty string, which would
/// become a malformed `AT+VTS=`; this crate requires at least one tone and at most
/// [`declared::MAX_TONES`].
pub fn validate_tones(s: &str) -> Result<(), ControlError> {
    let ok = !s.is_empty()
        && s.len() <= declared::MAX_TONES
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'A'..='D' | '#' | '*'));
    ok.then_some(()).ok_or(ControlError::InvalidArgument(
        "tones must be 1-32 characters of 0-9 A-D # *",
    ))
}

fn check_device(handle: &DeviceRef, phone: &PhoneSnapshot) -> Result<(), ControlError> {
    if handle.owner != phone.device.owner {
        return Err(ControlError::ServiceRestarted);
    }
    if handle.path != phone.device.path {
        return Err(ControlError::NoSuchDevice);
    }
    if handle.address != phone.device.address {
        return Err(ControlError::StaleDevice);
    }
    Ok(())
}

fn live_call<'a>(
    call: &CallRef,
    phone: &'a PhoneSnapshot,
) -> Result<(&'a CallSnapshot, CallPhase), ControlError> {
    check_device(&call.device, phone)?;
    let snap = phone.call(&call.id).ok_or(ControlError::NoSuchCall)?;
    if snap.path != call.path || snap.fingerprint != call.fingerprint {
        return Err(ControlError::StaleCall);
    }
    match &snap.observation {
        CallObservation::Live(p) => Ok((snap, *p)),
        CallObservation::Disconnected => Err(ControlError::NoSuchCall),
        CallObservation::Unrecognized(s) => Err(ControlError::UnrecognizedCallState(s.clone())),
    }
}

fn phases(phone: &PhoneSnapshot) -> impl Iterator<Item = CallPhase> + '_ {
    phone.calls.iter().filter_map(|c| c.observation.phase())
}

fn declared(phone: &PhoneSnapshot, spec: MethodSpec) -> Result<MethodSpec, ControlError> {
    match phone.declared_api.declaration(&spec) {
        MethodDeclaration::Declared => Ok(spec),
        MethodDeclaration::Missing => Err(ControlError::NotDeclared {
            interface: spec.interface,
            member: spec.member,
            detail: "absent from introspection".to_string(),
        }),
        MethodDeclaration::SignatureMismatch { found } => Err(ControlError::NotDeclared {
            interface: spec.interface,
            member: spec.member,
            detail: format!(
                "declared with signature ({found}), expected ({})",
                spec.in_signature
            ),
        }),
    }
}

/// Reduce `action` to one method call, or explain why not. `phone` must come from a snapshot
/// taken immediately before dispatch against the same service owner.
pub fn plan(action: &Action, phone: &PhoneSnapshot) -> Result<PlannedCall, ControlError> {
    use CallPhase::*;
    let count = |p: CallPhase| phases(phone).filter(|&q| q == p).count();
    let device_call =
        |spec: MethodSpec, arg: Option<String>| -> Result<PlannedCall, ControlError> {
            Ok(PlannedCall {
                spec: declared(phone, spec)?,
                path: phone.device.path.clone(),
                arg,
            })
        };
    // Every phone-level action checks identity first.
    check_device(action.device(), phone)?;
    match action {
        Action::Dial { number, .. } => {
            validate_dial_string(number.expose())?;
            if phases(phone).any(|p| matches!(p, Dialing | Alerting)) {
                return Err(ControlError::WrongState(
                    "an outgoing call is already being set up",
                ));
            }
            device_call(declared::DIAL, Some(number.expose().to_string()))
        }
        Action::Answer { call } | Action::Reject { call } | Action::Hangup { call } => {
            let (snap, phase) = live_call(call, phone)?;
            let spec = match action {
                Action::Answer { .. } => {
                    // backend-native.c::hfp_hf_answer sends ATA only for `incoming`.
                    match phase {
                        Incoming => declared::ANSWER,
                        Waiting => {
                            return Err(ControlError::WrongState(
                                "a waiting call cannot be answered with Answer; use hold-and-answer or release-and-answer",
                            ));
                        }
                        _ => return Err(ControlError::WrongState("call is not ringing")),
                    }
                }
                Action::Reject { .. } => {
                    if !phase.is_ringing() {
                        return Err(ControlError::WrongState(
                            "only a ringing call can be rejected; use hangup to end a call in progress",
                        ));
                    }
                    declared::HANGUP
                }
                _ => declared::HANGUP,
            };
            Ok(PlannedCall {
                spec: declared(phone, spec)?,
                path: snap.path.clone(),
                arg: None,
            })
        }
        Action::HangupAll { .. } => {
            if phases(phone).next().is_none() {
                return Err(ControlError::WrongState("no live call"));
            }
            device_call(declared::HANGUP_ALL, None)
        }
        Action::SwapCalls { .. } => {
            if count(Held) == 0 {
                return Err(ControlError::WrongState("no held call"));
            }
            device_call(declared::SWAP_CALLS, None)
        }
        Action::ReleaseAndAnswer { .. } | Action::HoldAndAnswer { .. } => {
            if count(Active) == 0 || count(Waiting) == 0 {
                return Err(ControlError::WrongState(
                    "needs an active and a waiting call",
                ));
            }
            let spec = if matches!(action, Action::HoldAndAnswer { .. }) {
                declared::HOLD_AND_ANSWER
            } else {
                declared::RELEASE_AND_ANSWER
            };
            device_call(spec, None)
        }
        Action::ReleaseAndSwap { .. } | Action::CreateMultiparty { .. } => {
            if count(Waiting) != 0 {
                return Err(ControlError::WrongState("a call is waiting"));
            }
            if count(Active) == 0 || count(Held) == 0 {
                return Err(ControlError::WrongState("needs an active and a held call"));
            }
            let spec = if matches!(action, Action::CreateMultiparty { .. }) {
                declared::CREATE_MULTIPARTY
            } else {
                declared::RELEASE_AND_SWAP
            };
            device_call(spec, None)
        }
        Action::SendTones { tones, .. } => {
            validate_tones(tones.expose())?;
            if count(Active) == 0 {
                return Err(ControlError::WrongState("DTMF needs an active call"));
            }
            device_call(declared::SEND_TONES, Some(tones.expose().to_string()))
        }
        Action::ActivateAudio { .. } => {
            let Some(t) = &phone.transport else {
                return Err(ControlError::WrongState("phone exposes no audio transport"));
            };
            // hfp_hf_transport_activate refuses when a SCO socket already exists.
            if matches!(t.state, TransportState::Active | TransportState::Pending) {
                return Err(ControlError::WrongState(
                    "audio transport is already up or opening",
                ));
            }
            Ok(PlannedCall {
                spec: declared(phone, declared::ACTIVATE)?,
                path: phone.device.path.clone(),
                arg: None,
            })
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::declared::{DeclaredApi, DeclaredInterface};
    use crate::model::*;
    use bridge_core::calls::{CallCapabilities, CallId, Sensitive};

    pub fn iface(methods: &[(&str, &str)], props: &[(&str, &str)]) -> DeclaredInterface {
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

    pub fn full_api() -> DeclaredApi {
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
            transport: Some(iface(&[("Activate", "")], &[("State", "s")])),
            call: Some(iface(
                &[("Answer", ""), ("Hangup", "")],
                &[("State", "s"), ("LineIdentification", "s")],
            )),
            object_manager: true,
        }
    }

    pub fn device(path: &str, addr: &str) -> DeviceRef {
        DeviceRef {
            owner: ":1.7".into(),
            path: path.into(),
            address: Sensitive::new(addr),
        }
    }

    pub fn call(id: &str, state: &str, line: &str) -> CallSnapshot {
        CallSnapshot {
            id: CallId::new(id),
            path: format!("/org/pipewire/Telephony/ag1/{id}"),
            raw_state: state.into(),
            observation: CallObservation::from_dbus(state),
            fingerprint: CallFingerprint {
                line_identification: Some(Sensitive::new(line)),
                incoming_line: None,
                name: None,
            },
            multiparty: false,
        }
    }

    pub fn transport(state: &str) -> TransportSnapshot {
        TransportSnapshot {
            state: TransportState::from_dbus(state),
            raw_state: state.into(),
            codec_id: 1,
            codec_evidence: AudioCodecEvidence::from_id(1),
            reject_sco: Some(false),
        }
    }

    pub fn phone(calls: Vec<CallSnapshot>) -> PhoneSnapshot {
        PhoneSnapshot {
            device: device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22"),
            speaker_volume: Some(9),
            microphone_volume: Some(9),
            transport: Some(transport("idle")),
            calls,
            declared_api: full_api(),
            capabilities: CallCapabilities::unprobed(
                bridge_core::calls::CallBackend::BluetoothHfp {
                    host: bridge_core::calls::HfpHost::LinuxPipeWire,
                },
            ),
            evidence: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use bridge_core::calls::Sensitive;

    fn call_ref(p: &PhoneSnapshot, id: &str) -> CallRef {
        p.call_ref(&bridge_core::calls::CallId::new(id)).unwrap()
    }

    #[test]
    fn answer_only_when_incoming_and_targets_the_call_object() {
        let p = phone(vec![call("call2", "incoming", "+15550100")]);
        let plan = plan(
            &Action::Answer {
                call: call_ref(&p, "call2"),
            },
            &p,
        )
        .unwrap();
        assert_eq!(plan.spec, declared::ANSWER);
        assert_eq!(plan.path, "/org/pipewire/Telephony/ag1/call2");
        assert_eq!(plan.arg, None);

        let p = phone(vec![call("call2", "waiting", "+15550100")]);
        let err = super::plan(
            &Action::Answer {
                call: call_ref(&p, "call2"),
            },
            &p,
        )
        .unwrap_err();
        assert!(matches!(err, ControlError::WrongState(m) if m.contains("hold-and-answer")));

        let p = phone(vec![call("call2", "active", "+15550100")]);
        assert!(matches!(
            super::plan(
                &Action::Answer {
                    call: call_ref(&p, "call2")
                },
                &p
            ),
            Err(ControlError::WrongState(_))
        ));
    }

    #[test]
    fn reject_is_hangup_of_a_ringing_call_only() {
        let p = phone(vec![call("call1", "waiting", "+1")]);
        let pl = plan(
            &Action::Reject {
                call: call_ref(&p, "call1"),
            },
            &p,
        )
        .unwrap();
        assert_eq!(pl.spec, declared::HANGUP);
        let p = phone(vec![call("call1", "active", "+1")]);
        assert!(matches!(
            plan(
                &Action::Reject {
                    call: call_ref(&p, "call1")
                },
                &p
            ),
            Err(ControlError::WrongState(_))
        ));
        // ...whereas Hangup accepts an active one.
        assert_eq!(
            plan(
                &Action::Hangup {
                    call: call_ref(&p, "call1")
                },
                &p
            )
            .unwrap()
            .spec,
            declared::HANGUP
        );
    }

    #[test]
    fn stale_and_wrong_handles_are_refused_before_dispatch() {
        let p = phone(vec![call("call1", "incoming", "+15550100")]);
        let good = call_ref(&p, "call1");

        // The path was reused by a different caller.
        let mut reused = p.clone();
        reused.calls[0].fingerprint.line_identification = Some(Sensitive::new("+15550199"));
        assert_eq!(
            plan(&Action::Answer { call: good.clone() }, &reused).unwrap_err(),
            ControlError::StaleCall
        );

        // The call is gone.
        let none = phone(vec![]);
        assert_eq!(
            plan(&Action::Answer { call: good.clone() }, &none).unwrap_err(),
            ControlError::NoSuchCall
        );
        let mut ended = p.clone();
        ended.calls[0] = call("call1", "disconnected", "+15550100");
        assert_eq!(
            plan(
                &Action::Hangup {
                    call: call_ref(&p, "call1")
                },
                &ended
            )
            .unwrap_err(),
            ControlError::NoSuchCall
        );

        // ag1 now belongs to another handset.
        let mut other = p.clone();
        other.device.address = Sensitive::new("AA:BB:CC:99:99:99");
        assert_eq!(
            plan(&Action::Answer { call: good.clone() }, &other).unwrap_err(),
            ControlError::StaleDevice
        );

        // The service restarted (new unique name).
        let mut restarted = p.clone();
        restarted.device.owner = ":1.99".into();
        assert_eq!(
            plan(&Action::Answer { call: good.clone() }, &restarted).unwrap_err(),
            ControlError::ServiceRestarted
        );

        // A handle for ag2 never reaches ag1.
        let mut wrong_path = good;
        wrong_path.device.path = "/org/pipewire/Telephony/ag2".into();
        assert_eq!(
            plan(&Action::Answer { call: wrong_path }, &p).unwrap_err(),
            ControlError::NoSuchDevice
        );
    }

    #[test]
    fn unrecognized_call_state_is_never_acted_on() {
        let p = phone(vec![call("call1", "frobnicating", "+1")]);
        let r = call_ref(&p, "call1");
        assert_eq!(
            plan(&Action::Hangup { call: r }, &p).unwrap_err(),
            ControlError::UnrecognizedCallState("frobnicating".into())
        );
    }

    #[test]
    fn dial_validates_and_blocks_a_second_outgoing_call() {
        let d = device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22");
        let p = phone(vec![]);
        let ok = plan(
            &Action::Dial {
                device: d.clone(),
                number: Sensitive::new("+15550100"),
            },
            &p,
        )
        .unwrap();
        assert_eq!(ok.spec, declared::DIAL);
        assert_eq!(ok.path, "/org/pipewire/Telephony/ag1");
        assert_eq!(ok.arg.as_deref(), Some("+15550100"));
        for bad in ["", "tel:123", "12 34", &"1".repeat(81), "12\n"] {
            assert!(
                matches!(
                    plan(
                        &Action::Dial {
                            device: d.clone(),
                            number: Sensitive::new(bad)
                        },
                        &p
                    ),
                    Err(ControlError::InvalidArgument(_))
                ),
                "{bad:?}"
            );
        }
        let busy = phone(vec![call("call1", "dialing", "+1")]);
        assert!(matches!(
            plan(
                &Action::Dial {
                    device: d,
                    number: Sensitive::new("123")
                },
                &busy
            ),
            Err(ControlError::WrongState(_))
        ));
    }

    #[test]
    fn dtmf_needs_an_active_call_and_valid_tones() {
        let d = device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22");
        let active = phone(vec![call("call1", "active", "+1")]);
        let ok = plan(
            &Action::SendTones {
                device: d.clone(),
                tones: Sensitive::new("12#*A"),
            },
            &active,
        )
        .unwrap();
        assert_eq!(
            (ok.spec, ok.arg.as_deref()),
            (declared::SEND_TONES, Some("12#*A"))
        );
        assert!(matches!(
            plan(
                &Action::SendTones {
                    device: d.clone(),
                    tones: Sensitive::new("")
                },
                &active
            ),
            Err(ControlError::InvalidArgument(_))
        ));
        assert!(matches!(
            plan(
                &Action::SendTones {
                    device: d.clone(),
                    tones: Sensitive::new("1+")
                },
                &active
            ),
            Err(ControlError::InvalidArgument(_))
        ));
        let ringing = phone(vec![call("call1", "incoming", "+1")]);
        assert!(matches!(
            plan(
                &Action::SendTones {
                    device: d,
                    tones: Sensitive::new("1")
                },
                &ringing
            ),
            Err(ControlError::WrongState(_))
        ));
    }

    #[test]
    fn hold_swap_and_multiparty_preconditions() {
        let d = device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22");
        let none = phone(vec![call("call1", "active", "+1")]);
        assert!(plan(&Action::SwapCalls { device: d.clone() }, &none).is_err());
        let held = phone(vec![
            call("call1", "active", "+1"),
            call("call2", "held", "+2"),
        ]);
        assert_eq!(
            plan(&Action::SwapCalls { device: d.clone() }, &held)
                .unwrap()
                .spec,
            declared::SWAP_CALLS
        );
        assert_eq!(
            plan(&Action::CreateMultiparty { device: d.clone() }, &held)
                .unwrap()
                .spec,
            declared::CREATE_MULTIPARTY
        );
        let waiting = phone(vec![
            call("call1", "active", "+1"),
            call("call2", "waiting", "+2"),
        ]);
        assert_eq!(
            plan(&Action::HoldAndAnswer { device: d.clone() }, &waiting)
                .unwrap()
                .spec,
            declared::HOLD_AND_ANSWER
        );
        assert_eq!(
            plan(&Action::ReleaseAndAnswer { device: d.clone() }, &waiting)
                .unwrap()
                .spec,
            declared::RELEASE_AND_ANSWER
        );
        assert!(plan(&Action::ReleaseAndSwap { device: d }, &waiting).is_err());
    }

    #[test]
    fn audio_activation_only_from_idle_and_only_when_declared() {
        let d = device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22");
        let mut p = phone(vec![]);
        assert_eq!(
            plan(&Action::ActivateAudio { device: d.clone() }, &p)
                .unwrap()
                .spec,
            declared::ACTIVATE
        );
        p.transport = Some(transport("active"));
        assert!(matches!(
            plan(&Action::ActivateAudio { device: d.clone() }, &p),
            Err(ControlError::WrongState(_))
        ));
        p.transport = Some(transport("idle"));
        p.declared_api.transport = None;
        assert!(matches!(
            plan(&Action::ActivateAudio { device: d.clone() }, &p),
            Err(ControlError::NotDeclared { .. })
        ));
        p.declared_api = full_api();
        p.transport = None;
        assert!(matches!(
            plan(&Action::ActivateAudio { device: d }, &p),
            Err(ControlError::WrongState(_))
        ));
    }

    #[test]
    fn a_signature_mismatch_is_not_callable() {
        let d = device("/org/pipewire/Telephony/ag1", "AA:BB:CC:00:11:22");
        let mut p = phone(vec![]);
        p.declared_api
            .audio_gateway
            .as_mut()
            .unwrap()
            .methods
            .insert("Dial".into(), "ss".into());
        let err = plan(
            &Action::Dial {
                device: d,
                number: Sensitive::new("123"),
            },
            &p,
        )
        .unwrap_err();
        assert!(
            matches!(err, ControlError::NotDeclared { member: "Dial", .. }),
            "{err:?}"
        );
    }
}
