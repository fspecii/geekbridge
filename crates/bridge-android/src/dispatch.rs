// SPDX-License-Identifier: GPL-3.0-or-later
//! Request parsing, action dispatch and response encoding. Pure and JVM-free so host tests cover it.
use bridge_core::{
    clipboard::{
        ClipboardCapabilities, ClipboardPolicy, ClipboardSync, DEFAULT_MAX_TEXT_BYTES,
        LocalObservation, ObserveMode, ReadAccess, ReadContext, SendDecision, SendMode,
        Sensitivity, SkipReason, TextError,
    },
    messages::{MAX_TEXT_CHARS, OutgoingTextIntent, SimSelection, SubscriptionId, ValidationError},
};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Map, Number, Value, json};
use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::OnceLock,
};

use crate::transport::{self, Host};

/// The only request/response schema version this library speaks.
pub const REQUEST_VERSION: u64 = 1;
/// Largest accepted request in UTF-8 bytes: the worst-case JSON encoding (`\u00XX`, 6 bytes per
/// byte) of a clipboard item at the shared default limit, plus envelope. Equals
/// `bridge_core::clipboard::max_wire_bytes(DEFAULT_MAX_TEXT_BYTES)` (asserted in tests).
pub const MAX_REQUEST_BYTES: usize = 6 * DEFAULT_MAX_TEXT_BYTES + 1024;
/// `transport_data_send` carries up to 1 MiB of phone data (contact cards, calendar, messages) plus
/// the envelope; every other request keeps [`MAX_REQUEST_BYTES`].
pub const MAX_DATA_REQUEST_BYTES: usize = 1024 * 1024 + 2048;
/// Requests carry a handful of keys; more is never legitimate. `transport_start` is the widest: version, action,
/// state_dir, device_name, listen, discovery, allow_loopback, clipboard, calls, data and permits (11 keys).
const MAX_FIELDS: usize = 16;

/// The stateless actions. `transport::ACTIONS` lists the stateful ones.
const ACTIONS: [&str; 3] = ["version", "validate_sms", "validate_clipboard"];
/// Transport requests are small; anything larger is refused after the action is known.
pub(crate) const MAX_TRANSPORT_REQUEST_BYTES: usize = 16 * 1024;

/// Fixed response used if encoding or a panic guard fails. Must stay valid contract JSON.
const INTERNAL_ERROR_JSON: &str =
    r#"{"version":1,"ok":false,"error":{"code":"internal_error","message":"internal error"}}"#;

/// Error code plus a static message. Messages never interpolate request data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApiError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}
pub(crate) const fn err(code: &'static str, message: &'static str) -> ApiError {
    ApiError { code, message }
}

/// Failures the JNI layer detects before a request string exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// Kotlin passed `null` despite the non-null signature.
    Null,
    /// The JVM could not hand over the string contents.
    Unreadable,
    /// The string is larger than any acceptable request; it was not decoded.
    TooLarge,
    /// The string holds an unpaired UTF-16 surrogate, which has no UTF-8 form.
    IllFormedText,
}

/// Encoded error response for an input the JNI layer refused to hand to [`handle_request`].
pub fn input_error_response(error: InputError) -> String {
    let e = match error {
        InputError::Null => err("invalid_request", "request is missing"),
        InputError::Unreadable => err("input_unreadable", "request could not be read"),
        InputError::TooLarge => request_too_large(),
        InputError::IllFormedText => err("malformed_text", "request text is not well-formed"),
    };
    encode_error(e)
}

fn request_too_large() -> ApiError {
    err("request_too_large", "request exceeds the size limit")
}

/// One request handler plus the (initially stopped) phone transport it controls.
///
/// [`handle_request`] uses a single process-wide instance, which is what the JNI export calls.
/// Tests create their own instances so they do not share transport state. Creating a dispatcher
/// starts nothing: no thread, socket or file is touched until a `transport_start` request.
/// Dropping a dispatcher stops its transport.
#[derive(Default)]
pub struct Dispatcher {
    transport: Host,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Same contract as [`handle_request`].
    pub fn handle(&self, request: &str) -> String {
        guarded(|| respond(self, request))
    }
}

fn global() -> &'static Dispatcher {
    static GLOBAL: OnceLock<Dispatcher> = OnceLock::new();
    GLOBAL.get_or_init(Dispatcher::new)
}

/// Handle one request and always return a response document. Never panics: a panic anywhere below
/// is caught and reported as `internal_error` without any request data.
pub fn handle_request(request: &str) -> String {
    global().handle(request)
}

fn guarded(f: impl FnOnce() -> String) -> String {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(response) => response,
        Err(payload) => {
            // Dropping an arbitrary payload may itself panic.
            let _ = catch_unwind(AssertUnwindSafe(|| drop(payload)));
            INTERNAL_ERROR_JSON.to_owned()
        }
    }
}

fn respond(dispatcher: &Dispatcher, request: &str) -> String {
    match dispatch(dispatcher, request) {
        Ok(result) => encode(&json!({ "version": REQUEST_VERSION, "ok": true, "result": result })),
        Err(e) => encode_error(e),
    }
}

fn encode_error(e: ApiError) -> String {
    encode(&json!({
        "version": REQUEST_VERSION,
        "ok": false,
        "error": { "code": e.code, "message": e.message },
    }))
}

fn encode(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| INTERNAL_ERROR_JSON.to_owned())
}

// ------------------------------------------------------------------------------- parsing

/// A JSON value in which a repeated object key is an error at any depth (serde_json's own `Value`
/// keeps the last duplicate silently), so nested option objects get the same guarantee as the
/// envelope.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(Value::Bool(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Strict, E> {
                Ok(Strict(Value::from(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Strict, E> {
                Ok(Strict(Value::from(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Strict, E> {
                Ok(Strict(
                    Number::from_f64(v).map_or(Value::Null, Value::Number),
                ))
            }
            fn visit_str<E>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(Value::String(v.to_owned())))
            }
            fn visit_string<E>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(Value::String(v)))
            }
            fn visit_unit<E>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_none<E>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
                let mut out = Vec::new();
                while let Some(Strict(v)) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Strict(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
                let mut out = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if out.contains_key(&key) {
                        return Err(de::Error::custom("duplicate field"));
                    }
                    let Strict(v) = map.next_value()?;
                    out.insert(key, v);
                }
                Ok(Strict(Value::Object(out)))
            }
        }
        d.deserialize_any(V)
    }
}

/// Top-level object as ordered pairs. Unlike `serde_json::Map`, a repeated key is an error rather
/// than "last one wins", so a request cannot present two different values to different readers.
pub(crate) struct Fields(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Fields;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a request object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
                let mut out: Vec<(String, Value)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if out.len() >= MAX_FIELDS {
                        return Err(de::Error::custom("too many fields"));
                    }
                    if out.iter().any(|(k, _)| *k == key) {
                        return Err(de::Error::custom("duplicate field"));
                    }
                    let Strict(value) = map.next_value::<Strict>()?;
                    out.push((key, value));
                }
                Ok(Fields(out))
            }
        }
        d.deserialize_map(V)
    }
}

fn wrong_type() -> ApiError {
    err("invalid_field", "a field has the wrong type")
}

impl Fields {
    fn take(&mut self, key: &str) -> Option<Value> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.swap_remove(index).1)
    }
    pub(crate) fn take_string(&mut self, key: &'static str) -> Result<String, ApiError> {
        match self.take(key) {
            None => Err(err("missing_field", "a required field is missing")),
            Some(Value::String(s)) => Ok(s),
            Some(_) => Err(wrong_type()),
        }
    }
    /// `None` only when the key is absent; an explicit `null` is `invalid_field`.
    pub(crate) fn take_opt_string(
        &mut self,
        key: &'static str,
    ) -> Result<Option<String>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(wrong_type()),
        }
    }
    pub(crate) fn take_u64(&mut self, key: &'static str) -> Result<u64, ApiError> {
        match self.take(key) {
            None => Err(err("missing_field", "a required field is missing")),
            Some(Value::Number(n)) => n.as_u64().ok_or_else(wrong_type),
            Some(_) => Err(wrong_type()),
        }
    }
    pub(crate) fn take_opt_u64(&mut self, key: &'static str) -> Result<Option<u64>, ApiError> {
        if self.0.iter().any(|(k, _)| k == key) {
            self.take_u64(key).map(Some)
        } else {
            Ok(None)
        }
    }
    pub(crate) fn take_bool(&mut self, key: &'static str) -> Result<bool, ApiError> {
        match self.take(key) {
            None => Err(err("missing_field", "a required field is missing")),
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => Err(wrong_type()),
        }
    }
    pub(crate) fn take_opt_bool(&mut self, key: &'static str) -> Result<Option<bool>, ApiError> {
        if self.0.iter().any(|(k, _)| k == key) {
            self.take_bool(key).map(Some)
        } else {
            Ok(None)
        }
    }
    pub(crate) fn take_object(
        &mut self,
        key: &'static str,
    ) -> Result<serde_json::Map<String, Value>, ApiError> {
        match self.take(key) {
            None => Err(err("missing_field", "a required field is missing")),
            Some(Value::Object(map)) => Ok(map),
            Some(_) => Err(wrong_type()),
        }
    }
    /// An optional nested option object, with the same strictness as the envelope.
    pub(crate) fn take_opt_object(
        &mut self,
        key: &'static str,
    ) -> Result<Option<Fields>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Object(map)) => Ok(Some(Fields(map.into_iter().collect()))),
            Some(_) => Err(wrong_type()),
        }
    }
    /// An optional array of arbitrary values.
    pub(crate) fn take_opt_array(
        &mut self,
        key: &'static str,
    ) -> Result<Option<Vec<Value>>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Array(v)) => Ok(Some(v)),
            Some(_) => Err(wrong_type()),
        }
    }
    /// Call after taking every known field.
    pub(crate) fn finish(self) -> Result<(), ApiError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(err("unknown_field", "request contains an unknown field"))
        }
    }
}

fn dispatch(dispatcher: &Dispatcher, request: &str) -> Result<Value, ApiError> {
    if request.len() > MAX_DATA_REQUEST_BYTES {
        return Err(request_too_large());
    }
    let mut fields: Fields = serde_json::from_str(request).map_err(|e| {
        if e.is_data() {
            err(
                "invalid_request",
                "request must be a JSON object without duplicate keys",
            )
        } else {
            err("malformed_json", "request is not valid JSON")
        }
    })?;

    // Version first: an unsupported version says nothing reliable about the rest of the schema.
    match fields.take("version") {
        None => return Err(err("missing_field", "a required field is missing")),
        Some(v) if v.as_u64() == Some(REQUEST_VERSION) => {}
        Some(_) => return Err(err("unsupported_version", "unsupported request version")),
    }
    let action = fields.take_string("action")?;
    if action != "transport_data_send" && request.len() > MAX_REQUEST_BYTES {
        return Err(request_too_large());
    }
    match action.as_str() {
        "version" => {
            fields.finish()?;
            Ok(version_result())
        }
        "validate_sms" => validate_sms(fields),
        "validate_clipboard" => validate_clipboard(fields),
        a if transport::ACTIONS.contains(&a) => {
            // Clipboard text and HTML legitimately exceed the 16 KiB control-message limit; the
            // overall request limit and the clipboard size checks still apply.
            if !matches!(a, "transport_clipboard_send" | "transport_data_send")
                && request.len() > MAX_TRANSPORT_REQUEST_BYTES
            {
                return Err(request_too_large());
            }
            transport::handle(&dispatcher.transport, a, fields)
        }
        _ => Err(err("unknown_action", "unknown action")),
    }
}

// ------------------------------------------------------------------------------- actions

fn version_result() -> Value {
    json!({
        // Version of this workspace's Rust code; `bridge-core` shares the workspace version.
        "core_version": env!("CARGO_PKG_VERSION"),
        "request_versions": [REQUEST_VERSION],
        "actions": ACTIONS,
        // Stateful actions that do nothing until `transport_start`; see docs/android-transport.md.
        "transport_actions": transport::ACTIONS,
        "limits": {
            "max_request_bytes": MAX_REQUEST_BYTES,
            "max_transport_request_bytes": MAX_TRANSPORT_REQUEST_BYTES,
            "max_sms_chars": MAX_TEXT_CHARS,
            "max_clipboard_bytes": DEFAULT_MAX_TEXT_BYTES,
        },
        // True of `actions`: they validate and never act. `transport_actions` are the opposite and
        // are inert until an explicit `transport_start`.
        "validation_only": true,
    })
}

fn validate_sms(mut fields: Fields) -> Result<Value, ApiError> {
    let recipient = fields.take_string("recipient")?;
    let text = fields.take_string("text")?;
    let subscription = fields.take("subscription_id");
    fields.finish()?;

    let sim = match subscription {
        None => SimSelection::PhoneDefault,
        Some(Value::Number(n)) => {
            let Some(id) = n.as_i64() else {
                return Err(wrong_type());
            };
            let id = i32::try_from(id).unwrap_or(0); // out of range is as invalid as 0
            let id = SubscriptionId::new(id).map_err(sms_error)?;
            SimSelection::Subscription(id)
        }
        Some(_) => return Err(err("invalid_field", "a field has the wrong type")),
    };
    let intent = OutgoingTextIntent::new(&[recipient.as_str()], text, sim).map_err(sms_error)?;
    Ok(json!({
        "valid": true,
        "text_chars": intent.text().chars().count(),
        "has_subscription": matches!(intent.sim(), SimSelection::Subscription(_)),
    }))
}

fn sms_error(e: ValidationError) -> ApiError {
    match e {
        ValidationError::EmptyText => err("empty_text", "text is empty"),
        ValidationError::BlankText => err("blank_text", "text is only whitespace"),
        ValidationError::TextTooLong { .. } => {
            err("text_too_long", "text exceeds the length limit")
        }
        ValidationError::ForbiddenControlChar => err(
            "forbidden_control_char",
            "text contains a forbidden control character",
        ),
        ValidationError::EmptyAddress
        | ValidationError::AddressTooLong
        | ValidationError::AddressControlChar
        | ValidationError::NoRecipient
        | ValidationError::GroupNeedsMms { .. } => {
            err("invalid_recipient", "recipient is not acceptable")
        }
        ValidationError::InvalidSubscription(_) => {
            err("invalid_subscription", "subscription id is not acceptable")
        }
        _ => err("validation_failed", "input failed validation"),
    }
}

fn validate_clipboard(mut fields: Fields) -> Result<Value, ApiError> {
    let text = fields.take_string("text")?;
    fields.finish()?;

    // The empty/NUL rule is private to bridge-core, reachable only through the reducer. Drive a
    // throwaway manual-send reducer so the shared rule is the single source of truth. It performs
    // no I/O, keeps nothing after this call, and the decision's message is dropped unsent.
    let caps = ClipboardCapabilities {
        read: ReadAccess::UserGesture,
        write: false,
        observe: ObserveMode::None,
        reports_sensitivity: false,
        can_mark_written_sensitive: false,
    };
    let policy = ClipboardPolicy {
        send: SendMode::Manual,
        ..ClipboardPolicy::default()
    };
    let mut sync = ClipboardSync::new("local", "peer", policy, caps).map_err(|_| internal())?;
    let observation = LocalObservation::from_bytes(
        text.as_bytes(),
        DEFAULT_MAX_TEXT_BYTES,
        Sensitivity::Normal,
        ReadContext::UserGesture,
    )
    .map_err(clipboard_error)?;
    match sync.user_send(observation, 0, || "validate".to_owned()) {
        SendDecision::Send(_) => Ok(json!({
            "valid": true,
            "text_bytes": text.len(),
            "max_text_bytes": DEFAULT_MAX_TEXT_BYTES,
        })),
        SendDecision::Skip(SkipReason::Invalid(e)) => Err(clipboard_error(e)),
        SendDecision::Skip(_) => Err(internal()),
    }
}

fn clipboard_error(e: TextError) -> ApiError {
    match e {
        TextError::Empty => err("empty_text", "text is empty"),
        TextError::TooLarge => err("text_too_large", "text exceeds the size limit"),
        TextError::NotUtf8 => err("invalid_utf8", "text is not valid UTF-8"),
        TextError::ContainsNul => err("contains_nul", "text contains a NUL character"),
    }
}

fn internal() -> ApiError {
    err("internal_error", "internal error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_limit_matches_shared_clipboard_wire_bound() {
        assert_eq!(
            MAX_REQUEST_BYTES,
            bridge_core::clipboard::max_wire_bytes(DEFAULT_MAX_TEXT_BYTES)
        );
    }

    #[test]
    fn panic_is_caught_and_reports_no_payload() {
        let out = guarded(|| panic!("secret-payload-+15551234567"));
        assert_eq!(out, INTERNAL_ERROR_JSON);
        assert!(!out.contains("secret"));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["version"], 1);
    }

    #[test]
    fn panicking_payload_drop_is_also_contained() {
        struct Bomb;
        impl Drop for Bomb {
            fn drop(&mut self) {
                panic!("drop bomb");
            }
        }
        let out = guarded(|| std::panic::panic_any(Bomb));
        assert_eq!(out, INTERNAL_ERROR_JSON);
    }

    #[test]
    fn fixed_internal_error_is_valid_contract_json() {
        let v: Value = serde_json::from_str(INTERNAL_ERROR_JSON).unwrap();
        assert_eq!(v["error"]["code"], "internal_error");
    }
}
