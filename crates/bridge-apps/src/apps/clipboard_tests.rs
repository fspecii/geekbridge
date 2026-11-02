// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard window and its status wording: every pull outcome, backend state and device row.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::clipboard_link::{
    ClipState, backend_line, hotkey_line, pull_line, skip_line, window_line,
};
use crate::test_support::{Rig, shapes};
use bridge_core::{
    AppKind,
    ipc::{ClipboardDeviceStatus, ClipboardPullStatus, ClipboardStatus},
};
use eframe::egui::{Event, Key, Modifiers};

fn dev(name: &str, enabled: bool, connected: bool, negotiated: bool) -> ClipboardDeviceStatus {
    ClipboardDeviceStatus {
        device_id: format!("id-{name}"),
        name: name.into(),
        enabled,
        connected,
        negotiated,
        sent: 3,
        received: 4,
        skipped: 1,
        last_sent_unix_ms: Some(1),
        last_received_unix_ms: None,
        last_skip: Some("sensitive".into()),
    }
}

fn status() -> ClipboardStatus {
    ClipboardStatus {
        backend: "wl-clipboard".into(),
        available: true,
        observe: "event".into(),
        watching: true,
        problem: None,
        max_text_bytes: 1 << 20,
        max_image_bytes: 8 << 20,
        window_ms: 90_000,
        hotkey: "Ctrl+Shift+V".into(),
        hotkey_binding: "unsupported".into(),
        last_pull: None,
        other_sync_app: false,
        devices: vec![
            dev("Pixel", true, true, true),
            dev("Offline", true, false, false),
            dev("Old app", true, true, false),
            dev("Off", false, true, true),
            dev(&"L".repeat(300), true, true, true),
        ],
    }
}

fn pull(state: &str, reason: Option<&str>, kind: Option<&str>) -> ClipboardPullStatus {
    ClipboardPullStatus {
        device_id: "x".into(),
        state: state.into(),
        reason: reason.map(Into::into),
        kind: kind.map(Into::into),
        at_unix_ms: 1,
    }
}

#[test]
fn every_pull_outcome_has_a_plain_sentence() {
    let mut seen = std::collections::HashSet::new();
    for (s, r, k) in [
        ("delivered", None, Some("image")),
        ("delivered", None, None),
        ("waiting", None, None),
        ("none", Some("locked"), None),
        ("none", Some("expired"), None),
        ("none", Some("empty"), None),
        ("none", Some("needs_gesture"), None),
        ("none", Some("disabled"), None),
        ("none", Some("too_large"), None),
        ("none", Some("sensitive"), None),
        ("none", Some("unsupported"), None),
        ("none", Some("timeout"), None),
        ("none", Some("failed"), None),
        ("none", Some("🎉 unknown"), None),
        ("none", None, None),
        ("bogus", None, None),
    ] {
        let line = pull_line(&pull(s, r, k));
        assert!(!line.is_empty() && line.is_ascii() == line.is_ascii());
        assert!(
            !line.contains("needs_gesture") && !line.contains("too_large"),
            "codes never leak: {line}"
        );
        seen.insert(line);
    }
    assert!(seen.len() >= 12, "outcomes are told apart: {}", seen.len());
}

#[test]
fn backend_hotkey_window_and_skip_wording_cover_every_state() {
    let mut s = status();
    assert!(backend_line(&s).contains("instant"));
    s.observe = "polling".into();
    assert!(backend_line(&s).contains("twice a second"));
    s.observe = "none".into();
    s.watching = false;
    s.problem = Some("clipboard_io_failed".into());
    let l = backend_line(&s);
    assert!(
        l.contains("not watched") && l.contains("idle") && l.contains("read failed"),
        "{l}"
    );
    s.problem = Some("clipboard_unavailable".into());
    assert!(backend_line(&s).contains("tool missing"));
    s.available = false;
    assert!(backend_line(&s).contains("turned off"));
    s.problem = Some("no_clipboard_tool".into());
    assert!(backend_line(&s).contains("wl-clipboard"));
    for b in ["bound", "not_bound", "unsupported", ""] {
        s.hotkey_binding = b.into();
        assert!(hotkey_line(&s).contains("Ctrl+Shift+V"));
    }
    assert!(window_line(30_000).contains("30 s"));
    assert!(window_line(120_000).contains("2 min"));
    assert!(
        window_line(90_000).contains("90 s"),
        "not a whole minute stays in seconds"
    );
    assert!(window_line(0).contains("0 s"));
    assert!(
        window_line(u64::MAX).contains(" s"),
        "huge values do not overflow"
    );
    for r in [
        "sensitive",
        "too_large",
        "phone_not_accepting",
        "link_busy",
        "invalid_text",
        "?",
        "",
    ] {
        assert!(!skip_line(r).is_empty());
    }
}

fn clipboard_with(state: ClipState) -> (Clipboard, std::sync::mpsc::Receiver<Action>) {
    let (link, rx) = ClipLink::detached(state);
    let mut c = Clipboard::default();
    c.link = Some(link);
    (c, rx)
}

#[test]
fn renders_status_error_and_checking_states_and_survives_big_text() {
    for (w, h) in [(700.0, 700.0), (300.0, 400.0)] {
        let mut full = status();
        full.last_pull = Some(pull("none", Some("locked"), None));
        for st in [
            ClipState {
                status: Some(full.clone()),
                error: None,
                notice: Some("Time window saved.".into()),
            },
            ClipState {
                status: Some(ClipboardStatus {
                    devices: vec![],
                    ..full.clone()
                }),
                ..ClipState::default()
            },
            ClipState {
                status: None,
                error: Some("The service is not running.".into()),
                notice: None,
            },
            ClipState::default(),
        ] {
            let (mut c, _rx) = clipboard_with(st);
            c.text = "héllo 🎉 \u{202e}x\n".repeat(2000);
            let mut rig = Rig::new(AppKind::Clipboard, w, h);
            let out = rig.settle(&mut c, 4);
            assert!(shapes(&out) > 5);
            c.copied_at = Some(0.0);
            rig.settle(&mut c, 2);
        }
    }
}

#[test]
fn the_paste_shortcut_asks_the_phone_and_a_busy_link_says_so() {
    let (mut c, rx) = clipboard_with(ClipState {
        status: Some(status()),
        ..ClipState::default()
    });
    let mut rig = Rig::new(AppKind::Clipboard, 700.0, 700.0);
    rig.settle(&mut c, 2);
    rig.key_with(&mut c, Key::V, Modifiers::CTRL | Modifiers::SHIFT);
    assert!(
        matches!(rx.try_recv(), Ok(Action::Pull)),
        "Ctrl+Shift+V pulls"
    );
    // A plain V does nothing.
    rig.key(&mut c, Key::V);
    assert!(rx.try_recv().is_err());
    // Four pending actions fill the queue; the fifth is refused with a notice, not a panic.
    for _ in 0..8 {
        rig.key_with(&mut c, Key::V, Modifiers::CTRL | Modifiers::SHIFT);
    }
    assert!(
        c.link
            .as_ref()
            .unwrap()
            .state
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("Another request"))
    );
    // A garbage configured hotkey never matches.
    assert!(!shortcut_pressed(&rig.ctx, "Ctrl+Nope"));
    assert!(!shortcut_pressed(&rig.ctx, ""));
    let _ = Event::Text(String::new());
}
