//! Engine behaviour against the in-memory port: state, selection, gating, side effects, stale
//! references, dispatch and timeouts. No Windows needed.
use bridge_calls_windows::engine::*;
use bridge_calls_windows::model::*;
use bridge_calls_windows::project::core_call_id;
use bridge_calls_windows::readiness::*;
use bridge_calls_windows::testing::*;
use bridge_core::calls::*;

const SESSION: u64 = 7;

fn cfg() -> EngineConfig {
    EngineConfig {
        session: SESSION,
        ..EngineConfig::default()
    }
}

fn engine(mock: &MockPlatform) -> Engine<MockPlatform> {
    Engine::new(mock.clone(), cfg())
}

fn selected(mock: &MockPlatform) -> Engine<MockPlatform> {
    let mut e = engine(mock);
    e.select(&mock.first_id()).unwrap();
    mock.clear_log();
    e
}

/// A registered, connected phone with a ringing call, selected, log cleared.
fn ringing() -> (MockPlatform, Engine<MockPlatform>, CallRef) {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let mut e = selected(&mock);
    let r = first_call(&e.refresh());
    mock.clear_log();
    (mock, e, r)
}

fn first_call(s: &Snapshot) -> CallRef {
    s.selected().unwrap().calls[0].reference.clone()
}

fn refused(r: Result<impl std::fmt::Debug, CallsError>) -> Refusal {
    match r {
        Err(CallsError::Refused { refusal }) => refusal,
        other => panic!("expected refusal, got {other:?}"),
    }
}

// ------------------------------------------------------------------ read-only behaviour

#[test]
fn fresh_install_is_unregistered_and_refresh_has_no_side_effects() {
    let mock = MockPlatform::fresh();
    let mut e = engine(&mock);
    let snap = e.refresh();
    assert!(snap.env_blockers.is_empty());
    let d = &snap.devices[0];
    assert_eq!(
        d.readiness.blockers,
        vec![
            Blocker::AccessNotGranted {
                access: AccessState::NotRequested
            },
            Blocker::NotRegistered
        ]
    );
    assert_eq!(d.access, AccessState::NotRequested);
    assert!(mock.mutations().is_empty(), "{:?}", mock.log());
}

#[test]
fn every_environment_failure_is_typed_and_probe_stays_read_only() {
    type Tweak = fn(&mut EnvironmentFacts);
    let cases: Vec<(&str, Tweak, Blocker, bool)> = vec![
        (
            "not windows",
            |e| *e = EnvironmentFacts::not_windows(),
            Blocker::NotWindows,
            true,
        ),
        (
            "windows 10",
            |e| e.os.build = Some(19045),
            Blocker::UnsupportedOs { build: Some(19045) },
            true,
        ),
        (
            "contract",
            |e| e.contract.v6 = Some(false),
            Blocker::ContractMissing { required_major: 6 },
            true,
        ),
        (
            "identity",
            |e| e.package = PackageIdentity::Absent,
            Blocker::PackageIdentityMissing,
            false,
        ),
        (
            "privacy",
            |e| e.privacy = PrivacyAccess::DeniedByUser,
            Blocker::PrivacyBlocked {
                access: PrivacyAccess::DeniedByUser,
            },
            false,
        ),
    ];
    for (name, tweak, expected, hides_devices) in cases {
        let mock = MockPlatform::fresh();
        mock.with(|s| tweak(&mut s.env));
        let mut e = engine(&mock);
        let snap = e.refresh();
        assert_eq!(snap.env_blockers.first(), Some(&expected), "{name}");
        assert_eq!(snap.devices.is_empty(), hides_devices, "{name}");
        assert!(mock.mutations().is_empty(), "{name}: {:?}", mock.log());
    }
}

#[test]
fn unreadable_environment_hides_devices() {
    let mock = MockPlatform::fresh();
    mock.with(|s| {
        s.errors.insert(
            "environment",
            PlatformError::new(PlatformErrorKind::Os, "environment"),
        )
    });
    let snap = engine(&mock).refresh();
    assert_eq!(
        snap.env_blockers,
        vec![Blocker::EnvironmentUnreadable {
            kind: PlatformErrorKind::Os
        }]
    );
    assert!(snap.devices.is_empty());
}

#[test]
fn registered_phone_without_line_reports_no_line() {
    let mock = MockPlatform::connected();
    mock.with(|s| s.lines.clear());
    let snap = engine(&mock).refresh();
    assert_eq!(snap.devices[0].readiness.blockers, vec![Blocker::NoLine]);
}

#[test]
fn unreadable_transport_is_a_typed_blocker() {
    let mock = MockPlatform::fresh();
    mock.with(|s| {
        s.errors.insert(
            "transport_info",
            PlatformError::new(
                PlatformErrorKind::AccessDenied,
                "PhoneLineTransportDevice.FromId",
            )
            .with_hresult(0x80070005u32 as i32),
        )
    });
    let snap = engine(&mock).refresh();
    assert_eq!(
        snap.devices[0].readiness.blockers,
        vec![Blocker::DeviceUnreadable {
            kind: PlatformErrorKind::AccessDenied
        }]
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn audio_route_blocked_is_reported_not_hidden() {
    let mock = MockPlatform::connected();
    mock.with(|s| s.audio_routing = AudioRouting::CannotRouteToLocal);
    let snap = engine(&mock).refresh();
    let d = &snap.devices[0];
    assert!(d.readiness.control_ready());
    assert_eq!(d.readiness.audio, AudioReadiness::Blocked);
}

#[test]
fn more_than_the_bound_of_phones_is_flagged() {
    let mock = MockPlatform::fresh();
    mock.with(|s| {
        for n in 2..=12 {
            let id = device_id(n);
            s.registered.insert(id.key().to_owned(), false);
            s.transports.push(TransportRecord {
                id,
                name: None,
                address: None,
                enabled: true,
            });
        }
    });
    let snap = engine(&mock).refresh();
    assert_eq!(snap.devices.len(), MAX_DEVICES);
    assert!(snap.truncated);
}

// ------------------------------------------------------------------ selection

#[test]
fn selection_has_no_side_effects_and_unknown_devices_are_refused() {
    let mock = MockPlatform::fresh();
    let mut e = engine(&mock);
    assert_eq!(refused(e.select(&device_id(9))), Refusal::UnknownDevice);
    let snap = e.select(&mock.first_id()).unwrap();
    assert!(snap.selected().is_some());
    assert!(mock.mutations().is_empty(), "{:?}", mock.log());
}

#[test]
fn explicit_actions_need_a_selected_device() {
    let mock = MockPlatform::fresh();
    let mut e = engine(&mock);
    assert_eq!(refused(e.register()), Refusal::NoDeviceSelected);
    assert_eq!(refused(e.connect()), Refusal::NoDeviceSelected);
    assert_eq!(refused(e.request_access()), Refusal::NoDeviceSelected);
    assert!(mock.mutations().is_empty());
}

#[test]
fn a_selected_phone_that_disappears_is_unknown_not_acted_on() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    mock.with(|s| s.transports.clear());
    assert_eq!(refused(e.register()), Refusal::UnknownDevice);
    assert!(mock.mutations().is_empty());
}

// ------------------------------------------------------------------ registration and connect

#[test]
fn register_requests_access_then_registers_and_confirms() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    assert_eq!(e.register().unwrap(), RegisterOutcome::Registered);
    assert_eq!(mock.mutations(), vec!["request_access", "register"]);
    let snap = e.last_snapshot().unwrap();
    assert_eq!(snap.selected().unwrap().access, AccessState::Allowed);
}

#[test]
fn register_stops_at_access_denial_without_registering() {
    for (status, state) in [
        (AccessStatus::DeniedByUser, AccessState::DeniedByUser),
        (AccessStatus::DeniedBySystem, AccessState::DeniedBySystem),
        (AccessStatus::Unspecified, AccessState::Unspecified),
    ] {
        let mock = MockPlatform::fresh();
        mock.with(|s| s.access = status);
        let mut e = selected(&mock);
        assert_eq!(
            refused(e.register()),
            Refusal::Blocked {
                blocker: Blocker::AccessNotGranted { access: state }
            }
        );
        assert_eq!(mock.mutations(), vec!["request_access"]);
        let snap = e.last_snapshot().unwrap();
        assert_eq!(snap.selected().unwrap().access, state);
    }
}

#[test]
fn access_error_is_typed_and_remembered() {
    let mock = MockPlatform::fresh();
    mock.with(|s| {
        s.errors.insert(
            "request_access",
            PlatformError::new(PlatformErrorKind::AccessDenied, "RequestAccessAsync"),
        )
    });
    let mut e = selected(&mock);
    assert!(matches!(
        e.register(),
        Err(CallsError::Platform { error }) if error.kind == PlatformErrorKind::AccessDenied
    ));
    assert_eq!(mock.mutations(), vec!["request_access"]);
    let state = e.last_snapshot().unwrap().selected().unwrap().access;
    assert_eq!(state, AccessState::Failed(PlatformErrorKind::AccessDenied));
}

#[test]
fn environment_blockers_stop_register_before_any_mutating_call() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    mock.with(|s| s.env.package = PackageIdentity::Absent);
    assert_eq!(
        refused(e.register()),
        Refusal::Blocked {
            blocker: Blocker::PackageIdentityMissing
        }
    );
    mock.with(|s| s.env = EnvironmentFacts::not_windows());
    assert_eq!(
        refused(e.register()),
        Refusal::Blocked {
            blocker: Blocker::NotWindows
        }
    );
    assert!(mock.mutations().is_empty(), "{:?}", mock.log());
}

#[test]
fn register_when_already_registered_does_nothing() {
    let mock = MockPlatform::connected();
    let mut e = selected(&mock);
    assert_eq!(e.register().unwrap(), RegisterOutcome::AlreadyRegistered);
    assert!(mock.mutations().is_empty());
}

#[test]
fn register_never_unregisters_anything() {
    let mock = MockPlatform::fresh();
    let other = device_id(2);
    mock.with(|s| {
        s.registered.insert(other.key().to_owned(), true);
        s.transports.push(TransportRecord {
            id: other.clone(),
            name: None,
            address: None,
            enabled: true,
        });
    });
    let mut e = selected(&mock);
    e.register().unwrap();
    assert!(!mock.log().iter().any(|l| l.starts_with("unregister")));
    assert!(
        mock.with(|s| s.registered[other.key()]),
        "other phone untouched"
    );
}

#[test]
fn unregister_applies_only_to_the_selected_phone_and_only_when_asked() {
    let mock = MockPlatform::connected();
    let other = device_id(2);
    mock.with(|s| {
        s.registered.insert(other.key().to_owned(), true);
        s.transports.push(TransportRecord {
            id: other.clone(),
            name: None,
            address: None,
            enabled: true,
        });
    });
    let mut e = selected(&mock);
    assert_eq!(e.unregister().unwrap(), UnregisterOutcome::Unregistered);
    assert_eq!(mock.mutations(), vec!["unregister"]);
    assert!(mock.with(|s| s.registered[other.key()]));
    // Not registered any more: a second request is a no-op.
    mock.clear_log();
    assert_eq!(e.unregister().unwrap(), UnregisterOutcome::WasNotRegistered);
    assert!(mock.mutations().is_empty());
}

#[test]
fn connect_requires_registration_without_calling_windows() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    assert_eq!(
        refused(e.connect()),
        Refusal::Blocked {
            blocker: Blocker::NotRegistered
        }
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn connect_reports_whether_a_line_became_visible() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    e.register().unwrap();
    let id = mock.first_id();
    mock.with(|s| s.line_on_connect = Some(line_for(&id, 1, true)));
    mock.clear_log();
    let report = e.connect().unwrap();
    assert_eq!(report.history, ConnectHistory::ReturnedTrue);
    assert!(report.line_visible);
    assert_eq!(mock.mutations(), vec!["connect"]);
}

#[test]
fn connect_false_and_timeout_are_distinct_and_never_retried() {
    let mock = MockPlatform::fresh();
    let mut e = selected(&mock);
    e.register().unwrap();
    mock.with(|s| s.connect_result = false);
    mock.clear_log();
    assert_eq!(e.connect().unwrap().history, ConnectHistory::ReturnedFalse);
    assert_eq!(mock.mutations(), vec!["connect"]);

    mock.with(|s| {
        s.errors.insert(
            "connect",
            PlatformError::new(PlatformErrorKind::TimedOut, "ConnectAsync"),
        )
    });
    mock.clear_log();
    let report = e.connect().unwrap();
    assert_eq!(report.history, ConnectHistory::Uncertain);
    assert!(!report.line_visible);
    assert_eq!(mock.mutations(), vec!["connect"], "one attempt, no retry");
    let snap = e.last_snapshot().unwrap();
    assert_eq!(snap.selected().unwrap().connect, ConnectHistory::Uncertain);
}

// ------------------------------------------------------------------ dial

#[test]
fn dial_needs_a_dialable_line_and_a_valid_number() {
    let mock = MockPlatform::connected();
    let mut e = selected(&mock);
    let dial = |n: &str| WinCommand::Dial {
        number: Sensitive::new(n),
        route_to_local: false,
    };
    assert_eq!(refused(e.execute(&dial("12ab"))), Refusal::InvalidNumber);
    assert_eq!(refused(e.execute(&dial(""))), Refusal::InvalidNumber);

    mock.with(|s| s.lines[0].can_dial = Some(false));
    assert_eq!(
        refused(e.execute(&dial("+15550100"))),
        Refusal::Blocked {
            blocker: Blocker::LineNotDialable
        }
    );
    assert!(mock.mutations().is_empty());

    mock.with(|s| s.lines.clear());
    assert_eq!(
        refused(e.execute(&dial("+15550100"))),
        Refusal::Blocked {
            blocker: Blocker::NoLine
        }
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn dial_places_exactly_one_call_and_returns_a_reference() {
    let mock = MockPlatform::connected();
    let mut e = selected(&mock);
    let report = e
        .execute(&WinCommand::Dial {
            number: Sensitive::new("+15550123"),
            route_to_local: false,
        })
        .unwrap();
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(mock.mutations(), vec!["dial:len=9"]);
    let after = report.after.unwrap();
    assert_eq!(after.record.status, NativeCallStatus::Dialing);
    assert_eq!(Some(after.reference), report.dialed);
    assert!(report.route.is_none());
}

#[test]
fn dial_with_route_to_local_is_a_separate_reported_step() {
    let mock = MockPlatform::connected();
    let mut e = selected(&mock);
    let report = e
        .execute(&WinCommand::Dial {
            number: Sensitive::new("112"),
            route_to_local: true,
        })
        .unwrap();
    assert_eq!(report.route, Some(Outcome::Succeeded));
    assert_eq!(mock.mutations(), vec!["dial:len=3", "call_op:route_audio"]);
    assert_eq!(
        report.after.unwrap().record.audio_device,
        AudioDevice::Local
    );
}

#[test]
fn dial_with_blocked_audio_route_is_refused_before_dialing() {
    let mock = MockPlatform::connected();
    mock.with(|s| s.audio_routing = AudioRouting::CannotRouteToLocal);
    let mut e = selected(&mock);
    let cmd = WinCommand::Dial {
        number: Sensitive::new("112"),
        route_to_local: true,
    };
    assert_eq!(refused(e.execute(&cmd)), Refusal::AudioRouteBlocked);
    assert!(mock.mutations().is_empty());
}

#[test]
fn failed_dial_status_is_a_failure_not_a_success() {
    let mock = MockPlatform::connected();
    mock.with(|s| s.op_status = Some(OperationStatus::OtherFailure));
    let mut e = selected(&mock);
    let report = e
        .execute(&WinCommand::Dial {
            number: Sensitive::new("112"),
            route_to_local: true,
        })
        .unwrap();
    assert_eq!(
        report.outcome,
        Outcome::Failed {
            failure: OpFailure::Status {
                status: OperationStatus::OtherFailure
            }
        }
    );
    assert!(report.route.is_none(), "no routing after a failed dial");
}

// ------------------------------------------------------------------ call commands and dispatch

#[test]
fn answer_dispatches_accept_and_reads_back_the_new_state() {
    let (mock, mut e, r) = ringing();
    let report = e.execute(&WinCommand::Answer(r)).unwrap();
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(mock.mutations(), vec!["call_op:accept"]);
    assert_eq!(
        report.after.unwrap().record.status,
        NativeCallStatus::Talking
    );
}

#[test]
fn each_command_dispatches_its_own_method() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    let r = first_call(&e.refresh());
    let route = |to| WinCommand::RouteAudio {
        call: r.clone(),
        to,
    };
    let steps: Vec<(WinCommand, &str)> = vec![
        (WinCommand::Hold(r.clone()), "call_op:hold"),
        (WinCommand::Resume(r.clone()), "call_op:resume"),
        (
            WinCommand::SetMute {
                call: r.clone(),
                muted: true,
            },
            "call_op:mute",
        ),
        (
            WinCommand::SetMute {
                call: r.clone(),
                muted: false,
            },
            "call_op:unmute",
        ),
        (route(AudioDevice::Local), "call_op:route_audio"),
        (route(AudioDevice::Remote), "call_op:route_audio"),
        (WinCommand::End(r.clone()), "call_op:end"),
    ];
    for (cmd, expected) in steps {
        mock.clear_log();
        let name = cmd.name();
        // Resume needs a held call and Hold a talking one; the mock applies each effect.
        let report = e.execute(&cmd);
        if name == "resume" {
            // Resume arrives after Hold in the list above, so the call is held.
        }
        let report = report.unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(report.outcome, Outcome::Succeeded, "{name}");
        assert_eq!(mock.mutations(), vec![expected], "{name}");
    }
}

#[test]
fn reject_is_distinct_from_end() {
    let (mock, mut e, r) = ringing();
    e.execute(&WinCommand::Reject(r)).unwrap();
    assert_eq!(mock.mutations(), vec!["call_op:reject"]);
}

#[test]
fn tones_are_sent_one_by_one_and_validated_first() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    let r = first_call(&e.refresh());
    mock.clear_log();

    let report = e
        .execute(&WinCommand::SendTones {
            call: r.clone(),
            tones: Sensitive::new("12#*"),
        })
        .unwrap();
    assert_eq!(report.steps_done, 4);
    assert_eq!(mock.mutations(), vec!["call_op:dtmf"; 4]);

    mock.clear_log();
    let bad = |t: &str| WinCommand::SendTones {
        call: r.clone(),
        tones: Sensitive::new(t),
    };
    assert_eq!(refused(e.execute(&bad("1A"))), Refusal::UnsupportedTone);
    assert_eq!(refused(e.execute(&bad("1x"))), Refusal::InvalidTones);
    assert_eq!(refused(e.execute(&bad(""))), Refusal::InvalidTones);
    assert!(
        mock.mutations().is_empty(),
        "nothing sent for a bad sequence"
    );
}

#[test]
fn state_preconditions_are_refused_without_dispatch() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![
        call("ring", NativeCallStatus::Incoming),
        call("talk", NativeCallStatus::Talking),
    ]);
    let mut e = selected(&mock);
    let snap = e.refresh();
    let calls = &snap.selected().unwrap().calls;
    let by = |id: &str| {
        calls
            .iter()
            .find(|c| c.record.id.as_str() == id)
            .unwrap()
            .reference
            .clone()
    };
    mock.clear_log();
    let talk = by("talk");
    let ring = by("ring");
    let talking = NativeCallStatus::Talking;
    let incoming = NativeCallStatus::Incoming;
    assert_eq!(
        refused(e.execute(&WinCommand::Answer(talk.clone()))),
        Refusal::InvalidState { status: talking }
    );
    assert_eq!(
        refused(e.execute(&WinCommand::Hold(ring.clone()))),
        Refusal::InvalidState { status: incoming }
    );
    assert_eq!(
        refused(e.execute(&WinCommand::Resume(talk.clone()))),
        Refusal::InvalidState { status: talking }
    );
    assert_eq!(
        refused(e.execute(&WinCommand::End(ring.clone()))),
        Refusal::InvalidState { status: incoming }
    );
    assert_eq!(
        refused(e.execute(&WinCommand::RouteAudio {
            call: talk,
            to: AudioDevice::Unknown
        })),
        Refusal::InvalidState { status: talking }
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn hold_is_refused_when_the_phone_cannot_hold() {
    let mock = MockPlatform::connected();
    let mut c = call("c1", NativeCallStatus::Talking);
    c.hold_supported = Some(false);
    mock.set_calls(vec![c]);
    let mut e = selected(&mock);
    let r = first_call(&e.refresh());
    mock.clear_log();
    assert_eq!(
        refused(e.execute(&WinCommand::Hold(r))),
        Refusal::HoldUnsupported
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn call_commands_are_refused_while_not_ready() {
    let (mock, mut e, r) = ringing();
    mock.with(|s| s.lines.clear());
    assert_eq!(
        refused(e.execute(&WinCommand::Answer(r))),
        Refusal::Blocked {
            blocker: Blocker::NoLine
        }
    );
    assert!(mock.mutations().is_empty());
}

// ------------------------------------------------------------------ stale references

#[test]
fn a_call_that_ended_is_refused_as_stale_with_no_dispatch() {
    let (mock, mut e, r) = ringing();
    mock.set_calls(vec![]);
    assert_eq!(
        refused(e.execute(&WinCommand::Answer(r))),
        Refusal::StaleCall
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn an_ended_status_is_stale_even_if_windows_still_lists_it() {
    let (mock, mut e, r) = ringing();
    mock.set_calls(vec![call("c1", NativeCallStatus::Ended)]);
    assert_eq!(refused(e.execute(&WinCommand::End(r))), Refusal::StaleCall);
    assert!(mock.mutations().is_empty());
}

#[test]
fn a_reused_windows_call_id_after_line_loss_is_a_different_call() {
    let (mock, mut e, r) = ringing();
    let old_epoch = r.epoch();
    // The line disappears (phone out of range) and later returns with a call that reuses the id.
    let saved = mock.with(|s| std::mem::take(&mut s.lines));
    e.refresh();
    mock.with(|s| s.lines = saved);
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let snap = e.refresh();
    let new = first_call(&snap);
    assert_ne!(new.epoch(), old_epoch);
    assert_eq!(new.native_id(), r.native_id());

    assert_eq!(
        refused(e.execute(&WinCommand::Answer(r))),
        Refusal::StaleCall,
        "old reference must not act on the new call"
    );
    assert!(mock.mutations().is_empty());
    assert!(
        e.execute(&WinCommand::Answer(new))
            .unwrap()
            .outcome
            .is_success()
    );
}

#[test]
fn replacing_the_line_also_changes_the_epoch() {
    let (mock, mut e, r) = ringing();
    mock.with(|s| {
        s.lines[0].id = LineId::new("00000000-0000-0000-0000-00000000ffff").unwrap();
        let calls = s
            .calls
            .remove(&LineId::new("00000000-0000-0000-0000-000000000001").unwrap());
        s.calls.insert(s.lines[0].id.clone(), calls.unwrap());
    });
    let snap = e.refresh();
    assert_ne!(first_call(&snap).epoch(), r.epoch());
    assert_eq!(
        refused(e.execute(&WinCommand::Answer(r))),
        Refusal::StaleCall
    );
}

#[test]
fn a_phone_that_leaves_and_returns_gets_a_new_epoch() {
    let mock = MockPlatform::connected();
    let mut e = selected(&mock);
    let before = e.refresh().selected().unwrap().epoch;
    let saved = mock.with(|s| std::mem::take(&mut s.transports));
    e.refresh();
    mock.with(|s| s.transports = saved);
    let after = e.refresh();
    assert_ne!(after.devices[0].epoch, before);
}

#[test]
fn references_do_not_survive_an_engine_restart() {
    let (mock, _old_engine, r) = ringing();
    let mut restarted = Engine::new(
        mock.clone(),
        EngineConfig {
            session: SESSION + 1,
            ..EngineConfig::default()
        },
    );
    restarted.select(&mock.first_id()).unwrap();
    mock.clear_log();
    assert_eq!(
        refused(restarted.execute(&WinCommand::Answer(r))),
        Refusal::StaleCall
    );
    assert!(mock.mutations().is_empty());
}

#[test]
fn a_reference_for_one_phone_cannot_act_through_another() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let second = device_id(2);
    mock.with(|s| {
        s.registered.insert(second.key().to_owned(), true);
        s.transports.push(TransportRecord {
            id: second.clone(),
            name: None,
            address: None,
            enabled: true,
        });
        s.lines.push(line_for(&second, 2, true));
    });
    let mut e = engine(&mock);
    e.select(&mock.first_id()).unwrap();
    let r = first_call(&e.refresh());
    e.select(&second).unwrap();
    mock.clear_log();
    assert_eq!(
        refused(e.execute(&WinCommand::Answer(r))),
        Refusal::StaleCall
    );
    assert!(mock.mutations().is_empty());
}

// ------------------------------------------------------------------ calls per device

#[test]
fn active_calls_are_tracked_per_device_by_transport_id() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("a", NativeCallStatus::Talking)]);
    let second = device_id(2);
    mock.with(|s| {
        s.registered.insert(second.key().to_owned(), true);
        s.transports.push(TransportRecord {
            id: second.clone(),
            name: None,
            address: None,
            enabled: true,
        });
        let line = line_for(&second, 2, true);
        s.calls.insert(
            line.id.clone(),
            vec![
                call("b", NativeCallStatus::Incoming),
                call("c", NativeCallStatus::Held),
            ],
        );
        s.lines.push(line);
    });
    let snap = engine(&mock).refresh();
    assert_eq!(snap.devices.len(), 2);
    assert_eq!(snap.devices[0].calls.len(), 1);
    assert_eq!(snap.devices[1].calls.len(), 2);
    assert_ne!(snap.devices[0].epoch, snap.devices[1].epoch);
}

#[test]
fn transport_ids_match_lines_case_insensitively() {
    let mock = MockPlatform::connected();
    mock.with(|s| {
        let upper = s.transports[0].id.as_str().to_ascii_uppercase();
        s.lines[0].transport_device_id = Some(TransportDeviceId::new(upper).unwrap());
    });
    let snap = engine(&mock).refresh();
    assert!(snap.devices[0].line.is_some());
}

#[test]
fn several_lines_for_one_transport_choose_the_dialable_one() {
    let mock = MockPlatform::connected();
    mock.with(|s| {
        let id = s.transports[0].id.clone();
        s.lines[0].can_dial = Some(false);
        let mut extra = line_for(&id, 5, true);
        extra.can_dial = Some(true);
        s.lines.push(extra);
    });
    let snap = engine(&mock).refresh();
    let d = &snap.devices[0];
    assert!(d.line_ambiguous);
    assert_eq!(d.line.as_ref().unwrap().can_dial, Some(true));
}

#[test]
fn non_bluetooth_lines_are_ignored() {
    let mock = MockPlatform::connected();
    mock.with(|s| s.lines[0].transport = NativeTransport::Cellular);
    let snap = engine(&mock).refresh();
    assert_eq!(snap.devices[0].readiness.blockers, vec![Blocker::NoLine]);
}

// ------------------------------------------------------------------ timeouts and outcomes

#[test]
fn a_timeout_during_a_command_is_uncertain_and_never_retried() {
    let (mock, mut e, r) = ringing();
    mock.with(|s| {
        s.errors.insert(
            "call_op",
            PlatformError::new(PlatformErrorKind::TimedOut, "PhoneCall.AcceptIncomingAsync"),
        )
    });
    let report = e.execute(&WinCommand::Answer(r)).unwrap();
    assert_eq!(
        report.outcome,
        Outcome::Uncertain {
            reason: UncertainReason::DeadlineExceeded
        }
    );
    assert_eq!(
        mock.mutations(),
        vec!["call_op:accept"],
        "exactly one attempt"
    );
    assert!(report.after.is_some(), "state is re-read after the timeout");
}

#[test]
fn windows_own_timed_out_status_is_also_uncertain() {
    let (mock, mut e, r) = ringing();
    mock.with(|s| s.op_status = Some(OperationStatus::TimedOut));
    let report = e.execute(&WinCommand::Answer(r)).unwrap();
    assert_eq!(
        report.outcome,
        Outcome::Uncertain {
            reason: UncertainReason::OsTimedOut
        }
    );
}

#[test]
fn definite_os_failures_are_failures() {
    for status in [
        OperationStatus::ConnectionLost,
        OperationStatus::InvalidCallState,
        OperationStatus::OtherFailure,
    ] {
        let (mock, mut e, r) = ringing();
        mock.with(|s| s.op_status = Some(status));
        let report = e.execute(&WinCommand::Answer(r)).unwrap();
        assert_eq!(
            report.outcome,
            Outcome::Failed {
                failure: OpFailure::Status { status }
            }
        );
    }
}

#[test]
fn a_vanished_call_during_dispatch_is_a_typed_platform_failure() {
    let (mock, mut e, r) = ringing();
    // The call ends between the validating read and the operation: remove it from the call table
    // only (not from the listing the engine already took) by clearing after the first read.
    mock.with(|s| {
        s.errors.insert(
            "call_op",
            PlatformError::new(PlatformErrorKind::NotFound, "PhoneCall.GetFromId"),
        )
    });
    let report = e.execute(&WinCommand::Answer(r)).unwrap();
    assert!(matches!(
        report.outcome,
        Outcome::Failed {
            failure: OpFailure::Platform { error }
        } if error.kind == PlatformErrorKind::NotFound
    ));
}

#[test]
fn a_tone_timeout_stops_the_sequence() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    let r = first_call(&e.refresh());
    mock.with(|s| {
        s.errors.insert(
            "call_op",
            PlatformError::new(PlatformErrorKind::TimedOut, "PhoneCall.SendDtmfKeyAsync"),
        )
    });
    mock.clear_log();
    let report = e
        .execute(&WinCommand::SendTones {
            call: r,
            tones: Sensitive::new("123"),
        })
        .unwrap();
    assert!(matches!(report.outcome, Outcome::Uncertain { .. }));
    assert_eq!(report.steps_done, 0);
    assert_eq!(mock.mutations().len(), 1);
}

// ------------------------------------------------------------------ core projection

#[test]
fn refresh_produces_core_events_and_declared_capabilities() {
    let (mock, mut e, _r) = ringing();
    let events = e.take_core_events();
    assert!(events.iter().any(|ev| matches!(
        ev,
        CallEvent::CallUpdated {
            phase: CallPhase::Incoming,
            ..
        }
    )));
    let caps = e.capabilities();
    assert_eq!(caps.support(CallFeature::Answer), Support::Declared);
    for f in CallFeature::ALL {
        assert_ne!(caps.support(f), Support::Verified);
    }
    assert!(mock.mutations().is_empty());
}

#[test]
fn a_failed_call_read_does_not_end_calls_until_the_state_stays_unknown() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    e.refresh();
    e.take_core_events();
    mock.with(|s| {
        s.errors.insert(
            "list_calls",
            PlatformError::new(
                PlatformErrorKind::Os,
                "PhoneLine.GetAllActivePhoneCallsAsync",
            ),
        )
    });
    for _ in 0..2 {
        let snap = e.refresh();
        assert!(snap.selected().unwrap().calls_error.is_some());
        assert!(e.take_core_events().is_empty(), "unknown is not 'ended'");
    }
    e.refresh();
    let events = e.take_core_events();
    assert!(events.contains(&CallEvent::Link {
        state: LinkState::Down
    }));
    assert_eq!(
        e.core_state().recent_ended.back().unwrap().reason,
        EndReason::LinkLost
    );
}

#[test]
fn line_loss_ends_calls_as_link_lost() {
    let (mock, mut e, _r) = ringing();
    e.take_core_events();
    mock.with(|s| s.lines.clear());
    e.refresh();
    assert_eq!(e.core_state().link, LinkState::Down);
    assert_eq!(
        e.core_state().recent_ended.back().unwrap().reason,
        EndReason::LinkLost
    );
}

#[test]
fn core_commands_map_to_windows_operations() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![
        call("talk", NativeCallStatus::Talking),
        call("ring", NativeCallStatus::Incoming),
    ]);
    let mut e = selected(&mock);
    let epoch = e.refresh().selected().unwrap().epoch;
    let id = |n: &str| core_call_id(epoch, &NativeCallId::new(n).unwrap());
    mock.clear_log();

    // Waiting call: hang-up of a ringing call is a reject.
    e.execute_core(&CallCommand::Hangup { call: id("ring") })
        .unwrap();
    assert_eq!(mock.mutations(), vec!["call_op:reject"]);

    mock.clear_log();
    e.execute_core(&CallCommand::SetMicrophoneMute { muted: true })
        .unwrap();
    e.execute_core(&CallCommand::ConnectAudio).unwrap();
    e.execute_core(&CallCommand::SendTones {
        tones: Sensitive::new("42"),
    })
    .unwrap();
    e.execute_core(&CallCommand::Hangup { call: id("talk") })
        .unwrap();
    assert_eq!(
        mock.mutations(),
        vec![
            "call_op:mute",
            "call_op:route_audio",
            "call_op:dtmf",
            "call_op:dtmf",
            "call_op:end"
        ]
    );
}

#[test]
fn core_dial_and_core_rejections() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("talk", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    let epoch = e.refresh().selected().unwrap().epoch;
    mock.clear_log();

    assert_eq!(
        refused(e.execute_core(&CallCommand::MuteRinger)),
        Refusal::Core {
            rejection: CommandRejection::Unsupported {
                reason: UnsupportedReason::NotInProtocol
            }
        }
    );
    assert_eq!(
        refused(e.execute_core(&CallCommand::Dial {
            number: Sensitive::new("12ab")
        })),
        Refusal::Core {
            rejection: CommandRejection::InvalidArgument
        }
    );
    // A call id from another epoch is no such call.
    let stale = core_call_id(epoch + 99, &NativeCallId::new("talk").unwrap());
    assert_eq!(
        refused(e.execute_core(&CallCommand::Hangup { call: stale })),
        Refusal::Core {
            rejection: CommandRejection::NoSuchCall
        }
    );
    // Swap with nothing held.
    assert_eq!(
        refused(e.execute_core(&CallCommand::SwapCalls)),
        Refusal::Core {
            rejection: CommandRejection::InvalidState
        }
    );
    assert!(mock.mutations().is_empty());

    e.execute_core(&CallCommand::Dial {
        number: Sensitive::new("+15550123"),
    })
    .unwrap();
    assert_eq!(mock.mutations(), vec!["dial:len=9"]);
}

#[test]
fn core_commands_are_refused_when_capabilities_are_not_declared() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("talk", NativeCallStatus::Talking)]);
    let mut e = selected(&mock);
    mock.with(|s| s.lines.clear());
    mock.clear_log();
    assert!(matches!(
        e.execute_core(&CallCommand::SetMicrophoneMute { muted: true }),
        Err(CallsError::Refused {
            refusal: Refusal::Core {
                rejection: CommandRejection::NotProbed
            }
        })
    ));
    assert!(mock.mutations().is_empty());
}

#[test]
fn capabilities_are_unsupported_where_windows_apis_do_not_exist() {
    let mock = MockPlatform::fresh();
    mock.with(|s| s.env.os.build = Some(19045));
    let mut e = engine(&mock);
    e.refresh();
    let caps = e.capabilities();
    for f in CallFeature::ALL {
        assert!(
            matches!(caps.support(f), Support::Unsupported { .. }),
            "{f:?}"
        );
    }
}
