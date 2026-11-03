// SPDX-License-Identifier: GPL-3.0-or-later
//! The phone-data hub at realistic phone size: contacts, calendar and SMS driven through the same public
//! entry points the link uses (`link_up`, `on_packet`, `agenda_view_range`, `messages_view`).
//!
//! Default sizes are small so `cargo test` stays quick; `GEEKBRIDGE_SCALE=1` runs a heavy phone (5,000 contacts,
//! 1,000 events per sync, 20,000 texts over 800 threads) and enforces the time budgets in docs/reports/scale-report.md.
//! Numbers are printed (`cargo test -p bridge-service --test scale --release -- --nocapture`).

use bridge_core::{
    ipc::Feature,
    phone_link as wire,
    phone_view::{
        MAX_CONVERSATIONS_SHOWN, MAX_MESSAGES_LIMIT, MAX_MESSAGES_PER_PHONE,
        MAX_MESSAGES_PER_THREAD, MAX_MESSAGES_SHOWN, MAX_PEOPLE_LIMIT, MAX_PEOPLE_SHOWN,
    },
};
use bridge_kde::sms;
use bridge_service::phone_data::{Caps, Disposition, Out, PhoneData};
use serde_json::{Map, Value, json};
use std::time::Instant;
use tokio::sync::mpsc;

const ID: &str = "pppppppppppppppppppppppppppppppp";

fn big() -> bool {
    std::env::var("GEEKBRIDGE_SCALE").as_deref() == Ok("1")
}
fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}
fn caps() -> Caps {
    Caps {
        contacts: true,
        calendar: true,
        calendar_write: true,
        sms_read: true,
        sms_send: true,
        sms_status: true,
        battery_request: true,
        notification_request: true,
        notification_action: true,
    }
}
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4)
}
fn bytes_of(m: &Map<String, Value>) -> usize {
    serde_json::to_vec(m).map_or(0, |v| v.len())
}
fn drain(rx: &mut mpsc::Receiver<Out>) -> Vec<Out> {
    let mut v = Vec::new();
    while let Ok(o) = rx.try_recv() {
        v.push(o);
    }
    v
}
/// Wall time of `f` in milliseconds.
fn ms<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64() * 1000.0)
}
/// Time budgets only bite in the big, release-mode run on a normal machine; debug builds are ~10x slower.
fn budget(what: &str, took_ms: f64, limit_ms: f64) {
    let limit = if cfg!(debug_assertions) {
        limit_ms * 12.0
    } else {
        limit_ms
    };
    if big() {
        assert!(
            took_ms <= limit,
            "{what}: {took_ms:.1} ms > budget {limit:.0} ms"
        );
    }
}

const NAMES: [&str; 10] = [
    "Zoë Müller",
    "Émile Zola",
    "山田太郎",
    "李小龍",
    "Łukasz Żółć",
    "Ада Лавлейс",
    "🎉 Party Planner",
    "Ñandú",
    "Søren",
    "Çelik",
];
fn name(i: usize) -> String {
    format!("{} {i}", NAMES[i % NAMES.len()])
}
fn vcard(i: usize) -> String {
    let photo = if i.is_multiple_of(10) {
        format!("PHOTO;ENCODING=b;TYPE=JPEG:{}\n", "A".repeat(2000))
    } else {
        String::new()
    };
    format!(
        "BEGIN:VCARD\nVERSION:3.0\nFN:{}\nTEL;TYPE=CELL:+4670{}\nTEL;TYPE=HOME:+4687{}\nEMAIL:p{i}@example.org\n{photo}END:VCARD\n",
        name(i),
        1_000_000 + i,
        2_000_000 + i
    )
}

fn day() -> &'static str {
    "2026-11-02"
}

// ------------------------------------------------------------------------------------------ contacts

#[test]
fn contacts_sync_search_and_incremental_refresh() {
    let n: usize = if big() { 5000 } else { 500 };
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (true, false, false));
    drain(&mut rx);
    let rss0 = rss_kib();
    let t0 = Instant::now();
    let mut uids = obj(json!({}));
    let list: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
    uids.insert("uids".into(), json!(list));
    for k in &list {
        uids.insert(k.clone(), json!("1"));
    }
    assert_eq!(
        h.on_packet(ID, wire::CONTACTS_UIDS, &uids),
        Disposition::Accepted
    );
    // The desktop asks for batches; answer each like the phone (photos stripped by the phone, but a stray one is tolerated).
    let (mut rounds, mut first_screen_ms, mut wire_bytes) = (0, 0.0, bytes_of(&uids));
    loop {
        let asks = drain(&mut rx);
        let Some(ask) = asks
            .into_iter()
            .find(|o| o.kind == wire::CONTACTS_REQUEST_VCARDS)
        else {
            break;
        };
        wire_bytes += bytes_of(&ask.body);
        let want: Vec<String> = ask.body["uids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert!(
            want.len() <= wire::VCARD_BATCH,
            "a vCard request never asks for more than a batch"
        );
        let mut reply = obj(json!({"uids": want}));
        for k in &want {
            reply.insert(k.clone(), json!(vcard(k[1..].parse().unwrap())));
        }
        wire_bytes += bytes_of(&reply);
        assert_eq!(
            h.on_packet(ID, wire::CONTACTS_VCARDS, &reply),
            Disposition::Accepted
        );
        rounds += 1;
        if rounds == 1 {
            first_screen_ms = t0.elapsed().as_secs_f64() * 1000.0;
        }
    }
    let full_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(rounds, n.div_ceil(wire::VCARD_BATCH));

    let (v, view_ms) = ms(|| h.agenda_view_range("", day(), 0, 1, 0).unwrap());
    assert_eq!(v.people_total, n);
    assert!(
        v.people.len() <= MAX_PEOPLE_SHOWN && v.people.len() < n.max(MAX_PEOPLE_SHOWN + 1),
        "view is capped: {}",
        v.people.len()
    );
    let (v_all, view_all_ms) = ms(|| {
        h.agenda_view_range("", day(), 0, 1, MAX_PEOPLE_LIMIT)
            .unwrap()
    });
    assert!(v_all.people.len() <= MAX_PEOPLE_LIMIT);
    // Accent-aware order: Çelik is among the Cs, 山田 after every Latin name, emoji/digits first.
    let first: Vec<&str> = v_all.people.iter().map(|p| p.name.as_str()).collect();
    let pos = |needle: &str| first.iter().position(|s| s.starts_with(needle));
    if let (Some(e), Some(z)) = (pos("Émile"), pos("Zoë")) {
        assert!(e < z, "Émile sorts before Zoë");
    }
    let (found, search_ms) = ms(|| h.agenda_view_range("emile", day(), 0, 1, 0).unwrap());
    assert!(
        found.people_total >= n / NAMES.len() - 1,
        "accent-insensitive search finds Émile: {}",
        found.people_total
    );
    let (_, digits_ms) = ms(|| h.agenda_view_range("4670100", day(), 0, 1, 0).unwrap());
    let view_bytes = serde_json::to_vec(&v_all).unwrap().len();
    assert!(
        view_bytes < 256 * 1024,
        "a reply fits one IPC response: {view_bytes} B"
    );

    // Incremental: one contact changed -> one vCard fetched, not 5,000.
    h.refresh_agenda(Some(ID)).unwrap();
    let asks = drain(&mut rx);
    assert!(asks.iter().any(|o| o.kind == wire::CONTACTS_REQUEST_UIDS));
    let mut again = uids.clone();
    again.insert("k7".into(), json!("2"));
    let (_, relist_ms) = ms(|| h.on_packet(ID, wire::CONTACTS_UIDS, &again));
    let asks = drain(&mut rx);
    let fetch = asks
        .iter()
        .find(|o| o.kind == wire::CONTACTS_REQUEST_VCARDS)
        .expect("changed contact is fetched");
    assert_eq!(
        fetch.body["uids"].as_array().unwrap().len(),
        1,
        "only the changed contact is fetched"
    );

    let rss1 = rss_kib();
    println!(
        "scale contacts n={n}: first batch {first_screen_ms:.0} ms, full {full_ms:.0} ms ({rounds} round trips, {} KiB on the wire), \
         view {view_ms:.1} ms (<=300) / {view_all_ms:.1} ms (<=1000, {} KiB), search {search_ms:.1} ms, digit search {digits_ms:.1} ms, \
         relist {relist_ms:.1} ms, RSS +{} KiB",
        wire_bytes / 1024,
        view_bytes / 1024,
        rss1.saturating_sub(rss0)
    );
    budget("contacts full sync", full_ms, 3000.0);
    budget("contacts view", view_ms, 60.0);
    budget("contacts search", search_ms, 60.0);
}

#[test]
fn contacts_listing_over_the_cap_is_refused_not_stored() {
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (true, false, false));
    drain(&mut rx);
    let list: Vec<String> = (0..wire::MAX_CONTACT_UIDS + 1)
        .map(|i| format!("k{i}"))
        .collect();
    let mut uids = obj(json!({ "uids": list }));
    for k in &list {
        uids.insert(k.clone(), json!("1"));
    }
    assert_eq!(
        h.on_packet(ID, wire::CONTACTS_UIDS, &uids),
        Disposition::Invalid
    );
    assert_eq!(h.agenda_view("", day(), 0).unwrap().people_total, 0);
}

// ------------------------------------------------------------------------------------------ calendar

fn cal_event(i: usize, now: i64) -> Value {
    let hour = 3_600_000_i64;
    let start = now + (i as i64 - 300) * 6 * hour; // spread from the past into the future
    let (all_day, tz, dur) = match i % 5 {
        0 => (true, "UTC", 24 * hour),
        1 => (false, "Asia/Tokyo", 72 * hour), // multi-day
        2 => (false, "America/New_York", hour),
        _ => (false, "Europe/Berlin", hour / 2),
    };
    let start = if all_day {
        start - start.rem_euclid(24 * hour)
    } else {
        start
    };
    json!({"id": format!("{}@{start}", i / 4), "rev": format!("r{i}"), "title": format!("Event {i} {}", name(i)),
           "allDay": all_day, "startMs": start, "endMs": start + dur, "tz": tz, "calendarId": format!("{}", 1 + i % 8),
           "eventId": format!("{}", i / 4), "recurring": i.is_multiple_of(3), "location": "Café 東京", "description": "Notes ".repeat(30),
           "reminders": [10], "attendees": {"count": 3, "names": ["Ada", "Bob", "李"]}})
}

fn calendars() -> Value {
    Value::Array(
        (1..=8)
            .map(|i| json!({"id": format!("{i}"), "name": format!("Calendar {i}"), "account": format!("acct{}@example.org", i % 3),
                            "accountType": "com.google", "color": -16776961, "writable": i % 2 == 0}))
            .collect(),
    )
}

#[test]
fn calendar_full_sync_view_and_single_event_delta() {
    let n = if big() {
        wire::MAX_CALENDAR_EVENTS
    } else {
        300
    };
    let now = 1_793_523_600_000_i64; // 2026-11-02
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (false, true, false));
    drain(&mut rx);
    let events: Vec<Value> = (0..n).map(|i| cal_event(i, now)).collect();
    let full = obj(
        json!({"version": 1, "full": true, "base": null, "token": "t:1", "truncated": n >= wire::MAX_CALENDAR_EVENTS,
                          "calendars": calendars(), "events": events, "deleted": []}),
    );
    let full_bytes = bytes_of(&full);
    assert!(full_bytes < 900 * 1024);
    let rss0 = rss_kib();
    let (d, full_ms) = ms(|| h.on_packet(ID, wire::CALENDAR_SYNC, &full));
    assert_eq!(d, Disposition::Accepted);

    // Month-ish views: 31 days around today, and the widest window the UI can ask for.
    let (v, month_ms) = ms(|| h.agenda_view_range("", day(), 0, 31, 0).unwrap());
    let (wide, wide_ms) = ms(|| h.agenda_view_range("", "2025-11-02", 0, 56, 0).unwrap());
    assert!(v.calendars.len() == 8);
    assert!(
        wide.events.len() <= 400,
        "events shown stay capped: {}",
        wide.events.len()
    );
    assert!(
        v.events.windows(2).all(|w| w[0].start_ms <= w[1].start_ms),
        "sorted by start"
    );

    // One event changes: the delta carries exactly that event and the desktop applies it without a resync.
    let mut changed = cal_event(5, now);
    changed["rev"] = json!("r5-changed");
    changed["title"] = json!("Moved");
    let delta = obj(
        json!({"version": 1, "full": false, "base": "t:1", "token": "t:2", "truncated": false,
                           "calendars": calendars(), "events": [changed], "deleted": []}),
    );
    let delta_bytes = bytes_of(&delta);
    let (d, delta_ms) = ms(|| h.on_packet(ID, wire::CALENDAR_SYNC, &delta));
    assert_eq!(d, Disposition::Accepted);
    assert!(
        drain(&mut rx).is_empty(),
        "a delta that follows the last token asks for nothing"
    );
    assert_eq!(
        h.agenda_view("", day(), 0).unwrap().devices[0].events_count,
        n
    );
    assert!(
        delta_bytes * 20 < full_bytes || n < 100,
        "delta {delta_bytes} B vs full {full_bytes} B"
    );

    // More than the documented per-packet cap is refused outright.
    let too_many: Vec<Value> = (0..wire::MAX_CALENDAR_EVENTS + 1)
        .map(|i| cal_event(i, now))
        .collect();
    let bad = obj(
        json!({"version": 1, "full": true, "base": null, "token": "t:3", "calendars": calendars(), "events": too_many, "deleted": []}),
    );
    assert_eq!(
        h.on_packet(ID, wire::CALENDAR_SYNC, &bad),
        Disposition::Invalid
    );

    println!(
        "scale calendar n={n}: full {full_bytes} B apply {full_ms:.1} ms, month view {month_ms:.1} ms, widest view {wide_ms:.1} ms, \
         1-event delta {delta_bytes} B apply {delta_ms:.2} ms, RSS +{} KiB",
        rss_kib().saturating_sub(rss0)
    );
    budget("calendar apply", full_ms, 200.0);
    budget("calendar view", month_ms, 60.0);
}

// ------------------------------------------------------------------------------------------ messages

fn sms_msg(thread: i64, id: i64, ty: i64, date: u64) -> Value {
    json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + thread)}],
           "body": format!("message {id} about lunch 🎉 Café 東京 the quick brown fox jumps over the lazy dog"),
           "date": date, "type": ty, "read": 1, "thread_id": thread, "_id": id, "sub_id": 1})
}

#[test]
fn messages_threads_history_caps_and_search() {
    let threads: i64 = if big() { 800 } else { 120 };
    let per_phone: usize = if big() { MAX_MESSAGES_PER_PHONE } else { 3000 };
    let now = 1_793_523_600_000_u64;
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (false, false, true));
    let asks = drain(&mut rx);
    assert!(asks.iter().any(|o| o.kind == sms::REQUEST_CONVERSATIONS));
    let rss0 = rss_kib();

    // The phone lists its newest 300 threads, 100 per packet, with the unread map on the first.
    let listed = threads.min(300);
    let t0 = Instant::now();
    let mut id = 0_i64;
    let mut wire_bytes = 0;
    for chunk in (1..=listed).collect::<Vec<_>>().chunks(100) {
        let msgs: Vec<Value> = chunk
            .iter()
            .map(|t| {
                id += 1;
                sms_msg(*t, id, 1, now - *t as u64 * 60_000)
            })
            .collect();
        let mut p = obj(json!({"version": 2, "messages": msgs}));
        if chunk[0] == 1 {
            p.insert("threadsTotal".into(), json!(threads));
        }
        wire_bytes += bytes_of(&p);
        assert_eq!(h.on_packet(ID, sms::MESSAGES, &p), Disposition::Accepted);
    }
    let (list_view, list_ms) = ms(|| h.messages_view(None, None, None).unwrap());
    let first_screen_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        list_view.conversations.len(),
        listed.min(MAX_CONVERSATIONS_SHOWN as i64) as usize
    );
    assert!(
        list_view
            .conversations
            .windows(2)
            .all(|w| w[0].last_date_ms >= w[1].last_date_ms),
        "newest first"
    );

    // Fill threads with history, newest first, in 50-message pages like the phone answers: the big thread gets 2,500.
    let mut total = listed as usize;
    let mut next_ts = now - 10_000_000;
    let mut page_bytes = 0;
    let mut pages = 0;
    'fill: for round in 0.. {
        for t in 1..=listed {
            let take = if t == 1 || round < 6 { 50 } else { 0 };
            if take == 0 && t > 1 {
                continue;
            }
            let msgs: Vec<Value> = (0..take)
                .map(|_| {
                    id += 1;
                    next_ts -= 1000;
                    sms_msg(t, id, 1 + id % 2, next_ts)
                })
                .collect();
            let p = obj(json!({"version": 2, "messages": msgs}));
            page_bytes += bytes_of(&p);
            pages += 1;
            h.on_packet(ID, sms::MESSAGES, &p);
            total += take as usize;
            if total >= per_phone + 500 {
                break 'fill;
            }
        }
        if round > 400 {
            break;
        }
    }
    let _ = drain(&mut rx);

    let (view, view_ms) = ms(|| h.messages_view(None, None, None).unwrap());
    let held: usize = view.conversations.iter().map(|c| c.message_count).sum();
    assert!(held <= MAX_MESSAGES_PER_PHONE, "per-phone cap: {held}");
    let (sel, thread_ms) = ms(|| h.messages_view(Some(ID), Some(1), None).unwrap());
    let t1 = sel.selected.as_ref().expect("thread 1 selected");
    assert!(
        t1.messages.len() <= MAX_MESSAGES_SHOWN,
        "a reply carries at most {MAX_MESSAGES_SHOWN}"
    );
    assert!(
        t1.held <= MAX_MESSAGES_PER_THREAD,
        "per-thread cap: {}",
        t1.held
    );
    let reply_bytes = serde_json::to_vec(&sel).unwrap().len();
    assert!(
        reply_bytes < 256 * 1024,
        "reply fits one IPC response: {reply_bytes} B"
    );
    let (found, search_ms) = ms(|| h.messages_view(None, None, Some("lazy dog")).unwrap());
    assert!(found.conversations.len() <= MAX_CONVERSATIONS_SHOWN);
    let (_, name_ms) = ms(|| h.messages_view(None, None, Some("+467010000")).unwrap());

    // Opening an old thread asks for older history one page at a time, never everything.
    h.refresh_messages(ID, Some(1)).unwrap();
    let older = drain(&mut rx);
    let ask = older
        .iter()
        .find(|o| o.kind == sms::REQUEST_CONVERSATION)
        .expect("older page requested");
    assert_eq!(ask.body["numberToRequest"], json!(sms::PAGE_SIZE));

    println!(
        "scale messages threads={threads} held={held}: list {} KiB first screen {first_screen_ms:.0} ms (view {list_ms:.1} ms), \
         history {pages} pages {} KiB, list view {view_ms:.1} ms, thread view {thread_ms:.1} ms ({} KiB), \
         search {search_ms:.1} ms / {name_ms:.1} ms, RSS +{} KiB",
        wire_bytes / 1024,
        page_bytes / 1024,
        reply_bytes / 1024,
        rss_kib().saturating_sub(rss0)
    );
    budget("conversation list", view_ms, 40.0);
    budget("thread view", thread_ms, 40.0);
    budget("message search", search_ms, 120.0);
}

#[test]
fn turning_a_feature_off_releases_everything() {
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (true, true, true));
    drain(&mut rx);
    let p = obj(json!({"version": 2, "messages": [sms_msg(1, 1, 1, 1_793_523_600_000)]}));
    h.on_packet(ID, sms::MESSAGES, &p);
    h.set_enabled(ID, Feature::Sms, false);
    assert!(
        h.messages_view(None, None, None)
            .unwrap()
            .conversations
            .is_empty()
    );
}

#[test]
fn a_flood_of_history_stays_inside_the_documented_caps() {
    // 12 busy threads of 2,500 texts: over the 2,000-per-thread cap, and (big run) over the 20,000-per-phone cap.
    let (threads, each): (i64, usize) = if big() { (12, 2500) } else { (3, 2300) };
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (false, false, true));
    drain(&mut rx);
    h.on_packet(
        ID,
        sms::MESSAGES,
        &obj(json!({"version": 2, "messages": []})),
    );
    let rss0 = rss_kib();
    let (mut id, t0) = (0_i64, Instant::now());
    for t in 1..=threads {
        for page in 0..each.div_ceil(100) {
            let msgs: Vec<Value> = (0..100)
                .map(|k| {
                    id += 1;
                    sms_msg(
                        t,
                        id,
                        1 + id % 2,
                        1_700_000_000_000 + (page * 100 + k) as u64 * 1000,
                    )
                })
                .collect();
            h.on_packet(
                ID,
                sms::MESSAGES,
                &obj(json!({"version": 2, "messages": msgs})),
            );
        }
    }
    let flood_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let (view, view_ms) = ms(|| h.messages_view(None, None, None).unwrap());
    let held: usize = view.conversations.iter().map(|c| c.message_count).sum();
    assert!(held <= MAX_MESSAGES_PER_PHONE, "per-phone cap: {held}");
    for c in &view.conversations {
        assert!(
            c.message_count <= MAX_MESSAGES_PER_THREAD,
            "per-thread cap: {}",
            c.message_count
        );
    }
    let sel = h
        .messages_view(Some(ID), Some(1), None)
        .unwrap()
        .selected
        .unwrap();
    assert!(sel.capped, "the thread says older messages were dropped");
    assert!(!sel.may_have_older, "and stops offering Load older");
    assert_eq!(sel.messages.len(), MAX_MESSAGES_SHOWN);
    // Asking for the whole held thread: more than the default 100, still one response, newest kept.
    let wide = h
        .messages_view_window(Some(ID), Some(1), None, MAX_MESSAGES_LIMIT)
        .unwrap();
    let wide_sel = wide.selected.as_ref().unwrap();
    assert!(
        wide_sel.messages.len() > MAX_MESSAGES_SHOWN,
        "a window can show more than the default"
    );
    assert!(wide_sel.messages.len() <= MAX_MESSAGES_LIMIT);
    let wide_bytes = serde_json::to_vec(&wide).unwrap().len();
    assert!(
        wide_bytes < 256 * 1024,
        "a 2,000-message thread reply fits one IPC response: {wide_bytes} B"
    );
    assert_eq!(
        wide_sel.messages.last().unwrap().date_ms,
        1_700_000_000_000 + (each as u64 - 1) * 1000
    );
    assert!(
        wide_sel
            .messages
            .windows(2)
            .all(|w| w[0].date_ms <= w[1].date_ms),
        "oldest first"
    );
    // The newest messages are the ones kept.
    let newest = 1_700_000_000_000 + (each as u64 - 1) * 1000;
    assert_eq!(sel.messages.last().unwrap().date_ms, newest);
    println!(
        "scale flood threads={threads}x{each}: ingest {flood_ms:.0} ms, held {held}, list view {view_ms:.1} ms, RSS +{} KiB",
        rss_kib().saturating_sub(rss0)
    );
    budget("flood ingest", flood_ms, 4000.0);
    budget("list view at the cap", view_ms, 40.0);
}
