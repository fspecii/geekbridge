//! API-mock tests for call-audio routing. They use only the private in-memory server in
//! `audio_support`; nothing here connects to a real sound server or plays audio.

mod audio_support;

use std::time::{Duration, Instant};

use audio_support::*;
use bridge_calls::audio::model::ProfileKind;
use bridge_calls::audio::plan::{LocalProblem, LocalRole};
use bridge_calls::audio::{
    AudioRouteManager, AudioServer, Blocker, BtAddress, ControlEvidence, DeclaredFeasibility,
    EchoFailure, EchoMethod, EchoOutcome, EchoPolicy, HardwareEvidence, Health, LocalSelection,
    ModuleRole, RecoverRoute, RoutePlan, RouteRequest, ServerError, SessionId, StartError,
    StartOptions, StopReason, UnsupportedAudioServer,
};
use bridge_calls::{AudioCodecEvidence, TransportState};
use bridge_core::calls::AudioCodec;

fn addr(s: &str) -> BtAddress {
    BtAddress::parse(s).unwrap()
}

fn active_msbc() -> ControlEvidence {
    ControlEvidence {
        transport: TransportState::Active,
        codec: AudioCodecEvidence::Known(AudioCodec::Msbc),
    }
}

fn request() -> RouteRequest {
    RouteRequest::new(addr(PHONE), active_msbc())
}

fn sid(n: u64) -> SessionId {
    SessionId::parse(&format!("{n:016x}")).unwrap()
}

fn manager(m: &Mock) -> AudioRouteManager<Mock> {
    let mut n = 0u64;
    AudioRouteManager::with_options(
        m.clone(),
        StartOptions {
            echo_ready_timeout: Duration::from_millis(80),
            poll_interval: Duration::from_millis(5),
        },
    )
    .with_session_ids(move || {
        n += 1;
        sid(n)
    })
}

fn voice_mock() -> Mock {
    Mock::new(server("headset-head-unit-msbc"))
}

fn plan_for(mgr: &mut AudioRouteManager<Mock>, req: &RouteRequest) -> RoutePlan {
    mgr.prepare(req).unwrap().expect("route should be feasible")
}

fn blockers(mgr: &mut AudioRouteManager<Mock>, req: &RouteRequest) -> Vec<Blocker> {
    mgr.prepare(req)
        .unwrap()
        .expect_err("route should not be feasible")
}

// ---------------------------------------------------------------- read-only phases

#[test]
fn inventory_assess_and_prepare_never_mutate_the_server() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let inv = mgr.inventory().unwrap();
    assert_eq!(inv.phones.len(), 1);
    mgr.assess(&request()).unwrap();
    mgr.prepare(&request()).unwrap().unwrap();
    assert_eq!(m.mutations(), 0, "{:?}", m.log());
    assert!(m.log().iter().all(|c| *c == Call::Snapshot));
}

#[test]
fn inventory_separates_a2dp_playback_from_duplex_voice() {
    let a2dp = Mock::new(server("a2dp-sink"));
    let inv = manager(&a2dp).inventory().unwrap();
    let p = &inv.phones[0];
    assert_eq!(p.active_kind, Some(ProfileKind::A2dpSink));
    assert!(p.a2dp_sink.is_some());
    assert!(
        p.voice_input.is_none() && p.voice_output.is_none(),
        "A2DP has no microphone"
    );
    assert!(p.voice_profile_available);

    let voice = voice_mock();
    let inv = manager(&voice).inventory().unwrap();
    let p = &inv.phones[0];
    assert!(p.a2dp_sink.is_none());
    assert_eq!(p.voice_input.as_ref().unwrap().name, PHONE_SRC);
    assert_eq!(p.voice_output.as_ref().unwrap().name, PHONE_SINK);
    // Local candidates exclude monitors as inputs.
    assert!(
        inv.local_inputs
            .iter()
            .all(|e| !e.id.name.ends_with(".monitor"))
    );
    assert!(inv.local_outputs.iter().any(|e| e.id.name == SPEAKER));
}

#[test]
fn feasibility_keeps_declared_and_hardware_verified_separate() {
    let m = voice_mock();
    let a = manager(&m).assess(&request()).unwrap();
    assert!(matches!(a.declared, DeclaredFeasibility::Ready { .. }));
    assert!(matches!(
        a.hardware_verified,
        HardwareEvidence::NotVerified { .. }
    ));
    assert!(matches!(
        a.plan().unwrap().hardware_verified,
        HardwareEvidence::NotVerified { .. }
    ));
}

// ---------------------------------------------------------------- profile / device checks

#[test]
fn active_transport_alone_does_not_make_an_a2dp_card_routable() {
    let m = Mock::new(server("a2dp-sink"));
    let b = blockers(&mut manager(&m), &request());
    assert_eq!(
        b,
        vec![Blocker::CardNotInVoiceProfile {
            active: Some(ProfileKind::A2dpSink),
            voice_profile_available: true
        }]
    );
    assert!(
        b.iter().all(Blocker::is_transient),
        "WirePlumber may still switch the profile"
    );
}

#[test]
fn a_card_without_any_voice_profile_is_refused_not_awaited() {
    let mut snap = server("a2dp-sink");
    snap.cards[0]
        .profiles
        .retain(|p| !p.name.starts_with("headset"));
    let a = manager(&Mock::new(snap)).assess(&request()).unwrap();
    let DeclaredFeasibility::Refused { blockers } = a.declared else {
        panic!("expected Refused")
    };
    assert!(matches!(
        blockers[0],
        Blocker::CardNotInVoiceProfile {
            voice_profile_available: false,
            ..
        }
    ));
}

#[test]
fn transport_not_active_blocks_even_with_a_voice_profile() {
    let m = voice_mock();
    let mut req = request();
    req.control.transport = TransportState::Pending;
    assert_eq!(
        blockers(&mut manager(&m), &req),
        vec![Blocker::TransportNotActive {
            state: TransportState::Pending
        }]
    );
    req.control.transport = TransportState::Error;
    let a = manager(&m).assess(&req).unwrap();
    assert!(matches!(a.declared, DeclaredFeasibility::Refused { .. }));
}

#[test]
fn codec_disagreement_between_transport_and_profile_is_reported() {
    let m = voice_mock();
    let mut req = request();
    req.control.codec = AudioCodecEvidence::Known(AudioCodec::Cvsd);
    assert_eq!(
        blockers(&mut manager(&m), &req),
        vec![Blocker::CodecMismatch {
            transport: AudioCodec::Cvsd,
            profile: bridge_calls::audio::VoiceCodec::Msbc
        }]
    );
    // No codec evidence: allowed, but flagged.
    req.control.codec = AudioCodecEvidence::None;
    let plan = plan_for(&mut manager(&m), &req);
    assert!(
        plan.warnings
            .contains(&bridge_calls::audio::Warning::CodecEvidenceMissing)
    );
}

#[test]
fn a_different_phone_address_is_not_matched_by_name_guessing() {
    let m = voice_mock();
    let req = RouteRequest::new(addr(OTHER_PHONE), active_msbc());
    assert_eq!(
        blockers(&mut manager(&m), &req),
        vec![Blocker::PhoneCardAbsent]
    );
}

#[test]
fn a_leftover_a2dp_node_on_a_voice_card_is_never_used() {
    let mut snap = server("headset-head-unit-msbc");
    snap.sinks.push(bridge_calls::audio::model::SinkInfo {
        index: 20,
        name: "bluez_output.AA_BB_CC_00_11_22.2".into(),
        description: "stale a2dp".into(),
        card: Some(7),
        owner_module: None,
        props: props(&[
            ("api.bluez5.address", PHONE),
            ("api.bluez5.profile", "a2dp-sink"),
        ]),
    });
    let b = blockers(&mut manager(&Mock::new(snap)), &request());
    assert!(matches!(
        b.as_slice(),
        [Blocker::NonVoiceEndpoint {
            reported: ProfileKind::A2dpSink,
            ..
        }]
    ));
}

#[test]
fn a_node_reporting_another_address_is_refused() {
    let mut snap = server("headset-head-unit-msbc");
    snap.sinks
        .iter_mut()
        .find(|s| s.name == PHONE_SINK)
        .unwrap()
        .props = props(&[
        ("api.bluez5.address", OTHER_PHONE),
        ("api.bluez5.profile", "headset-head-unit"),
    ]);
    let a = manager(&Mock::new(snap)).assess(&request()).unwrap();
    let DeclaredFeasibility::Refused { blockers } = a.declared else {
        panic!("expected Refused")
    };
    assert!(
        blockers
            .iter()
            .any(|b| matches!(b, Blocker::EndpointAddressMismatch { .. }))
    );
}

#[test]
fn voice_profile_without_voice_nodes_yet_is_waiting() {
    let mut snap = server("headset-head-unit-msbc");
    snap.sinks.retain(|s| s.name != PHONE_SINK);
    snap.sources.retain(|s| s.name != PHONE_SRC);
    let a = manager(&Mock::new(snap)).assess(&request()).unwrap();
    assert_eq!(
        a.declared,
        DeclaredFeasibility::Waiting {
            blockers: vec![Blocker::VoiceEndpointsMissing {
                source: true,
                sink: true
            }]
        }
    );
}

#[test]
fn stale_or_unsuitable_local_devices_are_refused() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let mut req = request();
    req.local_output = LocalSelection::Named("no_such_sink".into());
    assert_eq!(
        blockers(&mut mgr, &req),
        vec![Blocker::LocalEndpointMissing {
            role: LocalRole::Output,
            name: "no_such_sink".into()
        }]
    );

    let mut req = request();
    req.local_input = LocalSelection::Named(format!("{SPEAKER}.monitor"));
    assert!(matches!(
        blockers(&mut mgr, &req)[..],
        [Blocker::LocalEndpointInvalid {
            role: LocalRole::Input,
            problem: LocalProblem::IsMonitor,
            ..
        }]
    ));

    // The phone's own sink as "local speaker" would loop the call back into the phone.
    let mut req = request();
    req.local_output = LocalSelection::Named(PHONE_SINK.into());
    assert!(matches!(
        blockers(&mut mgr, &req)[..],
        [Blocker::LocalEndpointInvalid {
            role: LocalRole::Output,
            problem: LocalProblem::IsThePhone,
            ..
        }]
    ));

    // Names that cannot be passed safely in a module argument.
    let mut snap = server("headset-head-unit-msbc");
    snap.sinks[0].name = "evil sink' sink=x".into();
    let mut req = request();
    req.local_output = LocalSelection::Named("evil sink' sink=x".into());
    assert!(matches!(
        blockers(&mut manager(&Mock::new(snap)), &req)[..],
        [Blocker::LocalEndpointInvalid {
            problem: LocalProblem::UnsafeName,
            ..
        }]
    ));

    // Nodes created by GeekBridge itself are never local endpoints.
    let mut snap = server("headset-head-unit-msbc");
    snap.sinks[0].name = "geekbridge_0000000000000001_aec_snk".into();
    let mut req = request();
    req.local_output = LocalSelection::Named("geekbridge_0000000000000001_aec_snk".into());
    assert!(matches!(
        blockers(&mut manager(&Mock::new(snap)), &req)[..],
        [Blocker::LocalEndpointInvalid {
            problem: LocalProblem::IsGeekBridgeVirtual,
            ..
        }]
    ));
}

#[test]
fn default_devices_are_pinned_by_name_at_prepare() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    assert_eq!(
        plan.request.local_output,
        LocalSelection::Named(SPEAKER.into())
    );
    assert_eq!(plan.request.local_input, LocalSelection::Named(MIC.into()));

    let mut snap = server("headset-head-unit-msbc");
    snap.default_source = None;
    assert_eq!(
        blockers(&mut manager(&Mock::new(snap)), &request()),
        vec![Blocker::NoDefaultLocalEndpoint {
            role: LocalRole::Input
        }]
    );
}

// ---------------------------------------------------------------- RX/TX mapping

#[test]
fn plan_maps_rx_to_speaker_and_tx_to_phone_sink() {
    let m = voice_mock();
    let plan = plan_for(&mut manager(&m), &request());
    assert_eq!(
        (plan.rx.from.name.as_str(), plan.rx.to.name.as_str()),
        (PHONE_SRC, SPEAKER)
    );
    assert_eq!(
        (plan.tx.from.name.as_str(), plan.tx.to.name.as_str()),
        (MIC, PHONE_SINK)
    );

    let s = plan.specs(&sid(1), false);
    assert!(s.echo.is_none());
    assert_eq!(
        (s.rx.source.as_str(), s.rx.sink.as_str()),
        (PHONE_SRC, SPEAKER)
    );
    assert_eq!(
        (s.tx.source.as_str(), s.tx.sink.as_str()),
        (MIC, PHONE_SINK)
    );
}

#[test]
fn echo_cancel_wraps_the_local_devices_in_both_directions() {
    let m = voice_mock();
    let mut req = request();
    req.echo = EchoPolicy::Preferred(EchoMethod::WebRtc);
    let plan = plan_for(&mut manager(&m), &req);
    let session = sid(1);
    let s = plan.specs(&session, true);
    let e = s.echo.unwrap();
    assert_eq!(
        (e.source_master.as_str(), e.sink_master.as_str()),
        (MIC, SPEAKER)
    );
    // RX plays into the canceller's reference sink; TX records the cleaned source.
    assert_eq!(
        (s.rx.source.as_str(), s.rx.sink.as_str()),
        (PHONE_SRC, session.echo_sink_name().as_str())
    );
    assert_eq!(
        (s.tx.source.as_str(), s.tx.sink.as_str()),
        (session.echo_source_name().as_str(), PHONE_SINK)
    );
}

// ---------------------------------------------------------------- start / stop

#[test]
fn start_loads_two_tagged_loopbacks_in_order_and_stop_unloads_only_those() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    assert_eq!(m.mutations(), 0);
    let report = mgr.start(&plan).unwrap();

    let loads = m.loads();
    assert_eq!(loads.len(), 2, "{loads:?}");
    assert!(loads.iter().all(|(n, _)| n == "module-loopback"));
    let sess = report.session.as_str();
    assert!(
        loads[0].1.starts_with(&format!(
            "source={PHONE_SRC} sink={SPEAKER} latency_msec=50 "
        )),
        "{}",
        loads[0].1
    );
    assert!(
        loads[1]
            .1
            .starts_with(&format!("source={MIC} sink={PHONE_SINK} latency_msec=50 ")),
        "{}",
        loads[1].1
    );
    for (_, a) in &loads {
        assert!(a.contains("source_dont_move=true") && a.contains("sink_dont_move=true"));
        assert!(a.contains(&format!("geekbridge.session={sess}")));
    }
    assert_eq!(report.echo, EchoOutcome::NotRequested);
    assert!(matches!(
        report.hardware_verified,
        HardwareEvidence::NotVerified { .. }
    ));
    assert_eq!(
        mgr.active()
            .unwrap()
            .modules
            .iter()
            .map(|m| m.role)
            .collect::<Vec<_>>(),
        [ModuleRole::LoopbackRx, ModuleRole::LoopbackTx]
    );

    let stop = mgr.stop(StopReason::Requested);
    assert!(stop.teardown.is_clean());
    // Reverse order, exactly our two indices, user modules still loaded.
    assert_eq!(m.unloads(), vec![101, 100]);
    assert!(m.has_module(10) && m.has_module(11));
    assert_eq!(
        m.module_args("module-loopback"),
        vec![USER_LOOPBACK_ARG.to_string()]
    );
    assert!(mgr.active().is_none());

    // Idempotent.
    m.clear_log();
    let again = mgr.stop(StopReason::Requested);
    assert!(again.session.is_none());
    assert_eq!(m.mutations(), 0);
}

#[test]
fn start_with_echo_loads_canceller_first_and_unloads_it_last() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Required(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let report = mgr.start(&plan).unwrap();
    let names: Vec<_> = m.loads().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        ["module-echo-cancel", "module-loopback", "module-loopback"]
    );
    assert!(matches!(report.echo, EchoOutcome::Active { .. }));
    let echo_arg = &m.loads()[0].1;
    assert!(
        echo_arg.contains(&format!("source_master={MIC} sink_master={SPEAKER}"))
            && echo_arg.ends_with("aec_method=webrtc")
    );
    assert!(
        m.loads()[1]
            .1
            .contains(&format!("sink={}", report.session.echo_sink_name()))
    );
    assert!(m.loads()[2].1.contains(&format!(
        "source={} sink={PHONE_SINK}",
        report.session.echo_source_name()
    )));
    mgr.stop(StopReason::Requested);
    assert_eq!(m.unloads(), vec![102, 101, 100]);
    assert!(m.has_module(10) && m.has_module(11));
}

#[test]
fn echo_nodes_that_appear_late_are_awaited_within_the_bound() {
    let m = voice_mock();
    m.st().echo = EchoBehavior::AfterPolls(3);
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Required(EchoMethod::ServerDefault);
    let plan = plan_for(&mut mgr, &req);
    let r = mgr.start(&plan).unwrap();
    assert!(matches!(r.echo, EchoOutcome::Active { .. }));
    assert!(!m.loads()[0].1.contains("aec_method"));
}

#[test]
fn preferred_echo_that_never_materialises_falls_back_visibly_within_the_timeout() {
    let m = voice_mock();
    m.st().echo = EchoBehavior::Never;
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Preferred(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let t0 = Instant::now();
    let r = mgr.start(&plan).unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "wait must be bounded"
    );
    assert_eq!(
        r.echo,
        EchoOutcome::FellBack {
            reason: EchoFailure::NodesDidNotAppear
        }
    );
    // Echo module was removed again; loopbacks go straight to the local devices.
    let loads = m.loads();
    assert_eq!(
        loads.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
        ["module-echo-cancel", "module-loopback", "module-loopback"]
    );
    assert_eq!(m.unloads(), vec![100]);
    assert!(
        loads[1]
            .1
            .starts_with(&format!("source={PHONE_SRC} sink={SPEAKER}"))
    );
    assert!(
        loads[2]
            .1
            .starts_with(&format!("source={MIC} sink={PHONE_SINK}"))
    );
    assert_eq!(mgr.active().unwrap().modules.len(), 2);
}

#[test]
fn preferred_echo_rejected_by_the_server_falls_back_required_does_not() {
    let reject_echo = |m: &Mock| {
        m.st().load_fault = Some(Box::new(|name, _| {
            (name == "module-echo-cancel").then_some(LoadFault::Reject)
        }));
    };
    let m = voice_mock();
    reject_echo(&m);
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Preferred(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let r = mgr.start(&plan).unwrap();
    assert!(matches!(
        r.echo,
        EchoOutcome::FellBack {
            reason: EchoFailure::LoadRejected { .. }
        }
    ));
    assert_eq!(mgr.active().unwrap().modules.len(), 2);

    let m = voice_mock();
    reject_echo(&m);
    let mut mgr = manager(&m);
    req.echo = EchoPolicy::Required(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let err = mgr.start(&plan).unwrap_err();
    assert!(matches!(
        err.cause,
        StartError::EchoRequiredUnavailable(EchoFailure::LoadRejected { .. })
    ));
    assert!(mgr.active().is_none());
    assert_eq!(
        m.loads().len(),
        1,
        "no loopback may be loaded without the required canceller"
    );
}

#[test]
fn required_echo_with_foreign_owned_nodes_is_rolled_back() {
    let m = voice_mock();
    m.st().echo = EchoBehavior::ForeignOwner;
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Required(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let err = mgr.start(&plan).unwrap_err();
    assert!(matches!(
        err.cause,
        StartError::EchoRequiredUnavailable(EchoFailure::NodesNotOurs)
    ));
    assert_eq!(err.cleanup.unloaded.len(), 1);
    assert!(m.has_module(10) && m.has_module(11));
}

// ---------------------------------------------------------------- transactions

#[test]
fn failure_at_the_second_loopback_rolls_back_everything_in_reverse() {
    let m = voice_mock();
    m.st().load_fault = Some(Box::new(|_, a| {
        a.contains("geekbridge.path=tx")
            .then_some(LoadFault::Reject)
    }));
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Required(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    let err = mgr.start(&plan).unwrap_err();
    assert!(matches!(
        err.cause,
        StartError::ModuleLoad(ModuleRole::LoopbackTx, _)
    ));
    assert_eq!(m.unloads(), vec![101, 100], "RX then echo, newest first");
    assert!(mgr.active().is_none() && mgr.cleanup_debt().is_empty());
    assert_eq!(
        m.module_names(),
        vec![
            (10, "module-loopback".into()),
            (11, "module-null-sink".into())
        ]
    );
    assert!(err.cleanup.is_clean());
}

#[test]
fn a_load_that_timed_out_but_succeeded_is_found_by_its_tag_and_removed() {
    let m = voice_mock();
    m.st().load_fault = Some(Box::new(|_, a| {
        a.contains("geekbridge.path=tx")
            .then_some(LoadFault::TimeoutButLoaded)
    }));
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    let err = mgr.start(&plan).unwrap_err();
    assert!(matches!(
        err.cause,
        StartError::ModuleLoad(ModuleRole::LoopbackTx, ServerError::Timeout { .. })
    ));
    assert_eq!(
        err.cleanup.swept.len(),
        1,
        "unknown-outcome module swept by tag"
    );
    assert_eq!(
        m.module_names(),
        vec![
            (10, "module-loopback".into()),
            (11, "module-null-sink".into())
        ]
    );
    assert!(mgr.cleanup_debt().is_empty());
}

#[test]
fn rollback_that_cannot_unload_keeps_debt_blocks_new_starts_and_retries() {
    let m = voice_mock();
    m.st().load_fault = Some(Box::new(|_, a| {
        a.contains("geekbridge.path=tx")
            .then_some(LoadFault::Reject)
    }));
    m.st().fail_unload.insert(100);
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    let err = mgr.start(&plan).unwrap_err();
    assert!(!err.cleanup.is_clean());
    assert_eq!(mgr.cleanup_debt().len(), 1);
    assert!(m.has_module(100));

    // A new start is refused while debt remains (and does not pile on more modules).
    m.st().load_fault = None;
    let loads_before = m.loads().len();
    let err = mgr.start(&plan).unwrap_err();
    assert!(matches!(err.cause, StartError::CleanupPending(1)));
    assert_eq!(m.loads().len(), loads_before);

    // The fault clears; stop() retries the debt.
    m.st().fail_unload.clear();
    let stop = mgr.stop(StopReason::Requested);
    assert!(stop.teardown.is_clean());
    assert!(mgr.cleanup_debt().is_empty() && !m.has_module(100));
    assert!(m.has_module(10) && m.has_module(11));
    mgr.start(&plan).unwrap();
}

#[test]
fn start_refuses_stale_or_unfeasible_plans_without_loading() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());

    // Devices replaced since prepare (same names, new indices).
    m.st()
        .snap
        .sinks
        .iter_mut()
        .find(|s| s.name == PHONE_SINK)
        .unwrap()
        .index = 99;
    assert!(matches!(
        mgr.start(&plan).unwrap_err().cause,
        StartError::StalePlan(_)
    ));

    // Server instance replaced.
    let m2 = voice_mock();
    let mut mgr2 = manager(&m2);
    let plan2 = plan_for(&mut mgr2, &request());
    m2.st().snap.identity.cookie = 1;
    assert!(matches!(
        mgr2.start(&plan2).unwrap_err().cause,
        StartError::StalePlan(_)
    ));

    // Profile fell back to A2DP: not feasible any more.
    let m3 = voice_mock();
    let mut mgr3 = manager(&m3);
    let plan3 = plan_for(&mut mgr3, &request());
    m3.st().snap = server("a2dp-sink");
    assert!(matches!(
        mgr3.start(&plan3).unwrap_err().cause,
        StartError::NotFeasible(_)
    ));

    assert_eq!(m.mutations() + m2.mutations() + m3.mutations(), 0);
}

#[test]
fn only_one_route_at_a_time_and_a_second_start_loads_nothing() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    mgr.start(&plan).unwrap();
    let before = m.loads().len();
    assert!(matches!(
        mgr.start(&plan).unwrap_err().cause,
        StartError::AlreadyActive
    ));
    assert_eq!(m.loads().len(), before);
}

#[test]
fn a_peer_that_vanishes_during_start_aborts_and_cleans_up() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    // After the TX loopback loads, the phone's card leaves the voice profile (SCO closed).
    m.st().after_load = Some(Box::new(|snap, arg| {
        if arg.contains("geekbridge.path=tx") {
            snap.cards[0].active_profile = Some("a2dp-sink".into());
        }
    }));
    let err = mgr.start(&plan).unwrap_err();
    assert!(
        matches!(err.cause, StartError::PeerDisappeared(_)),
        "{:?}",
        err.cause
    );
    assert_eq!(m.unloads(), vec![101, 100]);
    assert!(mgr.active().is_none() && mgr.cleanup_debt().is_empty());
    assert!(m.has_module(10) && m.has_module(11));
}

#[test]
fn a_loopback_that_unloads_itself_during_start_aborts() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    m.st().after_load = Some(Box::new(|snap, arg| {
        if arg.contains("geekbridge.path=tx") {
            snap.modules.retain(|x| x.index != 100);
        }
    }));
    let err = mgr.start(&plan).unwrap_err();
    assert!(
        matches!(
            err.cause,
            StartError::ModuleVanished(ModuleRole::LoopbackRx)
        ),
        "{:?}",
        err.cause
    );
    assert_eq!(m.unloads(), vec![101]);
}

// ---------------------------------------------------------------- peer disappearance

fn started() -> (Mock, AudioRouteManager<Mock>, RoutePlan) {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    mgr.start(&plan).unwrap();
    m.clear_log();
    (m, mgr, plan)
}

#[test]
fn reconcile_is_quiet_while_everything_is_present() {
    let (m, mut mgr, _) = started();
    assert!(matches!(mgr.reconcile(), Health::Healthy));
    assert_eq!(m.mutations(), 0);
}

#[test]
fn phone_endpoints_disappearing_ends_the_route_and_cleans_only_ours() {
    let (m, mut mgr, _) = started();
    m.vanish_phone_nodes();
    let Health::Ended(stop) = mgr.reconcile() else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::PhoneAudioLost);
    assert_eq!(m.unloads(), vec![101, 100]);
    assert!(m.has_module(10) && m.has_module(11));
    assert!(mgr.active().is_none());
}

#[test]
fn a_loopback_that_unloaded_itself_ends_the_route_and_removes_its_sibling() {
    let (m, mut mgr, _) = started();
    m.vanish_module(100);
    let Health::Ended(stop) = mgr.reconcile() else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::ModuleVanished);
    assert_eq!(stop.teardown.already_gone.len(), 1);
    assert_eq!(m.unloads(), vec![101], "only the survivor is unloaded");
    assert!(m.has_module(10) && m.has_module(11));
}

#[test]
fn profile_leaving_voice_ends_the_route() {
    let (m, mut mgr, _) = started();
    m.st().snap.cards[0].active_profile = Some("a2dp-sink".into());
    let Health::Ended(stop) = mgr.reconcile() else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::PhoneAudioLost);
    assert!(mgr.active().is_none());
}

#[test]
fn local_device_disappearing_is_reported_as_such() {
    let (m, mut mgr, _) = started();
    m.st().snap.sinks.retain(|s| s.name != SPEAKER);
    let Health::Ended(stop) = mgr.reconcile() else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::LocalEndpointLost);
}

#[test]
fn link_loss_for_another_phone_is_ignored() {
    let (m, mut mgr, _) = started();
    assert!(mgr.notify_link_lost(addr(OTHER_PHONE)).is_none());
    assert!(mgr.active().is_some());
    assert_eq!(m.mutations(), 0);
    let stop = mgr.notify_link_lost(addr(PHONE)).unwrap();
    assert_eq!(stop.reason, StopReason::LinkLost);
    assert_eq!(m.unloads(), vec![101, 100]);
    assert!(m.has_module(10) && m.has_module(11));
}

#[test]
fn transport_going_down_ends_the_route_active_does_not() {
    let (m, mut mgr, _) = started();
    assert!(mgr.notify_transport(addr(PHONE), active_msbc()).is_none());
    let mut idle = active_msbc();
    idle.transport = TransportState::Idle;
    assert!(mgr.notify_transport(addr(OTHER_PHONE), idle).is_none());
    let stop = mgr.notify_transport(addr(PHONE), idle).unwrap();
    assert_eq!(stop.reason, StopReason::TransportDown);
    assert_eq!(m.unloads(), vec![101, 100]);
}

// ---------------------------------------------------------------- ownership safety

#[test]
fn a_module_that_took_over_our_index_is_never_unloaded() {
    let (m, mut mgr, _) = started();
    // The user's module now sits at index 101 (server restart reusing counters, say).
    m.st()
        .snap
        .modules
        .iter_mut()
        .find(|x| x.index == 101)
        .unwrap()
        .argument = Some("source=a sink=b".into());
    let stop = mgr.stop(StopReason::Requested);
    assert_eq!(stop.teardown.left_untouched.len(), 1);
    assert_eq!(m.unloads(), vec![100]);
    assert!(m.has_module(101), "the foreign module must survive");

    // Different module name at our index: same outcome.
    let (m, mut mgr, _) = started();
    m.st()
        .snap
        .modules
        .iter_mut()
        .find(|x| x.index == 100)
        .unwrap()
        .name = "module-null-sink".into();
    mgr.stop(StopReason::Requested);
    assert_eq!(m.unloads(), vec![101]);
    assert!(m.has_module(100));
}

#[test]
fn modules_with_hidden_arguments_are_unloaded_only_on_the_same_connection() {
    let m = voice_mock();
    m.st().report_arguments = false;
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    mgr.start(&plan).unwrap();
    let stop = mgr.stop(StopReason::Requested);
    assert_eq!(
        stop.teardown.unloaded.len(),
        2,
        "same connection: the index we were handed is trusted"
    );
}

// ---------------------------------------------------------------- server loss and reconnect

#[test]
fn connection_loss_then_same_instance_adopts_without_duplicating_modules() {
    let (m, mut mgr, _) = started();
    m.st().down = true;
    assert!(matches!(
        mgr.reconcile(),
        Health::ServerUnreachable(ServerError::ConnectionLost(_))
    ));
    assert!(
        mgr.active().is_some(),
        "the route is kept while the server cannot be read"
    );
    m.st().down = false;
    let report = mgr.recover().unwrap();
    assert!(matches!(report.route, RecoverRoute::Adopted));
    assert!(
        m.loads().is_empty(),
        "adoption never loads modules: {:?}",
        m.log()
    );
    assert!(matches!(mgr.reconcile(), Health::Healthy));
    mgr.stop(StopReason::Requested);
    assert_eq!(m.unloads(), vec![101, 100]);
}

#[test]
fn server_restart_ends_the_route_and_leaves_a_new_servers_modules_alone() {
    let (m, mut mgr, _) = started();
    m.st().down = true;
    assert!(matches!(mgr.reconcile(), Health::ServerUnreachable(_)));
    m.restart(0xBEEF);
    // The restarted server has the user's own loopback at an index that collides with ours.
    m.st()
        .snap
        .modules
        .push(bridge_calls::audio::model::ModuleInfo {
            index: 100,
            name: "module-loopback".into(),
            argument: Some(USER_LOOPBACK_ARG.into()),
        });
    let report = mgr.recover().unwrap();
    let RecoverRoute::Ended(stop) = report.route else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::ServerRestarted);
    assert!(
        m.unloads().is_empty(),
        "no index from the old server may be unloaded: {:?}",
        m.log()
    );
    assert!(m.has_module(100));
    assert!(mgr.active().is_none());
    // A fresh explicit start works and loads exactly two modules (no duplicates).
    let plan = plan_for(&mut mgr, &request());
    m.clear_log();
    mgr.start(&plan).unwrap();
    assert_eq!(m.loads().len(), 2);
}

#[test]
fn unprovable_server_identity_never_trusts_hidden_modules_after_reconnect() {
    let m = voice_mock();
    m.st().snap.identity.cookie = 0;
    m.st().report_arguments = false;
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    mgr.start(&plan).unwrap();
    m.clear_log();
    m.st().down = true;
    assert!(matches!(mgr.reconcile(), Health::ServerUnreachable(_)));
    m.st().down = false;
    let RecoverRoute::Ended(stop) = mgr.recover().unwrap().route else {
        panic!("expected Ended")
    };
    assert_eq!(stop.reason, StopReason::ServerRestarted);
    assert!(m.unloads().is_empty());
    assert_eq!(stop.teardown.left_untouched.len(), 2);
}

#[test]
fn recover_reports_a_server_that_is_still_gone() {
    let (m, mut mgr, _) = started();
    m.st().down = true;
    m.st().reconnect_fails = true;
    assert!(matches!(mgr.recover(), Err(ServerError::Unavailable(_))));
    assert!(mgr.active().is_some());
}

#[test]
fn stop_with_the_server_gone_keeps_debt_and_never_claims_success() {
    let (m, mut mgr, _) = started();
    m.st().down = true;
    let stop = mgr.stop(StopReason::LinkLost);
    assert!(!stop.teardown.is_clean());
    assert_eq!(mgr.cleanup_debt().len(), 2);
    m.st().down = false;
    let report = mgr.recover().unwrap();
    assert_eq!(report.debt_remaining, 0);
    assert!(m.has_module(10) && m.has_module(11));
    assert!(!m.has_module(100) && !m.has_module(101));
}

#[test]
fn unload_failing_midway_reports_partial_cleanup() {
    let (m, mut mgr, _) = started();
    m.st().fail_unload.insert(101);
    let stop = mgr.stop(StopReason::Requested);
    assert_eq!(stop.teardown.failed.len(), 1);
    assert_eq!(stop.teardown.unloaded.len(), 1);
    assert_eq!(mgr.cleanup_debt().len(), 1);
}

// ---------------------------------------------------------------- orphans, drop, platform

#[test]
fn orphans_from_dead_sessions_are_listed_and_only_those_are_reaped() {
    let m = voice_mock();
    let orphan = format!(
        "source=x sink=y latency_msec=50 sink_input_properties='geekbridge.session={} geekbridge.path=rx'",
        sid(0xdead).as_str()
    );
    m.st()
        .snap
        .modules
        .push(bridge_calls::audio::model::ModuleInfo {
            index: 50,
            name: "module-loopback".into(),
            argument: Some(orphan),
        });
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    mgr.start(&plan).unwrap();
    let inv = mgr.inventory().unwrap();
    assert_eq!(
        inv.orphans.len(),
        1,
        "live session and user modules are not orphans: {:?}",
        inv.orphans
    );
    assert_eq!(inv.orphans[0].index, 50);
    m.clear_log();
    let report = mgr.reap_orphans(&inv.orphans);
    assert!(report.is_clean());
    assert_eq!(m.unloads(), vec![50]);
    assert!(m.has_module(10) && m.has_module(100) && m.has_module(101));
}

#[test]
fn reap_refuses_modules_that_are_no_longer_orphans() {
    let m = voice_mock();
    let mut mgr = manager(&m);
    let plan = plan_for(&mut mgr, &request());
    let report = mgr.start(&plan).unwrap();
    // Forge an "orphan" entry that actually points at our live module.
    let fake = bridge_calls::audio::OrphanModule {
        index: 100,
        module: "module-loopback".into(),
        session: report.session,
    };
    let r = mgr.reap_orphans(&[fake]);
    assert_eq!(r.left_untouched.len(), 1);
    assert!(m.has_module(100));
}

#[test]
fn dropping_the_manager_unloads_what_it_still_owns() {
    let m = voice_mock();
    {
        let mut mgr = manager(&m);
        let plan = plan_for(&mut mgr, &request());
        mgr.start(&plan).unwrap();
        assert!(m.has_module(100));
    }
    assert!(!m.has_module(100) && !m.has_module(101));
    assert!(m.has_module(10) && m.has_module(11));
}

#[test]
fn the_server_trait_has_no_way_to_change_defaults_or_move_streams() {
    // Everything a full session does is visible in the call log: reads, load and unload only.
    let m = voice_mock();
    let mut mgr = manager(&m);
    let mut req = request();
    req.echo = EchoPolicy::Required(EchoMethod::WebRtc);
    let plan = plan_for(&mut mgr, &req);
    mgr.start(&plan).unwrap();
    mgr.stop(StopReason::Requested);
    assert!(
        m.log()
            .iter()
            .all(|c| matches!(c, Call::Snapshot | Call::Load(..) | Call::Unload(_)))
    );
    assert_eq!(m.st().snap.default_sink.as_deref(), Some(SPEAKER));
    assert_eq!(m.st().snap.default_source.as_deref(), Some(MIC));
    assert_eq!(
        m.st().snap.cards[0].active_profile.as_deref(),
        Some("headset-head-unit-msbc")
    );
}

#[test]
fn unsupported_platform_server_fails_every_operation_explicitly() {
    let mut s = UnsupportedAudioServer::new("windows");
    assert!(matches!(s.snapshot(), Err(ServerError::Unavailable(m)) if m.contains("windows")));
    assert!(s.load_module("module-loopback", "").is_err());
    assert!(s.unload_module(1).is_err());
    assert!(s.reconnect().is_err());
    let mut mgr = AudioRouteManager::new(UnsupportedAudioServer::new("macos"));
    assert!(mgr.inventory().is_err());
    assert!(mgr.prepare(&request()).is_err());
}
