// SPDX-License-Identifier: GPL-3.0-or-later
//! A polite goodbye from the computer to a GeekBridge phone, so the phone can tell "the user hung up" from "the
//! link was lost" and does not immediately redial over a manual disconnect.
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`GOODBYE`] | computer to phone | `reason`: `user_disconnect`, `shutdown` or `unpair` |
//!
//! GeekBridge-only: the phone declares [`GOODBYE`] as an incoming capability, and the computer sends it only to a
//! phone that did (so never to a stock KDE Connect peer, which does not know the type). Sent just before the
//! computer closes the socket. What the phone does with it:
//!
//! * `user_disconnect` and `unpair`: no redial for [`SUPPRESS_FOR`], or until the user connects by hand or the
//!   computer dials again (any new connection ends the suppression).
//! * `shutdown`: nothing special. The computer is going away, so the phone redials with its normal back-off and
//!   gets back in as soon as the service is up again.
use std::time::Duration;

use serde_json::{Map, Value, json};

pub const GOODBYE: &str = "geekbridge.goodbye";

/// How long a phone stays off the computer after `user_disconnect`/`unpair`; the computer's own automatic
/// redial for that phone stays paused as long.
pub const SUPPRESS_FOR: Duration = Duration::from_secs(5 * 60);

pub fn is_goodbye_kind(kind: &str) -> bool {
    kind == GOODBYE
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The user disconnected this phone on the computer (button, tray, `geekbridge-ctl disconnect`).
    UserDisconnect,
    /// The computer's service is stopping.
    Shutdown,
    /// The computer forgot this phone.
    Unpair,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserDisconnect => "user_disconnect",
            Self::Shutdown => "shutdown",
            Self::Unpair => "unpair",
        }
    }

    /// Should the receiver stay off the sender for [`SUPPRESS_FOR`]?
    pub fn suppresses_redial(self) -> bool {
        !matches!(self, Self::Shutdown)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GoodbyeError {
    Malformed(&'static str),
}

pub fn body(reason: Reason) -> Map<String, Value> {
    match json!({ "reason": reason.as_str() }) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Strict about the reason (an unknown one is an error, so the receiver ignores the packet and redials as for a
/// plain loss), lenient about extra keys.
pub fn parse(body: &Map<String, Value>) -> Result<Reason, GoodbyeError> {
    match body.get("reason").and_then(Value::as_str) {
        Some("user_disconnect") => Ok(Reason::UserDisconnect),
        Some("shutdown") => Ok(Reason::Shutdown),
        Some("unpair") => Ok(Reason::Unpair),
        Some(_) => Err(GoodbyeError::Malformed("reason")),
        None => Err(GoodbyeError::Malformed("reason missing")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_round_trips() {
        for r in [Reason::UserDisconnect, Reason::Shutdown, Reason::Unpair] {
            assert_eq!(parse(&body(r)), Ok(r));
        }
    }

    #[test]
    fn only_a_deliberate_leave_suppresses_the_redial() {
        assert!(Reason::UserDisconnect.suppresses_redial());
        assert!(Reason::Unpair.suppresses_redial());
        assert!(!Reason::Shutdown.suppresses_redial());
    }

    #[test]
    fn unknown_or_odd_reasons_are_errors_and_extras_are_ignored() {
        for v in [
            json!({}),
            json!({"reason": 1}),
            json!({"reason": null}),
            json!({"reason": "USER_DISCONNECT"}),
            json!({"reason": "later"}),
            json!({"reason": ["shutdown"]}),
        ] {
            let m = v.as_object().unwrap().clone();
            assert!(parse(&m).is_err(), "{v}");
        }
        let m = json!({"reason": "shutdown", "future": [1, 2]});
        assert_eq!(parse(m.as_object().unwrap()), Ok(Reason::Shutdown));
    }

    #[test]
    fn the_type_is_geekbridge_only() {
        assert!(GOODBYE.starts_with("geekbridge."));
        assert!(is_goodbye_kind(GOODBYE) && !is_goodbye_kind("kdeconnect.ping"));
    }
}
