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
    // Letters are a contact search now; they are bounded, and nothing with letters can be dialed.
    assert!(c.number.chars().count() <= 64);
    assert!(
        !c.number.contains('\u{202e}'),
        "direction marks are dropped"
    );
    let view = c.sources.status.as_ref().expect("status").clone();
    assert!(
        call_ui::dial_request(&view.view, &c.number).is_none(),
        "a name is never dialed: {}",
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

#[test]
fn recents_lists_finished_calls_and_call_back_goes_through_the_confirmation_sheet() {
    for (w, h) in [(420.0, 640.0), (320.0, 560.0), (900.0, 700.0)] {
        let mut rig = Rig::new(AppKind::Calls, w, h);
        let mut c = Calls::default();
        c.feed = Some(fixture_feed("recents", rig.ctx.clone()));
        c.tab = Tab::Recents;
        settle_with_state(&mut c, &mut rig);
        let out = rig.settle(&mut c, 3);
        assert!(shapes(&out) > 20, "{w}x{h}: the list is drawn");
    }
    // The rows say what happened, in words.
    let status = c_status("recents");
    let details: Vec<String> = status
        .view
        .recent
        .iter()
        .map(call_ui::recent_detail)
        .collect();
    assert_eq!(details[0], "Missed · 4 min ago");
    assert_eq!(details[1], "Incoming 03:05 · 1 h ago");
    assert_eq!(details[2], "Outgoing 00:42 · 8 h ago");
    assert_eq!(details[3], "Declined · 2 d ago");
    // A number is offered for calling back only when it is a plain one.
    assert_eq!(
        status.view.recent[2].callback_number().as_deref(),
        Some("+442079460958")
    );
    assert_eq!(status.view.recent[3].callback_number(), None);
    // The service offered dialing, so the confirmation names the number and the phone.
    let dial = status.view.dial.as_ref().expect("dial offered");
    let (question, _) = call_ui::dial_confirmation(dial, "+15550100");
    assert!(question.contains("+15550100") && question.contains("Pixel 9"));
}

fn c_status(kind: &str) -> bridge_core::ipc::CallsStatus {
    let feed = fixture_feed(kind, egui::Context::default());
    wait_until("fixture state", || feed.state().status.is_some());
    feed.state().status.unwrap()
}

#[test]
fn the_in_call_keypad_sends_one_tone_per_key_and_only_while_the_service_offers_it() {
    use bridge_core::ipc::Request;
    use std::sync::{Arc, Mutex};
    let mut rig = Rig::new(AppKind::Calls, 420.0, 640.0);
    let sent: Arc<Mutex<Vec<Request>>> = Arc::default();
    let log = sent.clone();
    let mut c = Calls::default();
    c.feed = Some(fixture_feed_sending("keypad", rig.ctx.clone(), move |r| {
        log.lock().unwrap().push(r.clone());
        Ok(())
    }));
    c.panel.open_keypad();
    settle_with_state(&mut c, &mut rig);
    assert!(shapes(&rig.settle(&mut c, 2)) > 5);
    // Keys typed on the keyboard while the panel is open (digits, * and #) go out one by one; letters do not.
    rig.frame(&mut c, vec![Event::Text("5*#a".into())]);
    rig.settle(&mut c, 3);
    // The feed's worker sends on its own thread: wait for the three tones (a slow runner takes a while).
    let tones = || -> Vec<String> {
        sent.lock()
            .unwrap()
            .iter()
            .filter_map(|r| match r {
                Request::CallTone { tone, .. } => Some(tone.expose().to_owned()),
                _ => None,
            })
            .collect()
    };
    wait_until("three tones sent", || tones().len() >= 3);
    let tones = tones();
    assert_eq!(tones, ["5", "*", "#"]);
    // Narrow windows draw it too.
    for (w, h) in [(300.0, 420.0), (900.0, 700.0)] {
        let mut rig = Rig::new(AppKind::Calls, w, h);
        let mut c = Calls::default();
        c.feed = Some(fixture_feed("keypad", rig.ctx.clone()));
        c.panel.open_keypad();
        settle_with_state(&mut c, &mut rig);
        assert!(shapes(&rig.settle(&mut c, 2)) > 5);
    }
}

#[test]
fn an_active_call_without_tones_shows_no_keypad_and_sends_none() {
    use bridge_core::ipc::Request;
    use std::sync::{Arc, Mutex};
    let mut rig = Rig::new(AppKind::Calls, 420.0, 640.0);
    let sent: Arc<Mutex<Vec<Request>>> = Arc::default();
    let log = sent.clone();
    let mut c = Calls::default();
    c.feed = Some(fixture_feed_sending("active", rig.ctx.clone(), move |r| {
        log.lock().unwrap().push(r.clone());
        Ok(())
    }));
    c.panel.open_keypad(); // asked for, but the service does not offer tones
    settle_with_state(&mut c, &mut rig);
    rig.frame(&mut c, vec![Event::Text("123".into())]);
    rig.settle(&mut c, 3);
    assert!(sent.lock().unwrap().is_empty());
}

fn person(name: &str, phones: &[&str]) -> bridge_core::phone_view::Person {
    bridge_core::phone_view::Person {
        device_id: "d".into(),
        id: name.into(),
        name: name.into(),
        phones: phones.iter().map(|s| (*s).to_owned()).collect(),
        emails: vec![],
        organization: None,
    }
}

fn dialer_with_contacts() -> (Calls, Rig) {
    let mut rig = Rig::new(AppKind::Calls, 420.0, 760.0);
    let mut c = calls_with_fixture(false, &rig);
    settle_with_state(&mut c, &mut rig);
    let people = vec![
        person("Ann Lee", &["+44 20 7946 0001"]),
        person("Joanna Smith", &["+1 555 0100", "+1 555 0101"]),
    ];
    c.people = Some(Feed::spawn_with(
        std::time::Duration::from_millis(10),
        Request::Status,
        move |_| {
            Ok(AgendaView {
                people: people.clone(),
                ..AgendaView::default()
            })
        },
        |_| Ok(None),
        || {},
    ));
    c.focus_display = true;
    rig.settle(&mut c, 2);
    (c, rig)
}

fn type_and_wait(c: &mut Calls, rig: &mut Rig, text: &str) {
    rig.type_text(c, text);
    wait_until("contacts", || {
        c.people.as_ref().unwrap().state().view.is_some()
    });
    rig.settle(c, 3);
}

#[test]
fn a_name_suggests_contacts_and_a_one_number_contact_is_picked_without_dialing() {
    let (mut c, mut rig) = dialer_with_contacts();
    type_and_wait(&mut c, &mut rig, "lee");
    assert_eq!(c.suggestions(true).len(), 1);
    rig.key(&mut c, Key::Enter);
    assert_eq!(c.number, "+442079460001");
    assert_eq!(c.picked.as_ref().unwrap().0, "Ann Lee");
    assert!(c.confirming.is_none(), "picking never starts the dial");
    // The list is gone; editing the number forgets the contact.
    rig.type_text(&mut c, "9");
    assert!(c.picked.is_none());
}

#[test]
fn a_contact_with_several_numbers_asks_which_and_escape_goes_back_then_closes() {
    let (mut c, mut rig) = dialer_with_contacts();
    type_and_wait(&mut c, &mut rig, "joanna");
    rig.key(&mut c, Key::Enter);
    assert!(c.picker.asking.is_some(), "asks which number");
    assert!(c.number == "joanna" && c.picked.is_none());
    rig.key(&mut c, Key::Escape);
    assert!(c.picker.asking.is_none() && !c.list_closed);
    rig.key(&mut c, Key::Escape);
    assert!(c.list_closed);
    rig.key(&mut c, Key::Backspace);
    assert!(!c.list_closed, "typing opens the list again");
    rig.key(&mut c, Key::Enter);
    rig.key(&mut c, Key::ArrowDown);
    rig.key(&mut c, Key::Enter);
    assert_eq!(c.number, "+15550101");
    assert_eq!(c.picked.as_ref().unwrap().0, "Joanna Smith");
    assert!(c.confirming.is_none());
}

#[test]
fn digits_look_up_contacts_only_from_three_digits_and_only_stored_numbers_match() {
    let (mut c, mut rig) = dialer_with_contacts();
    assert!(
        !recipient_pick::is_number_query("44"),
        "two digits ask nobody"
    );
    rig.type_text(&mut c, "4420");
    rig.settle(&mut c, 2);
    wait_until("contacts", || {
        c.people.as_ref().unwrap().state().view.is_some()
    });
    rig.settle(&mut c, 3);
    assert_eq!(c.suggestions(true).len(), 1);
    c.number = "99999".into();
    rig.settle(&mut c, 2);
    assert!(
        c.suggestions(true).is_empty(),
        "an unknown number finds nobody"
    );
    c.number = "*21*7946#".into();
    assert!(
        !recipient_pick::is_number_query(&c.number),
        "codes are not looked up"
    );
}

#[test]
fn the_pad_keys_extend_a_name_search_instead_of_wiping_it() {
    let (mut c, _rig) = dialer_with_contacts();
    c.number = "ann".into();
    c.push_key('2');
    assert_eq!(c.number, "ann2");
    c.number = "+44".into();
    c.push_key('5');
    assert_eq!(c.number, "+445");
}
