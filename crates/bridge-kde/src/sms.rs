// SPDX-License-Identifier: GPL-3.0-or-later
//! The stock KDE Connect SMS packets (`kdeconnect.sms.*`), as the desktop side speaks them.
//!
//! Field names and quirks follow the stock KDE Connect SMS plugin as observed on the wire
//! (see `docs/phone-data-link.md`). The phone never acknowledges `kdeconnect.sms.request`; the
//! GeekBridge phone additionally sends `geekbridge.sms.status` keyed by the `requestId` this module
//! adds to the request (stock phones ignore unknown fields). Incoming data is untrusted: it is
//! bounded and converted with [`bridge_core::messages::Message::from_wire`].
use bridge_core::messages::{Message, OutgoingTextIntent, ThreadId, ValidationError, WireMessage};
use serde_json::{Map, Value, json};

pub const REQUEST: &str = "kdeconnect.sms.request";
pub const REQUEST_CONVERSATIONS: &str = "kdeconnect.sms.request_conversations";
pub const REQUEST_CONVERSATION: &str = "kdeconnect.sms.request_conversation";
pub const MESSAGES: &str = "kdeconnect.sms.messages";
/// `version` written on `kdeconnect.sms.request` and expected on `kdeconnect.sms.messages`.
pub const VERSION: u64 = 2;
/// Most messages accepted from one `kdeconnect.sms.messages` packet.
pub const MAX_MESSAGES_PER_PACKET: usize = 500;
/// Most messages the desktop asks for in one `request_conversation`.
pub const PAGE_SIZE: u32 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmsWireError {
    Malformed,
    TooManyMessages,
}

pub fn request_conversations_body() -> Map<String, Value> {
    Map::new()
}

/// `range_start_ms` is inclusive on the phone side (`date <= rangeStartTimestamp`, newest first).
pub fn request_conversation_body(
    thread: ThreadId,
    range_start_ms: Option<u64>,
    count: u32,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("threadID".into(), json!(thread.0));
    if let Some(start) = range_start_ms {
        m.insert("rangeStartTimestamp".into(), json!(start));
    }
    m.insert("numberToRequest".into(), json!(count.min(PAGE_SIZE)));
    m
}

/// A send request for one recipient and no attachments. `request_id` is GeekBridge's own addition.
pub fn send_request_body(intent: &OutgoingTextIntent, request_id: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("version".into(), json!(VERSION));
    m.insert("sendSms".into(), json!(true));
    m.insert(
        "addresses".into(),
        Value::Array(
            intent
                .recipients()
                .iter()
                .map(|a| json!({ "address": a.as_str() }))
                .collect(),
        ),
    );
    m.insert("messageBody".into(), json!(intent.text()));
    if let Some(sub) = intent.sim().kde_sub_id() {
        m.insert("subID".into(), json!(sub));
    }
    m.insert("requestId".into(), json!(request_id));
    m
}

#[derive(Debug, Default)]
pub struct ParsedMessages {
    pub messages: Vec<Message>,
    /// Elements that failed validation (counted, never retried or logged with content).
    pub rejected: usize,
}

pub fn parse_messages(body: &Map<String, Value>) -> Result<ParsedMessages, SmsWireError> {
    if body.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(SmsWireError::Malformed);
    }
    let list = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(SmsWireError::Malformed)?;
    if list.len() > MAX_MESSAGES_PER_PACKET {
        return Err(SmsWireError::TooManyMessages);
    }
    let mut out = ParsedMessages::default();
    for v in list {
        match serde_json::from_value::<WireMessage>(v.clone())
            .map_err(|_| ValidationError::InvalidField("message"))
            .and_then(Message::from_wire)
        {
            Ok(m) => out.messages.push(m),
            Err(_) => out.rejected += 1,
        }
    }
    Ok(out)
}

/// Per-thread unread counts a GeekBridge phone adds to the conversation-list answer
/// (`"unread": {"<thread id>": n}`), because the stock list carries only the newest message of each
/// thread and the real unread count is otherwise unknowable. Bounded: <= 300 threads, counts
/// capped at 10_000, anything else is skipped.
pub fn parse_unread(body: &Map<String, Value>) -> Vec<(ThreadId, u32)> {
    let Some(map) = body.get("unread").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .take(300)
        .filter_map(|(k, v)| {
            let t = k.parse::<i64>().ok().filter(|t| *t >= 0)?;
            let n = u32::try_from(v.as_u64()?.min(10_000)).ok()?;
            Some((ThreadId(t), n))
        })
        .collect()
}

/// SIM subscriptions a GeekBridge phone adds to its `kdeconnect.sms.messages` packets
/// (`"sims": [{"id", "slot", "label"}]`; stock phones send none). Untrusted: at most
/// [`bridge_core::phone_view::MAX_SIMS`], ids must be selectable subscription ids, labels are
/// stripped of control characters and cut to 24 characters. `None` = the field is absent, so the
/// caller keeps what it had; `Some(vec![])` = the phone says it has no usable SIM list.
pub fn parse_sims(body: &Map<String, Value>) -> Option<Vec<bridge_core::phone_view::SimItem>> {
    use bridge_core::{messages::SubscriptionId, phone_view::SimItem};
    let list = body.get("sims")?.as_array()?;
    let mut out: Vec<SimItem> = Vec::new();
    for v in list.iter().take(bridge_core::phone_view::MAX_SIMS) {
        let Some(id) = v
            .get("id")
            .and_then(Value::as_i64)
            .and_then(|i| i32::try_from(i).ok())
            .and_then(|i| SubscriptionId::new(i).ok())
        else {
            continue;
        };
        if out.iter().any(|s| s.id == id.get()) {
            continue;
        }
        let slot = v
            .get("slot")
            .and_then(Value::as_u64)
            .and_then(|s| u8::try_from(s).ok())
            .filter(|s| *s <= 8)
            .unwrap_or(0);
        let label: String = v
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control() && !crate::identity::is_invisible_format(*c))
            .take(24)
            .collect();
        out.push(SimItem {
            id: id.get(),
            slot,
            label: label.trim().to_owned(),
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::messages::{Direction, SimSelection, SubscriptionId};

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn send_request_matches_the_stock_shape() {
        let intent = OutgoingTextIntent::new(
            &["+15551234567"],
            "hello",
            SimSelection::Subscription(SubscriptionId::new(3).unwrap()),
        )
        .unwrap();
        let b = send_request_body(&intent, "gb-1");
        assert_eq!(b["version"], 2);
        assert_eq!(b["sendSms"], true);
        assert_eq!(b["addresses"][0]["address"], "+15551234567");
        assert_eq!(b["messageBody"], "hello");
        assert_eq!(b["subID"], 3);
        assert_eq!(b["requestId"], "gb-1");
        let d = OutgoingTextIntent::new(&["1"], "x", SimSelection::PhoneDefault).unwrap();
        assert!(!send_request_body(&d, "r").contains_key("subID"));
    }

    #[test]
    fn conversation_request_is_bounded() {
        let b = request_conversation_body(ThreadId(7), Some(1234), 10_000);
        assert_eq!(b["threadID"], 7);
        assert_eq!(b["rangeStartTimestamp"], 1234);
        assert_eq!(b["numberToRequest"], PAGE_SIZE);
        assert!(
            !request_conversation_body(ThreadId(7), None, 5).contains_key("rangeStartTimestamp")
        );
    }

    #[test]
    fn messages_parse_stock_shape_and_count_bad_elements() {
        let body = obj(json!({"version": 2, "messages": [
            {"addresses": [{"address": "+15551234567"}], "body": "hi", "date": 1730000000000_i64,
             "type": 1, "read": 0, "thread_id": 4, "_id": 9, "sub_id": 1, "event": 1},
            {"addresses": [{"address": "+15551234567"}], "body": "yo", "date": "1730000001000",
             "type": "2", "read": true, "thread_id": 4, "_id": 10},
            {"body": "no type"},
        ]}));
        let p = parse_messages(&body).unwrap();
        assert_eq!((p.messages.len(), p.rejected), (2, 1));
        assert_eq!(p.messages[0].direction(), Direction::Incoming);
        assert_eq!(p.messages[1].direction(), Direction::Outgoing);
    }

    #[test]
    fn wrong_version_or_shape_or_size_is_refused() {
        assert!(parse_messages(&obj(json!({"version": 1, "messages": []}))).is_err());
        assert!(parse_messages(&obj(json!({"version": 2}))).is_err());
        let many = vec![json!({}); MAX_MESSAGES_PER_PACKET + 1];
        assert_eq!(
            parse_messages(&obj(json!({"version": 2, "messages": many}))).unwrap_err(),
            SmsWireError::TooManyMessages
        );
    }

    #[test]
    fn sim_labels_lose_direction_overrides_and_zero_width_characters() {
        let b = obj(json!({"sims": [{"id": 3, "slot": 1, "label": "Ev\u{202e}il\u{200b}SIM"}]}));
        assert_eq!(parse_sims(&b).unwrap()[0].label, "EvilSIM");
    }

    #[test]
    fn sims_are_optional_bounded_and_cleaned() {
        assert!(parse_sims(&obj(json!({"version": 2, "messages": []}))).is_none());
        let b = obj(json!({"sims": [
            {"id": 3, "slot": 1, "label": "Carrier\u{7}A"},
            {"id": 3, "slot": 2, "label": "dup"},
            {"id": 0, "slot": 1, "label": "unknown"},
            {"id": 9, "slot": 99, "label": "x".repeat(80)},
            {"id": 11}, {"id": 12}, {"id": 13},
        ]}));
        let sims = parse_sims(&b).unwrap();
        assert_eq!(sims.len(), 2); // only the first 4 entries are inspected: 3, dup, 0, 9
        assert_eq!(
            (sims[0].id, sims[0].slot, sims[0].label.as_str()),
            (3, 1, "CarrierA")
        );
        assert_eq!((sims[1].id, sims[1].slot, sims[1].label.len()), (9, 0, 24));
        assert_eq!(parse_sims(&obj(json!({"sims": []}))).unwrap().len(), 0);
    }

    #[test]
    fn unread_counts_are_optional_and_bounded() {
        assert!(parse_unread(&obj(json!({"version": 2}))).is_empty());
        let mut got = parse_unread(&obj(
            json!({"unread": {"4": 3, "x": 1, "-2": 1, "9": 99999, "7": "no"}}),
        ));
        got.sort_by_key(|(t, _)| t.0);
        assert_eq!(got, vec![(ThreadId(4), 3), (ThreadId(9), 10_000)]);
    }
}
