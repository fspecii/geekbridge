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

    // ---- test-gaps-4

    #[test]
    fn every_level_from_minus_one_to_a_hundred_and_one_is_classified() {
        for c in -5i64..=105 {
            let r = parse(&obj(json!({"currentCharge": c, "isCharging": true})));
            match c {
                -1 => assert_eq!(r, Ok(Reading::Unknown)),
                0..=100 => assert_eq!(
                    r,
                    Ok(Reading::Level {
                        percent: c as u8,
                        charging: true,
                        low: false
                    }),
                    "{c}"
                ),
                _ => assert_eq!(r, Err(BatteryError::Malformed("currentCharge")), "{c}"),
            }
        }
    }

    #[test]
    fn threshold_event_is_low_only_for_exactly_one() {
        let low = |t: Value| match parse(&obj(
            json!({"currentCharge": 9, "isCharging": false, "thresholdEvent": t}),
        )) {
            Ok(Reading::Level { low, .. }) => Ok(low),
            Ok(Reading::Unknown) => unreachable!(),
            Err(e) => Err(e),
        };
        assert_eq!(low(json!(1)), Ok(true));
        for not_low in [json!(0), json!(2), json!(-1), json!(null)] {
            assert_eq!(low(not_low.clone()), Ok(false), "{not_low}");
        }
        for bad in [json!("1"), json!(1.5), json!(true), json!([1]), json!({})] {
            assert_eq!(
                low(bad.clone()),
                Err(BatteryError::Malformed("thresholdEvent")),
                "{bad}"
            );
        }
        // Missing is the same as none.
        assert_eq!(
            parse(&obj(json!({"currentCharge": 9, "isCharging": false}))),
            Ok(Reading::Level {
                percent: 9,
                charging: false,
                low: false
            })
        );
    }

    #[test]
    fn the_error_names_the_first_field_that_is_wrong() {
        let name = |v: Value| match parse(&obj(v)) {
            Err(BatteryError::Malformed(f)) => f,
            other => panic!("{other:?}"),
        };
        assert_eq!(name(json!({})), "currentCharge");
        assert_eq!(name(json!({"currentCharge": 5})), "isCharging");
        assert_eq!(
            name(json!({"currentCharge": 500})),
            "isCharging",
            "the range is judged after the other fields"
        );
        assert_eq!(
            name(json!({"currentCharge": 500, "isCharging": 0})),
            "isCharging"
        );
        assert_eq!(
            name(json!({"currentCharge": u64::MAX, "isCharging": true})),
            "currentCharge"
        );
        assert_eq!(
            name(json!({"currentCharge": null, "isCharging": true})),
            "currentCharge"
        );
    }

    #[test]
    fn a_built_body_parses_back_for_every_input_and_never_exceeds_a_hundred() {
        for percent in [0u8, 1, 14, 15, 16, 99, 100, 101, 200, 255] {
            for charging in [false, true] {
                for low in [false, true] {
                    let b = body(percent, charging, low);
                    assert_eq!(
                        parse(&b),
                        Ok(Reading::Level {
                            percent: percent.min(100),
                            charging,
                            low
                        }),
                        "{percent} {charging} {low}"
                    );
                    assert_eq!(b.len(), 3, "exactly the stock fields");
                }
            }
        }
    }

    #[test]
    fn kinds_and_the_request_body() {
        assert!(
            is_battery_kind("kdeconnect.battery") && is_battery_kind("kdeconnect.battery.request")
        );
        for other in [
            "",
            "kdeconnect.batteryx",
            "kdeconnect.Battery",
            "geekbridge.battery",
            "kdeconnect.battery.",
        ] {
            assert!(!is_battery_kind(other), "{other}");
        }
        assert_eq!(request_body()["request"], true);
        assert_eq!(request_body().len(), 1);
        assert_ne!(BATTERY, REQUEST);
    }
}
