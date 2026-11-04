// SPDX-License-Identifier: GPL-3.0-or-later
//! The computer's answer to a share from a GeekBridge phone, so the phone can say what became of it instead of
//! waiting for a connection that will never come.
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`RESULT`] | computer to phone | `transferId` (a file) or `textId` (text or link), `outcome`, optional `reason` |
//!
//! The phone numbers what it shares and adds [`SHARE_ID_KEY`] to its `kdeconnect.share.request` body, but only
//! when the computer declared [`RESULT`] (inside TLS). GeekBridge-only: the phone declares [`RESULT`] as an
//! incoming capability, the computer sends it only to a phone that did, so a stock KDE Connect peer is never
//! sent the packet and never sees the extra key. The capability name is kept out of the plaintext identity like
//! every `geekbridge.*` name.
//!
//! | outcome | meaning |
//! | --- | --- |
//! | `accepted` | the computer takes the file and is about to fetch it |
//! | `declined` | the user declined, or the offer lapsed (`reason`: `user`, `expired`) |
//! | `receive_off` | this computer is not set to receive from this phone (`reason`: `not_allowed`) |
//! | `too_large` | the file or text is over the computer's limit |
//! | `busy` | no room for it now (`reason`: `too_many_offers`, `too_many_transfers`) |
//! | `saved` | text kept on the computer (shown there, never acted on) |
//! | `link_saved` | a link kept on the computer; it is never opened automatically |
//!
//! Every packet is a plain answer to one share and is sent at most once per share (a later one for the same id
//! is ignored by the phone).
use serde_json::{Map, Value};

pub const RESULT: &str = "geekbridge.share.result";
/// Body key the phone adds to its share packet: a positive integer, unique per phone connection.
pub const SHARE_ID_KEY: &str = "geekbridgeShareId";
const FILE_KEY: &str = "transferId";
const TEXT_KEY: &str = "textId";
/// Largest id accepted (what a JSON number carries exactly).
pub const MAX_ID: u64 = (1 << 53) - 1;
pub const MAX_REASON_CHARS: usize = 32;

pub fn is_result_kind(kind: &str) -> bool {
    kind == RESULT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Declined,
    ReceiveOff,
    TooLarge,
    Busy,
    Saved,
    LinkSaved,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::ReceiveOff => "receive_off",
            Self::TooLarge => "too_large",
            Self::Busy => "busy",
            Self::Saved => "saved",
            Self::LinkSaved => "link_saved",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "accepted" => Self::Accepted,
            "declined" => Self::Declined,
            "receive_off" => Self::ReceiveOff,
            "too_large" => Self::TooLarge,
            "busy" => Self::Busy,
            "saved" => Self::Saved,
            "link_saved" => Self::LinkSaved,
            _ => return None,
        })
    }

    /// An answer that ends the share without the file moving.
    pub fn refuses(self) -> bool {
        matches!(
            self,
            Self::Declined | Self::ReceiveOff | Self::TooLarge | Self::Busy
        )
    }
}

/// What the answer is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    File(u64),
    Text(u64),
}

impl Subject {
    pub fn id(self) -> u64 {
        match self {
            Self::File(i) | Self::Text(i) => i,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareResult {
    pub subject: Subject,
    pub outcome: Outcome,
    pub reason: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResultError {
    Malformed(&'static str),
}

/// The id a phone put in its share packet; anything but a positive integer within range is no id.
pub fn share_id(body: &Map<String, Value>) -> Option<u64> {
    body.get(SHARE_ID_KEY)
        .and_then(Value::as_u64)
        .filter(|n| (1..=MAX_ID).contains(n))
}

pub fn valid_reason(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REASON_CHARS
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn body(r: &ShareResult) -> Map<String, Value> {
    let mut m = Map::new();
    match r.subject {
        Subject::File(i) => m.insert(FILE_KEY.into(), Value::from(i)),
        Subject::Text(i) => m.insert(TEXT_KEY.into(), Value::from(i)),
    };
    m.insert("outcome".into(), Value::from(r.outcome.as_str()));
    if let Some(reason) = r.reason.as_deref().filter(|s| valid_reason(s)) {
        m.insert("reason".into(), Value::from(reason));
    }
    m
}

/// Strict about the id and the outcome (anything else is ignored by the receiver), lenient about extra keys and
/// about a reason it cannot use (dropped, the outcome still stands).
pub fn parse(body: &Map<String, Value>) -> Result<ShareResult, ResultError> {
    let id = |key: &str| -> Result<Option<u64>, ResultError> {
        match body.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_ID).contains(n))
                .map(Some)
                .ok_or(ResultError::Malformed("id")),
        }
    };
    let subject = match (id(FILE_KEY)?, id(TEXT_KEY)?) {
        (Some(f), None) => Subject::File(f),
        (None, Some(t)) => Subject::Text(t),
        (None, None) => return Err(ResultError::Malformed("id missing")),
        (Some(_), Some(_)) => return Err(ResultError::Malformed("two ids")),
    };
    let outcome = body
        .get("outcome")
        .and_then(Value::as_str)
        .and_then(Outcome::from_name)
        .ok_or(ResultError::Malformed("outcome"))?;
    // A text can only be saved, refused or rejected; a file cannot be "saved" by this packet.
    let fits = match subject {
        Subject::File(_) => !matches!(outcome, Outcome::Saved | Outcome::LinkSaved),
        Subject::Text(_) => outcome != Outcome::Accepted,
    };
    if !fits {
        return Err(ResultError::Malformed("outcome does not fit"));
    }
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .filter(|s| valid_reason(s))
        .map(str::to_owned);
    Ok(ShareResult {
        subject,
        outcome,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn every_outcome_round_trips_for_the_subjects_it_fits() {
        for o in [
            Outcome::Accepted,
            Outcome::Declined,
            Outcome::ReceiveOff,
            Outcome::TooLarge,
            Outcome::Busy,
        ] {
            let r = ShareResult {
                subject: Subject::File(7),
                outcome: o,
                reason: Some("user".into()),
            };
            assert_eq!(parse(&body(&r)), Ok(r));
        }
        for o in [
            Outcome::Saved,
            Outcome::LinkSaved,
            Outcome::Declined,
            Outcome::ReceiveOff,
            Outcome::TooLarge,
            Outcome::Busy,
        ] {
            let r = ShareResult {
                subject: Subject::Text(9),
                outcome: o,
                reason: None,
            };
            assert_eq!(parse(&body(&r)), Ok(r));
        }
    }

    #[test]
    fn odd_ids_outcomes_and_pairings_are_errors_extras_and_bad_reasons_are_not() {
        for v in [
            json!({}),
            json!({"outcome": "accepted"}),
            json!({"transferId": 0, "outcome": "accepted"}),
            json!({"transferId": -1, "outcome": "accepted"}),
            json!({"transferId": 1.5, "outcome": "accepted"}),
            json!({"transferId": "1", "outcome": "accepted"}),
            json!({"transferId": 9007199254740992u64, "outcome": "accepted"}),
            json!({"transferId": 1, "textId": 1, "outcome": "declined"}),
            json!({"transferId": 1, "outcome": "ACCEPTED"}),
            json!({"transferId": 1, "outcome": "later"}),
            json!({"transferId": 1, "outcome": ["accepted"]}),
            json!({"transferId": 1, "outcome": "saved"}),
            json!({"textId": 1, "outcome": "accepted"}),
        ] {
            assert!(parse(&obj(v.clone())).is_err(), "{v}");
        }
        let m = json!({"textId": 4, "outcome": "saved", "reason": "Not Valid!", "future": [1]});
        let r = parse(&obj(m)).unwrap();
        assert_eq!(r.reason, None);
        assert_eq!(r.subject, Subject::Text(4));
    }

    #[test]
    fn the_phones_id_is_a_positive_integer_or_nothing() {
        let ok = obj(json!({ SHARE_ID_KEY: 12 }));
        assert_eq!(share_id(&ok), Some(12));
        for v in [
            json!(0),
            json!(-3),
            json!("5"),
            json!(1.5),
            json!(null),
            json!(1u64 << 53),
        ] {
            assert_eq!(share_id(&obj(json!({ SHARE_ID_KEY: v }))), None, "{v}");
        }
        assert_eq!(share_id(&Map::new()), None);
    }

    #[test]
    fn a_reason_that_could_carry_text_is_never_sent() {
        let r = ShareResult {
            subject: Subject::File(1),
            outcome: Outcome::Declined,
            reason: Some("a b\n".into()),
        };
        assert!(body(&r).get("reason").is_none());
        assert!(valid_reason("too_many_offers") && !valid_reason(&"x".repeat(33)));
    }

    #[test]
    fn the_type_is_geekbridge_only() {
        assert!(RESULT.starts_with("geekbridge."));
        assert!(is_result_kind(RESULT) && !is_result_kind("kdeconnect.share.request"));
    }
}
