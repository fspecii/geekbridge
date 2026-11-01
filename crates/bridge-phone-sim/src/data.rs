//! A scripted phone that answers the contacts, calendar and SMS requests of the desktop service
//! (`docs/phone-data-link.md`) the way the GeekBridge Android app does. Pure: packets in, packets
//! out, no sockets, so tests and the `geekbridge-phone-data-sim` binary can drive it over any link.
//! Nothing here reads a real phone.
use bridge_core::phone_link as wire;
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
            SMS_REQUEST_CONVERSATIONS,
            SMS_REQUEST_CONVERSATION,
            SMS_REQUEST,
        ]),
        s(&[
            "kdeconnect.ping",
            wire::CONTACTS_UIDS,
            wire::CONTACTS_VCARDS,
            wire::CALENDAR_SYNC,
            SMS_MESSAGES,
            wire::SMS_STATUS,
        ]),
    )
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
}

pub struct PhoneScript {
    pub contacts: Vec<SimContact>,
    pub events: Vec<SimEvent>,
    /// Message JSON objects in the stock `kdeconnect.sms.messages` shape.
    pub messages: Vec<Value>,
    /// Include the `delivered` report (carriers do not always send one).
    pub deliver: bool,
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
            contacts: Vec::new(),
            events: Vec::new(),
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
            s.events.push(SimEvent {
                id: id.into(),
                rev: format!("r{}", s.events.len() + 1),
                title: title.into(),
                all_day: false,
                start_ms: start,
                end_ms: start + len,
                location: loc.map(str::to_owned),
            });
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
        s.events.push(SimEvent {
            id: "16@1".into(),
            rev: "r6".into(),
            title: "Team offsite".into(),
            all_day: true,
            start_ms: day,
            end_ms: day + 86_400_000,
            location: None,
        });
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
                           "startMs": e.start_ms, "endMs": e.end_ms, "tz": "UTC", "calendarId": "1"});
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
            "calendars": [{"id": "1", "name": "Personal"}],
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

    /// Answers one desktop request with the packets the phone would send back.
    pub fn handle(&mut self, kind: &str, body: &Map<String, Value>) -> Vec<Reply> {
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
            _ => Vec::new(),
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
        assert!(s.full && s.events.len() == 6);
        let delta = p.handle(
            wire::CALENDAR_REQUEST,
            &wire::calendar_request_body("b", Some(&s.token)),
        );
        let d = wire::parse_calendar_sync(&delta[0].1).unwrap();
        assert!(!d.full && d.events.is_empty() && d.base.as_deref() == Some(s.token.as_str()));
    }
}
