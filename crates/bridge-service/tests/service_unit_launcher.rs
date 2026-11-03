// SPDX-License-Identifier: GPL-3.0-or-later
//! The shipped systemd user unit and the autostart launcher on an Omarchy-style login, where the user manager learns
//! `WAYLAND_DISPLAY` only after the unit may already have started (docs/omarchy.md). The launcher runs against a stub
//! `systemctl` that records its calls; nothing touches a real user manager.
#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

fn packaging() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux")
}

#[test]
fn the_unit_is_ordered_after_the_graphical_session_without_pulling_it_in() {
    let unit = fs::read_to_string(packaging().join("geekbridge-service.service")).unwrap();
    let after: Vec<&str> = unit
        .lines()
        .filter_map(|l| l.strip_prefix("After="))
        .flat_map(str::split_whitespace)
        .collect();
    assert!(after.contains(&"graphical-session.target"), "{after:?}");
    // Ordering only: a Wants=/Requires=/BindsTo= on the target would start a graphical session on a console login.
    for l in unit.lines().filter(|l| !l.starts_with('#')) {
        assert!(
            !(l.starts_with("Wants=") || l.starts_with("Requires=") || l.starts_with("BindsTo="))
                || !l.contains("graphical-session"),
            "{l}"
        );
    }
    // Still started by default.target so X11/i3/sway sessions without the target keep working.
    assert!(unit.contains("WantedBy=default.target"));
    assert!(!unit.contains("PrivateTmp=yes"));
}

/// Run the launcher with a stub `systemctl`; `manager_has_wayland` is what `show-environment` reports.
fn launch(manager_has_wayland: bool, session_wayland: Option<&str>) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = tmp.path().join("calls");
    let env_line = if manager_has_wayland {
        "echo WAYLAND_DISPLAY=wayland-1"
    } else {
        "echo PATH=/usr/bin"
    };
    let stub = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in\n  *show-environment*) {env_line} ;;\nesac\nexit 0\n",
        log.display()
    );
    let path = bin.join("systemctl");
    fs::write(&path, stub).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(packaging().join("geekbridge-service-launch"))
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    if let Some(w) = session_wayland {
        cmd.env("WAYLAND_DISPLAY", w);
    }
    assert!(cmd.status().unwrap().success());
    fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_service_started_before_the_session_is_restarted_once_with_the_environment() {
    let calls = launch(false, Some("wayland-1"));
    assert!(
        calls
            .iter()
            .any(|c| c == "--user import-environment WAYLAND_DISPLAY"),
        "{calls:?}"
    );
    assert_eq!(
        calls.last().unwrap(),
        "--user restart geekbridge-service.service"
    );
}

#[test]
fn a_manager_that_already_has_the_session_just_starts_the_unit() {
    let calls = launch(true, Some("wayland-1"));
    assert_eq!(
        calls.last().unwrap(),
        "--user start geekbridge-service.service"
    );
    assert!(!calls.iter().any(|c| c.contains("restart")), "{calls:?}");
}

#[test]
fn no_wayland_session_means_no_restart() {
    let calls = launch(false, None);
    assert_eq!(
        calls.last().unwrap(),
        "--user start geekbridge-service.service"
    );
}
