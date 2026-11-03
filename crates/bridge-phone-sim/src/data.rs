// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted phone that answers the contacts, calendar and SMS requests of the desktop service
//! (`docs/phone-data-link.md`) the way the GeekBridge Android app does. Pure: packets in, packets
//! out, no sockets, so tests and the `geekbridge-phone-data-sim` binary can drive it over any link.
//! Nothing here reads a real phone.
use bridge_core::phone_link as wire;
use bridge_core::photos_link as photos;
use serde_json::{Map, Value, json};

pub const SMS_MESSAGES: &str = "kdeconnect.sms.messages";
pub const SMS_REQUEST: &str = "kdeconnect.sms.request";
pub const SMS_REQUEST_CONVERSATIONS: &str = "kdeconnect.sms.request_conversations";
pub const SMS_REQUEST_CONVERSATION: &str = "kdeconnect.sms.request_conversation";

/// What the phone puts in its identity packet: (incoming, outgoing).
pub fn capabilities() -> (Vec<String>, Vec<String>) {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    (
        s(&[
            "kdeconnect.ping",
            wire::CONTACTS_REQUEST_UIDS,
            wire::CONTACTS_REQUEST_VCARDS,
            wire::CALENDAR_REQUEST,
            wire::CALENDAR_WRITE,
            SMS_REQUEST_CONVERSATIONS,
            SMS_REQUEST_CONVERSATION,
            SMS_REQUEST,
        ]),
        s(&[
            "kdeconnect.ping",
            wire::CONTACTS_UIDS,
            wire::CONTACTS_VCARDS,
            wire::CALENDAR_SYNC,
            wire::CALENDAR_WRITE_RESULT,
            SMS_MESSAGES,
            wire::SMS_STATUS,
        ]),
    )
}

/// Capabilities of a phone whose user also switched on battery sharing (stock `kdeconnect.battery` names).
pub fn capabilities_with_battery() -> (Vec<String>, Vec<String>) {
    let (mut i, mut o) = capabilities();
    i.push(bridge_core::battery_link::REQUEST.to_owned());
    o.push(bridge_core::battery_link::BATTERY.to_owned());
    (i, o)
}

/// Capabilities of a phone whose user also switched on notification mirroring (stock names, no reply).
pub fn capabilities_with_notifications() -> (Vec<String>, Vec<String>) {
    let (mut i, mut o) = capabilities();
    i.push(bridge_core::notification_link::REQUEST.to_owned());
    i.push(bridge_core::notification_link::ACTION.to_owned());
    o.push(bridge_core::notification_link::NOTIFICATION.to_owned());
    (i, o)
}

/// Capabilities of a GeekBridge phone whose user also switched on recent photos (`geekbridge.photos.*`).
pub fn capabilities_with_photos() -> (Vec<String>, Vec<String>) {
    let (mut i, mut o) = capabilities();
    i.push(photos::REQUEST.to_owned());
    o.push(photos::LIST.to_owned());
    o.push(photos::THUMB.to_owned());
    (i, o)
}

/// Capabilities of a phone whose user did not enable calendar editing (the default in the app).
pub fn capabilities_read_only() -> (Vec<String>, Vec<String>) {
    let (mut i, mut o) = capabilities();
    i.retain(|k| k != wire::CALENDAR_WRITE);
    o.retain(|k| k != wire::CALENDAR_WRITE_RESULT);
    (i, o)
}

pub type Reply = (&'static str, Map<String, Value>);

#[derive(Clone, Debug)]
pub struct SimContact {
    pub uid: String,
    pub timestamp: String,
    pub vcard: String,
}

#[derive(Clone, Debug)]
pub struct SimEvent {
    pub id: String,
    pub rev: String,
    pub title: String,
    pub all_day: bool,
    pub start_ms: i64,
    pub end_ms: i64,
    pub location: Option<String>,
    pub calendar_id: String,
    /// `Events._ID` (the part of `id` before the `@`).
    pub event_id: String,
    pub reminders: Vec<u32>,
    pub attendees: Vec<String>,
    pub recurring: bool,
}

impl SimEvent {
    pub fn new(id: &str, title: &str, start_ms: i64, end_ms: i64) -> Self {
        Self {
            id: id.into(),
            rev: "r0".into(),
            title: title.into(),
            all_day: false,
            start_ms,
            end_ms,
            location: None,
            calendar_id: "1".into(),
            event_id: id.split('@').next().unwrap_or(id).into(),
            reminders: vec![10],
            attendees: Vec::new(),
            recurring: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimCalendar {
    pub id: String,
    pub name: String,
    pub account: String,
    pub account_type: String,
    pub color: i32,
    pub writable: bool,
}

/// One made-up photo: the list entry and the thumbnail JPEG the phone would send for it.
#[derive(Clone)]
pub struct SimPhoto {
    pub photo: photos::Photo,
    pub thumb: Vec<u8>,
    pub thumb_width: u32,
    pub thumb_height: u32,
}

/// The made-up thumbnails in `fixtures/photos` (160 pixels on the long edge), with their sizes.
pub const DEMO_THUMBS: [(&[u8], u32, u32); 8] = [
    (include_bytes!("../fixtures/photos/photo-1.jpg"), 160, 120),
    (include_bytes!("../fixtures/photos/photo-2.jpg"), 120, 160),
    (include_bytes!("../fixtures/photos/photo-3.jpg"), 160, 120),
    (include_bytes!("../fixtures/photos/photo-4.jpg"), 160, 120),
    (include_bytes!("../fixtures/photos/photo-5.jpg"), 160, 90),
    (include_bytes!("../fixtures/photos/photo-6.jpg"), 120, 160),
    (include_bytes!("../fixtures/photos/photo-7.jpg"), 160, 120),
    (include_bytes!("../fixtures/photos/photo-8.jpg"), 160, 120),
];

pub struct PhoneScript {
    /// Recent photos, newest first. Answered only when [`Self::photos_on`].
    pub photos: Vec<SimPhoto>,
    /// The user turned recent photos on: requests are answered (otherwise the phone would not have declared them).
    pub photos_on: bool,
    /// The phone only shows the photos its user picked.
    pub photos_partial: bool,
    /// Photo ids the computer asked to be sent in full, in order.
    pub photo_gets: Vec<String>,
    pub contacts: Vec<SimContact>,
    pub events: Vec<SimEvent>,
    pub calendars: Vec<SimCalendar>,
    /// Answer `geekbridge.calendar.write` (the user enabled editing and Android granted it).
    pub writes_enabled: bool,
    /// Apply writes but never answer, like a link that dies right after the phone wrote.
    pub silent_writes: bool,
    /// Operation ids applied, in order (a repeated id must not appear twice).
    pub write_log: Vec<String>,
    results: std::collections::HashMap<String, Map<String, Value>>,
    next_event: i64,
    /// Message JSON objects in the stock `kdeconnect.sms.messages` shape.
    pub messages: Vec<Value>,
    /// Include the `delivered` report (carriers do not always send one).
    pub deliver: bool,
    /// SIM subscriptions put into every `kdeconnect.sms.messages` packet (`{id, slot, label}`).
    pub sims: Vec<Value>,
    token: u64,
    last_sent: Option<(String, Vec<(String, String)>)>,
    next_id: i64,
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

pub fn vcard(name: &str, phone: &str, email: Option<&str>, org: Option<&str>) -> String {
    let mut v = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nN:{name};;;;\r\nTEL;TYPE=CELL:{phone}\r\n"
    );
    if let Some(e) = email {
        v.push_str(&format!("EMAIL:{e}\r\n"));
    }
    if let Some(o) = org {
        v.push_str(&format!("ORG:{o}\r\n"));
    }
    v.push_str("END:VCARD\r\n");
    v
}

impl PhoneScript {
    pub fn empty() -> Self {
        Self {
            photos: Vec::new(),
            photos_on: false,
            photos_partial: false,
            photo_gets: Vec::new(),
            contacts: Vec::new(),
            events: Vec::new(),
            calendars: vec![
                SimCalendar {
                    id: "1".into(),
                    name: "Personal".into(),
                    account: "demo@gmail.com".into(),
                    account_type: "com.google".into(),
                    color: 0xFF1E88E5_u32 as i32,
                    writable: true,
                },
                SimCalendar {
                    id: "2".into(),
                    name: "Holidays".into(),
                    account: "demo@gmail.com".into(),
                    account_type: "com.google".into(),
                    color: 0xFF43A047_u32 as i32,
                    writable: false,
                },
            ],
            sims: Vec::new(),
            writes_enabled: true,
            silent_writes: false,
            write_log: Vec::new(),
            results: std::collections::HashMap::new(),
            next_event: 200,
            messages: Vec::new(),
            deliver: true,
            token: 0,
            last_sent: None,
            next_id: 100,
        }
    }

    /// Made-up people, events around `now_ms` and a few conversations.
    pub fn demo(now_ms: i64) -> Self {
        let mut s = Self::empty();
        for (i, (n, p, e, o)) in [
            (
                "Ada Lovelace",
                "+44 20 7946 0001",
                Some("ada@example.org"),
                Some("Analytical Engines"),
            ),
            (
                "Grace Hopper",
                "+1 555 010 0002",
                Some("grace@example.org"),
                Some("Compiler Guild"),
            ),
            ("Alan Turing", "+44 20 7946 0003", None, None),
            (
                "Katherine Johnson",
                "+1 555 010 0004",
                Some("kj@example.org"),
                Some("Orbital Mechanics"),
            ),
            (
                "Margaret Hamilton",
                "+1 555 010 0005",
                None,
                Some("Apollo Software"),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            s.contacts.push(SimContact {
                uid: format!("lookup-{i}"),
                timestamp: format!("{}", 1_700_000_000_000_i64 + i as i64),
                vcard: vcard(n, p, e, o),
            });
        }
        const HOUR: i64 = 3_600_000;
        let day = now_ms.div_euclid(86_400_000) * 86_400_000;
        let mut ev = |id: &str, title: &str, start: i64, len: i64, loc: Option<&str>| {
            let mut e = SimEvent::new(id, title, start, start + len);
            e.rev = format!("r{}", s.events.len() + 1);
            e.location = loc.map(str::to_owned);
            s.events.push(e);
        };
        ev("11@1", "Standup", day + 9 * HOUR, HOUR / 2, Some("Room 4"));
        ev("12@1", "Design review", day + 11 * HOUR, HOUR, None);
        ev(
            "13@1",
            "Lunch with Grace",
            day + 12 * HOUR + HOUR / 2,
            HOUR,
            Some("Cafe Lovelace"),
        );
        ev("14@1", "Dentist", day + 15 * HOUR, HOUR, Some("Main St 4"));
        ev(
            "15@1",
            "Planning",
            day + 24 * HOUR + 10 * HOUR,
            2 * HOUR,
            None,
        );
        let mut offsite = SimEvent::new("16@1", "Team offsite", day, day + 86_400_000);
        offsite.rev = "r6".into();
        offsite.all_day = true;
        offsite.attendees = vec!["Ada Lovelace".into(), "Grace Hopper".into()];
        s.events.push(offsite);
        let mut holiday = SimEvent::new(
            "17@2",
            "Public holiday",
            day + 2 * 86_400_000,
            day + 3 * 86_400_000,
        );
        holiday.rev = "r7".into();
        holiday.all_day = true;
        holiday.calendar_id = "2".into();
        s.events.push(holiday);
        let mut weekly = SimEvent::new("18@1", "Weekly sync", day + 14 * HOUR, day + 15 * HOUR);
        weekly.rev = "r8".into();
        weekly.recurring = true;
        weekly.reminders = vec![5, 30];
        s.events.push(weekly);
        let m = |id: i64, thread: i64, ty: i64, who: &str, body: &str, ago_min: i64, read: i64| {
            json!({"addresses": [{"address": who}], "body": body,
                   "date": now_ms - ago_min * 60_000, "type": ty, "read": read,
                   "thread_id": thread, "_id": id, "sub_id": 1, "event": 1})
        };
        s.messages = vec![
            m(
                1,
                1,
                1,
                "+44 20 7946 0001",
                "Are we still on for Thursday?",
                190,
                1,
            ),
            m(
                2,
                1,
                2,
                "+44 20 7946 0001",
                "Yes! 10:00 at the lab.",
                185,
                1,
            ),
            m(
                3,
                1,
                1,
                "+44 20 7946 0001",
                "Perfect. I'll bring the engine plans.",
                40,
                0,
            ),
            m(
                4,
                2,
                1,
                "+1 555 010 0002",
                "Your package has shipped.",
                600,
                1,
            ),
            m(
                5,
                3,
                2,
                "+1 555 010 0004",
                "Thanks for the trajectory numbers.",
                1500,
                1,
            ),
            m(
                6,
                3,
                1,
                "+1 555 010 0004",
                "Anytime. Double-check the burn time.",
                1490,
                1,
            ),
        ];
        // Two SIMs, an emoji thread, a picture message and an older day, so the window shows them.
        s.sims = vec![
            json!({"id": 3, "slot": 1, "label": "Personal"}),
            json!({"id": 5, "slot": 2, "label": "Work"}),
        ];
        let mut more = vec![
            m(
                7,
                4,
                1,
                "+44 20 7946 0003",
                "Morning! 🌞 Did the proof compile?",
                60 * 24 + 30,
                1,
            ),
            m(
                8,
                4,
                2,
                "+44 20 7946 0003",
                "Almost. One lemma left ✅",
                60 * 24 + 25,
                1,
            ),
            m(
                9,
                4,
                1,
                "+44 20 7946 0003",
                "Ping me when it is done, I will check it tonight.",
                8,
                0,
            ),
            m(
                10,
                4,
                1,
                "+44 20 7946 0003",
                "Also: the paper is due Friday.",
                7,
                0,
            ),
            m(
                11,
                5,
                1,
                "+49 151 1234 5678",
                "Your verification code is 482913",
                3 * 60 * 24,
                1,
            ),
        ];
        // A picture message: the phone reports the attachment, the body is the text part only.
        more.push(
            json!({"addresses": [{"address": "+1 555 010 0002"}], "body": "Look at this!",
            "date": now_ms - 25 * 60_000, "type": 1, "read": 0, "thread_id": 2, "_id": 4001,
            "sub_id": 3, "event": 1,
            "attachments": [{"part_id": 1, "mime_type": "image/jpeg", "unique_identifier": "u1"}]}),
        );
        s.messages.extend(more);
        s.photos = DEMO_THUMBS
            .iter()
            .enumerate()
            .map(|(i, (jpeg, w, h))| SimPhoto {
                photo: photos::Photo {
                    id: format!("p{}", i + 1),
                    name: format!("IMG_{:04}.jpg", 2041 - i),
                    taken_ms: (now_ms - (3 + 7 * i as i64) * 3_600_000).max(0) as u64,
                    size: 2_400_000 + 130_000 * i as u64,
                    width: if h > w { 3024 } else { 4032 },
                    height: if h > w { 4032 } else { 3024 },
                },
                thumb: jpeg.to_vec(),
                thumb_width: *w,
                thumb_height: *h,
            })
            .collect();
        s
    }

    pub fn thread_newest(&self) -> Vec<Value> {
        let mut by: Vec<(i64, i64, Value)> = Vec::new();
        for m in &self.messages {
            let t = m["thread_id"].as_i64().unwrap_or(0);
            let d = m["date"].as_i64().unwrap_or(0);
            match by.iter_mut().find(|(tt, ..)| *tt == t) {
                Some(e) if e.1 < d => *e = (t, d, m.clone()),
                Some(_) => {}
                None => by.push((t, d, m.clone())),
            }
        }
        by.sort_by_key(|(_, d, _)| std::cmp::Reverse(*d));
        by.into_iter().map(|(_, _, m)| m).collect()
    }

    fn messages_packet(list: Vec<Value>) -> Reply {
        (SMS_MESSAGES, obj(json!({"version": 2, "messages": list})))
    }

    /// A text arrives on the phone; the packet the app would push for it.
    pub fn incoming_sms(&mut self, thread: i64, from: &str, body: &str, now_ms: i64) -> Reply {
        self.next_id += 1;
        let m = json!({"addresses": [{"address": from}], "body": body, "date": now_ms, "type": 1,
                       "read": 0, "thread_id": thread, "_id": self.next_id, "sub_id": 1, "event": 1});
        self.messages.push(m.clone());
        Self::messages_packet(vec![m])
    }

    /// Calendar delta push as the app does when the provider changes.
    pub fn calendar_changed(&mut self) -> Reply {
        self.calendar_sync(self.last_sent.as_ref().map(|(t, _)| t.clone()), None)
    }

    fn event_json(e: &SimEvent) -> Value {
        let mut v = json!({"id": e.id, "rev": e.rev, "title": e.title, "allDay": e.all_day,
                           "startMs": e.start_ms, "endMs": e.end_ms, "tz": "UTC",
                           "calendarId": e.calendar_id, "eventId": e.event_id,
                           "recurring": e.recurring, "reminders": e.reminders});
        if !e.attendees.is_empty() {
            v["attendees"] = json!({"count": e.attendees.len(), "names": e.attendees});
        }
        if let Some(l) = &e.location {
            v["location"] = json!(l);
        }
        v
    }

    fn calendar_sync(&mut self, since: Option<String>, request_id: Option<&str>) -> Reply {
        self.token += 1;
        let token = format!("sim:{}", self.token);
        let now: Vec<(String, String)> = self
            .events
            .iter()
            .map(|e| (e.id.clone(), e.rev.clone()))
            .collect();
        let delta = match (&since, &self.last_sent) {
            (Some(s), Some((t, prev))) if s == t => Some(prev.clone()),
            _ => None,
        };
        let mut body = json!({
            "version": 1, "token": token,
            "windowStartMs": 0, "windowEndMs": 0, "truncated": false,
            "calendars": self.calendars.iter().map(|c| json!({
                "id": c.id, "name": c.name, "account": c.account, "accountType": c.account_type,
                "color": c.color, "writable": c.writable})).collect::<Vec<_>>(),
        });
        match delta {
            Some(prev) => {
                body["full"] = json!(false);
                body["base"] = json!(since);
                body["events"] = Value::Array(
                    self.events
                        .iter()
                        .filter(|e| !prev.iter().any(|(i, r)| *i == e.id && *r == e.rev))
                        .map(Self::event_json)
                        .collect(),
                );
                body["deleted"] = Value::Array(
                    prev.iter()
                        .filter(|(i, _)| !now.iter().any(|(n, _)| n == i))
                        .map(|(i, _)| json!({"id": i}))
                        .collect(),
                );
            }
            None => {
                body["full"] = json!(true);
                body["base"] = Value::Null;
                body["events"] = Value::Array(self.events.iter().map(Self::event_json).collect());
                body["deleted"] = json!([]);
            }
        }
        if let Some(r) = request_id {
            body["requestId"] = json!(r);
        }
        self.last_sent = Some((token, now));
        (wire::CALENDAR_SYNC, obj(body))
    }

    fn calendar_write(&mut self, body: &Map<String, Value>) -> Vec<Reply> {
        let op = body
            .get("opId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(prev) = self.results.get(&op) {
            // Idempotent: the same operation id gets its first answer, nothing is written again.
            let mut again = prev.clone();
            again.insert("duplicate".into(), json!(true));
            return vec![(wire::CALENDAR_WRITE_RESULT, again)];
        }
        let fail = |reason: &str| -> Map<String, Value> {
            obj(json!({"opId": op, "state": "failed", "reason": reason}))
        };
        let action = body.get("action").and_then(Value::as_str).unwrap_or("");
        let ev = body.get("event");
        let result = match action {
            "create" => {
                let cal = body.get("calendarId").and_then(Value::as_str).unwrap_or("");
                match self.calendars.iter().find(|c| c.id == cal) {
                    None => fail("calendar_missing"),
                    Some(c) if !c.writable => fail("not_writable"),
                    Some(_) => {
                        self.next_event += 1;
                        let eid = self.next_event.to_string();
                        let g = |k: &str| ev.and_then(|e| e.get(k));
                        let start = g("startMs").and_then(Value::as_i64).unwrap_or(0);
                        let mut e = SimEvent::new(
                            &format!("{eid}@{start}"),
                            g("title").and_then(Value::as_str).unwrap_or(""),
                            start,
                            g("endMs").and_then(Value::as_i64).unwrap_or(start),
                        );
                        e.rev = format!("w{}", self.write_log.len() + 1);
                        e.all_day = g("allDay").and_then(Value::as_bool).unwrap_or(false);
                        e.calendar_id = cal.to_owned();
                        e.location = g("location").and_then(Value::as_str).map(str::to_owned);
                        e.reminders = g("reminderMin")
                            .and_then(Value::as_u64)
                            .map(|m| vec![m as u32])
                            .unwrap_or_default();
                        self.events.push(e);
                        obj(json!({"opId": op, "state": "applied", "eventId": eid}))
                    }
                }
            }
            "update" | "delete" => {
                let eid = body.get("eventId").and_then(Value::as_str).unwrap_or("");
                match self.events.iter().position(|e| e.event_id == eid) {
                    None => fail("not_found"),
                    Some(i) if self.events[i].recurring => fail("recurring"),
                    // The desktop edited a version this phone no longer has: refuse, overwrite nothing.
                    Some(i)
                        if body
                            .get("baseRev")
                            .and_then(Value::as_str)
                            .is_some_and(|b| b != self.events[i].rev) =>
                    {
                        fail("changed")
                    }
                    Some(i) if action == "delete" => {
                        self.events.remove(i);
                        obj(json!({"opId": op, "state": "applied", "eventId": eid}))
                    }
                    Some(i) => {
                        let g = |k: &str| ev.and_then(|e| e.get(k));
                        let e = &mut self.events[i];
                        if let Some(t) = g("title").and_then(Value::as_str) {
                            e.title = t.into();
                        }
                        e.start_ms = g("startMs").and_then(Value::as_i64).unwrap_or(e.start_ms);
                        e.end_ms = g("endMs").and_then(Value::as_i64).unwrap_or(e.end_ms);
                        e.location = g("location").and_then(Value::as_str).map(str::to_owned);
                        e.rev = format!("w{}", self.write_log.len() + 1);
                        obj(json!({"opId": op, "state": "applied", "eventId": eid}))
                    }
                }
            }
            _ => fail("invalid"),
        };
        if result.get("state").and_then(Value::as_str) == Some("applied") {
            self.write_log.push(op.clone());
        }
        self.results.insert(op, result.clone());
        if self.silent_writes {
            Vec::new()
        } else {
            vec![(wire::CALENDAR_WRITE_RESULT, result)]
        }
    }

    /// Answers one desktop request with the packets the phone would send back.
    pub fn handle(&mut self, kind: &str, body: &Map<String, Value>) -> Vec<Reply> {
        let mut out = self.handle_request(kind, body);
        if kind == SMS_REQUEST_CONVERSATIONS {
            // A GeekBridge phone adds the real unread count per thread to the conversation list.
            let mut unread = Map::new();
            for m in &self.messages {
                if m["type"] == 1 && m["read"] == 0 {
                    let t = m["thread_id"].to_string();
                    let n = unread.get(&t).and_then(Value::as_u64).unwrap_or(0) + 1;
                    unread.insert(t, json!(n));
                }
            }
            if let Some((_, b)) = out.iter_mut().find(|(k, _)| *k == SMS_MESSAGES) {
                b.insert("unread".into(), Value::Object(unread));
            }
        }
        if !self.sims.is_empty() {
            for (k, b) in &mut out {
                if *k == SMS_MESSAGES {
                    b.insert("sims".into(), Value::Array(self.sims.clone()));
                }
            }
        }
        out
    }

    /// A text arriving on the phone, with the SIM list a GeekBridge phone adds to its pushes.
    pub fn push_incoming(
        &mut self,
        thread: i64,
        from: &str,
        body: &str,
        now_ms: i64,
    ) -> Vec<Reply> {
        let mut r = self.incoming_sms(thread, from, body, now_ms);
        if !self.sims.is_empty() {
            r.1.insert("sims".into(), Value::Array(self.sims.clone()));
        }
        vec![r]
    }

    fn handle_request(&mut self, kind: &str, body: &Map<String, Value>) -> Vec<Reply> {
        match kind {
            wire::CONTACTS_REQUEST_UIDS => {
                let mut m = Map::new();
                m.insert(
                    "uids".into(),
                    Value::Array(self.contacts.iter().map(|c| json!(c.uid)).collect()),
                );
                for c in &self.contacts {
                    m.insert(c.uid.clone(), json!(c.timestamp));
                }
                vec![(wire::CONTACTS_UIDS, m)]
            }
            wire::CONTACTS_REQUEST_VCARDS => {
                let want: Vec<&str> = body
                    .get("uids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).take(25).collect())
                    .unwrap_or_default();
                let mut m = Map::new();
                let mut got = Vec::new();
                for c in self
                    .contacts
                    .iter()
                    .filter(|c| want.contains(&c.uid.as_str()))
                {
                    m.insert(c.uid.clone(), json!(c.vcard));
                    got.push(json!(c.uid));
                }
                m.insert("uids".into(), Value::Array(got));
                vec![(wire::CONTACTS_VCARDS, m)]
            }
            wire::CALENDAR_REQUEST => {
                let since = body.get("since").and_then(Value::as_str).map(str::to_owned);
                let rid = body.get("requestId").and_then(Value::as_str);
                vec![self.calendar_sync(since, rid)]
            }
            wire::CALENDAR_WRITE if self.writes_enabled => self.calendar_write(body),
            SMS_REQUEST_CONVERSATIONS => vec![Self::messages_packet(self.thread_newest())],
            SMS_REQUEST_CONVERSATION => {
                let t = body.get("threadID").and_then(Value::as_i64).unwrap_or(-1);
                let start = body
                    .get("rangeStartTimestamp")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX);
                let n = body
                    .get("numberToRequest")
                    .and_then(Value::as_u64)
                    .unwrap_or(50) as usize;
                let mut list: Vec<Value> = self
                    .messages
                    .iter()
                    .filter(|m| m["thread_id"] == t && m["date"].as_i64().unwrap_or(0) <= start)
                    .cloned()
                    .collect();
                list.sort_by_key(|m| std::cmp::Reverse(m["date"].as_i64().unwrap_or(0)));
                list.truncate(n);
                vec![Self::messages_packet(list)]
            }
            SMS_REQUEST => {
                let rid = body.get("requestId").and_then(Value::as_str).unwrap_or("");
                let who = body
                    .get("addresses")
                    .and_then(|a| a.pointer("/0/address"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let text = body
                    .get("messageBody")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = |s: &str| -> Reply {
                    (wire::SMS_STATUS, obj(json!({"requestId": rid, "state": s})))
                };
                let mut out = vec![status("accepted")];
                self.next_id += 1;
                let thread = self
                    .messages
                    .iter()
                    .find(|m| m["addresses"][0]["address"] == who)
                    .and_then(|m| m["thread_id"].as_i64())
                    .unwrap_or(900 + self.next_id);
                let m = json!({"addresses": [{"address": who}], "body": text, "date": now_ms_i64(),
                               "type": 2, "read": 1, "thread_id": thread, "_id": self.next_id,
                               "sub_id": 1, "event": 1});
                self.messages.push(m.clone());
                out.push(Self::messages_packet(vec![m]));
                out.push(status("sent"));
                if self.deliver {
                    out.push(status("delivered"));
                }
                out
            }
            photos::REQUEST if self.photos_on => self.photos_request(body),
            _ => Vec::new(),
        }
    }

    /// `geekbridge.photos.request`: a list, a thumbnail, or "send this one in full" (recorded; the file itself
    /// goes over the share channel, which this packet-level script does not speak).
    fn photos_request(&mut self, body: &Map<String, Value>) -> Vec<Reply> {
        match photos::parse_request(body) {
            Ok(photos::Ask::List { limit }) => {
                let list = photos::List {
                    items: self
                        .photos
                        .iter()
                        .take(limit)
                        .map(|p| p.photo.clone())
                        .collect(),
                    partial: self.photos_partial,
                };
                vec![(photos::LIST, photos::list_body(&list))]
            }
            Ok(photos::Ask::Thumb { id }) => {
                let reply = match self.photos.iter().find(|p| p.photo.id == id) {
                    Some(p) => photos::thumb_body(&id, p.thumb_width, p.thumb_height, &p.thumb),
                    None => photos::thumb_gone_body(&id),
                };
                vec![(photos::THUMB, reply)]
            }
            Ok(photos::Ask::Get { id }) => {
                if self.photos.iter().any(|p| p.photo.id == id) {
                    self.photo_gets.push(id);
                }
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }
}

fn now_ms_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_answers_in_the_documented_shapes() {
        let mut p = PhoneScript::demo(now_ms_i64());
        let r = p.handle(wire::CONTACTS_REQUEST_UIDS, &Map::new());
        assert_eq!(wire::parse_uids(&r[0].1).unwrap().len(), 5);
        let r = p.handle(
            wire::CONTACTS_REQUEST_VCARDS,
            &wire::request_vcards_body(&["lookup-0".to_owned()]),
        );
        let cards = wire::parse_vcards(&r[0].1).unwrap();
        assert_eq!(
            wire::contact_from_vcard(&cards[0].1).unwrap().display_name,
            "Ada Lovelace"
        );
        let full = p.handle(
            wire::CALENDAR_REQUEST,
            &wire::calendar_request_body("a", None),
        );
        let s = wire::parse_calendar_sync(&full[0].1).unwrap();
        assert!(s.full && s.events.len() == 8);
        assert_eq!(s.calendars.len(), 2);
        assert_eq!(s.calendars[0].account.as_deref(), Some("demo@gmail.com"));
        let delta = p.handle(
            wire::CALENDAR_REQUEST,
            &wire::calendar_request_body("b", Some(&s.token)),
        );
        let d = wire::parse_calendar_sync(&delta[0].1).unwrap();
        assert!(!d.full && d.events.is_empty() && d.base.as_deref() == Some(s.token.as_str()));
    }

    fn write(p: &mut PhoneScript, v: Value) -> Vec<Reply> {
        p.handle(wire::CALENDAR_WRITE, &obj(v))
    }

    #[test]
    fn calendar_writes_apply_once_per_operation_id() {
        let mut p = PhoneScript::demo(now_ms_i64());
        let before = p.events.len();
        let create = json!({"version": 1, "opId": "op1", "action": "create", "calendarId": "1",
            "event": {"title": "Lunch", "allDay": false, "startMs": 1000, "endMs": 5000, "tz": "UTC"}});
        let r = write(&mut p, create.clone());
        assert_eq!(r[0].1["state"], "applied");
        let eid = r[0].1["eventId"].as_str().unwrap().to_owned();
        assert_eq!(p.events.len(), before + 1);
        // Same id again: first answer, flagged duplicate, nothing written twice.
        let again = write(&mut p, create);
        assert_eq!(again[0].1["duplicate"], true);
        assert_eq!(again[0].1["eventId"], json!(eid));
        assert_eq!(p.events.len(), before + 1);
        assert_eq!(p.write_log, vec!["op1"]);

        let d = write(
            &mut p,
            json!({"opId": "op2", "action": "delete", "eventId": eid}),
        );
        assert_eq!(d[0].1["state"], "applied");
        assert_eq!(p.events.len(), before);
        let ro = write(
            &mut p,
            json!({"opId": "op3", "action": "create", "calendarId": "2", "event": {}}),
        );
        assert_eq!(ro[0].1["reason"], "not_writable");
        let rec = write(
            &mut p,
            json!({"opId": "op4", "action": "delete", "eventId": "18"}),
        );
        assert_eq!(rec[0].1["reason"], "recurring");

        // An edit made from an older version is refused and changes nothing; the current one goes through.
        let made = write(
            &mut p,
            json!({"version": 1, "opId": "op6", "action": "create", "calendarId": "1",
                "event": {"title": "Sync", "allDay": false, "startMs": 1000, "endMs": 5000, "tz": "UTC"}}),
        );
        let mid = made[0].1["eventId"].as_str().unwrap().to_owned();
        let at = p.events.iter().position(|e| e.event_id == mid).unwrap();
        let rev = p.events[at].rev.clone();
        let stale = write(
            &mut p,
            json!({"opId": "op7", "action": "update", "eventId": mid, "baseRev": "old",
                "event": {"title": "Mine", "startMs": 1000, "endMs": 5000}}),
        );
        assert_eq!(
            (stale[0].1["state"].as_str(), stale[0].1["reason"].as_str()),
            (Some("failed"), Some("changed"))
        );
        assert_eq!(p.events[at].title, "Sync");
        let fresh = write(
            &mut p,
            json!({"opId": "op8", "action": "update", "eventId": mid, "baseRev": rev,
                "event": {"title": "Mine", "startMs": 1000, "endMs": 5000}}),
        );
        assert_eq!(fresh[0].1["state"], "applied");
        assert_eq!(p.events[at].title, "Mine");

        p.silent_writes = true;
        let quiet = json!({"opId": "op5", "action": "create", "calendarId": "1",
            "event": {"title": "Q", "startMs": 1, "endMs": 2}});
        assert!(
            write(&mut p, quiet.clone()).is_empty(),
            "applied but never answered"
        );
        let retry = write(&mut p, quiet);
        assert_eq!(
            retry[0].1["duplicate"], true,
            "the retry still learns the outcome"
        );
        let off = capabilities_read_only();
        assert!(!off.0.iter().any(|k| k == wire::CALENDAR_WRITE));
    }
}
