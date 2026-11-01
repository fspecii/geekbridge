//! Actor behaviour: bounded queue, deadlines, cancelled-while-queued guarantee, watch and events.
use bridge_calls_windows::engine::*;
use bridge_calls_windows::model::*;
use bridge_calls_windows::platform::Signal;
use bridge_calls_windows::service::*;
use bridge_calls_windows::testing::*;
use bridge_core::calls::*;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn config() -> ServiceConfig {
    ServiceConfig {
        engine: EngineConfig {
            session: 1,
            timeouts: Timeouts {
                step: ms(100),
                refresh: ms(200),
                access: ms(200),
                connect: ms(200),
                command: ms(100),
            },
            ..EngineConfig::default()
        },
        queue: 4,
        event_queue: 8,
        poll_interval: ms(50),
        debounce: ms(20),
        margin: ms(50),
    }
}

fn start(mock: &MockPlatform, cfg: ServiceConfig) -> CallsService {
    let m = mock.clone();
    CallsService::spawn(move || Ok(m), cfg).unwrap()
}

fn count(mock: &MockPlatform, name: &str) -> usize {
    mock.log().iter().filter(|l| l.starts_with(name)).count()
}

#[test]
fn refresh_select_register_connect_through_the_handle() {
    let mock = MockPlatform::fresh();
    let service = start(&mock, config());
    let h = service.handle();
    let snap = h.refresh().unwrap();
    assert_eq!(snap.devices.len(), 1);
    assert!(mock.mutations().is_empty(), "refresh is read-only");

    let id = snap.devices[0].id.clone();
    h.select(&id).unwrap();
    assert_eq!(h.register().unwrap(), RegisterOutcome::Registered);
    let line = line_for(&id, 1, true);
    mock.with(|s| s.line_on_connect = Some(line));
    assert!(h.connect().unwrap().line_visible);
    assert_eq!(
        mock.mutations(),
        vec!["request_access", "register", "connect"]
    );
}

#[test]
fn refusals_come_back_typed_through_the_actor() {
    let mock = MockPlatform::fresh();
    let service = start(&mock, config());
    let h = service.handle();
    match h.register() {
        Err(ServiceError::Calls {
            error:
                CallsError::Refused {
                    refusal: Refusal::NoDeviceSelected,
                },
        }) => {}
        other => panic!("{other:?}"),
    }
    assert!(mock.mutations().is_empty());
}

#[test]
fn platform_is_created_on_the_actor_thread_and_only_used_there() {
    let caller = std::thread::current().id();
    let factory_thread = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = factory_thread.clone();
    let mock = MockPlatform::fresh();
    let m = mock.clone();
    let service = CallsService::spawn(
        move || {
            *seen.lock().unwrap() = Some(std::thread::current().id());
            Ok(m)
        },
        config(),
    )
    .unwrap();
    service.handle().refresh().unwrap();
    let on = factory_thread.lock().unwrap().unwrap();
    assert_ne!(on, caller);
}

#[test]
fn factory_failure_is_returned_from_spawn() {
    let err = CallsService::spawn(
        || -> Result<MockPlatform, PlatformError> {
            Err(PlatformError::new(PlatformErrorKind::Os, "RoInitialize"))
        },
        config(),
    )
    .err()
    .unwrap();
    assert_eq!(err.kind, PlatformErrorKind::Os);
}

#[test]
fn a_command_that_outlives_its_deadline_is_reported_uncertain() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let service = start(&mock, config());
    let h = service.handle();
    let id = mock.first_id();
    h.select(&id).unwrap();
    let r = h.refresh().unwrap().devices[0].calls[0].reference.clone();

    // Budget for Execute = 2*200 + 2*100 + 50 = 650 ms; the operation takes longer.
    mock.with(|s| s.call_op_delay = Some(ms(1000)));
    let started = Instant::now();
    let result = h.execute(WinCommand::Answer(r));
    assert_eq!(
        result.unwrap_err(),
        ServiceError::TimedOut {
            executed: Executed::Maybe
        }
    );
    assert!(
        started.elapsed() < ms(950),
        "caller is not held past its budget"
    );
}

#[test]
fn a_request_cancelled_while_queued_never_runs() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let service = start(&mock, config());
    let h = service.handle();
    h.select(&mock.first_id()).unwrap();
    let r = h.refresh().unwrap().devices[0].calls[0].reference.clone();

    mock.with(|s| s.call_op_delay = Some(ms(1200)));
    let stuck = {
        let h = h.clone();
        std::thread::spawn(move || h.execute(WinCommand::Answer(r)))
    };
    std::thread::sleep(ms(100)); // the actor is now inside the delayed operation
    let before = count(&mock, "environment");

    // This refresh queues behind the stuck operation and its caller gives up first.
    let err = h.refresh().unwrap_err();
    assert_eq!(
        err,
        ServiceError::TimedOut {
            executed: Executed::No
        }
    );

    let stuck = stuck.join().unwrap();
    assert!(
        matches!(stuck, Err(ServiceError::TimedOut { .. })),
        "{stuck:?}"
    );
    std::thread::sleep(ms(1300)); // let the actor finish the first command and drain the queue
    // `before` was taken inside the stuck command, after its first refresh. Only its closing
    // refresh may follow; the cancelled request's refresh would make it two.
    assert_eq!(count(&mock, "environment") - before, 1);
}

#[test]
fn a_full_queue_is_refused_not_blocked() {
    let mock = MockPlatform::connected();
    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    let mut cfg = config();
    cfg.queue = 1;
    let service = start(&mock, cfg);
    let h = service.handle();
    h.select(&mock.first_id()).unwrap();
    let r = h.refresh().unwrap().devices[0].calls[0].reference.clone();

    mock.with(|s| s.call_op_delay = Some(ms(800)));
    let busy = {
        let h = h.clone();
        std::thread::spawn(move || h.execute(WinCommand::Answer(r)))
    };
    std::thread::sleep(ms(100));
    let waiting = {
        let h = h.clone();
        std::thread::spawn(move || h.refresh())
    };
    std::thread::sleep(ms(100));
    let started = Instant::now();
    assert_eq!(h.refresh().unwrap_err(), ServiceError::QueueFull);
    assert!(started.elapsed() < ms(50), "QueueFull must not wait");
    let _ = busy.join();
    let _ = waiting.join();
}

#[test]
fn requests_after_shutdown_report_stopped() {
    let mock = MockPlatform::fresh();
    let service = start(&mock, config());
    let h = service.handle();
    drop(service);
    assert_eq!(h.refresh().unwrap_err(), ServiceError::Stopped);
}

#[test]
fn watching_turns_os_signals_into_events_and_stops_cleanly() {
    let mock = MockPlatform::connected();
    let service = start(&mock, config());
    let h = service.handle();
    h.select(&mock.first_id()).unwrap();
    let events = h.subscribe();
    h.watch(true).unwrap();
    // Drain the events from starting to watch.
    while events.recv_timeout(ms(150)).is_ok() {}

    mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
    mock.fire(Signal::CallsChanged);
    let mut saw_ringing = false;
    let end = Instant::now() + ms(1000);
    while Instant::now() < end && !saw_ringing {
        match events.recv_timeout(ms(100)) {
            Ok(ServiceEvent::Core {
                events,
                capabilities,
            }) => {
                saw_ringing = events.iter().any(|e| {
                    matches!(
                        e,
                        CallEvent::CallUpdated {
                            phase: CallPhase::Incoming,
                            ..
                        }
                    )
                });
                assert_eq!(capabilities.support(CallFeature::Answer), Support::Declared);
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_ringing, "a signal should surface the ringing call");
    assert!(mock.mutations().is_empty(), "watching never acts");

    h.watch(false).unwrap();
    assert!(mock.lock().sink.is_none());
}

#[test]
fn a_slow_subscriber_is_reset_rather_than_silently_missing_events() {
    let mock = MockPlatform::connected();
    let mut cfg = config();
    cfg.event_queue = 1;
    let service = start(&mock, cfg);
    let h = service.handle();
    h.select(&mock.first_id()).unwrap();
    let events = h.subscribe();

    // Cause several core changes without reading from `events`.
    for status in [
        NativeCallStatus::Incoming,
        NativeCallStatus::Talking,
        NativeCallStatus::Held,
    ] {
        mock.set_calls(vec![call("c1", status)]);
        h.refresh().unwrap();
    }
    mock.set_calls(vec![]);
    h.refresh().unwrap();
    h.refresh().unwrap();

    // The subscriber now reads. What it missed is flagged and repaired on the next publication.
    while events.recv_timeout(ms(100)).is_ok() {}
    mock.set_calls(vec![call("c2", NativeCallStatus::Incoming)]);
    h.refresh().unwrap();
    let mut saw_reset = false;
    while let Ok(ev) = events.recv_timeout(ms(100)) {
        if let ServiceEvent::CoreReset { state } = ev {
            saw_reset = true;
            assert!(state.calls.len() <= 1);
        }
    }
    assert!(saw_reset);
}

#[test]
fn spawn_default_off_windows_reports_not_windows_and_acts_on_nothing() {
    if cfg!(windows) {
        return;
    }
    let service = bridge_calls_windows::spawn_default(ServiceConfig::default()).unwrap();
    let h = service.handle();
    let snap = h.refresh().unwrap();
    assert_eq!(
        snap.env_blockers,
        vec![bridge_calls_windows::readiness::Blocker::NotWindows]
    );
    assert!(matches!(
        h.register(),
        Err(ServiceError::Calls {
            error: CallsError::Refused { .. }
        })
    ));
    let caps = h.capabilities().unwrap();
    assert!(matches!(
        caps.support(CallFeature::Answer),
        Support::Unsupported { .. }
    ));
}
