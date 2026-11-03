// SPDX-License-Identifier: GPL-3.0-or-later
//! Recent photos from a paired phone: a short list and small thumbnails, on request only.
//!
//! GeekBridge-only packets (a stock KDE Connect peer neither declares nor sees them). The design is in
//! `docs/design/recent-photos.md`.
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`REQUEST`] | computer to phone | `op` = `list` (+ `limit`), `thumb` (+ `id`) or `get` (+ `id`) |
//! | [`LIST`] | phone to computer | `items` (at most [`MAX_ITEMS`]): `id`, `name`, `taken` (ms), `size`, `w`, `h`; `partial` |
//! | [`THUMB`] | phone to computer | `id`, `w`, `h`, `jpeg` (base64, at most [`MAX_THUMB_BYTES`] decoded); or `id` + `gone` |
//!
//! * Ids are opaque to the computer (`[A-Za-z0-9_-]`, at most [`MAX_ID`]); the phone made them up and will only
//!   honour one it listed recently, so the computer cannot ask for an arbitrary file.
//! * A thumbnail is a small JPEG inside the packet. A full image never travels in these packets: `get` makes the
//!   phone send the file over the ordinary share channel (payload socket, pinned certificate), which this
//!   computer accepts only for a photo it asked for.
//! * `partial` is true when the phone lets the app see only the photos the user picked (Android 14 partial access).
//!
//! Parsing is strict about shape and bounded in everything: a list longer than the cap, a repeated id, a thumbnail that
//! is not a JPEG or is too large, or an absurd dimension is an error. Text from the phone is cut to its cap with control
//! characters replaced, never trusted for a path (a saved file gets its name from the share channel's own filename rules).
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};

pub const REQUEST: &str = "geekbridge.photos.request";
pub const LIST: &str = "geekbridge.photos.list";
pub const THUMB: &str = "geekbridge.photos.thumb";

pub fn is_photos_kind(kind: &str) -> bool {
    matches!(kind, REQUEST | LIST | THUMB)
}

/// Most photos in one list, and the most the computer ever asks for.
pub const MAX_ITEMS: usize = 24;
pub const MAX_ID: usize = 32;
pub const MAX_NAME: usize = 100;
/// Largest thumbnail, decoded from base64.
pub const MAX_THUMB_BYTES: usize = 24 * 1024;
/// Longest edge of a thumbnail in pixels.
pub const MAX_THUMB_EDGE: u32 = 256;
/// Largest picture dimension or size the list may claim (sanity bound, not a feature).
pub const MAX_DIMENSION: u32 = 100_000;
pub const MAX_PHOTO_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PhotosError {
    Malformed(&'static str),
}

/// What the computer asks of the phone.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    List { limit: usize },
    Thumb { id: String },
    Get { id: String },
}

/// One listed photo. `name` is display text only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Photo {
    pub id: String,
    pub name: String,
    pub taken_ms: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct List {
    pub items: Vec<Photo>,
    /// The phone shows this app only the photos the user picked.
    pub partial: bool,
}

/// A decoded thumbnail answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Thumb {
    Image {
        id: String,
        width: u32,
        height: u32,
        jpeg: Vec<u8>,
    },
    /// The phone no longer has (or no longer shows) that photo.
    Gone { id: String },
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Characters that reorder surrounding text (bidirectional overrides, embeddings, isolates and marks).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Control characters become spaces, bidirectional controls are dropped (a file name must not be able to reorder the
/// text around it), the text is trimmed and cut to `max` characters.
pub fn clean_text(s: &str, max: usize) -> String {
    let replaced: String = s
        .chars()
        .filter(|c| !is_bidi_control(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    replaced.trim().chars().take(max).collect()
}

pub fn looks_like_jpeg(b: &[u8]) -> bool {
    b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
}

fn uint(body: &Map<String, Value>, key: &'static str, max: u64) -> Result<u64, PhotosError> {
    match body.get(key).and_then(Value::as_u64) {
        Some(n) if n <= max => Ok(n),
        _ => Err(PhotosError::Malformed(key)),
    }
}

fn id_of(body: &Map<String, Value>) -> Result<String, PhotosError> {
    match body.get("id") {
        Some(Value::String(s)) if valid_id(s) => Ok(s.clone()),
        _ => Err(PhotosError::Malformed("id")),
    }
}

fn object(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else { unreachable!() };
    m
}

/// Decode a [`REQUEST`] body.
pub fn parse_request(body: &Map<String, Value>) -> Result<Ask, PhotosError> {
    match body.get("op").and_then(Value::as_str) {
        Some("list") => {
            let limit = uint(body, "limit", MAX_ITEMS as u64)?;
            if limit == 0 {
                return Err(PhotosError::Malformed("limit"));
            }
            Ok(Ask::List {
                limit: limit as usize,
            })
        }
        Some("thumb") => Ok(Ask::Thumb { id: id_of(body)? }),
        Some("get") => Ok(Ask::Get { id: id_of(body)? }),
        _ => Err(PhotosError::Malformed("op")),
    }
}

pub fn list_request_body(limit: usize) -> Map<String, Value> {
    object(json!({ "op": "list", "limit": limit.clamp(1, MAX_ITEMS) }))
}

pub fn thumb_request_body(id: &str) -> Map<String, Value> {
    object(json!({ "op": "thumb", "id": id }))
}

pub fn get_request_body(id: &str) -> Map<String, Value> {
    object(json!({ "op": "get", "id": id }))
}

/// Decode a [`LIST`] body.
pub fn parse_list(body: &Map<String, Value>) -> Result<List, PhotosError> {
    let Some(Value::Array(items)) = body.get("items") else {
        return Err(PhotosError::Malformed("items"));
    };
    if items.len() > MAX_ITEMS {
        return Err(PhotosError::Malformed("items"));
    }
    let partial = match body.get("partial") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        _ => return Err(PhotosError::Malformed("partial")),
    };
    let mut out = Vec::with_capacity(items.len());
    for v in items {
        let Value::Object(o) = v else {
            return Err(PhotosError::Malformed("item"));
        };
        let id = id_of(o)?;
        if out.iter().any(|p: &Photo| p.id == id) {
            return Err(PhotosError::Malformed("id"));
        }
        let name = match o.get("name") {
            Some(Value::String(s)) => clean_text(s, MAX_NAME),
            _ => return Err(PhotosError::Malformed("name")),
        };
        out.push(Photo {
            id,
            name,
            taken_ms: uint(o, "taken", u64::MAX >> 12)?,
            size: uint(o, "size", MAX_PHOTO_BYTES)?,
            width: uint(o, "w", u64::from(MAX_DIMENSION))? as u32,
            height: uint(o, "h", u64::from(MAX_DIMENSION))? as u32,
        });
    }
    Ok(List {
        items: out,
        partial,
    })
}

/// Body of a [`LIST`] (the phone side; used by the simulated phone and tests).
pub fn list_body(list: &List) -> Map<String, Value> {
    let items: Vec<Value> = list
        .items
        .iter()
        .take(MAX_ITEMS)
        .map(|p| {
            json!({
                "id": p.id, "name": p.name, "taken": p.taken_ms,
                "size": p.size, "w": p.width, "h": p.height,
            })
        })
        .collect();
    object(json!({ "items": items, "partial": list.partial }))
}

/// Decode a [`THUMB`] body.
pub fn parse_thumb(body: &Map<String, Value>) -> Result<Thumb, PhotosError> {
    let id = id_of(body)?;
    match body.get("gone") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(true)) => return Ok(Thumb::Gone { id }),
        _ => return Err(PhotosError::Malformed("gone")),
    }
    let width = uint(body, "w", u64::from(MAX_THUMB_EDGE))? as u32;
    let height = uint(body, "h", u64::from(MAX_THUMB_EDGE))? as u32;
    if width == 0 || height == 0 {
        return Err(PhotosError::Malformed("w"));
    }
    let Some(Value::String(text)) = body.get("jpeg") else {
        return Err(PhotosError::Malformed("jpeg"));
    };
    // Refuse before decoding: base64 is 4 characters per 3 bytes.
    if text.len() > MAX_THUMB_BYTES.div_ceil(3) * 4 {
        return Err(PhotosError::Malformed("jpeg"));
    }
    let jpeg = STANDARD
        .decode(text)
        .map_err(|_| PhotosError::Malformed("jpeg"))?;
    if jpeg.len() > MAX_THUMB_BYTES || !looks_like_jpeg(&jpeg) {
        return Err(PhotosError::Malformed("jpeg"));
    }
    Ok(Thumb::Image {
        id,
        width,
        height,
        jpeg,
    })
}

/// Body of a [`THUMB`] carrying an image (the phone side).
pub fn thumb_body(id: &str, width: u32, height: u32, jpeg: &[u8]) -> Map<String, Value> {
    object(json!({ "id": id, "w": width, "h": height, "jpeg": STANDARD.encode(jpeg) }))
}

/// Body of a [`THUMB`] saying the photo is no longer available.
pub fn thumb_gone_body(id: &str) -> Map<String, Value> {
    object(json!({ "id": id, "gone": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn photo(n: usize) -> Photo {
        Photo {
            id: format!("p{n}"),
            name: format!("IMG_{n:04}.jpg"),
            taken_ms: 1_700_000_000_000 + n as u64,
            size: 2_500_000,
            width: 4000,
            height: 3000,
        }
    }

    const JPEG: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0xD9];

    #[test]
    fn a_list_round_trips() {
        let l = List {
            items: (1..=3).map(photo).collect(),
            partial: true,
        };
        assert_eq!(parse_list(&list_body(&l)), Ok(l));
    }

    #[test]
    fn requests_round_trip_and_are_strict() {
        assert_eq!(
            parse_request(&list_request_body(12)),
            Ok(Ask::List { limit: 12 })
        );
        assert_eq!(
            parse_request(&list_request_body(500)),
            Ok(Ask::List { limit: MAX_ITEMS }),
            "the builder clamps"
        );
        assert_eq!(
            parse_request(&thumb_request_body("p1")),
            Ok(Ask::Thumb { id: "p1".into() })
        );
        assert_eq!(
            parse_request(&get_request_body("p-1_x")),
            Ok(Ask::Get { id: "p-1_x".into() })
        );
        for bad in [
            json!({}),
            json!({"op": "list"}),
            json!({"op": "list", "limit": 0}),
            json!({"op": "list", "limit": 25}),
            json!({"op": "list", "limit": -1}),
            json!({"op": "get"}),
            json!({"op": "get", "id": "../x"}),
            json!({"op": "get", "id": ""}),
            json!({"op": "get", "id": "a".repeat(33)}),
            json!({"op": "delete", "id": "p1"}),
            json!({"op": 3}),
        ] {
            assert!(parse_request(&obj(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_list_is_bounded_and_strict() {
        let too_many = List {
            items: (1..=MAX_ITEMS + 1).map(photo).collect(),
            partial: false,
        };
        let mut b = list_body(&too_many);
        // The builder cuts at the cap; build an over-long one by hand.
        assert_eq!(b["items"].as_array().unwrap().len(), MAX_ITEMS);
        let one = b["items"][0].clone();
        b.get_mut("items")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "zz", "name": "x", "taken": 1, "size": 1, "w": 1, "h": 1}));
        assert!(parse_list(&b).is_err(), "more than the cap");
        // Repeated id.
        let dup = obj(json!({"items": [one.clone(), one]}));
        assert!(parse_list(&dup).is_err());
        for item in [
            json!({"id": "p1", "name": "a", "taken": 1, "size": 1, "w": 100001, "h": 1}),
            json!({"id": "p1", "name": "a", "taken": -1, "size": 1, "w": 1, "h": 1}),
            json!({"id": "p1", "name": "a", "taken": 1, "size": 1u64 << 40, "w": 1, "h": 1}),
            json!({"id": "p 1", "name": "a", "taken": 1, "size": 1, "w": 1, "h": 1}),
            json!({"id": "p1", "name": 7, "taken": 1, "size": 1, "w": 1, "h": 1}),
            json!({"id": "p1", "name": "a", "taken": 1.5, "size": 1, "w": 1, "h": 1}),
            json!("p1"),
        ] {
            assert!(
                parse_list(&obj(json!({ "items": [item.clone()] }))).is_err(),
                "{item}"
            );
        }
        assert!(parse_list(&obj(json!({"items": [], "partial": 1}))).is_err());
        assert!(parse_list(&obj(json!({"items": "x"}))).is_err());
        assert_eq!(parse_list(&obj(json!({"items": []}))), Ok(List::default()));
    }

    #[test]
    fn photo_names_are_clean_and_cut() {
        let b = obj(json!({"items": [{
            "id": "p1", "name": format!("  a\u{0}b\n\u{202E}{}  ", "x".repeat(300)),
            "taken": 1, "size": 1, "w": 1, "h": 1,
        }]}));
        let l = parse_list(&b).unwrap();
        let n = &l.items[0].name;
        assert!(n.chars().count() <= MAX_NAME);
        assert!(!n.chars().any(|c| c.is_control() || c == '\u{202E}'));
        assert!(n.starts_with("a b x"));
    }

    #[test]
    fn a_thumbnail_round_trips_and_is_checked() {
        let b = thumb_body("p1", 160, 120, &JPEG);
        assert_eq!(
            parse_thumb(&b),
            Ok(Thumb::Image {
                id: "p1".into(),
                width: 160,
                height: 120,
                jpeg: JPEG.to_vec()
            })
        );
        assert_eq!(
            parse_thumb(&thumb_gone_body("p9")),
            Ok(Thumb::Gone { id: "p9".into() })
        );
        let big = vec![0xFF; MAX_THUMB_BYTES + 1];
        let mut jpeg_big = vec![0xFF, 0xD8, 0xFF];
        jpeg_big.extend(big);
        for bad in [
            thumb_body("p1", 160, 120, b"GIF89a-not-a-jpeg"),
            thumb_body("p1", 160, 120, &jpeg_big),
            thumb_body("p1", 0, 120, &JPEG),
            thumb_body("p1", 160, 257, &JPEG),
            thumb_body("../", 160, 120, &JPEG),
            obj(json!({"id": "p1", "w": 1, "h": 1, "jpeg": "!!!not base64"})),
            obj(json!({"id": "p1", "w": 1, "h": 1})),
            obj(json!({"id": "p1", "gone": "yes"})),
        ] {
            assert!(parse_thumb(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_thumbnail_exactly_at_the_cap_is_accepted() {
        let mut j = vec![0xFF, 0xD8, 0xFF];
        j.resize(MAX_THUMB_BYTES, 0x11);
        assert!(matches!(
            parse_thumb(&thumb_body("p1", 10, 10, &j)),
            Ok(Thumb::Image { .. })
        ));
    }
}
