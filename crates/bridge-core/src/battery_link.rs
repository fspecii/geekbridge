// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone battery state over the stock KDE Connect packets (`kdeconnect.battery`).
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`BATTERY`] | phone to computer | `currentCharge` (0..=100), `isCharging` (bool), `thresholdEvent` (0 none, 1 low; optional) |
//! | [`REQUEST`] | computer to phone | `request: true`: send the current state now |
//!
//! These are the names and fields stock KDE Connect uses, so a stock peer interoperates in both
//! directions. The computer reads a reading only from a paired phone whose `battery` consent is on and
//! keeps it in memory for the life of the link. Parsing is strict about shape and lenient about stock
//! extras: unknown fields are ignored, an out-of-range or non-integer charge is an error.
use serde_json::{Map, Value, json};

pub const BATTERY: &str = "kdeconnect.battery";
pub const REQUEST: &str = "kdeconnect.battery.request";

pub fn is_battery_kind(kind: &str) -> bool {
    matches!(kind, BATTERY | REQUEST)
}

/// One decoded battery packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A usable level.
    Level {
        percent: u8,
        charging: bool,
        low: bool,
    },
    /// The phone does not know its level (stock sends -1): nothing to show.
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatteryError {
    Malformed(&'static str),
}

pub fn parse(body: &Map<String, Value>) -> Result<Reading, BatteryError> {
    let charge = body
        .get("currentCharge")
        .and_then(Value::as_i64)
        .ok_or(BatteryError::Malformed("currentCharge"))?;
    let charging = body
        .get("isCharging")
        .and_then(Value::as_bool)
        .ok_or(BatteryError::Malformed("isCharging"))?;
    let low = match body.get("thresholdEvent") {
        None | Some(Value::Null) => false,
        Some(v) => {
            v.as_i64()
                .ok_or(BatteryError::Malformed("thresholdEvent"))?
                == 1
        }
    };
    match charge {
        -1 => Ok(Reading::Unknown),
        0..=100 => Ok(Reading::Level {
            percent: charge as u8,
            charging,
            low,
        }),
        _ => Err(BatteryError::Malformed("currentCharge")),
    }
}

/// Body of a [`BATTERY`] packet (stock field names).
pub fn body(percent: u8, charging: bool, low: bool) -> Map<String, Value> {
    let Value::Object(m) = json!({
        "currentCharge": percent.min(100),
        "isCharging": charging,
        "thresholdEvent": i32::from(low),
    }) else {
        unreachable!()
    };
    m
}

/// Body of a [`REQUEST`] packet.
pub fn request_body() -> Map<String, Value> {
    let Value::Object(m) = json!({ "request": true }) else {
        unreachable!()
    };
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_stock_packet_round_trips() {
        let b = body(42, true, false);
        assert_eq!(
            parse(&b),
            Ok(Reading::Level {
                percent: 42,
                charging: true,
                low: false
            })
        );
        assert_eq!(b["currentCharge"], 42);
        assert_eq!(b["thresholdEvent"], 0);
        assert!(matches!(
            parse(&body(15, false, true)),
            Ok(Reading::Level { low: true, .. })
        ));
    }

    #[test]
    fn unknown_level_is_not_a_reading_and_garbage_is_an_error() {
        assert_eq!(
            parse(&obj(json!({"currentCharge": -1, "isCharging": false}))),
            Ok(Reading::Unknown)
        );
        for bad in [
            json!({"currentCharge": 101, "isCharging": false}),
            json!({"currentCharge": -2, "isCharging": false}),
            json!({"currentCharge": "50", "isCharging": false}),
            json!({"currentCharge": 50.5, "isCharging": false}),
            json!({"currentCharge": 50}),
            json!({"currentCharge": 50, "isCharging": 1}),
            json!({"isCharging": true}),
            json!({"currentCharge": 50, "isCharging": true, "thresholdEvent": "low"}),
        ] {
            assert!(parse(&obj(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn extra_stock_fields_are_ignored() {
        assert!(
            parse(&obj(
                json!({"currentCharge": 7, "isCharging": false, "thresholdEvent": 1, "x": [1]})
            ))
            .is_ok()
        );
    }
}
