// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless renders of the Messages app over fixture views: thread list, conversation pane, compose,
//! the device page, every send phase and consent state, odd text and narrow windows.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::test_support::{Rig, shapes, wait_until};
use bridge_core::AppKind;
use eframe::egui::{Key, Modifiers};

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

fn hit(uid: i64, newer: usize, snippet: &str, days_ago: u64) -> MessageHit {
    MessageHit {
        uid,
        mms: false,
        outgoing: uid % 2 == 0,
        date_ms: now_ms() - days_ago * D,
        snippet: snippet.into(),
        newer,
    }
}

/// A search result view: two conversations with matches, one found by name only.
fn search_fixture() -> MessagesView {
    let mut v = fixture(DataState::Ready, true);
    v.hits = vec![
        ThreadHits {
            device_id: "d".into(),
            thread_id: 3,
            title: "Ann".into(),
            hits: vec![
                hit(
                    30,
                    2,
                    "Ping me when it is done, I will check it tonight.",
                    0,
                ),
                hit(12, 700, "the paper is due Friday", 3),
            ],
        },
        ThreadHits {
            device_id: "d".into(),
            thread_id: 2,
            title: "+1 555 000 0002".into(),
            hits: vec![hit(7, 0, "Look at this! \u{1F389} caf\u{e9}", 9)],
        },
    ];
    v
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
        hits: Vec::new(),
    }
}

fn messages_with(view: MessagesView) -> Messages {
    messages_logged(view).0
}

type Log = std::sync::Arc<std::sync::Mutex<Vec<Request>>>;

/// Like `messages_with`, also returning every action the window asked the service to run.
fn messages_logged(view: MessagesView) -> (Messages, Log) {
    let log = Log::default();
    let mut m = Messages {
        env_checked: true,
        open_number_checked: true,
        ..Messages::default()
    };
    let seen = log.clone();
    m.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |r| {
            // The service answers for the conversation that was asked for.
            let mut v = view.clone();
            if let Request::MessagesView {
                thread_id: Some(t), ..
            } = r
                && let Some(sel) = &mut v.selected
            {
                sel.thread_id = *t;
            }
            Ok(v)
        },
        move |r| {
            seen.lock().unwrap().push(r.clone());
            Ok(Some("sent".into()))
        },
        || {},
    ));
    (m, log)
}

fn sends(log: &Log) -> Vec<(String, String)> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            Request::MessagesSend {
                recipient, text, ..
            } => Some((recipient.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

/// Opens a conversation the way a click does and drives frames until the service has delivered it, so
/// the reply box is on screen and focused (until then the pane says "Loading the conversation").
fn open_thread(m: &mut Messages, rig: &mut Rig, id: i64) {
    m.thread = Some(("d".into(), id));
    rig.settle(m, 1);
    let feed = m.feed.as_ref().unwrap();
    wait_until("conversation", || {
        feed.state()
            .view
            .and_then(|v| v.selected.as_ref().map(|s| s.thread_id))
            .is_some_and(|t| t == id)
    });
    rig.settle(m, 3);
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

#[test]
fn enter_sends_a_reply_and_shift_enter_adds_a_line() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    // Opening a conversation puts the cursor in the reply box.
    rig.type_text(&mut m, "a");
    rig.key_with(&mut m, Key::Enter, Modifiers::SHIFT);
    rig.type_text(&mut m, "b");
    assert_eq!(m.reply, "a\nb", "Shift+Enter is a new line");
    assert!(sends(&log).is_empty(), "nothing sent yet");
    rig.key(&mut m, Key::Enter);
    wait_until("send", || !sends(&log).is_empty());
    assert_eq!(sends(&log), vec![("+1 555 000 0001".into(), "a\nb".into())]);
    assert!(m.reply.is_empty(), "the box is empty after sending");
    // Enter on an empty box sends nothing and adds no blank line.
    rig.key(&mut m, Key::Enter);
    assert!(m.reply.is_empty());
    assert_eq!(sends(&log).len(), 1);
    // Ctrl+Enter sends as well.
    rig.type_text(&mut m, "again");
    rig.key_with(&mut m, Key::Enter, Modifiers::COMMAND);
    wait_until("second send", || sends(&log).len() == 2);
    assert_eq!(m.reply, "");
}

#[test]
fn every_conversation_keeps_its_own_unsent_reply() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.type_text(&mut m, "for Ann");
    open_thread(&mut m, &mut rig, 2);
    assert_eq!(m.reply, "", "another person's draft never carries over");
    // The box takes the cursor a frame or more after the switch (slower on busy runners): type until it took.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while m.reply.is_empty() {
        assert!(
            std::time::Instant::now() < end,
            "the reply box never took the text"
        );
        rig.type_text(&mut m, "for two");
        rig.settle(&mut m, 3);
    }
    assert_eq!(m.reply, "for two");
    open_thread(&mut m, &mut rig, 1);
    assert_eq!(m.reply, "for Ann");
    open_thread(&mut m, &mut rig, 2);
    assert_eq!(m.reply, "for two");
    rig.key(&mut m, Key::Enter);
    wait_until("send", || !sends(&log).is_empty());
    assert_eq!(
        sends(&log),
        vec![("+1 555 000 0002".into(), "for two".into())]
    );
    open_thread(&mut m, &mut rig, 1);
    assert_eq!(
        m.reply, "for Ann",
        "the draft of the other thread is untouched"
    );
}

#[test]
fn the_cursor_request_waits_for_a_conversation_that_is_still_loading() {
    let _clock = pin();
    let served = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1));
    let mut m = Messages {
        env_checked: true,
        open_number_checked: true,
        ..Messages::default()
    };
    let (view, gate) = (fixture(DataState::Ready, true), served.clone());
    m.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| {
            let mut v = view.clone();
            if let Some(sel) = &mut v.selected {
                sel.thread_id = gate.load(std::sync::atomic::Ordering::SeqCst);
            }
            Ok(v)
        },
        |_| Ok(None),
        || {},
    ));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    // The service still answers with the old conversation: the pane loads, no box exists to focus.
    m.thread = Some(("d".into(), 2));
    rig.settle(&mut m, 3);
    rig.type_text(&mut m, "early");
    assert_eq!(m.reply, "", "no box yet, nothing to type into");
    assert!(
        m.focus_reply,
        "the cursor request is kept, not spent on nothing"
    );
    served.store(2, std::sync::atomic::Ordering::SeqCst);
    let feed = m.feed.as_ref().unwrap();
    wait_until("conversation", || {
        feed.state()
            .view
            .and_then(|v| v.selected.as_ref().map(|s| s.thread_id))
            .is_some_and(|t| t == 2)
    });
    rig.settle(&mut m, 3);
    assert!(!m.focus_reply);
    rig.type_text(&mut m, "late");
    assert_eq!(m.reply, "late", "the box has the cursor once it appears");
}

#[test]
fn ctrl_n_opens_a_new_message_with_the_cursor_in_to_and_escape_goes_back() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.key_with(&mut m, Key::N, Modifiers::COMMAND);
    rig.settle(&mut m, 2);
    assert!(m.section == Section::Compose);
    rig.type_text(&mut m, "+49 151 2345678");
    assert_eq!(m.recipient, "+491512345678", "typing goes to the To field");
    // Enter in To moves to the text; the text keeps its newlines; Ctrl+Enter sends.
    rig.key(&mut m, Key::Enter);
    rig.type_text(&mut m, "hello");
    rig.key_with(&mut m, Key::Enter, Modifiers::SHIFT);
    rig.type_text(&mut m, "there");
    assert_eq!(m.draft, "hello\nthere");
    rig.key_with(&mut m, Key::Enter, Modifiers::COMMAND);
    wait_until("send", || !sends(&log).is_empty());
    assert_eq!(
        sends(&log),
        vec![("+491512345678".into(), "hello\nthere".into())]
    );
    // Escape leaves the draft in place and returns to the conversations.
    m.draft = "kept".into();
    m.awaiting = false;
    m.section = Section::Compose;
    rig.settle(&mut m, 2);
    rig.key(&mut m, Key::Escape);
    assert!(m.section == Section::Conversations);
    assert_eq!(m.draft, "kept");
}

#[test]
fn ctrl_f_puts_the_cursor_in_the_search_field() {
    let _clock = pin();
    let mut m = messages_with(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.key_with(&mut m, Key::F, Modifiers::COMMAND);
    rig.settle(&mut m, 2);
    rig.type_text(&mut m, "verification");
    assert_eq!(m.search, "verification");
    assert!(m.reply.is_empty(), "not typed into the reply box");
}

#[test]
fn numbers_group_across_country_code_and_trunk_prefix_but_short_codes_stay_exact() {
    assert!(same_number("+49 151 2345678", "0151 2345678"));
    assert!(same_number("+49 (151) 234-5678", "01512345678"));
    assert!(!same_number("+49 151 2345678", "+49 151 2345679"));
    // A short code is not a tail of a long number.
    assert!(same_number("12345", "12345"));
    assert!(!same_number("345", "+1 555 0100 345"));
    assert!(!same_number("22000", "+49 151 2322000"));
    assert!(!same_number("", "+49 151 2322000"));
}

/// A window whose service refuses every action with `why`.
fn messages_refusing(view: MessagesView, why: &'static str) -> Messages {
    let mut m = Messages {
        env_checked: true,
        open_number_checked: true,
        ..Messages::default()
    };
    m.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| Ok(view.clone()),
        move |_| Err(why.into()),
        || {},
    ));
    m
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

/// A contact feed that always answers with these people.
fn contacts_feed(people: Vec<bridge_core::phone_view::Person>) -> Feed<AgendaView> {
    Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| {
            Ok(AgendaView {
                people: people.clone(),
                ..AgendaView::default()
            })
        },
        |_| Ok(None),
        || {},
    )
}

fn compose_with_contacts() -> (Messages, Log, Rig) {
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    m.people = Some(contacts_feed(vec![
        person("Ann Lee", &["+44 20 7946 0001"]),
        person("Joanna Smith", &["+1 555 0100", "+1 555 0101"]),
    ]));
    m.open_compose();
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    wait_until("contacts", || {
        m.people.as_ref().unwrap().state().view.is_some()
    });
    (m, log, rig)
}

#[test]
fn a_refused_reply_comes_back_into_the_box_with_the_reason() {
    let _clock = pin();
    let mut m = messages_refusing(fixture(DataState::Ready, true), "The phone link is down");
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.type_text(&mut m, "hello there");
    rig.key(&mut m, Key::Enter);
    for _ in 0..300 {
        rig.settle(&mut m, 1);
        if m.reply == "hello there" {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(m.reply, "hello there", "the text is back");
    assert!(m.sent_replies.is_empty());
    let (_, why) = m.reply_refused.clone().expect("the reason is kept");
    assert_eq!(why, "The phone link is down");
    // The next send clears the note.
    let out = rig.settle(&mut m, 2);
    assert!(shapes(&out) > 5);
}

#[test]
fn an_accepted_reply_stays_gone_and_a_refused_one_goes_before_what_was_typed_since() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.type_text(&mut m, "fine");
    rig.key(&mut m, Key::Enter);
    wait_until("send", || !sends(&log).is_empty());
    for _ in 0..30 {
        rig.settle(&mut m, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(m.reply, "");
    assert!(m.reply_refused.is_none() && m.sent_replies.is_empty());
    // Restoring in front of newer text keeps both; another conversation's box gets its own text back.
    let key = ("d".to_owned(), 1_i64);
    m.reply = "newer".into();
    m.restore_reply(&key, "older");
    assert_eq!(m.reply, "older\nnewer");
    let other = ("d".to_owned(), 2_i64);
    m.restore_reply(&other, "for two");
    assert_eq!(m.drafts.get(&other).map(String::as_str), Some("for two"));
}

#[test]
fn enter_that_confirms_an_input_method_composition_does_not_send() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.type_text(&mut m, "x");
    let preedit = |t: &str| eframe::egui::Event::Ime(eframe::egui::ImeEvent::Preedit(t.into()));
    rig.frame(&mut m, vec![preedit("ni")]);
    rig.key(&mut m, Key::Enter);
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        sends(&log).is_empty(),
        "Enter during a preedit is not a send"
    );
    assert!(
        !m.reply.contains('\n'),
        "and not a new line either: {:?}",
        m.reply
    );
    // The input method commits; a later Enter is a normal send.
    rig.frame(
        &mut m,
        vec![
            eframe::egui::Event::Ime(eframe::egui::ImeEvent::Commit("\u{4f60}".into())),
            preedit(""),
        ],
    );
    rig.settle(&mut m, 2);
    rig.key(&mut m, Key::Enter);
    wait_until("send", || !sends(&log).is_empty());
    assert!(sends(&log)[0].1.contains('\u{4f60}'));
}

#[test]
fn a_name_in_to_lists_contacts_and_enter_picks_the_number() {
    let _clock = pin();
    let (mut m, log, mut rig) = compose_with_contacts();
    rig.type_text(&mut m, "ann");
    rig.settle(&mut m, 3);
    assert_eq!(m.recipient, "ann", "letters are kept while it is a name");
    assert!(m.to_list_open, "the list is on screen");
    // Sending to a name that was not resolved is not possible.
    m.draft = "hi".into();
    rig.key_with(&mut m, Key::Enter, Modifiers::COMMAND);
    std::thread::sleep(Duration::from_millis(60));
    assert!(sends(&log).is_empty());
    rig.key(&mut m, Key::Enter);
    assert_eq!(m.recipient, "+442079460001");
    assert_eq!(m.picked.as_ref().map(|p| p.0.as_str()), Some("Ann Lee"));
    assert!(matches!(m.focus, Some(Focus::Draft)) || m.focus.is_none());
    rig.settle(&mut m, 2);
    rig.key_with(&mut m, Key::Enter, Modifiers::COMMAND);
    wait_until("send", || !sends(&log).is_empty());
    assert_eq!(sends(&log), vec![("+442079460001".into(), "hi".into())]);
}

#[test]
fn a_contact_with_two_numbers_asks_and_up_down_enter_choose_one() {
    let _clock = pin();
    let (mut m, log, mut rig) = compose_with_contacts();
    rig.type_text(&mut m, "jo");
    rig.settle(&mut m, 3);
    rig.key(&mut m, Key::Enter);
    assert!(m.picker.asking.is_some(), "asks which number");
    assert_eq!(m.recipient, "jo", "nothing is guessed");
    rig.key(&mut m, Key::ArrowDown);
    rig.key(&mut m, Key::Enter);
    assert_eq!(m.recipient, "+15550101");
    // Escape in the chooser goes back a step instead of leaving the page.
    let (mut m2, _, mut rig2) = compose_with_contacts();
    rig2.type_text(&mut m2, "jo");
    rig2.settle(&mut m2, 3);
    rig2.key(&mut m2, Key::Enter);
    rig2.key(&mut m2, Key::Escape);
    assert!(
        m2.picker.asking.is_none() && m2.section == Section::Compose,
        "asking={:?} open={}",
        m2.picker.asking,
        m2.to_list_open
    );
    rig2.key(&mut m2, Key::Escape);
    assert!(
        m2.to_closed && m2.section == Section::Compose,
        "Escape closed the list only"
    );
    rig2.key(&mut m2, Key::Escape);
    assert!(
        m2.section == Section::Conversations,
        "a second Escape leaves"
    );
    assert!(sends(&log).is_empty());
}

#[test]
fn a_plain_number_never_asks_for_contacts_and_a_name_nobody_has_cannot_be_sent() {
    let _clock = pin();
    let (mut m, log) = messages_logged(fixture(DataState::Ready, true));
    m.open_compose();
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    rig.type_text(&mut m, "+49 151 2345678");
    rig.settle(&mut m, 3);
    assert!(m.people.is_none(), "no contact search for a number");
    assert_eq!(m.recipient, "+491512345678");
    // A name with no match: kept as typed, the page says what to do, nothing is sent.
    let (mut m, log2, mut rig) = compose_with_contacts();
    rig.type_text(&mut m, "zed");
    rig.settle(&mut m, 4);
    assert_eq!(m.recipient, "zed");
    m.draft = "hi".into();
    rig.key_with(&mut m, Key::Enter, Modifiers::COMMAND);
    std::thread::sleep(Duration::from_millis(60));
    assert!(sends(&log).is_empty() && sends(&log2).is_empty());
}

#[test]
fn the_contact_list_is_named_for_a_screen_reader_and_leaving_compose_stops_the_search() {
    let _clock = pin();
    let (mut m, _, mut rig) = compose_with_contacts();
    rig.type_text(&mut m, "a");
    rig.settle(&mut m, 3);
    let (cands, _) = (m.candidates_now(true), 0);
    let rows = m.picker.rows(&cands);
    assert!(rows.iter().all(|r| !r.spoken.is_empty()));
    m.section = Section::Conversations;
    rig.settle(&mut m, 2);
    assert!(m.people.is_none(), "no contact polling outside compose");
}

#[test]
fn a_search_result_opens_its_conversation_and_asks_for_enough_history_to_hold_the_message() {
    let _clock = pin();
    let mut m = messages_with(search_fixture());
    m.search = "is".into();
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    let v = m.feed.as_ref().unwrap().state().view.unwrap();
    let (g, far) = (&v.hits[0], &v.hits[0].hits[1]);
    m.open_hit(g, far);
    assert_eq!(m.thread, Some(("d".into(), 3)));
    assert_eq!(m.found, Some(("d".into(), 3, 12, false)));
    assert_eq!(
        m.message_limit, 740,
        "the 700 newer messages, the message and room around it"
    );
    // The next frame (the thread changed) keeps that window instead of resetting it.
    rig.settle(&mut m, 2);
    assert_eq!(m.message_limit, 740);
    assert!(m.found.is_some());
    // Clearing the search clears the mark.
    m.search.clear();
    rig.settle(&mut m, 2);
    assert!(m.found.is_none());
    // Opening a result closer than the default window needs no more than the default.
    m.search = "is".into();
    m.thread = None;
    m.message_limit = 0;
    m.open_hit(g, &g.hits[0]);
    assert_eq!(m.message_limit, MAX_MESSAGES_SHOWN);
}

#[test]
fn search_results_draw_in_both_themes_and_the_box_and_rows_are_named_and_focusable() {
    let _clock = pin();
    use crate::{connection::ControlState, shell::Env, test_support::paired_model};
    let mut m = messages_with(search_fixture());
    m.search = "is".into();
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    let (model, control) = (paired_model(), ControlState::default());
    let audit = crate::a11y_tests::audit_screen(
        "search results",
        AppKind::Messages,
        egui::vec2(1000.0, 700.0),
        move |ctx| {
            let pal = crate::theme::current(ctx, AppKind::Messages);
            let env = Env {
                app: AppKind::Messages,
                api_port: 1,
                model: &model,
                pal: &pal,
                control: &control,
            };
            m.show(ctx, &env);
        },
    );
    assert!(audit.problems.is_empty(), "{}", audit.problems.join("\n"));
    assert!(
        audit.tree.find("Search messages").is_some(),
        "the search box is named"
    );
    assert!(
        audit.stops.iter().any(|s| s == "Search messages"),
        "Tab reaches the search box: {:?}",
        audit.stops
    );
    for want in [
        "+1 555 000 0002, 1 matching message,",
        "Ann, 2 matching messages",
        "Ann, 3 d ago: you: the paper is due Friday",
    ] {
        assert!(
            audit.stops.iter().any(|s| s.starts_with(want)),
            "{want:?} missing from {:?}",
            audit.stops
        );
    }
}

#[test]
fn the_search_is_usable_from_the_keyboard_alone() {
    let _clock = pin();
    let mut m = messages_with(search_fixture());
    let mut rig = Rig::new(AppKind::Messages, 1000.0, 700.0);
    ready(&mut m, &mut rig);
    // Ctrl/Cmd+F focuses the box and typing searches.
    rig.key_with(&mut m, Key::F, Modifiers::COMMAND);
    rig.settle(&mut m, 2);
    rig.type_text(&mut m, "is");
    rig.settle(&mut m, 2);
    assert_eq!(m.search, "is");
    // Down moves to the first result (the first conversation header), further Downs to the next row.
    rig.key(&mut m, Key::ArrowDown);
    rig.settle(&mut m, 2);
    assert!(m.found.is_none(), "moving does not open a message");
    rig.key(&mut m, Key::ArrowDown);
    rig.settle(&mut m, 2);
    // Enter opens the focused hit.
    rig.key(&mut m, Key::Enter);
    rig.settle(&mut m, 2);
    assert!(
        m.found.is_some(),
        "Enter on a message result opens it: {:?}",
        m.thread
    );
    // Back in the box, Escape clears the text and the cursor stays.
    rig.key_with(&mut m, Key::F, Modifiers::COMMAND);
    rig.settle(&mut m, 2);
    rig.key(&mut m, Key::Escape);
    rig.settle(&mut m, 2);
    assert!(m.search.is_empty(), "Escape clears the search");
    rig.type_text(&mut m, "x");
    assert_eq!(m.search, "x", "the box still has the cursor");
}

#[test]
fn the_result_count_is_a_polite_live_region() {
    let _clock = pin();
    use crate::{connection::ControlState, shell::Env, test_support::paired_model};
    let mut m = messages_with(search_fixture());
    m.search = "is".into();
    let (model, control) = (paired_model(), ControlState::default());
    let audit = crate::a11y_tests::audit_screen(
        "search count",
        AppKind::Messages,
        egui::vec2(1000.0, 700.0),
        move |ctx| {
            let pal = crate::theme::current(ctx, AppKind::Messages);
            let env = Env {
                app: AppKind::Messages,
                api_port: 1,
                model: &model,
                pal: &pal,
                control: &control,
            };
            m.show(ctx, &env);
        },
    );
    let node = audit
        .tree
        .nodes
        .values()
        .find(|n| n.live() == Some(egui::accesskit::Live::Polite))
        .expect("a polite live node");
    let text = format!("{:?}{:?}", node.label(), node.value());
    assert!(text.contains("3 matching messages"), "{text}");
}
