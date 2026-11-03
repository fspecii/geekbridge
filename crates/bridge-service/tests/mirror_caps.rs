// SPDX-License-Identifier: GPL-3.0-or-later
//! Notification mirror memory limits, age limit and banner rate limit, through the public API only.
//!
//! The mirror keeps phone notifications in memory only; these tests pin the bounds that stop a chatty or
//! hostile phone from growing it: per-phone and per-computer caps, the three-hour life, ordering, and the
//! banner burst limit.
use bridge_core::notification_link::Mirrored;
use bridge_service::mirror::{
    Choice, MAX_DEVICES, MAX_PER_DEVICE, Mirror, NotifySendMirror, Posted, TTL,
};
use std::time::{Duration, Instant};

fn n(id: &str) -> Mirrored {
    Mirrored {
        id: id.to_owned(),
        app: "Chat".into(),
        title: format!("title {id}"),
        text: "code 123456".into(),
        time_ms: Some(1),
        clearable: true,
        silent: false,
        only_once: false,
        actions: vec![],
    }
}

fn ids(m: &Mirror, device: Option<&str>, now: Instant) -> Vec<String> {
    m.view(device, vec![], now)
        .items
        .into_iter()
        .map(|i| i.id)
        .collect()
}

#[test]
fn an_entry_lives_exactly_ttl_and_not_a_moment_longer() {
    let m = Mirror::default();
    let t0 = Instant::now();
    m.post("a", "Pixel", n("1"), t0);
    assert!(m.get("a", "1", t0 + TTL).is_some(), "still held at TTL");
    assert!(
        m.get("a", "1", t0 + TTL + Duration::from_millis(1))
            .is_none(),
        "gone just after"
    );
    // Expiry also empties the phone's slot, so the count and the view agree.
    assert_eq!(m.count("a"), 0);
    assert!(ids(&m, None, t0 + TTL + Duration::from_secs(1)).is_empty());
}

#[test]
fn an_update_restarts_the_clock_and_moves_the_entry_to_the_front() {
    let m = Mirror::default();
    let t0 = Instant::now();
    m.post("a", "Pixel", n("old"), t0);
    m.post("a", "Pixel", n("new"), t0 + Duration::from_secs(10));
    let later = t0 + TTL + Duration::from_secs(20);
    // "old" is posted again (an update) just before it would expire: it survives, "new" is still younger.
    assert_eq!(
        m.post("a", "Pixel", n("old"), t0 + TTL - Duration::from_secs(1)),
        Posted::Updated
    );
    assert_eq!(
        ids(&m, None, later),
        vec!["old"],
        "only the refreshed one is left"
    );
    assert!(m.get("a", "new", later).is_none());
}

#[test]
fn the_oldest_goes_first_when_a_phone_exceeds_its_cap_and_updates_do_not_evict() {
    let m = Mirror::default();
    let t = Instant::now();
    for i in 0..MAX_PER_DEVICE {
        m.post(
            "a",
            "Pixel",
            n(&format!("n{i}")),
            t + Duration::from_millis(i as u64),
        );
    }
    assert_eq!(m.count("a"), MAX_PER_DEVICE);
    // Updating the oldest at a full cap evicts nothing.
    let at = t + Duration::from_secs(1);
    assert_eq!(m.post("a", "Pixel", n("n0"), at), Posted::Updated);
    assert_eq!(m.count("a"), MAX_PER_DEVICE);
    assert!(m.get("a", "n1", at).is_some());
    // A genuinely new one evicts the oldest *remaining* (n1, since n0 was refreshed).
    assert_eq!(m.post("a", "Pixel", n("fresh"), at), Posted::New);
    assert_eq!(m.count("a"), MAX_PER_DEVICE);
    assert!(m.get("a", "n1", at).is_none() && m.get("a", "n0", at).is_some());
    assert!(m.get("a", "fresh", at).is_some());
}

#[test]
fn another_phone_can_never_push_out_a_phones_notifications() {
    let m = Mirror::default();
    let t = Instant::now();
    m.post("a", "Pixel", n("a1"), t);
    for i in 0..(MAX_PER_DEVICE * 3) {
        m.post("b", "Tablet", n(&format!("b{i}")), t);
    }
    assert_eq!(m.count("a"), 1);
    assert_eq!(m.count("b"), MAX_PER_DEVICE);
    // The same notification id on two phones is two notifications.
    m.post("b", "Tablet", n("a1"), t);
    assert_eq!(m.get("a", "a1", t).unwrap().title, "title a1");
    assert!(m.remove("b", "a1"));
    assert!(
        m.get("a", "a1", t).is_some(),
        "removing one phone's does not touch the other's"
    );
}

#[test]
fn a_full_table_refuses_a_new_phone_until_one_expires_or_is_cleared() {
    let m = Mirror::default();
    let t0 = Instant::now();
    for d in 0..MAX_DEVICES {
        m.post(&format!("d{d}"), "P", n("x"), t0);
    }
    // Refused: reported as New (so the caller's banner policy still applies) but not kept.
    assert_eq!(m.post("extra", "P", n("x"), t0), Posted::New);
    assert_eq!(m.count("extra"), 0);
    // A phone that is already held can still post.
    assert_eq!(m.post("d0", "P", n("y"), t0), Posted::New);
    assert_eq!(m.count("d0"), 2);
    // Clearing one makes room.
    m.clear("d1");
    m.post("extra", "P", n("x"), t0);
    assert_eq!(m.count("extra"), 1);
    // Expiry makes room too: after the TTL a new phone fits although nothing was cleared by hand.
    let later = t0 + TTL + Duration::from_secs(1);
    assert_eq!(m.post("late", "P", n("z"), later), Posted::New);
    assert_eq!(m.count("late"), 1);
    assert_eq!(m.count("d0"), 0, "the old ones are gone");
}

#[test]
fn removing_the_last_notification_frees_the_phones_slot() {
    let m = Mirror::default();
    let t = Instant::now();
    for d in 0..MAX_DEVICES {
        m.post(&format!("d{d}"), "P", n("only"), t);
    }
    assert!(m.remove("d3", "only"));
    assert!(
        !m.remove("d3", "only"),
        "second removal reports nothing held"
    );
    assert!(!m.remove("never", "only"));
    m.post("newcomer", "P", n("x"), t);
    assert_eq!(m.count("newcomer"), 1);
}

#[test]
fn the_view_is_newest_first_filterable_and_carries_the_enabled_list() {
    let m = Mirror::default();
    let t0 = Instant::now();
    m.post("a", "Pixel", n("a1"), t0);
    m.post("b", "Tablet", n("b1"), t0 + Duration::from_secs(1));
    m.post("a", "Pixel", n("a2"), t0 + Duration::from_secs(2));
    let now = t0 + Duration::from_secs(3);
    assert_eq!(ids(&m, None, now), vec!["a2", "b1", "a1"]);
    assert_eq!(ids(&m, Some("a"), now), vec!["a2", "a1"]);
    assert!(ids(&m, Some("nobody"), now).is_empty());
    let v = m.view(None, vec!["a".into(), "z".into()], now);
    assert_eq!(v.enabled_devices, vec!["a", "z"]);
    let first = &v.items[0];
    assert_eq!(
        (
            first.device_id.as_str(),
            first.device_name.as_str(),
            first.app.as_str()
        ),
        ("a", "Pixel", "Chat")
    );
}

#[test]
fn a_phone_name_is_cleaned_and_bounded() {
    let m = Mirror::default();
    let t = Instant::now();
    m.post("a", &format!("Pi\u{7}x\nel{}", "x".repeat(200)), n("1"), t);
    let name = m.view(None, vec![], t).items[0].device_name.clone();
    assert!(name.chars().count() <= 64, "{}", name.len());
    assert!(!name.chars().any(char::is_control) && name.starts_with("Pixel"));
}

#[test]
fn banners_come_as_a_burst_then_one_per_gap_and_idle_time_never_banks_more_than_the_burst() {
    let m = Mirror::default();
    let t0 = Instant::now();
    let burst = (0..100).filter(|_| m.banner_allowed("dev", t0)).count();
    assert!(burst >= 2, "{burst}");
    assert!(!m.banner_allowed("dev", t0), "exhausted");
    // Half a gap is not enough, a whole one is.
    assert!(!m.banner_allowed("dev", t0 + Duration::from_millis(500)));
    assert!(m.banner_allowed("dev", t0 + Duration::from_millis(2500)));
    assert!(!m.banner_allowed("dev", t0 + Duration::from_millis(2500)));
    // A very long quiet spell refills only up to the burst, not to the number of gaps that passed.
    let much_later = t0 + Duration::from_secs(24 * 3600);
    let refilled = (0..100)
        .filter(|_| m.banner_allowed("dev", much_later))
        .count();
    assert_eq!(refilled, burst);
    // Time going backwards (clock step) neither panics nor grants extra banners.
    assert!(!m.banner_allowed("dev", t0));
}

#[test]
fn the_banner_limit_does_not_hide_a_notification_from_the_list() {
    let m = Mirror::default();
    let t = Instant::now();
    for i in 0..30 {
        let _ = m.banner_allowed("dev", t);
        m.post("a", "Pixel", n(&format!("n{i}")), t);
    }
    assert_eq!(
        m.count("a"),
        30,
        "the list keeps everything the banner policy withheld"
    );
}

#[test]
fn content_is_hidden_until_the_user_chooses_to_show_it() {
    let m = Mirror::default();
    assert!(!m.show_content());
    m.set_show_content(true);
    assert!(m.show_content());
    m.set_show_content(false);
    assert!(!m.show_content());
}

#[test]
fn a_button_press_names_only_a_button_the_banner_offered() {
    let actions = vec!["Reply".to_owned(), "Archive".to_owned()];
    assert_eq!(
        NotifySendMirror::choice("dismiss\n", &actions),
        Some(Choice::Dismiss)
    );
    assert_eq!(
        NotifySendMirror::choice(" a0 ", &actions),
        Some(Choice::Action("Reply".into()))
    );
    assert_eq!(
        NotifySendMirror::choice("a1", &actions),
        Some(Choice::Action("Archive".into()))
    );
    for bad in [
        "a2",
        "a-1",
        "a",
        "A0",
        "0",
        "dismiss now",
        "a0 a1",
        "a99999999999999999999",
        "default",
        "\u{0}",
    ] {
        assert_eq!(NotifySendMirror::choice(bad, &actions), None, "{bad:?}");
    }
    // A phone-supplied button title can never be mistaken for the dismiss key or another index.
    let tricky = vec!["dismiss".to_owned(), "a0".to_owned()];
    assert_eq!(
        NotifySendMirror::choice("a0", &tricky),
        Some(Choice::Action("dismiss".into()))
    );
    assert_eq!(
        NotifySendMirror::choice("dismiss", &tricky),
        Some(Choice::Dismiss)
    );
}

#[test]
fn many_threads_posting_removing_and_reading_never_break_the_bounds() {
    let m = std::sync::Arc::new(Mirror::default());
    let t = Instant::now();
    let workers: Vec<_> = (0..6)
        .map(|w| {
            let m = m.clone();
            std::thread::spawn(move || {
                for i in 0..400 {
                    let dev = format!("d{}", (w + i) % (MAX_DEVICES + 4));
                    m.post(
                        &dev,
                        "P",
                        n(&format!("n{}", i % 90)),
                        t + Duration::from_millis(i as u64),
                    );
                    if i % 7 == 0 {
                        m.remove(&dev, &format!("n{}", (i + 3) % 90));
                    }
                    if i % 53 == 0 {
                        m.clear(&dev);
                    }
                    let _ = m.view(None, vec![], t + Duration::from_millis(i as u64));
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let view = m.view(None, vec![], t + Duration::from_secs(1));
    let mut per: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for i in &view.items {
        *per.entry(i.device_id.as_str()).or_default() += 1;
    }
    assert!(per.len() <= MAX_DEVICES, "{} phones", per.len());
    assert!(per.values().all(|c| *c <= MAX_PER_DEVICE));
}
