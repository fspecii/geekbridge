// SPDX-License-Identifier: GPL-3.0-or-later
//! The `notify-send` notifiers for incoming calls and offered files, against a stub `notify-send` that records its argv
//! and prints the action the user "clicked": answer / decline / accept map to the right callback, anything else (an
//! unknown action, no output, a crash, no tool installed) calls nothing and never panics.
#![cfg(unix)]

use bridge_service::{
    call_control::{CallNotifier, NotifySendCalls, RingChoice},
    share::{Notifier, NotifySend},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

fn install(dir: &std::path::Path, body: &str) {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let tmp = bin.join(".n");
    fs::write(&tmp, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&tmp, bin.join("notify-send")).unwrap();
}

#[test]
fn clicked_actions_reach_the_callbacks_and_everything_else_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("argv");
    // SAFETY: the only test in this process.
    unsafe { std::env::set_var("PATH", tmp.path().join("bin")) };
    let wait = Duration::from_millis(1500);

    // Calls.
    for (printed, expect) in [
        ("answer", Some("answer")),
        ("decline", Some("decline")),
        ("bogus", None),
        ("", None),
    ] {
        install(
            tmp.path(),
            &format!("echo \"$@\" > '{}'; echo '{printed}'", log.display()),
        );
        let (tx, rx) = mpsc::channel();
        NotifySendCalls.ringing(
            "Alice\nInjected --action=x",
            true,
            true,
            Box::new(move |c| {
                tx.send(match c {
                    RingChoice::Answer => "answer",
                    RingChoice::Decline => "decline",
                })
                .unwrap()
            }),
        );
        assert_eq!(rx.recv_timeout(wait).ok(), expect, "{printed:?}");
        let argv = fs::read_to_string(&log).unwrap();
        assert!(argv.contains("--action=answer=Answer") && argv.contains("-- Incoming call"));
    }
    // A choice the notification did not offer is not honoured even if the tool prints it.
    install(tmp.path(), "echo answer");
    let (tx, rx) = mpsc::channel();
    NotifySendCalls.ringing("Bob", false, true, Box::new(move |_| tx.send(()).unwrap()));
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());

    // Files.
    for (printed, expect) in [
        ("accept", Some(true)),
        ("decline", Some(false)),
        ("nonsense", None),
    ] {
        install(tmp.path(), &format!("echo '{printed}'"));
        let (tx, rx) = mpsc::channel();
        NotifySend.offered(
            "Phone",
            "a.txt",
            Some(10),
            Box::new(move |a| tx.send(a).unwrap()),
        );
        assert_eq!(
            rx.recv_timeout(if expect.is_some() {
                wait
            } else {
                Duration::from_millis(600)
            })
            .ok(),
            expect
        );
    }

    // The tool crashing or missing: no callback, no panic.
    install(tmp.path(), "exit 139");
    let called = Arc::new(Mutex::new(false));
    let c = called.clone();
    NotifySend.offered(
        "Phone",
        "a.txt",
        None,
        Box::new(move |_| *c.lock().unwrap() = true),
    );
    NotifySend.received_text("Phone\u{202e}", true);
    NotifySend.received_text("Phone", false);
    fs::remove_file(tmp.path().join("bin/notify-send")).unwrap();
    let c = called.clone();
    NotifySend.offered(
        "Phone",
        "a.txt",
        None,
        Box::new(move |_| *c.lock().unwrap() = true),
    );
    NotifySendCalls.ringing(
        "Carol",
        true,
        true,
        Box::new(|_| panic!("no tool, no choice")),
    );
    NotifySend.received_text("Phone", false);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!*called.lock().unwrap());
}
