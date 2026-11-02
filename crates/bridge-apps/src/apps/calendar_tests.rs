// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless renders of the calendar app over fixture views: every mode, odd data, narrow windows,
//! the editor and the details pane. They check that nothing panics and that the logic around the
//! painting (selection, stepping, creating) reacts as intended.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::state::DiscoveryModel;
use crate::test_support::{Rig, shapes, wait_until};
use bridge_core::AppKind;
use bridge_core::phone_view::{DataDevice, EditPhase};
use eframe::egui::{Key, Modifiers};

fn device(edit: DataState, writable: bool) -> DataDevice {
    DataDevice {
        device_id: "p".into(),
        name: "Pixel".into(),
        contacts: DataState::Ready,
        calendar: DataState::Ready,
        sms: DataState::ConsentOff,
        calendar_edit: edit,
        contacts_count: 0,
        events_count: 1,
        calendar_names: vec![],
        calendar_writable: writable,
        calendar_truncated: false,
        contacts_capped: false,
        threads_on_phone: None,
        contacts_synced_unix_ms: None,
        calendar_synced_unix_ms: None,
        sms_status_reports: false,
        sms_send: false,
        sims: vec![],
    }
}

fn ev(id: &str, title: &str, start_ms: i64, end_ms: i64) -> EventItem {
    EventItem {
        device_id: "p".into(),
        id: id.into(),
        title: title.into(),
        all_day: false,
        start_ms,
        end_ms,
        location: Some("Room 4".into()),
        description: Some("Bring the notes.\nSecond line.".into()),
        calendar_id: Some("1".into()),
        event_id: Some(id.split('@').next().unwrap().into()),
        recurring: false,
        tz: Some("Europe/Berlin".into()),
        reminders_min: vec![10, 60],
        attendee_count: 2,
        attendees: vec!["ann@example.org".into(), "bo@example.org".into()],
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

const H: i64 = 3_600_000;
const D: i64 = 86_400_000;

fn fixture_view(edit: DataState) -> AgendaView {
    let n = now_ms();
    let mut events = vec![
        ev("1@1", "Stand-up", n, n + H / 2),
        ev("2@1", "Overlaps stand-up", n + H / 4, n + H),
        ev("3@1", "Tomorrow", n + D, n + D + H),
        ev("4@1", "Last week", n - 6 * D, n - 6 * D + H),
        ev("5@1", "Zero length", n + 2 * H, n + 2 * H),
        ev("6@1", "עברית ملاحظات 📅 e\u{301}", n + 3 * H, n + 4 * H),
        ev("7@1", &"very long title ".repeat(40), n + 5 * H, n + 7 * H),
        ev("8@1", "Spans midnight", n + 20 * H, n + 30 * H),
    ];
    let mut all_day = ev("9@1", "Holiday", (n / D) * D, (n / D) * D + 2 * D);
    all_day.all_day = true;
    events.push(all_day);
    let mut rec = ev("10@1", "Weekly sync", n + 6 * H, n + 7 * H);
    rec.recurring = true;
    events.push(rec);
    let mut orphan = ev("11@1", "No calendar", n + H, n + 2 * H);
    orphan.calendar_id = None;
    orphan.event_id = None;
    orphan.location = None;
    orphan.description = None;
    orphan.attendees.clear();
    orphan.reminders_min.clear();
    events.push(orphan);
    AgendaView {
        devices: vec![device(edit, true)],
        calendars: vec![
            CalendarItem {
                device_id: "p".into(),
                id: "1".into(),
                name: "Mine".into(),
                account: Some("me@example.org".into()),
                account_type: Some("com.google".into()),
                color: Some(0xff33_66cc),
                writable: true,
            },
            CalendarItem {
                device_id: "p".into(),
                id: "2".into(),
                name: "Holidays".into(),
                account: None,
                account_type: None,
                color: None,
                writable: false,
            },
        ],
        edits: vec![
            EditItem {
                op_id: "o1".into(),
                device_id: "p".into(),
                action: CalendarEditAction::Create,
                title: "Dentist".into(),
                phase: EditPhase::HandedToPhone,
                reason: None,
                created_unix_ms: 1,
                updated_unix_ms: 2,
            },
            EditItem {
                op_id: "o2".into(),
                device_id: "p".into(),
                action: CalendarEditAction::Delete,
                title: "Old".into(),
                phase: EditPhase::Failed,
                reason: Some("no_permission".into()),
                created_unix_ms: 1,
                updated_unix_ms: 2,
            },
        ],
        events,
        events_truncated: true,
        ..AgendaView::default()
    }
}

fn calendar_with(view: AgendaView) -> Calendar {
    let mut cal = Calendar::default();
    let shared = view.clone();
    cal.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| Ok(shared.clone()),
        |_| Ok(Some("done".into())),
        || {},
    ));
    cal
}

fn ready(cal: &mut Calendar, rig: &mut Rig) {
    // The first frame sets the request; the worker answers; later frames draw the data.
    rig.settle(cal, 2);
    let feed = cal.feed.as_ref().unwrap();
    wait_until("view", || feed.state().view.is_some());
    rig.settle(cal, 3);
}

#[test]
fn every_mode_draws_the_fixture_at_wide_and_narrow_sizes() {
    for (w, h) in [(1100.0, 760.0), (480.0, 600.0), (260.0, 320.0)] {
        for mode in [Mode::Month, Mode::Week, Mode::Day, Mode::List] {
            let mut cal = calendar_with(fixture_view(DataState::Ready));
            cal.mode = mode;
            let mut rig = Rig::new(AppKind::Agenda, w, h);
            ready(&mut cal, &mut rig);
            let out = rig.settle(&mut cal, 2);
            assert!(shapes(&out) > 10, "{mode:?} at {w}x{h} drew nothing");
        }
    }
}

#[test]
fn stepping_and_jumping_move_the_shown_day_in_every_mode() {
    let mut cal = Calendar::default();
    let now = localtime::now();
    for (mode, min_gap, max_gap) in [
        (Mode::Day, 1, 1),
        (Mode::Week, 7, 7),
        (Mode::Month, 28, 31),
        (Mode::List, 1, 31),
    ] {
        cal.mode = mode;
        cal.picked = None;
        let before = cal.shown(&now);
        cal.step(1, &now);
        let after = cal.shown(&now);
        assert!(after != before, "{mode:?}: next moved");
        assert!(
            (i64::from(off(after)) - i64::from(off(before))).abs() <= 60,
            "dst jump at most an hour"
        );
        let gap = (after.to_days() - before.to_days()).abs();
        assert!((min_gap..=max_gap).contains(&gap), "{mode:?}: {gap}");
        cal.step(-2, &now);
        assert!(cal.shown(&now) != after);
        cal.select_day(now.date, &now);
        assert_eq!(cal.shown(&now), now.date);
        assert!(!cal.title(&now).is_empty());
    }
}

#[test]
fn clicking_an_event_opens_the_details_and_escape_style_state_is_clean() {
    let mut cal = calendar_with(fixture_view(DataState::Ready));
    cal.mode = Mode::List;
    let mut rig = Rig::new(AppKind::Agenda, 1100.0, 760.0);
    ready(&mut cal, &mut rig);
    cal.selected = Some(("p".into(), "1@1".into()));
    let out = rig.settle(&mut cal, 3);
    assert!(shapes(&out) > 10);
    // A selection that no longer exists in the view must not crash or stick as a ghost pane.
    cal.selected = Some(("p".into(), "gone".into()));
    rig.settle(&mut cal, 3);
    cal.confirm_delete = true;
    cal.selected = Some(("p".into(), "2@1".into()));
    rig.settle(&mut cal, 3);
    // Recurring and orphaned events are shown but cannot be edited from here.
    for id in ["10@1", "11@1", "8@1", "9@1", "5@1", "7@1", "6@1"] {
        cal.selected = Some(("p".into(), id.into()));
        for mode in [Mode::Day, Mode::Week, Mode::Month, Mode::List] {
            cal.mode = mode;
            rig.settle(&mut cal, 2);
        }
    }
}

#[test]
fn the_editor_opens_for_new_and_existing_events_and_survives_bad_input() {
    let view = fixture_view(DataState::Ready);
    let mut cal = calendar_with(view.clone());
    let mut rig = Rig::new(AppKind::Agenda, 1000.0, 760.0);
    ready(&mut cal, &mut rig);
    cal.open_new(localtime::now().date, 9);
    assert!(cal.editor.is_some());
    rig.settle(&mut cal, 3);
    for bad in [
        "",
        "not a date",
        "9999-99-99",
        "2024-02-30",
        "\u{202e}rtl",
        &"x".repeat(5000),
    ] {
        if let Some(e) = cal.editor.as_mut() {
            e.fields.title = bad.into();
            e.fields.start_date = bad.into();
            e.fields.start_time = bad.into();
            e.fields.end_date = bad.into();
            e.fields.end_time = bad.into();
            e.error = Some(bad.chars().take(50).collect());
            e.calendar = Some(("p".into(), "1".into()));
        }
        rig.settle(&mut cal, 2);
    }
    // Existing event editor.
    let first = view.events[0].clone();
    if let Some(block) = edit_block(&view, &first) {
        panic!("a plain event on a writable phone should be editable: {block}");
    }
    cal.editor = Some(Editor {
        existing: Some(Existing {
            device_id: "p".into(),
            event_id: "1".into(),
            calendar_label: "Mine".into(),
        }),
        fields: Fields::new_on(localtime::now().date, 9),
        calendar: Some(("p".into(), "1".into())),
        error: None,
    });
    rig.settle(&mut cal, 3);
    // Narrow window with the editor open.
    let mut narrow = Rig::new(AppKind::Agenda, 240.0, 300.0);
    narrow.settle(&mut cal, 3);
}

#[test]
fn missing_service_unpaired_and_error_states_render_their_explanations() {
    for model in [
        DiscoveryModel::Checking,
        DiscoveryModel::ServiceUnavailable {
            reason: "refused".into(),
        },
        DiscoveryModel::Ready {
            discovery_error: Some("mdns".into()),
            devices: vec![],
        },
    ] {
        for mode in [Mode::Month, Mode::Week, Mode::Day, Mode::List] {
            let mut cal = Calendar::default();
            cal.mode = mode;
            cal.feed = Some(Feed::spawn_with(
                Duration::from_millis(10),
                Request::Status,
                |_| Err("The service is not running.".to_string()),
                |_| Err("refused".into()),
                || {},
            ));
            let mut rig = Rig::new(AppKind::Agenda, 800.0, 600.0);
            rig.model = model.clone();
            rig.settle(&mut cal, 2);
            let feed = cal.feed.as_ref().unwrap();
            wait_until("problem", || feed.state().problem.is_some());
            rig.settle(&mut cal, 3);
        }
    }
    // Every consent / link state a phone can be in.
    for st in [
        DataState::ConsentOff,
        DataState::Offline,
        DataState::NotOffered,
        DataState::Waiting,
        DataState::Ready,
    ] {
        let mut v = fixture_view(DataState::ConsentOff);
        v.devices[0].calendar = st;
        v.devices[0].calendar_edit = st;
        if st != DataState::Ready {
            v.events.clear();
        }
        let mut cal = calendar_with(v);
        let mut rig = Rig::new(AppKind::Agenda, 800.0, 600.0);
        ready(&mut cal, &mut rig);
    }
    // No device at all, and a hundred of them.
    let mut cal = calendar_with(AgendaView::default());
    let mut rig = Rig::new(AppKind::Agenda, 800.0, 600.0);
    ready(&mut cal, &mut rig);
    let mut many = fixture_view(DataState::Ready);
    many.devices = (0..100)
        .map(|i| DataDevice {
            device_id: format!("d{i}"),
            name: format!("Phone {i}"),
            ..device(DataState::Ready, i % 2 == 0)
        })
        .collect();
    let mut cal = calendar_with(many);
    ready(&mut cal, &mut Rig::new(AppKind::Agenda, 800.0, 600.0));
}

#[test]
fn a_thousand_events_in_one_day_still_render() {
    let n = now_ms();
    let mut v = fixture_view(DataState::Ready);
    v.events = (0..1000)
        .map(|i| {
            ev(
                &format!("{i}@1"),
                &format!("Event {i}"),
                n + (i % 24) * H / 2,
                n + (i % 24) * H / 2 + H,
            )
        })
        .collect();
    for mode in [Mode::Day, Mode::Week, Mode::Month, Mode::List] {
        let mut cal = calendar_with(v.clone());
        cal.mode = mode;
        let mut rig = Rig::new(AppKind::Agenda, 900.0, 700.0);
        ready(&mut cal, &mut rig);
    }
}

#[test]
fn keyboard_and_clicks_on_the_empty_window_do_not_panic() {
    let mut cal = calendar_with(fixture_view(DataState::Ready));
    let mut rig = Rig::new(AppKind::Agenda, 1000.0, 700.0);
    ready(&mut cal, &mut rig);
    for key in [
        Key::Tab,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::ArrowDown,
        Key::Enter,
        Key::Escape,
        Key::Space,
    ] {
        rig.key(&mut cal, key);
        rig.key_with(&mut cal, key, Modifiers::SHIFT);
    }
    for mode in [Mode::Day, Mode::Week, Mode::Month, Mode::List] {
        cal.mode = mode;
        for y in (60..700).step_by(60) {
            for x in (40..1000).step_by(120) {
                rig.click(&mut cal, eframe::egui::pos2(x as f32, y as f32));
            }
        }
    }
}
