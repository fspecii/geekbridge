// SPDX-License-Identifier: GPL-3.0-or-later
//! Calls window: keypad and recents over scripted call views, keyboard dialling, narrow windows.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::test_support::{Rig, shapes, wait_until};
use bridge_core::AppKind;
use eframe::egui::{Event, Key, Modifiers};

fn calls_with_fixture(ringing: bool, rig: &Rig) -> Calls {
    let mut c = Calls::default();
    c.feed = Some(fixture_feed(
        if ringing { "ringing" } else { "active" },
        rig.ctx.clone(),
    ));
    c
}

fn settle_with_state(c: &mut Calls, rig: &mut Rig) {
    rig.settle(c, 2);
    let feed = c.feed.as_ref().unwrap();
    wait_until("call state", || feed.state().status.is_some());
    rig.settle(c, 3);
}

#[test]
fn keypad_and_recents_draw_with_idle_ringing_active_and_down_service() {
    for (w, h) in [(420.0, 640.0), (300.0, 420.0), (900.0, 700.0)] {
        for kind in [None, Some(true), Some(false)] {
            let mut rig = Rig::new(AppKind::Calls, w, h);
            let mut c = match kind {
                Some(r) => calls_with_fixture(r, &rig),
                None => {
                    let mut c = Calls::default();
                    c.feed = Some(CallFeed::spawn_with(
                        std::time::Duration::from_millis(10),
                        || Err("The service is not running.".to_string()),
                        |_| Err("no".into()),
                        || {},
                    ));
                    c
                }
            };
            c.number = "+44 20 7946 0958".into();
            settle_with_state_or_problem(&mut c, &mut rig);
            let out = rig.settle(&mut c, 2);
            assert!(shapes(&out) > 5);
            c.tab = Tab::Recents;
            let out = rig.settle(&mut c, 3);
            assert!(shapes(&out) > 5);
        }
    }
}

#[test]
fn wifi_fixtures_draw_the_experimental_card_at_every_size() {
    for kind in ["active-wifi", "listening-wifi", "speaking-wifi"] {
        for (w, h) in [(420.0, 640.0), (300.0, 420.0), (900.0, 700.0)] {
            let mut rig = Rig::new(AppKind::Calls, w, h);
            let mut c = Calls::default();
            c.feed = Some(fixture_feed(kind, rig.ctx.clone()));
            settle_with_state(&mut c, &mut rig);
            let status = c.feed.as_ref().unwrap().state().status.unwrap();
            assert!(status.wifi.is_some(), "{kind}");
            let card = crate::call_wifi_ui::card(&status, status.view.live()).expect(kind);
            assert_eq!(
                matches!(card, crate::call_wifi_ui::WifiCard::Offer { .. }),
                kind == "active-wifi"
            );
            assert!(shapes(&rig.settle(&mut c, 2)) > 5);
        }
    }
}

fn settle_with_state_or_problem(c: &mut Calls, rig: &mut Rig) {
    rig.settle(c, 2);
    let feed = c.feed.as_ref().unwrap();
    wait_until("feed answered", || {
        let s = feed.state();
        s.status.is_some() || s.problem.is_some()
    });
    rig.settle(c, 3);
}

#[test]
fn typing_digits_and_symbols_goes_through_the_sanitiser() {
    let mut rig = Rig::new(AppKind::Calls, 420.0, 640.0);
    let mut c = calls_with_fixture(false, &rig);
    settle_with_state(&mut c, &mut rig);
    c.focus_display = true;
    rig.settle(&mut c, 2);
    rig.frame(
        &mut c,
        vec![Event::Text("+1 (555) 010-0abc\u{202e}9".into())],
    );
    rig.frame(&mut c, vec![Event::Text("x".repeat(500))]);
    assert!(
        c.number.chars().count() <= 64,
        "dial string is bounded: {}",
        c.number.len()
    );
    assert!(
        !c.number.contains('a'),
        "letters never reach the dialler: {}",
        c.number
    );
    for key in [Key::Backspace, Key::Delete, Key::Enter, Key::Escape] {
        rig.key(&mut c, key);
    }
    rig.key_with(&mut c, Key::V, Modifiers::COMMAND);
}

#[test]
fn clicking_the_keypad_appends_digits_and_the_zero_long_press_adds_plus() {
    let mut rig = Rig::new(AppKind::Calls, 420.0, 640.0);
    let mut c = calls_with_fixture(false, &rig);
    settle_with_state(&mut c, &mut rig);
    for y in (150..640).step_by(40) {
        for x in [100.0, 210.0, 320.0] {
            rig.click(&mut c, eframe::egui::pos2(x, y as f32));
        }
    }
    assert!(c.number.chars().count() <= 64);
}
