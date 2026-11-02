// SPDX-License-Identifier: GPL-3.0-or-later
//! Contacts: collation, sections and the headless render over odd and huge phonebooks.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::state::DiscoveryModel;
use crate::test_support::{Rig, shapes, wait_until};
use bridge_core::AppKind;
use bridge_core::phone_view::{DataDevice, DataState};
use std::time::Duration;

fn person(id: usize, name: &str) -> Person {
    Person {
        device_id: "d".into(),
        id: id.to_string(),
        name: name.into(),
        phones: vec!["+1 555 0100".into(), "020 7946 0000".into()],
        emails: vec!["a@example.org".into()],
        organization: id.is_multiple_of(2).then(|| "ACME 🏭".into()),
    }
}

fn device(contacts: DataState) -> DataDevice {
    DataDevice {
        device_id: "d".into(),
        name: "Pixel".into(),
        contacts,
        calendar: DataState::ConsentOff,
        sms: DataState::Ready,
        calendar_edit: DataState::ConsentOff,
        contacts_count: 5,
        events_count: 0,
        calendar_names: vec![],
        calendar_writable: false,
        calendar_truncated: false,
        contacts_capped: true,
        threads_on_phone: None,
        contacts_synced_unix_ms: Some(1),
        calendar_synced_unix_ms: None,
        sms_status_reports: false,
        sms_send: true,
        sims: vec![],
    }
}

fn contacts_with(view: AgendaView) -> Contacts {
    let mut c = Contacts::default();
    c.feed = Some(Feed::spawn_with(
        Duration::from_millis(10),
        Request::Status,
        move |_| Ok(view.clone()),
        |_| Ok(Some("done".into())),
        || {},
    ));
    c
}

fn ready(c: &mut Contacts, rig: &mut Rig) {
    rig.settle(c, 2);
    let feed = c.feed.as_ref().unwrap();
    wait_until("view", || feed.state().view.is_some());
    rig.settle(c, 3);
}

const NAMES: &[&str] = &[
    "Ann",
    "Émile",
    "Zoë",
    "ångström",
    "Ωmega",
    "李雷",
    "עברי",
    "محمد",
    "😀 Emoji",
    "42nd Street",
    "",
    " ",
    "\u{202e}Evil",
    "Ünal",
    "ß",
    "İstanbul",
    "ǅ",
    "Longname ",
    "a\u{301}",
];

#[test]
fn collation_puts_every_script_somewhere_sensible_and_never_panics() {
    for n in NAMES {
        let c = letter_of(n);
        assert!(
            c == '#' || c == '…' || c.is_ascii_uppercase(),
            "{n:?} -> {c:?}"
        );
        let _ = initials(n);
    }
    assert_eq!(letter_of("Émile"), 'E');
    assert_eq!(letter_of("42"), '#');
    assert_eq!(letter_of("李雷"), '…');
    let people: Vec<Person> = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| person(i, n))
        .collect();
    let secs = sections(&people);
    let covered: usize = secs.iter().map(|s| s.range.len()).sum();
    assert_eq!(covered, people.len(), "every person lands in a section");
    for s in &secs {
        assert!(offset_of(&secs, s.letter).is_some());
    }
    assert_eq!(offset_of(&secs, 'Q'), None);
    assert!(sections(&[]).is_empty());
}

#[test]
fn renders_full_empty_capped_and_failing_phonebooks_at_every_width() {
    let people: Vec<Person> = (0..300)
        .map(|i| person(i, NAMES[i % NAMES.len()]))
        .collect();
    for (w, h) in [(900.0, 600.0), (420.0, 500.0), (250.0, 300.0)] {
        let mut v = AgendaView::default();
        v.people = people.clone();
        v.people_total = 5000;
        v.devices = vec![device(DataState::Ready)];
        let mut c = contacts_with(v);
        let mut rig = Rig::new(AppKind::Contacts, w, h);
        ready(&mut c, &mut rig);
        c.person = Some(("d".into(), "3".into()));
        c.jump = Some(200.0);
        let out = rig.settle(&mut c, 3);
        assert!(shapes(&out) > 5);
        c.person = Some(("d".into(), "missing".into()));
        rig.settle(&mut c, 2);
        c.query = "zzzz".repeat(40);
        rig.settle(&mut c, 2);
        assert!(c.query.chars().count() <= 64, "search text is bounded");
    }
    for st in [
        DataState::ConsentOff,
        DataState::Offline,
        DataState::NotOffered,
        DataState::Waiting,
        DataState::Ready,
    ] {
        let mut v = AgendaView::default();
        v.devices = vec![device(st)];
        let mut c = contacts_with(v);
        let mut rig = Rig::new(AppKind::Contacts, 800.0, 600.0);
        ready(&mut c, &mut rig);
    }
    for model in [
        DiscoveryModel::Checking,
        DiscoveryModel::ServiceUnavailable { reason: "x".into() },
    ] {
        let mut c = Contacts::default();
        c.feed = Some(Feed::spawn_with(
            Duration::from_millis(10),
            Request::Status,
            |_| Err("The service is not running.".into()),
            |_| Err("no".into()),
            || {},
        ));
        let mut rig = Rig::new(AppKind::Contacts, 800.0, 600.0);
        rig.model = model;
        rig.settle(&mut c, 2);
        let feed = c.feed.as_ref().unwrap();
        wait_until("problem", || feed.state().problem.is_some());
        rig.settle(&mut c, 3);
    }
}

#[test]
fn clicking_around_the_list_and_details_does_not_panic() {
    let mut v = AgendaView::default();
    v.people = (0..40).map(|i| person(i, NAMES[i % NAMES.len()])).collect();
    v.people_total = 40;
    v.devices = vec![device(DataState::Ready)];
    let mut c = contacts_with(v);
    let mut rig = Rig::new(AppKind::Contacts, 900.0, 600.0);
    ready(&mut c, &mut rig);
    for y in (60..600).step_by(25) {
        for x in [30.0, 150.0, 280.0, 500.0, 700.0] {
            rig.click(&mut c, eframe::egui::pos2(x, y as f32));
        }
    }
}
