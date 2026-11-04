// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless renders of the Messages app over fixture views: thread list, conversation pane, compose,
//! the device page, every send phase and consent state, odd text and narrow windows.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::test_support::{Rig, shapes, wait_until};
use bridge_core::AppKind;
use eframe::egui::Key;

const H: u64 = 3_600_000;
const D: u64 = 86_400_000;

fn device(sms: DataState, send: bool) -> DataDevice {
    DataDevice {
        device_id: "d".into(),
        name: "Pixel".into(),
        contacts: DataState::Ready,
        calendar: DataState::Ready,
        sms,
        calendar_edit: DataState::ConsentOff,
        contacts_count: 3,
        events_count: 0,
        calendar_names: vec![],
        calendar_writable: false,
        calendar_truncated: false,
        contacts_capped: false,
        threads_on_phone: Some(12),
        contacts_synced_unix_ms: None,
        calendar_synced_unix_ms: None,
        sms_status_reports: true,
        sms_send: send,
        sims: vec![
            SimItem {
                id: 3,
                slot: 1,
                label: "Carrier A".into(),
            },
            SimItem {
                id: 4,
                slot: 2,
                label: "Carrier B".into(),
            },
        ],
    }
}

fn msg(uid: i64, outgoing: bool, body: &str, date_ms: u64) -> MessageItem {
    MessageItem {
        uid,
        mms: uid % 5 == 0,
        outgoing,
        body: body.into(),
        date_ms,
        attachments: (uid % 3) as u32,
        sub_id: Some(3),
    }
}

/// Every test runs on the same pinned day (Wednesday 4 Nov 2026, noon UTC).
fn pin() -> crate::localtime::FixedClock {
    crate::localtime::FixedClock::set(1_793_793_600_000)
}

fn now_ms() -> u64 {
    u64::try_from(crate::localtime::unix_now_ms()).unwrap_or(0)
}

fn convo(id: i64, title: &str, body: &str, unread: u32) -> ConversationItem {
    ConversationItem {
        device_id: "d".into(),
        thread_id: id,
        addresses: vec![format!("+1 555 000 {id:04}")],
        title: title.into(),
        last_body: body.into(),
        last_date_ms: now_ms() - id as u64 * D,
        last_outgoing: id % 2 == 0,
        unread,
        message_count: 30,
        matched: (id == 2).then(|| "a matching piece".into()),
    }
}

fn send_item(id: u64, phase: SendPhase, reason: Option<&str>) -> SendItem {
    SendItem {
        send_id: id,
        device_id: "d".into(),
        recipient: "+1 555 123 4567".into(),
        text: format!("outgoing {id}"),
        phase,
        reason: reason.map(Into::into),
        created_unix_ms: now_ms() - 1000,
        updated_unix_ms: now_ms(),
    }
}

fn fixture(sms: DataState, send: bool) -> MessagesView {
    let n = now_ms();
    let bodies = [
        "hello",
        "",
        "emoji 🎉🎉🎉 and e\u{301} combining",
        "שלום עולם مرحبا",
        "http://example.org/a/very/long/url/that/never/breaks/anywhere/at/all/ever",
        "line one\nline two\n\nline four",
        &"word ".repeat(400),
    ];
    let messages = (0..60)
        .map(|i| {
            msg(
                i,
                i % 3 == 0,
                bodies[i as usize % bodies.len()],
                n - (60 - i as u64) * 6 * H,
            )
        })
        .collect();
    MessagesView {
        devices: vec![device(sms, send)],
        conversations: vec![
            convo(1, "Ann", "see you", 2),
            convo(2, "+1 555 000 0002", "hi", 0),
            convo(3, "עברית", "שלום", 120),
            convo(4, &"Very long name ".repeat(20), &"long ".repeat(200), 0),
            convo(5, "", "", 1),
        ],
        conversations_total: 400,
        selected: Some(SelectedThread {
            device_id: "d".into(),
            thread_id: 1,
            messages,
            may_have_older: true,
            held: 60,
            capped: true,
        }),
        sends: vec![
            send_item(1, SendPhase::Queued, None),
            send_item(2, SendPhase::HandedToPhone, None),
            send_item(3, SendPhase::AcceptedByPhone, None),
            send_item(4, SendPhase::Sent, None),
            send_item(5, SendPhase::Delivered, None),
            send_item(6, SendPhase::Failed, Some("generic_failure")),
            send_item(7, SendPhase::Failed, Some("something odd")),
            send_item(8, SendPhase::Uncertain, None),
        ],
    }
}

fn messages_with(view: MessagesView) -> Messages {
    let mut m = Messages {
        env_checked: true,
        open_number_checked: true,
        ..Messages::default()
    };
    m.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| Ok(view.clone()),
        |_| Ok(Some("sent".into())),
        || {},
    ));
    m
}

fn ready(m: &mut Messages, rig: &mut Rig) {
    rig.settle(m, 2);
    let feed = m.feed.as_ref().unwrap();
    wait_until("view", || feed.state().view.is_some());
    rig.settle(m, 3);
}

#[test]
fn every_section_draws_for_every_sms_state_and_window_size() {
    let _clock = pin();
    for sms in [
        DataState::ConsentOff,
        DataState::Offline,
        DataState::NotOffered,
        DataState::Waiting,
        DataState::Ready,
    ] {
        for (w, h) in [(1000.0, 700.0), (420.0, 560.0), (240.0, 300.0)] {
            for section in [
                Section::Conversations,
                Section::Compose,
                Section::Device("p1".into()),
            ] {
                let mut view = fixture(sms, sms == DataState::Ready);
                if sms != DataState::Ready {
                    view.conversations.clear();
                    view.selected = None;
                }
                let mut m = messages_with(view);
                m.section = section.clone();
                m.recipient = "+44 20 7946 0001".into();
                m.draft = "héllo 🎉".repeat(30);
                let mut rig = Rig::new(AppKind::Messages, w, h);
                ready(&mut m, &mut rig);
                let out = rig.settle(&mut m, 2);
                assert!(shapes(&out) > 5, "{sms:?} {w}x{h}");
            }
        }
    }
}

#[test]
fn the_conversation_pane_opens_a_thread_searches_and_loads_older_messages() {
    let _clock = pin();
    let mut m = messages_with(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    assert_eq!(m.thread, Some(("d".into(), 1)), "first conversation opens");
    // A search that hides the open thread drops the stale pane.
    m.search = "zzz".into();
    m.thread = Some(("d".into(), 99));
    rig.settle(&mut m, 3);
    assert!(m.thread.is_none(), "stale thread dropped by a search");
    m.search.clear();
    rig.settle(&mut m, 3);
    assert!(m.thread.is_some(), "follows the first result again");
    // Switching thread resets the history limit.
    m.message_limit = 200;
    m.thread = Some(("d".into(), 2));
    rig.settle(&mut m, 2);
    assert_eq!(m.message_limit, 0);
    // Scroll the long thread a few times.
    for _ in 0..4 {
        rig.frame(
            &mut m,
            vec![eframe::egui::Event::MouseWheel {
                unit: eframe::egui::MouseWheelUnit::Line,
                delta: eframe::egui::vec2(0.0, 20.0),
                modifiers: eframe::egui::Modifiers::NONE,
            }],
        );
    }
}

#[test]
fn a_number_handed_over_by_contacts_opens_its_thread_or_a_new_message() {
    let _clock = pin();
    for (number, expect_compose) in [("+1 555 000 0003", false), ("+49 30 1234", true)] {
        let mut m = messages_with(fixture(DataState::Ready, true));
        m.open_number = Some(number.into());
        let mut rig = Rig::new(AppKind::Messages, 900.0, 600.0);
        ready(&mut m, &mut rig);
        if expect_compose {
            assert!(m.section == Section::Compose);
            assert_eq!(m.recipient, number);
        } else {
            assert!(m.section == Section::Conversations);
            assert_eq!(m.thread, Some(("d".into(), 3)));
        }
    }
}

#[test]
fn compose_keeps_the_draft_until_the_service_accepts_and_survives_odd_text() {
    let _clock = pin();
    let mut m = messages_with(fixture(DataState::Ready, true));
    m.open_compose();
    assert!(matches!(m.focus, Some(Focus::Recipient)));
    let mut rig = Rig::new(AppKind::Messages, 800.0, 600.0);
    ready(&mut m, &mut rig);
    for text in [
        "",
        "plain",
        "emoji 🎉 unicode ✓ switches the encoding",
        &"x".repeat(161),
        &"é".repeat(70),
        &"🎉".repeat(2000),
        "\u{202e}rtl override\u{202c}",
        "nul\0byte",
    ] {
        m.draft = text.into();
        m.recipient = text.chars().take(20).collect();
        rig.settle(&mut m, 2);
        for s in ["", "a", &"y".repeat(5000)] {
            let _ = segment_label(s);
            let _ = segment_hint(s);
        }
    }
    rig.type_text(&mut m, "typed");
    rig.key(&mut m, Key::Tab);
    rig.key(&mut m, Key::Enter);
    rig.key_with(&mut m, Key::Enter, eframe::egui::Modifiers::COMMAND);
}

#[test]
fn a_failing_service_shows_a_problem_and_keeps_the_ui_usable() {
    let _clock = pin();
    for model in [
        crate::state::DiscoveryModel::Checking,
        crate::state::DiscoveryModel::ServiceUnavailable {
            reason: "refused".into(),
        },
    ] {
        let mut m = Messages {
            env_checked: true,
            open_number_checked: true,
            ..Messages::default()
        };
        m.feed = Some(Feed::spawn_with(
            Duration::from_millis(10),
            Request::Status,
            |_| Err("The service is not running.".into()),
            |_| Err("refused".into()),
            || {},
        ));
        let mut rig = Rig::new(AppKind::Messages, 800.0, 600.0);
        rig.model = model;
        rig.settle(&mut m, 2);
        let feed = m.feed.as_ref().unwrap();
        wait_until("problem", || feed.state().problem.is_some());
        for section in [
            Section::Conversations,
            Section::Compose,
            Section::Device("x".into()),
        ] {
            m.section = section;
            rig.settle(&mut m, 3);
        }
    }
}

#[test]
fn many_devices_and_empty_views_render() {
    let _clock = pin();
    let mut view = MessagesView::default();
    view.devices = (0..30).map(|_| device(DataState::Ready, true)).collect();
    let mut m = messages_with(view);
    let mut rig = Rig::new(AppKind::Messages, 600.0, 500.0);
    ready(&mut m, &mut rig);
    let mut m = messages_with(MessagesView::default());
    ready(&mut m, &mut Rig::new(AppKind::Messages, 600.0, 500.0));
}
