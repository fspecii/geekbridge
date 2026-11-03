// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone notification mirroring over the stock KDE Connect packets.
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`NOTIFICATION`] | phone to computer | `id`, `appName`, `title`, `text`, `ticker`, `time`, `isClearable`, `silent`, `onlyOnce`, `actions` (titles); or `id` + `isCancel: true` when it went away |
//! | [`REQUEST`] | computer to phone | `request: true` (send what is showing now) or `cancel: <id>` (dismiss it) |
//! | [`ACTION`] | computer to phone | `key: <id>`, `action: <title>`: press that button |
//!
//! `kdeconnect.notification.reply` (typed replies) is deliberately not part of this build. These are the
//! names and fields stock KDE Connect uses, so a stock peer interoperates in both directions. Parsing is
//! strict about the shape of the identifiers and lenient about stock extras: unknown fields are ignored
//! (an icon travels as a payload, which the computer never fetches), text is stripped of control
//! characters and cut to fixed lengths.
use serde_json::{Map, Value, json};

pub const NOTIFICATION: &str = "kdeconnect.notification";
pub const REQUEST: &str = "kdeconnect.notification.request";
pub const ACTION: &str = "kdeconnect.notification.action";

/// Longest id, app name, title, text, ticker and action title kept (characters).
pub const MAX_ID: usize = 128;
pub const MAX_APP: usize = 64;
pub const MAX_TITLE: usize = 160;
pub const MAX_TEXT: usize = 600;
pub const MAX_ACTION: usize = 48;
/// Buttons kept per notification.
pub const MAX_ACTIONS: usize = 3;

pub fn is_notification_kind(kind: &str) -> bool {
    matches!(kind, NOTIFICATION | REQUEST | ACTION)
}

/// One mirrored notification as the computer keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirrored {
    pub id: String,
    pub app: String,
    pub title: String,
    pub text: String,
    /// Phone clock, milliseconds since the epoch, when it sent one.
    pub time_ms: Option<u64>,
    /// The user can swipe it away on the phone (so a dismiss from here is meaningful).
    pub clearable: bool,
    /// The phone marks it as one that already existed (a reply to `request`): no alert.
    pub silent: bool,
    /// Alert only the first time; later updates change the text quietly.
    pub only_once: bool,
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Post(Mirrored),
    Cancel { id: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotificationError {
    Malformed(&'static str),
}

/// Text without control characters, cut to `max` characters, trimmed.
fn clip(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

fn text_field(
    body: &Map<String, Value>,
    key: &'static str,
    max: usize,
) -> Result<String, NotificationError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(clip(s, max)),
        Some(_) => Err(NotificationError::Malformed(key)),
    }
}

fn flag(body: &Map<String, Value>, key: &'static str) -> Result<bool, NotificationError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(NotificationError::Malformed(key)),
    }
}

/// A valid id: non-empty, no control characters, at most [`MAX_ID`] characters.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_ID && !id.chars().any(char::is_control)
}

/// Decode a [`NOTIFICATION`] body.
pub fn parse(body: &Map<String, Value>) -> Result<Event, NotificationError> {
    let id = match body.get("id") {
        Some(Value::String(s)) if valid_id(s) => s.clone(),
        _ => return Err(NotificationError::Malformed("id")),
    };
    if flag(body, "isCancel")? {
        return Ok(Event::Cancel { id });
    }
    let time_ms = match body.get("time") {
        None | Some(Value::Null) => None,
        // Stock Android sends the time as a decimal string, others as a number.
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        Some(v) => v.as_u64(),
    };
    let actions = match body.get("actions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| clip(s, MAX_ACTION))
            .filter(|s| !s.is_empty())
            .take(MAX_ACTIONS)
            .collect(),
        Some(_) => return Err(NotificationError::Malformed("actions")),
    };
    let title = text_field(body, "title", MAX_TITLE)?;
    let mut text = text_field(body, "text", MAX_TEXT)?;
    if text.is_empty() {
        text = text_field(body, "ticker", MAX_TEXT).unwrap_or_default();
        if text == title {
            text.clear();
        }
    }
    Ok(Event::Post(Mirrored {
        id,
        app: text_field(body, "appName", MAX_APP)?,
        title,
        text,
        time_ms,
        // Stock sends the flag always; a peer that leaves it out gets the safer reading (dismissable).
        clearable: match body.get("isClearable") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(NotificationError::Malformed("isClearable")),
        },
        silent: flag(body, "silent")?,
        only_once: flag(body, "onlyOnce")?,
        actions,
    }))
}

fn object(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else { unreachable!() };
    m
}

/// Body of a [`NOTIFICATION`] post (stock field names; no icon payload).
pub fn post_body(n: &Mirrored) -> Map<String, Value> {
    object(json!({
        "id": n.id,
        "appName": n.app,
        "title": n.title,
        "text": n.text,
        "ticker": if n.text.is_empty() { n.title.clone() } else { format!("{}: {}", n.title, n.text) },
        "time": n.time_ms.unwrap_or(0).to_string(),
        "isClearable": n.clearable,
        "silent": n.silent,
        "onlyOnce": n.only_once,
        "actions": n.actions,
    }))
}

/// Body of a [`NOTIFICATION`] cancel (it is gone on the phone).
pub fn cancel_event_body(id: &str) -> Map<String, Value> {
    object(json!({ "id": id, "isCancel": true }))
}

/// Body of a [`REQUEST`] asking for everything that is showing.
pub fn request_body() -> Map<String, Value> {
    object(json!({ "request": true }))
}

/// Body of a [`REQUEST`] dismissing one notification on the phone.
pub fn dismiss_body(id: &str) -> Map<String, Value> {
    object(json!({ "cancel": id }))
}

/// Body of an [`ACTION`]: press the button titled `action` of notification `id`.
pub fn action_body(id: &str, action: &str) -> Map<String, Value> {
    object(json!({ "key": id, "action": action }))
}

/// What the phone is asked to do by a [`REQUEST`] body.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    All,
    Dismiss(String),
}

/// Decode a [`REQUEST`] body (used by tests and the simulated phone; the app has its own Kotlin copy).
pub fn parse_request(body: &Map<String, Value>) -> Result<Ask, NotificationError> {
    if let Some(v) = body.get("cancel") {
        return match v {
            Value::String(s) if valid_id(s) => Ok(Ask::Dismiss(s.clone())),
            _ => Err(NotificationError::Malformed("cancel")),
        };
    }
    match body.get("request") {
        Some(Value::Bool(true)) => Ok(Ask::All),
        _ => Err(NotificationError::Malformed("request")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn sample() -> Mirrored {
        Mirrored {
            id: "n1".into(),
            app: "Chat".into(),
            title: "Ada".into(),
            text: "hello".into(),
            time_ms: Some(1_700_000_000_000),
            clearable: true,
            silent: false,
            only_once: false,
            actions: vec!["Mark as read".into()],
        }
    }

    #[test]
    fn a_post_round_trips_in_stock_field_names() {
        let b = post_body(&sample());
        assert_eq!(b["appName"], "Chat");
        assert_eq!(b["time"], "1700000000000");
        assert_eq!(parse(&b), Ok(Event::Post(sample())));
    }

    #[test]
    fn a_stock_phone_packet_with_extras_parses() {
        let b = obj(json!({
            "id": "0|com.x|1|null|10001", "appName": "Mail", "title": "Hi", "text": "",
            "ticker": "Hi: body", "time": "1700000000001", "isClearable": false, "silent": true,
            "onlyOnce": true, "actions": ["Reply", "Archive", "", 7], "requestReplyId": "abc",
            "payloadHash": "ff", "unknown": [1]
        }));
        let Ok(Event::Post(n)) = parse(&b) else {
            panic!("not a post")
        };
        assert_eq!(n.text, "Hi: body");
        assert!(!n.clearable && n.silent && n.only_once);
        assert_eq!(n.actions, ["Reply", "Archive"]);
        assert_eq!(n.time_ms, Some(1_700_000_000_001));
    }

    #[test]
    fn cancel_and_requests_round_trip() {
        assert_eq!(
            parse(&cancel_event_body("n1")),
            Ok(Event::Cancel { id: "n1".into() })
        );
        assert_eq!(parse_request(&request_body()), Ok(Ask::All));
        assert_eq!(
            parse_request(&dismiss_body("n1")),
            Ok(Ask::Dismiss("n1".into()))
        );
        assert_eq!(action_body("n1", "Archive")["key"], "n1");
    }

    #[test]
    fn garbage_is_an_error_and_text_is_bounded_and_clean() {
        for bad in [
            json!({}),
            json!({"id": ""}),
            json!({"id": 5}),
            json!({"id": "a\nb"}),
            json!({"id": "x".repeat(MAX_ID + 1)}),
            json!({"id": "a", "title": 4}),
            json!({"id": "a", "actions": "x"}),
            json!({"id": "a", "isClearable": "yes"}),
            json!({"id": "a", "isCancel": 1}),
        ] {
            assert!(parse(&obj(bad.clone())).is_err(), "{bad}");
        }
        let Ok(Event::Post(n)) = parse(&obj(json!({
            "id": "a", "title": "t\u{0}\n".repeat(500), "text": "x".repeat(5000),
            "actions": ["a", "b", "c", "d", "e"]
        }))) else {
            panic!()
        };
        assert!(n.title.chars().count() <= MAX_TITLE && !n.title.contains('\n'));
        assert_eq!(n.text.chars().count(), MAX_TEXT);
        assert_eq!(n.actions.len(), MAX_ACTIONS);
        assert!(parse_request(&obj(json!({"request": false}))).is_err());
        assert!(parse_request(&obj(json!({"cancel": 3}))).is_err());
    }
}
