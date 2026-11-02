// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop-side end-to-end scenarios: the real `bridge-calls` Session over PipeWireBackend, against
//! the simulator service on a private `dbus-daemon`, with an in-memory `FakeModem` as the phone.
//!
//! Only call signalling is simulated. Nothing here exercises Bluetooth, SCO audio or the AT
//! command exchange, so a pass says nothing about audio or hardware behaviour.
//!
//! Skips loudly when `dbus-daemon` is missing; set `GEEKBRIDGE_REQUIRE_DBUS=1` to fail instead.
//! `emulator_e2e` runs only when `GEEKBRIDGE_EMULATOR_SERIAL` is set.
#![cfg(target_os = "linux")]

#[path = "support/scenarios.rs"]
mod scenarios;

use std::sync::Arc;
use std::time::Duration;

use bridge_phone_sim::{AdbModem, FakeModem, Modem};
use scenarios::{
    BusStartError, Caps, DEFAULT_NUMBER, EMULATOR_NUMBER, Options, Phone, PrivateBus, SCENARIOS,
    ScenarioReport, SelfContained, Status, render_json, run_selected,
};

fn options() -> Options {
    Options {
        // Generous so a loaded CI machine does not flake; a healthy run takes milliseconds.
        step_timeout: Duration::from_secs(20),
        ..Options::default()
    }
}

/// A fresh private bus and simulator over a new `FakeModem`, or `None` (loudly) without
/// `dbus-daemon`.
fn fake_env() -> Option<SelfContained> {
    let modem = FakeModem::new();
    let sim_side: Arc<dyn Modem> = Arc::new(modem.clone());
    let phone_side: Arc<dyn Phone> = Arc::new(modem);
    match PrivateBus::start() {
        Err(BusStartError::Missing(e)) => {
            if std::env::var("GEEKBRIDGE_REQUIRE_DBUS").as_deref() == Ok("1") {
                panic!("dbus-daemon required but unavailable: {e}");
            }
            eprintln!(
                "SKIPPED: dbus-daemon not available ({e}); set GEEKBRIDGE_REQUIRE_DBUS=1 to fail instead"
            );
            return None;
        }
        Err(e) => panic!("{e}"),
        Ok(probe) => drop(probe),
    }
    Some(SelfContained::start(sim_side, phone_side).expect("start the simulator on a private bus"))
}

fn print_and_assert(reports: &[ScenarioReport]) {
    for r in reports {
        for s in &r.steps {
            eprintln!("{}", s.line());
        }
        eprintln!("{}", r.summary_line());
    }
    let failed: Vec<String> = reports
        .iter()
        .filter(|r| r.status == Status::Fail)
        .map(|r| {
            let step = r.failing_step().map(|s| s.line()).unwrap_or_default();
            format!("{}: {step}", r.id)
        })
        .collect();
    assert!(
        failed.is_empty(),
        "failed scenarios:\n{}",
        failed.join("\n")
    );
}

fn run_one(id: &str) {
    let Some(env) = fake_env() else { return };
    let mut h = env.harness(options());
    let reports = run_selected(&mut h, &[id.to_string()]);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].status, Status::Pass, "{id} did not pass");
    print_and_assert(&reports);
}

#[test]
fn s1_incoming_answered() {
    run_one("S1");
}

#[test]
fn s2_incoming_rejected() {
    run_one("S2");
}

#[test]
fn s3_missed() {
    run_one("S3");
}

#[test]
fn s4_outgoing() {
    run_one("S4");
}

#[test]
fn s5_outgoing_cancelled_by_desktop() {
    run_one("S5");
}

#[test]
fn s6_guard_rails() {
    run_one("S6");
}

#[test]
fn s7_link_loss() {
    run_one("S7");
}

#[test]
fn s8_service_hub() {
    run_one("S8");
}

/// All scenarios back to back on one simulator: each must leave the stack clean for the next,
/// and call-object paths the simulator reuses must not confuse the session.
#[test]
fn all_scenarios_share_one_simulator() {
    let Some(env) = fake_env() else { return };
    let mut h = env.harness(options());
    let reports = run_selected(&mut h, &[]);
    assert_eq!(reports.len(), SCENARIOS.len());
    print_and_assert(&reports);
}

/// The same scenarios twice in a row, to catch state that survives a scenario.
#[test]
fn scenarios_are_repeatable() {
    let Some(env) = fake_env() else { return };
    let mut h = env.harness(options());
    let only: Vec<String> = ["S1", "S2", "S3", "S4", "S5", "S6"]
        .map(String::from)
        .into();
    for _ in 0..2 {
        print_and_assert(&run_selected(&mut h, &only));
    }
}

#[test]
fn reports_never_contain_the_full_number() {
    let Some(env) = fake_env() else { return };
    let mut h = env.harness(options());
    let reports = run_selected(&mut h, &[]);
    print_and_assert(&reports);
    let mut text = render_json(&reports);
    for r in &reports {
        text.push_str(&r.summary_line());
        for s in &r.steps {
            text.push_str(&s.line());
        }
    }
    assert!(
        !text.contains(DEFAULT_NUMBER),
        "a report leaked the full number"
    );
    assert!(text.contains("\"simulated_signalling_only\":true"));
}

#[test]
fn a_wrong_expectation_fails_with_the_failing_step() {
    // Prove the harness can fail: with a 150 ms deadline and no simulator at all, the very first
    // precondition cannot hold and must be reported as the failing step.
    let Some(env) = fake_env() else { return };
    let mut h = env.harness(Options {
        step_timeout: Duration::from_millis(150),
        ..Options::default()
    });
    // Take the service away before anything runs.
    (h.service_killer.as_mut().expect("self-contained harness"))();
    let reports = run_selected(&mut h, &["S1".to_string()]);
    assert_eq!(reports[0].status, Status::Fail);
    let step = reports[0].failing_step().expect("a failing step");
    assert!(step.step.starts_with("precondition"), "{}", step.line());
    assert!(
        step.error.as_deref().unwrap_or("").contains("timed out"),
        "{}",
        step.line()
    );
}

/// Real emulator: a private bus with an in-process simulator driven by `AdbModem`, the desktop
/// stack as the real code. Runs only when `GEEKBRIDGE_EMULATOR_SERIAL` is set; optional
/// `GEEKBRIDGE_ADB` (default `adb`) and `GEEKBRIDGE_ADB_PORT`.
#[test]
fn emulator_e2e() {
    let Ok(serial) = std::env::var("GEEKBRIDGE_EMULATOR_SERIAL") else {
        eprintln!("SKIPPED: GEEKBRIDGE_EMULATOR_SERIAL is not set");
        return;
    };
    let adb = std::env::var("GEEKBRIDGE_ADB").unwrap_or_else(|_| "adb".to_string());
    let port = std::env::var("GEEKBRIDGE_ADB_PORT")
        .ok()
        .map(|p| p.parse::<u16>().expect("GEEKBRIDGE_ADB_PORT is a port"));
    let modem = Arc::new(AdbModem::new(adb.into(), serial, port));
    let sim_side: Arc<dyn Modem> = modem.clone();
    let phone_side: Arc<dyn Phone> = modem;
    let env = match SelfContained::start_with_interval(
        sim_side,
        phone_side,
        Duration::from_millis(100),
    ) {
        Ok(env) => env,
        Err(e) => panic!("cannot start the simulator on a private bus: {e}"),
    };
    let mut h = env.harness(Options {
        step_timeout: Duration::from_secs(30),
        number: EMULATOR_NUMBER.to_string(),
        caps: Caps::EMULATOR,
    });
    print_and_assert(&run_selected(&mut h, &[]));
}
