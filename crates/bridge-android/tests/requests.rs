// SPDX-License-Identifier: GPL-3.0-or-later
//! Host tests of the public request handler (no JVM). Mirrors the JSON contract in
//! `docs/android-contract.md`.
use bridge_core::{clipboard::DEFAULT_MAX_TEXT_BYTES, messages::MAX_TEXT_CHARS};
use geekbridge_android::{InputError, MAX_REQUEST_BYTES, handle_request, input_error_response};
use serde_json::{Value, json};

fn call(request: &Value) -> Value {
    call_raw(&request.to_string())
}

fn call_raw(request: &str) -> Value {
    let out = handle_request(request);
    let v: Value = serde_json::from_str(&out).expect("response is JSON");
    assert_eq!(v["version"], 1, "{out}");
    v
}

fn ok(v: &Value) -> &Value {
    assert_eq!(v["ok"], true, "{v}");
    assert!(v.get("error").is_none());
    &v["result"]
}

fn code(v: &Value) -> &str {
    assert_eq!(v["ok"], false, "{v}");
    assert!(v.get("result").is_none());
    let e = v["error"].as_object().expect("error object");
    assert_eq!(e.len(), 2, "error has exactly code and message: {v}");
    assert!(e["message"].as_str().is_some_and(|m| !m.is_empty()));
    e["code"].as_str().unwrap()
}

// ------------------------------------------------------------------ version

#[test]
fn version_action_reports_schema_limits_and_no_transport() {
    let v = call(&json!({"version": 1, "action": "version"}));
    let r = ok(&v);
    assert_eq!(r["core_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r["request_versions"], json!([1]));
    assert_eq!(
        r["actions"],
        json!(["version", "validate_sms", "validate_clipboard"])
    );
    assert_eq!(r["limits"]["max_request_bytes"], MAX_REQUEST_BYTES);
    assert_eq!(r["limits"]["max_sms_chars"], MAX_TEXT_CHARS);
    assert_eq!(r["limits"]["max_clipboard_bytes"], DEFAULT_MAX_TEXT_BYTES);
    assert_eq!(r["validation_only"], true);
    // Nothing in the result may suggest pairing, trust or call readiness.
    let text = r.to_string();
    for word in ["paired", "trusted", "call_ready", "connected"] {
        assert!(!text.contains(word), "{word}");
    }
}

#[test]
fn version_action_rejects_arguments() {
    let v = call(&json!({"version": 1, "action": "version", "extra": 1}));
    assert_eq!(code(&v), "unknown_field");
}

// ------------------------------------------------------------------ SMS

fn sms(recipient: &str, text: &str) -> Value {
    json!({"version": 1, "action": "validate_sms", "recipient": recipient, "text": text})
}

#[test]
fn sms_valid_with_and_without_subscription() {
    let v = call(&sms("+15551234567", "hello"));
    let r = ok(&v);
    assert_eq!(r["valid"], true);
    assert_eq!(r["text_chars"], 5);
    assert_eq!(r["has_subscription"], false);

    let mut req = sms("+15551234567", "hello");
    req["subscription_id"] = json!(3);
    let v = call(&req);
    assert_eq!(ok(&v)["has_subscription"], true);
}

#[test]
fn sms_result_never_echoes_input() {
    let v = call(&sms("+15551234567", "super secret body"));
    let out = v.to_string();
    assert!(!out.contains("15551234567"));
    assert!(!out.contains("secret"));
}

#[test]
fn sms_applies_shared_message_rules() {
    assert_eq!(code(&call(&sms("+1555", ""))), "empty_text");
    assert_eq!(code(&call(&sms("+1555", " \t\n "))), "blank_text");
    assert_eq!(
        code(&call(&sms("+1555", "a\u{0}b"))),
        "forbidden_control_char"
    );
    assert_eq!(
        code(&call(&sms("+1555", "a\u{1b}[31m"))),
        "forbidden_control_char"
    );
    // Tab/CR/LF are allowed by the shared rule.
    ok(&call(&sms("+1555", "a\tb\r\nc")));
    assert_eq!(code(&call(&sms("", "hi"))), "invalid_recipient");
    assert_eq!(code(&call(&sms("   ", "hi"))), "invalid_recipient");
    assert_eq!(code(&call(&sms("+15\n55", "hi"))), "invalid_recipient");
    assert_eq!(
        code(&call(&sms(&"9".repeat(65), "hi"))),
        "invalid_recipient"
    );
    ok(&call(&sms(&"9".repeat(64), "hi")));
    // Surrounding whitespace in the recipient is trimmed by the shared type, not rejected.
    ok(&call(&sms("  +1555  ", "hi")));
}

#[test]
fn sms_length_is_counted_in_unicode_scalars_not_bytes_or_utf16() {
    // 2000 four-byte, two-UTF-16-unit scalars: within the limit.
    let at_limit = "😀".repeat(MAX_TEXT_CHARS);
    assert!(at_limit.len() > MAX_TEXT_CHARS * 3);
    let v = call(&sms("+1555", &at_limit));
    assert_eq!(ok(&v)["text_chars"], MAX_TEXT_CHARS);
    let over = "😀".repeat(MAX_TEXT_CHARS + 1);
    assert_eq!(code(&call(&sms("+1555", &over))), "text_too_long");
    // Combining sequences count every scalar.
    let combining = "e\u{301}".repeat(MAX_TEXT_CHARS / 2 + 1);
    assert_eq!(code(&call(&sms("+1555", &combining))), "text_too_long");
}

#[test]
fn sms_subscription_rules() {
    for bad in [
        json!(0),
        json!(-1),
        json!(i32::MAX),
        json!(i64::MAX),
        json!(1u64 << 40),
    ] {
        let mut req = sms("+1555", "hi");
        req["subscription_id"] = bad.clone();
        assert_eq!(code(&call(&req)), "invalid_subscription", "{bad}");
    }
    for bad in [
        json!(1.5),
        json!(1.0),
        json!("1"),
        json!(null),
        json!(true),
        json!([1]),
    ] {
        let mut req = sms("+1555", "hi");
        req["subscription_id"] = bad.clone();
        assert_eq!(code(&call(&req)), "invalid_field", "{bad}");
    }
    let mut req = sms("+1555", "hi");
    req["subscription_id"] = json!(i32::MAX - 1);
    ok(&call(&req));
}

#[test]
fn sms_field_shape_errors() {
    let missing_text = json!({"version": 1, "action": "validate_sms", "recipient": "+1"});
    assert_eq!(code(&call(&missing_text)), "missing_field");
    let missing_recipient = json!({"version": 1, "action": "validate_sms", "text": "x"});
    assert_eq!(code(&call(&missing_recipient)), "missing_field");
    for bad in [json!(1), json!(null), json!(["+1"]), json!({"a": 1})] {
        let mut req = sms("+1", "x");
        req["recipient"] = bad.clone();
        assert_eq!(code(&call(&req)), "invalid_field", "{bad}");
        let mut req = sms("+1", "x");
        req["text"] = bad;
        assert_eq!(code(&call(&req)), "invalid_field");
    }
    // The old/plural spelling is an unknown field, not silently accepted.
    let mut req = sms("+1", "x");
    req["recipients"] = json!(["+1", "+2"]);
    assert_eq!(code(&call(&req)), "unknown_field");
}

// ------------------------------------------------------------------ clipboard

fn clip(text: &str) -> Value {
    json!({"version": 1, "action": "validate_clipboard", "text": text})
}

#[test]
fn clipboard_valid_reports_bytes_not_content() {
    let v = call(&clip("héllo 😀"));
    let r = ok(&v);
    assert_eq!(r["valid"], true);
    assert_eq!(r["text_bytes"], "héllo 😀".len());
    assert_eq!(r["max_text_bytes"], DEFAULT_MAX_TEXT_BYTES);
    assert!(!v.to_string().contains("llo"));
}

#[test]
fn clipboard_applies_shared_rules() {
    assert_eq!(code(&call(&clip(""))), "empty_text");
    assert_eq!(code(&call(&clip("a\u{0}b"))), "contains_nul");
    // Unlike SMS, whitespace-only and other control characters are valid clipboard text.
    ok(&call(&clip("   ")));
    ok(&call(&clip("\u{1b}[0m\n")));
}

#[test]
fn clipboard_limit_is_in_utf8_bytes_at_the_shared_default() {
    ok(&call(&clip(&"a".repeat(DEFAULT_MAX_TEXT_BYTES))));
    assert_eq!(
        code(&call(&clip(&"a".repeat(DEFAULT_MAX_TEXT_BYTES + 1)))),
        "text_too_large"
    );
    // 3-byte scalars: the byte limit applies, not a scalar count.
    let fits = "€".repeat(DEFAULT_MAX_TEXT_BYTES / 3);
    ok(&call(&clip(&fits)));
    let over = "€".repeat(DEFAULT_MAX_TEXT_BYTES / 3 + 1);
    assert_eq!(code(&call(&clip(&over))), "text_too_large");
}

#[test]
fn clipboard_max_text_survives_worst_case_json_escaping() {
    // 64 KiB of control characters escape to 6 bytes each and must still fit the request limit.
    let escaped = "\u{1}".repeat(DEFAULT_MAX_TEXT_BYTES);
    let req = clip(&escaped).to_string();
    assert!(req.len() > DEFAULT_MAX_TEXT_BYTES * 5);
    assert!(req.len() <= MAX_REQUEST_BYTES);
    ok(&call_raw(&req));
}

#[test]
fn clipboard_rejects_extra_and_missing_fields() {
    let mut req = clip("x");
    req["sensitive"] = json!(true);
    assert_eq!(code(&call(&req)), "unknown_field");
    let v = call(&json!({"version": 1, "action": "validate_clipboard"}));
    assert_eq!(code(&v), "missing_field");
    let v = call(&json!({"version": 1, "action": "validate_clipboard", "text": 5}));
    assert_eq!(code(&v), "invalid_field");
}

// ------------------------------------------------------------------ envelope strictness

#[test]
fn version_must_be_the_integer_one() {
    for bad in [
        "0",
        "2",
        "-1",
        "1.0",
        "1e0",
        "\"1\"",
        "null",
        "true",
        "[1]",
        "18446744073709551617",
    ] {
        let req = format!(r#"{{"version": {bad}, "action": "version"}}"#);
        assert_eq!(code(&call_raw(&req)), "unsupported_version", "{bad}");
    }
    assert_eq!(code(&call(&json!({"action": "version"}))), "missing_field");
}

#[test]
fn unsupported_version_wins_over_unknown_action_and_fields() {
    let v = call(&json!({"version": 2, "action": "nope", "zzz": 1}));
    assert_eq!(code(&v), "unsupported_version");
}

#[test]
fn action_must_be_a_known_string() {
    assert_eq!(code(&call(&json!({"version": 1}))), "missing_field");
    for bad in [json!(1), json!(null), json!(["version"]), json!({"a": 1})] {
        let v = call(&json!({"version": 1, "action": bad}));
        assert_eq!(code(&v), "invalid_field");
    }
    for bad in [
        "",
        "Version",
        "version ",
        "pair",
        "send_sms",
        "read_clipboard",
        "validate_sms\u{0}",
    ] {
        let v = call(&json!({"version": 1, "action": bad}));
        assert_eq!(code(&v), "unknown_action", "{bad:?}");
    }
}

#[test]
fn unknown_action_wins_over_unknown_fields_and_is_not_echoed() {
    let v = call(&json!({"version": 1, "action": "pair_with_phone-ZZZ", "token": "hunter2"}));
    assert_eq!(code(&v), "unknown_action");
    let out = v.to_string();
    assert!(!out.contains("ZZZ") && !out.contains("hunter2"));
}

#[test]
fn unknown_fields_are_rejected_and_never_echoed() {
    let mut req = sms("+1555", "hi");
    req["pairing_token"] = json!("tok-12345");
    let v = call(&req);
    assert_eq!(code(&v), "unknown_field");
    assert!(!v.to_string().contains("tok-12345"));
    assert!(!v.to_string().contains("pairing_token"));
}

#[test]
fn duplicate_keys_are_rejected_even_when_values_agree() {
    let v = call_raw(r#"{"version":1,"action":"version","action":"version"}"#);
    assert_eq!(code(&v), "invalid_request");
    let v = call_raw(r#"{"version":1,"version":2,"action":"version"}"#);
    assert_eq!(code(&v), "invalid_request");
}

#[test]
fn too_many_fields_rejected() {
    let mut obj = serde_json::Map::new();
    obj.insert("version".into(), json!(1));
    obj.insert("action".into(), json!("version"));
    for i in 0..20 {
        obj.insert(format!("k{i}"), json!(i));
    }
    assert_eq!(code(&call(&Value::Object(obj))), "invalid_request");
}

/// The widest real request (`transport_start` with every option, as the Android service sends it) must reach the
/// field-by-field validation instead of being refused as "too many fields" (it was, at a limit of 8).
#[test]
fn widest_transport_start_request_is_not_refused_as_too_many_fields() {
    let req = json!({
        "version": 1, "action": "transport_start", "state_dir": "/nonexistent-gb-test", "device_name": "p",
        "listen": {"port": 0}, "discovery": {"listen": false, "announce": true}, "allow_loopback": false,
        "clipboard": {}, "calls": {}, "data": {"contacts": true}, "permits": {},
    });
    assert_ne!(code(&call(&req)), "invalid_request");
}

#[test]
fn non_object_documents_are_invalid_requests() {
    for doc in ["[]", "[1]", "1", "\"version\"", "null", "true"] {
        assert_eq!(code(&call_raw(doc)), "invalid_request", "{doc}");
    }
}

#[test]
fn malformed_json_fails_predictably_without_leaking_input() {
    for doc in [
        "",
        "   ",
        "{",
        "}",
        r#"{"version":1,"action":"version""#,
        r#"{"version":1,"action":"version"} trailing"#,
        r#"{"version":1,"action":"version"}{"version":1}"#,
        "{'version':1}",
        r#"{"version":01,"action":"version"}"#,
        r#"{"version":1,"action":"validate_sms","text":"secret-body-unterminated"#,
        "{\"text\":\"bad escape \\q secret-body\"}",
        "\u{feff}{\"version\":1,\"action\":\"version\"}",
    ] {
        let v = call_raw(doc);
        assert_eq!(code(&v), "malformed_json", "{doc:?}");
        assert!(!v.to_string().contains("secret-body"));
    }
}

#[test]
fn deep_nesting_is_rejected_not_a_stack_overflow() {
    let deep = format!(
        r#"{{"version":1,"action":"version","x":{}{}}}"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    let v = call_raw(&deep);
    assert!(
        ["malformed_json", "request_too_large"].contains(&code(&v)),
        "{v}"
    );
    // Shallow enough to parse: still an unknown field, still bounded.
    let shallow = format!(
        r#"{{"version":1,"action":"version","x":{}{}}}"#,
        "[".repeat(50),
        "]".repeat(50)
    );
    assert_eq!(code(&call_raw(&shallow)), "unknown_field");
}

// ------------------------------------------------------------------ size limits

#[test]
fn oversized_request_rejected_before_parsing() {
    let exactly = {
        let prefix = r#"{"version":1,"action":"version","pad":""#;
        let suffix = r#""}"#;
        let pad = MAX_REQUEST_BYTES - prefix.len() - suffix.len();
        format!("{prefix}{}{suffix}", "a".repeat(pad))
    };
    assert_eq!(exactly.len(), MAX_REQUEST_BYTES);
    // At the limit it is parsed (and fails on the unknown field), proving the bound is inclusive.
    assert_eq!(code(&call_raw(&exactly)), "unknown_field");

    let over = format!("{exactly} ");
    assert_eq!(over.len(), MAX_REQUEST_BYTES + 1);
    assert_eq!(code(&call_raw(&over)), "request_too_large");

    // Not even valid JSON, and huge: the size check comes first.
    let garbage = "x".repeat(MAX_REQUEST_BYTES * 4);
    assert_eq!(code(&call_raw(&garbage)), "request_too_large");
}

#[test]
fn oversized_request_bound_is_in_bytes_not_chars() {
    // Multi-byte padding: few characters, too many bytes.
    let over = format!(
        r#"{{"version":1,"action":"version","pad":"{}"}}"#,
        "€".repeat(MAX_REQUEST_BYTES / 3 + 1)
    );
    assert!(over.chars().count() < MAX_REQUEST_BYTES);
    assert_eq!(code(&call_raw(&over)), "request_too_large");
}

#[test]
fn responses_are_small_and_independent_of_input_size() {
    let big = "a".repeat(DEFAULT_MAX_TEXT_BYTES + 1);
    let out = handle_request(&clip(&big).to_string());
    assert!(out.len() < 256, "{}", out.len());
}

// ------------------------------------------------------------------ unicode & escapes

#[test]
fn json_escapes_and_surrogate_pairs_decode() {
    let v =
        call_raw(r#"{"version":1,"action":"validate_sms","recipient":"+1555","text":"café 😀"}"#);
    assert_eq!(ok(&v)["text_chars"], 6);
    // Escaped NUL is still NUL.
    let v = call_raw(r#"{"version":1,"action":"validate_clipboard","text":"a\u0000b"}"#);
    assert_eq!(code(&v), "contains_nul");
    // Escaped key names are the same keys.
    let v = call_raw(r#"{"version":1,"action":"version"}"#);
    ok(&v);
}

#[test]
fn lone_surrogate_escapes_are_malformed_not_replaced() {
    for doc in [
        r#"{"version":1,"action":"validate_clipboard","text":"\ud800"}"#,
        r#"{"version":1,"action":"validate_clipboard","text":"\udc00x"}"#,
    ] {
        assert_eq!(code(&call_raw(doc)), "malformed_json", "{doc}");
    }
}

// ------------------------------------------------------------------ JNI-layer input errors

#[test]
fn input_errors_use_the_same_envelope() {
    let cases = [
        (InputError::Null, "invalid_request"),
        (InputError::Unreadable, "input_unreadable"),
        (InputError::TooLarge, "request_too_large"),
        (InputError::IllFormedText, "malformed_text"),
    ];
    for (e, expected) in cases {
        let v: Value = serde_json::from_str(&input_error_response(e)).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(code(&v), expected);
    }
}

// ------------------------------------------------------------------ no panics

#[test]
fn hostile_inputs_never_panic_and_always_yield_contract_json() {
    let mut corpus: Vec<String> = vec![
        "\0".into(),
        "{\"\0\":1}".into(),
        "{\"version\":1,\"action\":\"\u{0}\"}".into(),
        "\u{10ffff}".repeat(1000),
        "{".repeat(10_000),
        "\"".repeat(10_001),
        r#"{"version":1e999,"action":"version"}"#.into(),
        r#"{"version":-0,"action":"version"}"#.into(),
        r#"{"version":1,"action":"validate_sms","recipient":"+1","text":"x","subscription_id":1e400}"#.into(),
        r#"{"version":1,"action":"validate_sms","recipient":"+1","text":"x","subscription_id":-9223372036854775809}"#.into(),
    ];
    // Every truncation of a valid request, and every single-byte mutation of it.
    let valid = sms("+15551234567", "héllo 😀").to_string();
    for i in 0..=valid.len() {
        if valid.is_char_boundary(i) {
            corpus.push(valid[..i].to_owned());
        }
    }
    for i in 0..valid.len() {
        for &b in b"\"\\{},:0\n" {
            let mut bytes = valid.clone().into_bytes();
            bytes[i] = b;
            if let Ok(s) = String::from_utf8(bytes) {
                corpus.push(s);
            }
        }
    }
    for doc in corpus {
        let out = handle_request(&doc);
        let v: Value = serde_json::from_str(&out).expect("always JSON");
        assert_eq!(v["version"], 1);
        assert!(v["ok"].is_boolean());
        assert!(out.len() < 512);
    }
}
