// SPDX-License-Identifier: GPL-3.0-or-later
//! Debug-build diagnostics over the paired link (`geekbridge.debug.*` packets).
//!
//! Only a debug build of the Android app declares these packet types; a release phone and stock KDE
//! Connect peers never do, so they are never negotiated with them. The computer accepts them only from
//! a paired device whose `debug` consent is on.
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`LOG`] | phone to computer | `seq` (strictly increasing per phone process), `lines` (<= [`MAX_LINES`] strings) |
//! | [`STATE_REQUEST`] | computer to phone | `requestId` |
//! | [`STATE`] | phone to computer | `requestId`, `text` (a redacted state dump, <= [`MAX_STATE_BYTES`]) |
//!
//! Everything is bounded and strict: a malformed or oversize packet is an error, never a guess. Lines
//! are sanitised to printable text so a hostile phone cannot inject terminal escapes into the files or
//! the screen of `geekbridge-ctl phone-log -f`.
use serde_json::{Map, Value, json};

pub const LOG: &str = "geekbridge.debug.log";
pub const STATE_REQUEST: &str = "geekbridge.debug.state_request";
pub const STATE: &str = "geekbridge.debug.state";

pub const MAX_LINES: usize = 200;
pub const MAX_LINE_CHARS: usize = 1024;
/// Whole `lines` payload per packet.
pub const MAX_LOG_BYTES: usize = 64 * 1024;
pub const MAX_STATE_BYTES: usize = 128 * 1024;
pub const MAX_REQUEST_ID: usize = 64;

pub fn is_debug_kind(kind: &str) -> bool {
    matches!(kind, LOG | STATE_REQUEST | STATE)
}

#[derive(Debug, PartialEq, Eq)]
pub enum DebugError {
    Malformed(&'static str),
    TooLarge,
}

impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "malformed debug packet: {m}"),
            Self::TooLarge => f.write_str("debug packet over the size limit"),
        }
    }
}

/// Invisible format characters that can reorder or hide text on screen (bidi overrides and isolates, zero-width
/// marks, BOM, Arabic letter mark): not "control" to Rust, but a hostile phone could use them to make a log line
/// read differently from what it holds.
fn is_spoofing_format(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}' | '\u{061C}' | '\u{FEFF}')
}

/// One line made safe to store and print: control characters (including ESC) and invisible bidi/format
/// characters become `?`, tabs a space, and it is cut to [`MAX_LINE_CHARS`].
pub fn sanitize_line(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() || is_spoofing_format(c) => '?',
            c => c,
        })
        .take(MAX_LINE_CHARS)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct LogBatch {
    pub seq: u64,
    pub lines: Vec<String>,
}

pub fn parse_log(body: &Map<String, Value>) -> Result<LogBatch, DebugError> {
    let seq = body
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or(DebugError::Malformed("seq"))?;
    let raw = body
        .get("lines")
        .and_then(Value::as_array)
        .ok_or(DebugError::Malformed("lines"))?;
    if raw.len() > MAX_LINES {
        return Err(DebugError::TooLarge);
    }
    let mut total = 0usize;
    let mut lines = Vec::with_capacity(raw.len());
    for v in raw {
        let s = v.as_str().ok_or(DebugError::Malformed("line"))?;
        total = total.saturating_add(s.len());
        if total > MAX_LOG_BYTES {
            return Err(DebugError::TooLarge);
        }
        lines.push(sanitize_line(s));
    }
    Ok(LogBatch { seq, lines })
}

pub fn log_body(seq: u64, lines: &[String]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("seq".into(), json!(seq));
    m.insert("lines".into(), json!(lines));
    m
}

pub fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn state_request_body(request_id: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("requestId".into(), json!(request_id));
    m
}

#[derive(Debug, PartialEq, Eq)]
pub struct StateReply {
    pub request_id: String,
    pub text: String,
}

pub fn parse_state(body: &Map<String, Value>) -> Result<StateReply, DebugError> {
    let request_id = body
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|id| valid_request_id(id))
        .ok_or(DebugError::Malformed("requestId"))?;
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or(DebugError::Malformed("text"))?;
    if text.len() > MAX_STATE_BYTES {
        return Err(DebugError::TooLarge);
    }
    // Keep newlines (the dump is multi-line) but nothing else that could drive a terminal.
    let text = text
        .chars()
        .map(|c| match c {
            '\n' => '\n',
            '\t' => ' ',
            c if c.is_control() || is_spoofing_format(c) => '?',
            c => c,
        })
        .collect();
    Ok(StateReply {
        request_id: request_id.to_owned(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_log_packet_round_trips_and_is_sanitised() {
        let body = obj(json!({"seq": 7, "lines": ["ok line", "esc \u{1b}[31mred", "tab\there"]}));
        let b = parse_log(&body).unwrap();
        assert_eq!(b.seq, 7);
        assert_eq!(b.lines, ["ok line", "esc ?[31mred", "tab here"]);
        assert_eq!(parse_log(&log_body(3, &b.lines)).unwrap().seq, 3);
    }

    #[test]
    fn bidi_overrides_and_zero_width_marks_cannot_disguise_a_line() {
        let body =
            obj(json!({"seq": 1, "lines": ["ok \u{202E}gnirts\u{202C} a\u{200B}b\u{FEFF}"]}));
        assert_eq!(parse_log(&body).unwrap().lines, ["ok ?gnirts? a?b?"]);
        let st = obj(json!({"requestId": "r", "text": "x\u{2066}y\n"}));
        assert_eq!(parse_state(&st).unwrap().text, "x?y\n");
    }

    #[test]
    fn log_packets_are_strict_and_bounded() {
        assert_eq!(
            parse_log(&obj(json!({"lines": []}))),
            Err(DebugError::Malformed("seq"))
        );
        assert_eq!(
            parse_log(&obj(json!({"seq": 1, "lines": "x"}))),
            Err(DebugError::Malformed("lines"))
        );
        assert_eq!(
            parse_log(&obj(json!({"seq": 1, "lines": [5]}))),
            Err(DebugError::Malformed("line"))
        );
        let many = vec!["a"; MAX_LINES + 1];
        assert_eq!(
            parse_log(&obj(json!({"seq": 1, "lines": many}))),
            Err(DebugError::TooLarge)
        );
        let fat = vec!["x".repeat(2000); 40];
        assert_eq!(
            parse_log(&obj(json!({"seq": 1, "lines": fat}))),
            Err(DebugError::TooLarge)
        );
        let long = obj(json!({"seq": 1, "lines": ["y".repeat(5000)]}));
        assert_eq!(
            parse_log(&long).unwrap().lines[0].chars().count(),
            MAX_LINE_CHARS
        );
    }

    #[test]
    fn state_replies_need_a_valid_id_and_bounded_text() {
        let ok = obj(json!({"requestId": "r-1", "text": "a\nb\u{7}"}));
        let r = parse_state(&ok).unwrap();
        assert_eq!((r.request_id.as_str(), r.text.as_str()), ("r-1", "a\nb?"));
        assert!(parse_state(&obj(json!({"requestId": "bad id", "text": ""}))).is_err());
        assert!(parse_state(&obj(json!({"requestId": "r"}))).is_err());
        let big = "z".repeat(MAX_STATE_BYTES + 1);
        assert_eq!(
            parse_state(&obj(json!({"requestId": "r", "text": big}))),
            Err(DebugError::TooLarge)
        );
        assert!(valid_request_id("abc_DEF-9"));
        assert!(!valid_request_id(""));
        assert!(is_debug_kind(LOG) && !is_debug_kind("kdeconnect.ping"));
    }
}
