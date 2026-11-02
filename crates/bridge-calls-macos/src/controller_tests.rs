// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::mock::{ManualClock, MockAudio, MockNative};
use crate::model::{AgClass, ListEntry, ListStatus};
use crate::plan::{NativeOp, Refusal};
use crate::port::{NativeError, NativeErrorKind};
use bridge_core::calls::EndReason;
use bridge_core::calls::{CallId, CallPhase, Sensitive};

type Ctl = Controller<MockNative, MockAudio, ManualClock>;

fn make(cfg: Config) -> (Ctl, ManualClock) {
    let clock = ManualClock::new();
    (
        Controller::new(MockNative::new(), MockAudio::standard(), clock.clone(), cfg),
        clock,
    )
}
fn addr() -> BtAddress {
    MockNative::default_address()
}
fn id(s: &str) -> CallId {
    CallId::new(s)
}

/// Connect and complete the service-level connection; returns the generation.
fn up(ctl: &mut Ctl) -> Generation {
    let g = ctl.connect(&addr()).unwrap();
    ctl.native_mut().slc_up(g);
    ctl.pump();
    assert_eq!(ctl.phase(), Some(LinkPhase::Up));
    ctl.drain();
    g
}
fn ind(ctl: &mut Ctl, g: Generation, i: Indicator) {
    ctl.native_mut().indicator(g, i);
    ctl.pump();
}
fn calls_events(events: &[AdapterEvent]) -> Vec<&CallEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Call { event } => Some(event),
            _ => None,
        })
        .collect()
}
fn outcomes(events: &[AdapterEvent]) -> Vec<(Ticket, Outcome)> {
    events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Command {
                ticket, outcome, ..
            } => Some((*ticket, outcome.clone())),
            _ => None,
        })
        .collect()
}
fn has_diag(events: &[AdapterEvent], d: &Diag) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AdapterEvent::Diagnostic { diag } if diag == d))
}

// ---------------------------------------------------------------- inventory and permission

#[test]
fn inventory_is_read_only() {
    let (mut ctl, _) = make(Config::default());
    let inv = ctl.inventory().unwrap();
    assert_eq!(inv.phones.len(), 1);
    assert_eq!(inv.ag_capable().count(), 1);
    let n = ctl.native_mut();
    assert!(n.opened.is_empty() && n.performed.is_empty() && n.closed.is_empty());
    assert_eq!(ctl.phase(), None);
}

#[test]
fn denied_or_restricted_permission_blocks_everything_native() {
    for auth in [BluetoothAuth::Denied, BluetoothAuth::Restricted] {
        let (mut ctl, _) = make(Config::default());
        ctl.native_mut().auth = auth;
        assert_eq!(
            ctl.inventory(),
            Err(AdapterError::PermissionDenied {
                authorization: auth
            })
        );
        assert_eq!(
            ctl.connect(&addr()),
            Err(AdapterError::PermissionDenied {
                authorization: auth
            })
        );
        assert_eq!(ctl.native_mut().paired_reads, 0);
        assert!(ctl.native_mut().opened.is_empty());
    }
}

#[test]
fn undetermined_permission_needs_explicit_prompt_allowance() {
    let (mut ctl, _) = make(Config::default());
    ctl.native_mut().auth = BluetoothAuth::NotDetermined;
    assert_eq!(ctl.inventory(), Err(AdapterError::PermissionNotDetermined));
    assert_eq!(ctl.native_mut().paired_reads, 0);
    let (mut ctl, _) = make(Config {
        allow_permission_prompt: true,
        ..Config::default()
    });
    ctl.native_mut().auth = BluetoothAuth::NotDetermined;
    assert!(ctl.inventory().is_ok());
}

#[test]
fn probe_reports_denial_instead_of_failing() {
    let (mut ctl, _) = make(Config::default());
    ctl.native_mut().auth = BluetoothAuth::Denied;
    let r = ctl.probe();
    assert_eq!(r.authorization, BluetoothAuth::Denied);
    assert!(matches!(
        r.blocked,
        Some(AdapterError::PermissionDenied { .. })
    ));
    assert!(r.phones.is_empty());
    assert_eq!(ctl.native_mut().paired_reads, 0);
    // And the allowed case lists AG-capable phones without connecting.
    let (mut ctl, _) = make(Config::default());
    let r = ctl.probe();
    assert!(r.blocked.is_none());
    assert_eq!((r.phones.len(), r.ag_capable), (1, 1));
    assert!(ctl.native_mut().opened.is_empty());
}

// ---------------------------------------------------------------- explicit connect

#[test]
fn connect_requires_a_known_paired_hands_free_gateway() {
    let (mut ctl, _) = make(Config::default());
    let stranger = BtAddress::parse("AA:AA:AA:AA:AA:AA").unwrap();
    assert_eq!(ctl.connect(&stranger), Err(AdapterError::UnknownPhone));
    ctl.native_mut().phones.push(MockNative::phone(
        "AA:AA:AA:AA:AA:AA",
        "Speaker",
        AgClass::NotAdvertised,
    ));
    assert_eq!(
        ctl.connect(&stranger),
        Err(AdapterError::NotHandsFreeGateway)
    );
    ctl.native_mut().phones[1].paired = false;
    assert_eq!(ctl.connect(&stranger), Err(AdapterError::NotPaired));
    assert!(ctl.native_mut().opened.is_empty());

    let (mut ctl, _) = make(Config {
        allow_unadvertised: true,
        ..Config::default()
    });
    ctl.native_mut().phones.push(MockNative::phone(
        "AA:AA:AA:AA:AA:AA",
        "Phone",
        AgClass::NotAdvertised,
    ));
    assert!(ctl.connect(&stranger).is_ok());
}

#[test]
fn only_one_link_at_a_time() {
    let (mut ctl, _) = make(Config::default());
    let g = ctl.connect(&addr()).unwrap();
    assert_eq!(
        ctl.connect(&addr()),
        Err(AdapterError::LinkBusy { generation: g })
    );
}

#[test]
fn commands_are_refused_until_the_link_is_up() {
    let (mut ctl, _) = make(Config::default());
    assert_eq!(
        ctl.execute(MacCommand::Hold),
        Err(AdapterError::NoActiveLink)
    );
    ctl.connect(&addr()).unwrap();
    assert_eq!(
        ctl.execute(MacCommand::Hold),
        Err(AdapterError::LinkNotReady)
    );
}

#[test]
fn slc_success_brings_link_up_and_declares_but_never_verifies() {
    let (mut ctl, _) = make(Config::default());
    let g = ctl.connect(&addr()).unwrap();
    ctl.native_mut().slc_up(g);
    ctl.pump();
    let ev = ctl.drain();
    assert!(ev.iter().any(|e| matches!(
        e,
        AdapterEvent::Link { notice: LinkNotice::Up, generation } if *generation == g
    )));
    assert!(calls_events(&ev).contains(&&CallEvent::Link {
        state: LinkState::Up
    }));
    let caps = ctl.capabilities();
    assert_eq!(caps.planes.control, Support::Declared);
    assert_eq!(caps.planes.sco, Support::Declared);
    // No indicator has arrived: events are not yet declared.
    assert_eq!(caps.planes.events, Support::Unknown);
    assert!(caps.ops.values().all(|s| *s != Support::Verified));
    assert_eq!(caps.ag_features.0 & AgFeatures::THREE_WAY_CALLING, 1);
}

#[test]
fn slc_failure_with_permission_status_is_flagged_and_retires_the_delegate() {
    let (mut ctl, _) = make(Config::default());
    let g = ctl.connect(&addr()).unwrap();
    ctl.native_mut().emit(
        g,
        NativeEvent::SlcConnected(IoStatus(0xe000_02e2_u32 as i32)),
    );
    ctl.pump();
    let ev = ctl.drain();
    assert!(ev.iter().any(|e| matches!(
        e,
        AdapterEvent::Link {
            notice: LinkNotice::Down(DownReason::ConnectFailed {
                permission_suspected: true,
                ..
            }),
            ..
        }
    )));
    assert!(ctl.native_mut().retired.contains(&g));
    assert_eq!(ctl.phase(), None);
}

#[test]
fn connect_timeout_tears_down_and_late_callbacks_are_stale() {
    let (mut ctl, clock) = make(Config::default());
    let g = ctl.connect(&addr()).unwrap();
    clock.advance(Config::default().connect_timeout_ms);
    ctl.pump();
    let ev = ctl.drain();
    assert!(ev.iter().any(|e| matches!(
        e,
        AdapterEvent::Link {
            notice: LinkNotice::Down(DownReason::ConnectTimeout),
            ..
        }
    )));
    assert_eq!(ctl.native_mut().closed, vec![g]);
    // The connection completes after we gave up: must not resurrect the link.
    ctl.native_mut()
        .emit_unchecked(g, NativeEvent::SlcConnected(IoStatus::SUCCESS));
    ctl.pump();
    assert_eq!(ctl.phase(), None);
    assert_eq!(ctl.snapshot().stats.stale_events, 1);
}

#[test]
fn indicators_delivered_before_connected_are_kept() {
    let (mut ctl, _) = make(Config::default());
    let g = ctl.connect(&addr()).unwrap();
    // The framework reads indicators during SLC setup, before the `connected` callback.
    ctl.native_mut().indicator(g, Indicator::Call(true));
    ctl.pump();
    assert!(
        ctl.drain()
            .iter()
            .all(|e| !matches!(e, AdapterEvent::Call { .. }))
    );
    ctl.native_mut().slc_up(g);
    ctl.pump();
    let ev = ctl.drain();
    let calls = calls_events(&ev);
    assert!(matches!(
        calls[0],
        CallEvent::Link {
            state: LinkState::Up
        }
    ));
    assert!(calls.iter().any(|e| matches!(
        e,
        CallEvent::CallUpdated {
            phase: CallPhase::Active,
            ..
        }
    )));
    assert_eq!(ctl.capabilities().planes.events, Support::Declared);
}

#[test]
fn duplicate_connected_callback_is_ignored_and_counted() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ctl.native_mut().slc_up(g);
    ctl.pump();
    let ev = ctl.drain();
    assert!(has_diag(&ev, &Diag::DuplicateConnected));
    assert_eq!(ctl.phase(), Some(LinkPhase::Up));
}

// ---------------------------------------------------------------- calls and commands

#[test]
fn command_before_any_indicator_is_refused() {
    let (mut ctl, _) = make(Config::default());
    up(&mut ctl);
    let r = ctl.execute(MacCommand::Dial {
        number: Sensitive::new("123"),
    });
    assert_eq!(
        r,
        Err(AdapterError::Refused {
            refusal: Refusal::CallStateUnknown
        })
    );
    assert!(ctl.native_mut().performed.is_empty());
}

#[test]
fn incoming_call_answer_is_confirmed_by_the_observed_effect() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(1));
    ctl.native_mut()
        .emit(g, NativeEvent::CallerId(Some(Sensitive::new("+15551234"))));
    ctl.pump();
    let ev = ctl.drain();
    let ringing = calls_events(&ev);
    assert!(ringing.iter().any(|e| matches!(
        e,
        CallEvent::CallUpdated {
            phase: CallPhase::Incoming,
            ..
        }
    )));
    let call = ctl.snapshot().core.calls.keys().next().cloned().unwrap();
    let t = ctl
        .execute_core(&CallCommand::Answer { call: call.clone() })
        .unwrap();
    assert_eq!(ctl.native_mut().ops(), vec![NativeOp::AcceptCall]);
    // Nothing is claimed until the phone reports the effect.
    ctl.pump();
    assert!(outcomes(&ctl.drain()).is_empty());
    ind(&mut ctl, g, Indicator::Call(true));
    ind(&mut ctl, g, Indicator::CallSetup(0));
    let ev = ctl.drain();
    assert_eq!(outcomes(&ev), vec![(t, Outcome::Confirmed)]);
    assert_eq!(ctl.snapshot().core.calls[&call].phase, CallPhase::Active);
}

#[test]
fn unconfirmed_command_times_out_as_not_observed_not_failed() {
    let (mut ctl, clock) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(1));
    let call = ctl.snapshot().core.calls.keys().next().cloned().unwrap();
    let t = ctl.execute(MacCommand::Answer { call }).unwrap();
    clock.advance(Config::default().command_timeout_ms);
    ctl.pump();
    assert_eq!(
        outcomes(&ctl.drain()),
        vec![(
            t,
            Outcome::NotObserved {
                after_ms: 8_000,
                retry_safe: true
            }
        )]
    );
    // The slot is free again.
    assert!(
        ctl.execute(MacCommand::Hangup {
            call: id("mac-g1-c1")
        })
        .is_ok()
    );
}

#[test]
fn dial_is_never_retry_safe_and_blocks_a_second_command() {
    let (mut ctl, clock) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(0));
    let t = ctl
        .execute(MacCommand::Dial {
            number: Sensitive::new("+15551234"),
        })
        .unwrap();
    assert_eq!(
        ctl.native_mut().ops(),
        vec![NativeOp::Dial("+15551234".into())]
    );
    // A second control command while the dial is unconfirmed is refused outright.
    assert_eq!(
        ctl.execute(MacCommand::Dial {
            number: Sensitive::new("+15551234")
        }),
        Err(AdapterError::Busy)
    );
    clock.advance(Config::default().dial_timeout_ms);
    ctl.pump();
    assert_eq!(
        outcomes(&ctl.drain()),
        vec![(
            t,
            Outcome::NotObserved {
                after_ms: 15_000,
                retry_safe: false
            }
        )]
    );
    assert_eq!(ctl.native_mut().performed.len(), 1, "no automatic retry");
}

#[test]
fn dial_progress_confirms_and_ends_with_the_peer() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(0));
    let t = ctl
        .execute(MacCommand::Dial {
            number: Sensitive::new("555"),
        })
        .unwrap();
    ind(&mut ctl, g, Indicator::CallSetup(2));
    let ev = ctl.drain();
    assert_eq!(outcomes(&ev), vec![(t, Outcome::Confirmed)]);
    ind(&mut ctl, g, Indicator::CallSetup(3));
    ind(&mut ctl, g, Indicator::Call(true));
    ind(&mut ctl, g, Indicator::CallSetup(0));
    let snap = ctl.snapshot();
    assert_eq!(
        snap.core.calls.values().next().unwrap().phase,
        CallPhase::Active
    );
    // The peer hangs up: the end is reported after the settle window, with no phantom call.
    ind(&mut ctl, g, Indicator::Call(false));
    assert_eq!(ctl.snapshot().core.calls.len(), 1);
}

#[test]
fn dtmf_is_dispatched_but_never_confirmed() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    ctl.drain();
    let t = ctl
        .execute(MacCommand::SendTones {
            tones: Sensitive::new("12#"),
        })
        .unwrap();
    assert_eq!(outcomes(&ctl.drain()), vec![(t, Outcome::Dispatched)]);
    assert_eq!(
        ctl.native_mut().ops(),
        vec![NativeOp::SendDtmf(vec!['1', '2', '#'])]
    );
    // DTMF does not occupy the control slot.
    assert!(ctl.execute(MacCommand::Hold).is_ok());
}

#[test]
fn waiting_call_and_ambiguous_hangup_follow_the_planner() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    ind(&mut ctl, g, Indicator::CallSetup(1));
    let snap = ctl.snapshot();
    let ids: Vec<_> = snap.core.calls.keys().cloned().collect();
    assert_eq!(ids.len(), 2);
    let (active, waiting) = (id("mac-g1-c1"), id("mac-g1-c2"));
    assert_eq!(snap.core.calls[&waiting].phase, CallPhase::Waiting);
    // Ending the active call while another rings would hit an unidentified call: refused.
    assert_eq!(
        ctl.execute(MacCommand::Hangup {
            call: active.clone()
        }),
        Err(AdapterError::Refused {
            refusal: Refusal::AmbiguousCalls
        })
    );
    // Answering the waiting call holds the active one and is confirmed by the indicators.
    let t = ctl
        .execute(MacCommand::Answer {
            call: waiting.clone(),
        })
        .unwrap();
    assert_eq!(ctl.native_mut().ops(), vec![NativeOp::HoldCall]);
    ind(&mut ctl, g, Indicator::CallHeld(1));
    ind(&mut ctl, g, Indicator::CallSetup(0));
    assert_eq!(outcomes(&ctl.drain()), vec![(t, Outcome::Confirmed)]);
    let snap = ctl.snapshot();
    assert_eq!(snap.core.calls[&active].phase, CallPhase::Held);
    assert_eq!(snap.core.calls[&waiting].phase, CallPhase::Active);
}

#[test]
fn hold_then_resume_round_trip() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    let th = ctl.execute(MacCommand::Hold).unwrap();
    ind(&mut ctl, g, Indicator::CallHeld(2));
    assert_eq!(outcomes(&ctl.drain()), vec![(th, Outcome::Confirmed)]);
    let tr = ctl.execute(MacCommand::Resume).unwrap();
    ind(&mut ctl, g, Indicator::CallHeld(0));
    assert_eq!(outcomes(&ctl.drain()), vec![(tr, Outcome::Confirmed)]);
    assert_eq!(
        ctl.native_mut().ops(),
        vec![NativeOp::HoldCall, NativeOp::HoldCall]
    );
}

#[test]
fn hold_is_refused_when_the_gateway_lacks_three_way_calling() {
    let (mut ctl, _) = make(Config::default());
    ctl.native_mut().features = AgFeatures(AgFeatures::EC_NR);
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    let r = ctl.execute(MacCommand::Hold);
    assert!(matches!(r, Err(AdapterError::Rejected { .. })), "{r:?}");
    assert!(ctl.native_mut().performed.is_empty());
}

#[test]
fn mute_commands_are_refused_with_no_platform_mapping() {
    let (mut ctl, _) = make(Config::default());
    up(&mut ctl);
    assert_eq!(
        ctl.execute_core(&CallCommand::SetMicrophoneMute { muted: true }),
        Err(AdapterError::Refused {
            refusal: Refusal::NoPlatformMapping("mute_microphone")
        })
    );
    assert_eq!(
        ctl.execute_core(&CallCommand::MuteRinger),
        Err(AdapterError::Refused {
            refusal: Refusal::NoPlatformMapping("mute_ringer")
        })
    );
}

#[test]
fn native_failure_on_dispatch_leaves_nothing_pending() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(1));
    let call = ctl.snapshot().core.calls.keys().next().cloned().unwrap();
    ctl.native_mut().fail_next_perform = Some(NativeError::other("perform", "scripted"));
    assert!(matches!(
        ctl.execute(MacCommand::Answer { call: call.clone() }),
        Err(AdapterError::Native { .. })
    ));
    assert_eq!(ctl.snapshot().pending, 0);
    assert!(ctl.execute(MacCommand::Answer { call }).is_ok());
}

#[test]
fn list_entries_enrich_without_creating_calls() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    assert_eq!(
        ctl.native_mut().call_list_requests,
        1,
        "unknown party triggers one list request"
    );
    ctl.native_mut().emit(
        g,
        NativeEvent::CurrentCall(ListEntry {
            index: Some(1),
            status: ListStatus::Active,
            incoming: None,
            number: Some(Sensitive::new("+1555")),
            name: None,
        }),
    );
    ctl.pump();
    let call = ctl.snapshot().core.calls.values().next().cloned().unwrap();
    assert_eq!(call.party.number.unwrap().expose(), "+1555");
    // Rate limited.
    ind(&mut ctl, g, Indicator::CallHeld(2));
    assert_eq!(ctl.native_mut().call_list_requests, 1);
}

// ---------------------------------------------------------------- generations, loss, staleness

#[test]
fn link_loss_ends_calls_aborts_pending_and_blocks_commands() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    let t = ctl.execute(MacCommand::Hold).unwrap();
    ctl.drain();
    ctl.native_mut()
        .emit(g, NativeEvent::SlcDisconnected(IoStatus(-1)));
    ctl.pump();
    let ev = ctl.drain();
    assert_eq!(
        outcomes(&ev),
        vec![(
            t,
            Outcome::Aborted {
                reason: DownReason::Lost { status: -1 }
            }
        )]
    );
    let calls = calls_events(&ev);
    assert!(calls.contains(&&CallEvent::Link {
        state: LinkState::Down
    }));
    let snap = ctl.snapshot();
    assert!(snap.core.calls.is_empty());
    assert_eq!(
        snap.core.recent_ended.back().unwrap().reason,
        EndReason::LinkLost
    );
    assert_eq!(snap.core.audio, AudioLinkState::Disconnected);
    assert_eq!(
        ctl.execute(MacCommand::Hold),
        Err(AdapterError::NoActiveLink)
    );
    assert!(ctl.native_mut().retired.contains(&g));
    // Capabilities fall back to unprobed.
    assert_eq!(ctl.capabilities().planes.control, Support::Unknown);
}

#[test]
fn callbacks_after_disconnect_and_from_old_generations_are_dropped() {
    let (mut ctl, clock) = make(Config::default());
    let g1 = up(&mut ctl);
    ctl.disconnect().unwrap();
    assert_eq!(ctl.phase(), Some(LinkPhase::Closing));
    // While closing, call callbacks are ignored (not applied).
    ctl.native_mut().indicator(g1, Indicator::Call(true));
    ctl.pump();
    assert!(ctl.snapshot().core.calls.is_empty());
    ctl.native_mut()
        .emit(g1, NativeEvent::SlcDisconnected(IoStatus::SUCCESS));
    ctl.pump();
    let ev = ctl.drain();
    assert!(ev.iter().any(|e| matches!(
        e,
        AdapterEvent::Link {
            notice: LinkNotice::Down(DownReason::Requested),
            ..
        }
    )));
    assert_eq!(ctl.phase(), None);

    // A callback already in flight for generation 1 arrives after reconnecting as generation 2.
    clock.advance(1);
    let g2 = up(&mut ctl);
    assert_ne!(g1, g2);
    ctl.native_mut()
        .emit_unchecked(g1, NativeEvent::Indicator(Indicator::Call(true)));
    ctl.native_mut()
        .emit_unchecked(g1, NativeEvent::SlcDisconnected(IoStatus(-1)));
    ctl.pump();
    assert_eq!(
        ctl.phase(),
        Some(LinkPhase::Up),
        "stale disconnect must not kill generation 2"
    );
    assert!(ctl.snapshot().core.calls.is_empty());
    assert_eq!(ctl.snapshot().stats.stale_events, 2);
    // Fresh events for generation 2 still work.
    ind(&mut ctl, g2, Indicator::Call(true));
    assert_eq!(ctl.snapshot().core.calls.len(), 1);
}

#[test]
fn call_ids_do_not_collide_across_generations() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    let first = ctl.snapshot().core.calls.keys().next().cloned().unwrap();
    ctl.native_mut()
        .emit(g, NativeEvent::SlcDisconnected(IoStatus::SUCCESS));
    ctl.pump();
    let g2 = up(&mut ctl);
    ind(&mut ctl, g2, Indicator::Call(true));
    let second = ctl.snapshot().core.calls.keys().next().cloned().unwrap();
    assert_ne!(first, second);
    // A stale handle from the first link can no longer be targeted.
    ind(&mut ctl, g2, Indicator::CallSetup(0));
    assert_eq!(
        ctl.execute(MacCommand::Hangup { call: first }),
        Err(AdapterError::Rejected {
            rejection: bridge_core::calls::CommandRejection::NoSuchCall
        })
    );
}

#[test]
fn close_timeout_finalises_a_link_that_never_reports_back() {
    let (mut ctl, clock) = make(Config::default());
    let g = up(&mut ctl);
    ctl.disconnect().unwrap();
    clock.advance(Config::default().close_timeout_ms);
    ctl.pump();
    assert!(ctl.drain().iter().any(|e| matches!(
        e,
        AdapterEvent::Link {
            notice: LinkNotice::Down(DownReason::CloseTimeout),
            ..
        }
    )));
    assert!(ctl.native_mut().retired.contains(&g));
    assert_eq!(ctl.disconnect(), Err(AdapterError::NoActiveLink));
}

#[test]
fn generation_budget_bounds_retained_delegates() {
    let (mut ctl, _) = make(Config {
        max_generations: 2,
        ..Config::default()
    });
    for _ in 0..2 {
        let g = ctl.connect(&addr()).unwrap();
        ctl.native_mut()
            .emit(g, NativeEvent::SlcDisconnected(IoStatus(-1)));
        ctl.pump();
    }
    assert_eq!(ctl.connect(&addr()), Err(AdapterError::TooManyLinks));
}

#[test]
fn open_failure_reports_native_error_and_leaves_no_link() {
    let (mut ctl, _) = make(Config::default());
    ctl.native_mut().fail_open = Some(NativeError::new("open_link", NativeErrorKind::InitFailed));
    assert!(matches!(
        ctl.connect(&addr()),
        Err(AdapterError::Native { .. })
    ));
    assert_eq!(ctl.phase(), None);
    assert!(ctl.connect(&addr()).is_ok());
}

#[test]
fn unhandled_results_and_malformed_payloads_are_counted_without_content() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ctl.native_mut().emit(
        g,
        NativeEvent::UnhandledResult(Sensitive::new("+CLIP: \"+15551234567\"")),
    );
    ctl.native_mut()
        .emit(g, NativeEvent::Malformed("call_setup_out_of_range"));
    ctl.pump();
    let ev = ctl.drain();
    assert!(has_diag(&ev, &Diag::UnhandledResult { len: 21 }));
    assert!(has_diag(
        &ev,
        &Diag::Malformed {
            what: "call_setup_out_of_range"
        }
    ));
    let json = serde_json::to_string(&ev).unwrap();
    assert!(!json.contains("5551234567"), "{json}");
    let s = ctl.snapshot().stats;
    assert_eq!((s.unhandled_results, s.malformed_events), (1, 1));
}

// ---------------------------------------------------------------- SCO and routing

#[test]
fn sco_open_reports_link_not_duplex_and_no_codec_guess() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    ctl.drain();
    let t = ctl
        .execute(MacCommand::ConnectAudio {
            strategy: AudioStrategy::default(),
        })
        .unwrap();
    assert_eq!(
        ctl.native_mut().ops(),
        vec![NativeOp::TransferAudioToComputer]
    );
    assert_eq!(ctl.snapshot().core.audio, AudioLinkState::Connecting);
    ctl.native_mut()
        .emit(g, NativeEvent::ScoOpened(IoStatus::SUCCESS));
    ctl.pump();
    let ev = ctl.drain();
    assert_eq!(outcomes(&ev), vec![(t, Outcome::Confirmed)]);
    assert_eq!(
        ctl.snapshot().core.audio,
        AudioLinkState::Connected {
            codec: AudioCodec::Unknown
        }
    );
    assert_eq!(ctl.capabilities().proof, AudioProof::ScoLinkReported);
    // Disconnect uses the inverse of how audio was moved.
    let t2 = ctl.execute(MacCommand::DisconnectAudio).unwrap();
    assert_eq!(
        *ctl.native_mut().ops().last().unwrap(),
        NativeOp::TransferAudioToPhone
    );
    ctl.native_mut()
        .emit(g, NativeEvent::ScoClosed(IoStatus::SUCCESS));
    ctl.pump();
    assert_eq!(outcomes(&ctl.drain()), vec![(t2, Outcome::Confirmed)]);
    assert_eq!(ctl.snapshot().core.audio, AudioLinkState::Disconnected);
}

#[test]
fn sco_failure_and_timeout_return_audio_to_disconnected() {
    let (mut ctl, clock) = make(Config::default());
    let g = up(&mut ctl);
    let t = ctl
        .execute(MacCommand::ConnectAudio {
            strategy: AudioStrategy::ConnectSco,
        })
        .unwrap();
    ctl.native_mut()
        .emit(g, NativeEvent::ScoOpened(IoStatus(-5)));
    ctl.pump();
    assert_eq!(
        outcomes(&ctl.drain()),
        vec![(t, Outcome::AudioFailed { status: -5 })]
    );
    assert_eq!(ctl.snapshot().core.audio, AudioLinkState::Disconnected);
    // And a silent attempt times out.
    let t = ctl
        .execute(MacCommand::ConnectAudio {
            strategy: AudioStrategy::ConnectSco,
        })
        .unwrap();
    clock.advance(Config::default().audio_timeout_ms);
    ctl.pump();
    assert!(matches!(outcomes(&ctl.drain())[0], (tt, Outcome::NotObserved { .. }) if tt == t));
    assert_eq!(ctl.snapshot().core.audio, AudioLinkState::Disconnected);
}

#[test]
fn audio_and_call_control_have_separate_in_flight_slots() {
    let (mut ctl, _) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::Call(true));
    ctl.execute(MacCommand::Hold).unwrap();
    assert!(
        ctl.execute(MacCommand::ConnectAudio {
            strategy: AudioStrategy::default()
        })
        .is_ok()
    );
    assert_eq!(ctl.execute(MacCommand::Resume), Err(AdapterError::Busy));
}

#[test]
fn routes_need_a_live_link_and_are_restored_when_it_ends() {
    let (mut ctl, _) = make(Config::default());
    let req = RouteRequest {
        session: "call-1".into(),
        input_uid: "bt-in".into(),
        output_uid: "bt-out".into(),
    };
    assert_eq!(ctl.route_apply(&req), Err(AdapterError::NoActiveLink));
    assert_eq!(ctl.routes_mut().port_mut().set_calls(), 0);
    let g = up(&mut ctl);
    let before = (
        ctl.routes_mut().port_mut().default_input().unwrap(),
        ctl.routes_mut().port_mut().default_output().unwrap(),
    );
    let report = ctl.route_apply(&req).unwrap();
    assert!(report.input_changed && report.output_changed);
    assert_eq!(ctl.capabilities().proof, AudioProof::RouteApplied);
    // The phone walks out of range: the route this controller made is undone.
    ctl.native_mut()
        .emit(g, NativeEvent::SlcDisconnected(IoStatus(-1)));
    ctl.pump();
    let ev = ctl.drain();
    assert!(
        ev.iter()
            .any(|e| matches!(e, AdapterEvent::RouteRestored { report } if report.complete()))
    );
    let after = (
        ctl.routes_mut().port_mut().default_input().unwrap(),
        ctl.routes_mut().port_mut().default_output().unwrap(),
    );
    assert_eq!(before, after);
    assert_eq!(ctl.capabilities().proof, AudioProof::None);
}

#[test]
fn duplex_bluetooth_device_after_sco_raises_proof_only_to_device_visible() {
    let clock = ManualClock::new();
    let mut ctl = Controller::new(
        MockNative::new(),
        MockAudio::standard().with_duplex_bluetooth("11-22-33-44-55-66"),
        clock,
        Config::default(),
    );
    let g = ctl.connect(&addr()).unwrap();
    ctl.native_mut().slc_up(g);
    ctl.pump();
    ctl.audio_devices().unwrap();
    assert_eq!(ctl.capabilities().proof, AudioProof::None, "no SCO yet");
    ctl.native_mut()
        .emit(g, NativeEvent::ScoOpened(IoStatus::SUCCESS));
    ctl.pump();
    ctl.audio_devices().unwrap();
    assert_eq!(ctl.capabilities().proof, AudioProof::RouteDeviceVisible);
    assert!(ctl.capabilities().proof < AudioProof::RouteApplied);
    let dup = ctl
        .audio_devices()
        .unwrap()
        .into_iter()
        .find(|d| d.is_duplex_bluetooth())
        .unwrap();
    assert!(dup.address_hint(&addr()));
}

#[test]
fn unexplained_active_beside_ringing_call_is_reported_but_flagged_uncertain() {
    // The core model forbids relabelling `Incoming` as `Waiting`; the adapter must survive that,
    // keep reporting the real active call, and refuse ambiguous control afterwards.
    let (mut ctl, clock) = make(Config::default());
    let g = up(&mut ctl);
    ind(&mut ctl, g, Indicator::CallSetup(1));
    ind(&mut ctl, g, Indicator::Call(true));
    clock.advance(crate::tracker::SETTLE_MS);
    ctl.pump();
    let ev = ctl.drain();
    assert!(has_diag(
        &ev,
        &Diag::CoreRejected {
            rejection: "invalid_transition"
        }
    ));
    let snap = ctl.snapshot();
    assert_eq!(
        snap.core.calls.len(),
        2,
        "the active call is still reported"
    );
    assert!(snap.identity_uncertain);
    assert_eq!(
        ctl.execute(MacCommand::Answer {
            call: id("mac-g1-c1")
        }),
        Err(AdapterError::Refused {
            refusal: Refusal::IdentityUncertain
        })
    );
}

#[test]
fn framework_default_hf_features_are_surfaced_for_the_hardware_run() {
    let (mut ctl, _) = make(Config::default());
    ctl.native_mut().default_hf_features = Some(0x24);
    ctl.connect(&addr()).unwrap();
    assert!(has_diag(
        &ctl.drain(),
        &Diag::HostFeatureDefault {
            default_hf_features: 0x24
        }
    ));
}
