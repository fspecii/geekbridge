//! The stock KDE Connect SMS packets (`kdeconnect.sms.*`), as the desktop side speaks them.
//!
//! Field names and quirks come from `docs/architecture.md` (read from the pinned kdeconnect-android
//! and kdeconnect-kde sources). The phone never acknowledges `kdeconnect.sms.request`; the
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
}
