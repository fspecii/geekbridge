// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests of the Linux adapter against a mock `org.pipewire.Telephony` service on a
//! private `dbus-daemon`. They place no calls: the mock only records what it receives.
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use bridge_calls::pipewire::{BusTarget, PipeWireBackend, PipeWireConfig};
use bridge_calls::probe::{self, BusChoice, ProbeOptions, Verdict};
use bridge_calls::{
    Action, CallBackend, ControlError, DeviceRef, RemoteErrorKind, ServiceStatus, Session,
};
use bridge_core::calls::{
    Applied, AudioCodec, AudioLinkState, CallCommand, CallDirection, CallEvent, CallFeature,
    CallId, CallPhase, EndReason, LinkState, Sensitive, Support,
};
use common::*;

const ADDR1: &str = "AA:BB:CC:00:11:22";
const ADDR2: &str = "AA:BB:CC:99:88:77";
const AG1: &str = "/org/pipewire/Telephony/ag1";

fn backend(bus: &PrivateBus) -> PipeWireBackend {
    backend_with_timeout(bus, Duration::from_secs(5))
}
fn backend_with_timeout(bus: &PrivateBus, t: Duration) -> PipeWireBackend {
    PipeWireBackend::connect(PipeWireConfig {
        bus: BusTarget::Address(bus.address.clone()),
        method_timeout: t,
    })
    .unwrap()
}
fn session(bus: &PrivateBus) -> Session {
    Session::new(Box::new(backend(bus)))
}
fn device_of(s: &Session) -> DeviceRef {
    s.phones().next().expect("a phone").0.clone()
}
fn cid(s: &str) -> CallId {
    CallId::new(s)
}

macro_rules! bus_or_skip {
    () => {
        match PrivateBus::start() {
            Some(b) => b,
            None => return,
        }
    };
}

#[test]
fn enumerates_service_phone_calls_interfaces_and_declared_capabilities() {
    let bus = bus_or_skip!();
    let b = backend(&bus);
    // No service yet: explicit absence, not an error.
    assert_eq!(b.snapshot().unwrap().service, ServiceStatus::Absent);

    let mock = MockService::start(&bus);
    assert!(
        b.snapshot().unwrap().phones.is_empty(),
        "service present, no phone"
    );
    mock.add_phone(1, ADDR1, Some(("idle", 2)));
    mock.add_call(1, 3, "incoming", "+15550100", "Ada");

    let snap = b.snapshot().unwrap();
    assert_eq!(
        snap.service,
        ServiceStatus::Present {
            owner: mock.unique_name()
        }
    );
    assert_eq!(snap.phones.len(), 1);
    let p = &snap.phones[0];
    assert_eq!(p.device.path, AG1);
    assert_eq!(p.device.address.expose(), ADDR1);
    assert_eq!((p.speaker_volume, p.microphone_volume), (Some(9), Some(7)));
    let t = p.transport.as_ref().unwrap();
    assert_eq!(t.raw_state, "idle");
    assert_eq!(t.codec_id, 2);
    assert_eq!(p.calls.len(), 1);
    let c = &p.calls[0];
    assert_eq!(c.id, cid("call3"));
    assert_eq!(c.path, "/org/pipewire/Telephony/ag1/call3");
    assert_eq!(c.observation.phase(), Some(CallPhase::Incoming));
    assert_eq!(
        c.fingerprint.line_identification.as_ref().unwrap().expose(),
        "+15550100"
    );
    assert_eq!(c.fingerprint.name.as_ref().unwrap().expose(), "Ada");
    assert_eq!(
        c.fingerprint.incoming_line, None,
        "empty strings are absent"
    );

    // Declared API came from live introspection of the mock's generated XML.
    let api = &p.declared_api;
    assert_eq!(api.audio_gateway.as_ref().unwrap().methods["Dial"], "s");
    assert_eq!(
        api.audio_gateway.as_ref().unwrap().methods["SendTones"],
        "s"
    );
    assert_eq!(api.transport.as_ref().unwrap().methods["Activate"], "");
    assert_eq!(api.call.as_ref().unwrap().methods["Hangup"], "");
    assert!(api.object_manager);

    // Declared, never verified.
    for f in [
        CallFeature::Dial,
        CallFeature::Answer,
        CallFeature::Hangup,
        CallFeature::SendTones,
        CallFeature::SwapCalls,
        CallFeature::AudioLinkControl,
        CallFeature::CallerIdentity,
        CallFeature::CallStateEvents,
    ] {
        assert_eq!(p.capabilities.support(f), Support::Declared, "{f:?}");
    }
    for f in CallFeature::ALL {
        assert_ne!(p.capabilities.support(f), Support::Verified, "{f:?}");
    }
    // Idle transport: no audio claim.
    assert_eq!(
        p.capabilities.support(CallFeature::AudioDownlink),
        Support::Unknown
    );

    // The probe made no control calls and the snapshot made none either.
    assert!(mock.log().is_empty());
}

#[test]
fn session_maps_the_full_call_lifecycle_to_neutral_events() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("idle", 2)));
    let mut s = session(&bus);

    let ev = s.refresh().unwrap();
    assert!(matches!(
        ev[0].event,
        CallEvent::Link {
            state: LinkState::Up
        }
    ));
    let dev = device_of(&s);

    mock.add_call(1, 1, "incoming", "+15550100", "Ada");
    let ev = s.refresh().unwrap();
    assert_eq!(ev.len(), 1);
    assert!(matches!(
        &ev[0].event,
        CallEvent::CallUpdated { id, direction: CallDirection::Incoming, phase: CallPhase::Incoming, .. } if id == &cid("call1")
    ));

    mock.set_call_state(1, 1, "active");
    mock.set_transport(1, "active", 2);
    let ev = s.refresh().unwrap();
    assert!(ev.iter().any(|e| matches!(
        &e.event,
        CallEvent::CallUpdated {
            phase: CallPhase::Active,
            ..
        }
    )));
    assert!(ev.iter().any(|e| matches!(
        &e.event,
        CallEvent::Audio {
            state: AudioLinkState::Connected {
                codec: AudioCodec::Msbc
            }
        }
    )));
    let st = s.state(&dev).unwrap();
    assert_eq!(
        st.calls[&cid("call1")].direction,
        CallDirection::Incoming,
        "direction survives phase change"
    );

    mock.set_call_state(1, 1, "held");
    s.refresh().unwrap();
    assert_eq!(
        s.state(&dev).unwrap().calls[&cid("call1")].phase,
        CallPhase::Held
    );

    mock.set_call_state(1, 1, "active");
    s.refresh().unwrap();
    // Server removes the object after `disconnected`; either order must end the call once.
    mock.set_call_state(1, 1, "disconnected");
    let ev = s.refresh().unwrap();
    assert!(
        ev.iter()
            .any(|e| matches!(&e.event, CallEvent::CallEnded { id, .. } if id == &cid("call1")))
    );
    mock.remove_call(1, 1);
    mock.set_transport(1, "idle", 2);
    let ev = s.refresh().unwrap();
    assert!(
        ev.iter()
            .all(|e| !matches!(e.event, CallEvent::CallEnded { .. })),
        "no double end: {ev:?}"
    );
    let st = s.state(&dev).unwrap();
    assert!(st.calls.is_empty());
    assert_eq!(st.recent_ended.back().unwrap().reason, EndReason::Completed);
    assert_eq!(st.audio, AudioLinkState::Disconnected);

    // An outgoing call: dialing -> alerting -> active, direction Outgoing.
    mock.add_call(1, 2, "dialing", "+15550111", "");
    s.refresh().unwrap();
    assert_eq!(
        s.state(&dev).unwrap().calls[&cid("call2")].direction,
        CallDirection::Outgoing
    );
    mock.set_call_state(1, 2, "alerting");
    mock.set_call_state(1, 2, "active");
    s.refresh().unwrap();
    assert_eq!(
        s.state(&dev).unwrap().calls[&cid("call2")].phase,
        CallPhase::Active
    );
}

#[test]
fn unknown_call_state_string_is_shown_but_never_creates_a_call_or_dispatch() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, None);
    mock.add_call(1, 1, "response_and_hold", "+1555", "");
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);
    assert!(s.state(&dev).unwrap().calls.is_empty());
    let snap = s.phone(&dev).unwrap();
    assert_eq!(snap.calls.len(), 1, "still visible in the snapshot");
    let err = s.reject(&dev, &cid("call1")).unwrap_err();
    assert!(
        matches!(err, ControlError::UnrecognizedCallState(ref st) if st == "response_and_hold"),
        "{err:?}"
    );
    assert!(mock.log().is_empty());
}

#[test]
fn phone_disconnect_ends_calls_as_link_lost() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("active", 2)));
    mock.add_call(1, 1, "active", "+15550100", "");
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);
    assert_eq!(
        s.state(&dev).unwrap().audio,
        AudioLinkState::Connected {
            codec: AudioCodec::Msbc
        }
    );

    mock.remove_phone(1);
    let ev = s.refresh().unwrap();
    assert!(matches!(
        ev.last().unwrap().event,
        CallEvent::Link {
            state: LinkState::Down
        }
    ));
    assert!(s.state(&dev).is_none(), "phone is gone from the live set");
    assert_eq!(s.phones().count(), 0);
    // The reducer recorded LinkLost (observed through the event's applied result).
    assert_eq!(ev.last().unwrap().applied, Applied::Changed);

    // Reconnecting the same handset yields a fresh link, not the old calls.
    mock.add_phone(1, ADDR1, None);
    let ev = s.refresh().unwrap();
    assert!(matches!(
        ev[0].event,
        CallEvent::Link {
            state: LinkState::Up
        }
    ));
    assert!(s.state(&device_of(&s)).unwrap().calls.is_empty());
}

#[test]
fn service_loss_and_restart_void_old_handles() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, None);
    mock.add_call(1, 1, "active", "+15550100", "");
    let mut s = session(&bus);
    s.refresh().unwrap();
    let old_dev = device_of(&s);
    let old_call = s.phone(&old_dev).unwrap().call_ref(&cid("call1")).unwrap();
    let old_unique = mock.unique_name();

    // Service dies: name vanishes, every phone goes link-down with LinkLost.
    drop(mock);
    let ev = s.refresh().unwrap();
    assert!(ev.iter().any(|e| matches!(
        e.event,
        CallEvent::Link {
            state: LinkState::Down
        }
    )));
    assert_eq!(s.service(), &ServiceStatus::Absent);
    assert_eq!(s.phones().count(), 0);
    let err = s.execute(&old_dev, &CallCommand::SwapCalls).unwrap_err();
    assert_eq!(err, ControlError::NoSuchDevice);

    // Service comes back (new unique name) with the same path and handset.
    let mock2 = MockService::start(&bus);
    assert_ne!(mock2.unique_name(), old_unique);
    mock2.add_phone(1, ADDR1, None);
    mock2.add_call(1, 1, "active", "+15550100", "");
    s.refresh().unwrap();
    let new_dev = device_of(&s);
    assert_ne!(new_dev, old_dev, "owner is part of the identity");

    // The old call handle must not reach the new, unrelated object even though path and caller match.
    let b = backend(&bus);
    let err = b.invoke(&Action::Hangup { call: old_call }).unwrap_err();
    assert_eq!(err, ControlError::ServiceRestarted);
    assert!(
        mock2.log().is_empty(),
        "nothing was sent: {:?}",
        mock2.log()
    );
}

#[test]
fn dispatch_sends_exactly_the_declared_method_and_arguments() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("idle", 1)));
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);

    // Dial.
    s.execute(
        &dev,
        &CallCommand::Dial {
            number: Sensitive::new("+15550100"),
        },
    )
    .unwrap();
    assert_eq!(
        mock.log(),
        vec![Invocation {
            path: AG1.into(),
            interface: AG,
            member: "Dial",
            arg: "+15550100".into()
        }]
    );

    // Incoming: reject.
    mock.add_call(1, 1, "incoming", "+15550142", "");
    s.refresh().unwrap();
    s.reject(&dev, &cid("call1")).unwrap();
    // Incoming: answer.
    s.execute(&dev, &CallCommand::Answer { call: cid("call1") })
        .unwrap();
    // Active: DTMF, hangup, audio link.
    mock.set_call_state(1, 1, "active");
    s.refresh().unwrap();
    s.execute(
        &dev,
        &CallCommand::SendTones {
            tones: Sensitive::new("12#*A"),
        },
    )
    .unwrap();
    s.execute(&dev, &CallCommand::Hangup { call: cid("call1") })
        .unwrap();
    s.execute(&dev, &CallCommand::ConnectAudio).unwrap();
    s.invoke(&Action::HangupAll {
        device: dev.clone(),
    })
    .unwrap();

    let call = "/org/pipewire/Telephony/ag1/call1";
    assert_eq!(
        mock.log()[1..].to_vec(),
        vec![
            Invocation {
                path: call.into(),
                interface: CALL,
                member: "Hangup",
                arg: "".into()
            }, // reject
            Invocation {
                path: call.into(),
                interface: CALL,
                member: "Answer",
                arg: "".into()
            },
            Invocation {
                path: AG1.into(),
                interface: AG,
                member: "SendTones",
                arg: "12#*A".into()
            },
            Invocation {
                path: call.into(),
                interface: CALL,
                member: "Hangup",
                arg: "".into()
            },
            Invocation {
                path: AG1.into(),
                interface: TRANSPORT,
                member: "Activate",
                arg: "".into()
            },
            Invocation {
                path: AG1.into(),
                interface: AG,
                member: "HangupAll",
                arg: "".into()
            },
        ]
    );
}

#[test]
fn refused_commands_send_nothing() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("idle", 1)));
    mock.add_call(1, 1, "active", "+15550100", "");
    mock.add_call(1, 2, "waiting", "+15550101", "");
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);

    // Bad arguments (model pre-flight and adapter validation).
    for bad in ["", "abc", "+1 555", "tel:1"] {
        assert!(
            s.execute(
                &dev,
                &CallCommand::Dial {
                    number: Sensitive::new(bad)
                }
            )
            .is_err(),
            "{bad:?}"
        );
    }
    // Waiting call cannot be answered with plain Answer; reject of an active call is refused.
    assert!(matches!(
        s.execute(&dev, &CallCommand::Answer { call: cid("call2") }),
        Err(ControlError::WrongState(_))
    ));
    assert!(matches!(
        s.reject(&dev, &cid("call1")),
        Err(ControlError::WrongState(_))
    ));
    // Unsupported surface.
    assert!(matches!(
        s.execute(&dev, &CallCommand::MuteRinger),
        Err(ControlError::Preflight(_))
    ));
    // Idle transport: the model refuses DisconnectAudio as a state error first...
    assert!(matches!(
        s.execute(&dev, &CallCommand::DisconnectAudio),
        Err(ControlError::Preflight(
            bridge_core::calls::CommandRejection::InvalidState
        ))
    ));
    assert!(matches!(
        s.execute(&dev, &CallCommand::SetMicrophoneMute { muted: true }),
        Err(ControlError::Preflight(_) | ControlError::Unsupported(_))
    ));
    // Unknown call.
    assert!(
        s.execute(&dev, &CallCommand::Hangup { call: cid("call9") })
            .is_err()
    );
    // ...and with the link up, the API itself has no way to close it.
    mock.set_transport(1, "active", 2);
    s.refresh().unwrap();
    assert!(matches!(
        s.execute(&dev, &CallCommand::DisconnectAudio),
        Err(ControlError::Unsupported(_))
    ));
    assert!(matches!(
        s.execute(&dev, &CallCommand::ConnectAudio),
        Err(ControlError::Preflight(
            bridge_core::calls::CommandRejection::InvalidState
        ))
    ));
    assert!(mock.log().is_empty(), "{:?}", mock.log());
}

#[test]
fn stale_call_and_device_handles_are_refused_without_dispatch() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("idle", 1)));
    mock.add_call(1, 1, "incoming", "+15550100", "Ada");
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);
    let call = s.phone(&dev).unwrap().call_ref(&cid("call1")).unwrap();
    let b = backend(&bus);

    // call1 ends and its object path is reused by a different caller before the next refresh.
    mock.remove_call(1, 1);
    mock.add_call(1, 1, "incoming", "+15550199", "Mallory");
    assert_eq!(
        b.invoke(&Action::Answer { call: call.clone() })
            .unwrap_err(),
        ControlError::StaleCall
    );

    // The call is gone altogether.
    mock.remove_call(1, 1);
    assert_eq!(
        b.invoke(&Action::Hangup { call: call.clone() })
            .unwrap_err(),
        ControlError::NoSuchCall
    );

    // ag1 is reused by a different handset.
    mock.remove_phone(1);
    mock.add_phone(1, ADDR2, None);
    assert_eq!(
        b.invoke(&Action::Dial {
            device: dev.clone(),
            number: Sensitive::new("123")
        })
        .unwrap_err(),
        ControlError::StaleDevice
    );
    // A handle for a phone that does not exist.
    let ghost = DeviceRef {
        path: "/org/pipewire/Telephony/ag7".into(),
        ..dev
    };
    assert_eq!(
        b.invoke(&Action::SwapCalls { device: ghost }).unwrap_err(),
        ControlError::NoSuchDevice
    );
    assert!(
        mock.log().is_empty(),
        "nothing may be dispatched: {:?}",
        mock.log()
    );
}

#[test]
fn service_errors_are_typed_and_distinct_from_unknown_outcomes() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, None);
    let b = backend(&bus);
    let snap = b.snapshot().unwrap();
    let dev = snap.phones[0].device.clone();
    let dial = Action::Dial {
        device: dev.clone(),
        number: Sensitive::new("+15550100"),
    };

    for (name, kind) in [
        ("InvalidState", RemoteErrorKind::InvalidState),
        ("InProgress", RemoteErrorKind::InProgress),
        ("NotSupported", RemoteErrorKind::NotSupported),
        ("InvalidFormat", RemoteErrorKind::InvalidFormat),
        ("Failed", RemoteErrorKind::Failed),
        ("CME", RemoteErrorKind::Cme),
    ] {
        *mock.shared.fail_next.lock().unwrap() = Some(name);
        match b.invoke(&dial).unwrap_err() {
            ControlError::Remote {
                kind: k,
                name: n,
                message,
            } => {
                assert_eq!(k, kind);
                assert_eq!(n, format!("org.pipewire.Telephony.Error.{name}"));
                assert!(!message.is_empty());
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    // Every refused call was still delivered exactly once with the right argument.
    assert_eq!(mock.log().len(), 6);
    assert!(
        mock.log()
            .iter()
            .all(|i| i.member == "Dial" && i.arg == "+15550100")
    );
    let err = ControlError::Remote {
        kind: RemoteErrorKind::Failed,
        name: String::new(),
        message: String::new(),
    };
    assert!(!err.outcome_unknown());
}

#[test]
fn a_reply_timeout_reports_unknown_outcome() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, None);
    let b = backend_with_timeout(&bus, Duration::from_millis(300));
    let dev = b.snapshot().unwrap().phones[0].device.clone();
    *mock.shared.delay_next.lock().unwrap() = Some(Duration::from_millis(1500));
    let err = b
        .invoke(&Action::Dial {
            device: dev,
            number: Sensitive::new("123"),
        })
        .unwrap_err();
    assert_eq!(err, ControlError::Timeout);
    assert!(err.outcome_unknown());
    // The call really was delivered: that is exactly why the outcome is "unknown", not "failed".
    assert_eq!(mock.log().len(), 1);
}

#[test]
fn missing_transport_interface_degrades_audio_control_conservatively() {
    let bus = bus_or_skip!();
    let mock = MockService::start_with(
        &bus,
        MockOptions {
            with_transport: false,
        },
    );
    mock.add_phone(1, ADDR1, Some(("idle", 1)));
    let mut s = session(&bus);
    s.refresh().unwrap();
    let dev = device_of(&s);
    let caps = s.phone(&dev).unwrap().capabilities.clone();
    assert_eq!(
        caps.support(CallFeature::AudioLinkControl),
        Support::Unknown
    );
    assert_eq!(
        caps.support(CallFeature::Dial),
        Support::Declared,
        "other features unaffected"
    );
    let err = s.execute(&dev, &CallCommand::ConnectAudio).unwrap_err();
    assert!(matches!(err, ControlError::Preflight(_)), "{err:?}");
    assert!(mock.log().is_empty());
}

#[test]
fn signals_wake_the_watcher_and_quiet_times_out() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    let b = backend(&bus);
    // Drain startup signals until the bus has been quiet for a moment (event-driven, so a slow
    // machine only makes this loop longer, never the assertion below wrong).
    let mut quiet = false;
    for _ in 0..50 {
        if !b.wait_for_change(Duration::from_millis(200)).unwrap() {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "bus never went quiet after startup");
    assert!(
        !b.wait_for_change(Duration::from_millis(150)).unwrap(),
        "quiet bus must time out"
    );

    mock.add_phone(1, ADDR1, None);
    assert!(b.wait_for_change(Duration::from_secs(3)).unwrap());
    mock.add_call(1, 1, "incoming", "+1555", "");
    mock.set_call_state(1, 1, "active");
    assert!(
        b.wait_for_change(Duration::from_secs(3)).unwrap(),
        "burst wakes once"
    );
    // Service death is signalled via NameOwnerChanged. Drain the burst first.
    for _ in 0..50 {
        if !b.wait_for_change(Duration::from_millis(200)).unwrap() {
            break;
        }
    }
    drop(mock);
    assert!(b.wait_for_change(Duration::from_secs(3)).unwrap());
    assert_eq!(b.snapshot().unwrap().service, ServiceStatus::Absent);
}

#[test]
fn pump_combines_wait_and_refresh() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    let mut s = session(&bus);
    s.refresh().unwrap();
    let _ = s.pump(Duration::from_millis(200));
    mock.add_phone(1, ADDR1, None);
    let ev = s.pump(Duration::from_secs(3)).unwrap();
    assert!(ev.iter().any(|e| matches!(
        e.event,
        CallEvent::Link {
            state: LinkState::Up
        }
    )));
}

#[test]
fn probe_reports_registered_phone_masks_private_data_and_sends_no_control_calls() {
    let bus = bus_or_skip!();
    let mock = MockService::start(&bus);
    mock.add_phone(1, ADDR1, Some(("idle", 2)));
    mock.add_call(1, 1, "incoming", "+15550142", "Ada");
    let opts = ProbeOptions {
        session: BusChoice::Address(bus.address.clone()),
        system: BusChoice::Skip,
        show_private: false,
    };
    let r = probe::run(&opts);
    assert!(r.read_only && r.redacted && r.hardware_verified.is_empty());
    assert_eq!(r.summary.verdict, Verdict::PhoneRegistered);
    assert_eq!((r.summary.phones, r.summary.calls), (1, 1));
    let t = r.buses[0].telephony.as_ref().unwrap();
    assert_eq!(t.owner.as_deref(), Some(mock.unique_name().as_str()));
    assert!(
        t.manager_interfaces
            .iter()
            .any(|i| i == "org.freedesktop.DBus.ObjectManager")
    );
    let phone = &t.phones[0];
    assert_eq!(phone.address, "AA:BB:CC:xx:xx:xx");
    assert_eq!(phone.calls[0].number.as_deref(), Some("***42"));
    assert_eq!(phone.calls[0].name.as_deref(), Some("<redacted>"));
    assert!(
        phone
            .capabilities
            .iter()
            .any(|e| e.feature == CallFeature::Dial && e.support == Support::Declared)
    );
    assert!(
        r.capability_evidence
            .iter()
            .all(|e| e.support != Support::Verified)
    );
    let json = serde_json::to_string(&r).unwrap();
    assert!(
        !json.contains("+15550142") && !json.contains(ADDR1) && !json.contains("Ada"),
        "{json}"
    );

    let shown = probe::run(&ProbeOptions {
        show_private: true,
        ..opts
    });
    let json = serde_json::to_string(&shown).unwrap();
    assert!(json.contains(ADDR1) && json.contains("+15550142"));

    assert!(
        mock.log().is_empty(),
        "the probe must never invoke a control method"
    );
}

#[test]
fn probe_states_absence_explicitly() {
    let bus = bus_or_skip!();
    // No service at all.
    let opts = ProbeOptions {
        session: BusChoice::Address(bus.address.clone()),
        system: BusChoice::Skip,
        show_private: false,
    };
    let r = probe::run(&opts);
    assert_eq!(r.summary.verdict, Verdict::NoTelephonyService);
    assert!(
        r.statements
            .iter()
            .any(|s| s.contains("no telephony backend")),
        "{:?}",
        r.statements
    );

    // Service but no phone.
    let mock = MockService::start(&bus);
    let r = probe::run(&opts);
    assert_eq!(r.summary.verdict, Verdict::NoPhone);
    assert!(
        r.statements.iter().any(|s| s.starts_with("No phone")),
        "{:?}",
        r.statements
    );
    assert!(
        r.capability_evidence
            .iter()
            .filter(|e| e.feature == CallFeature::Dial)
            .all(|e| e.support == Support::Unknown)
    );
    assert!(mock.log().is_empty());
    let text = probe::render_text(&r);
    assert!(text.contains("No phone"));
}
