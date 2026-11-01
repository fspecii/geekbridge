//! Stock KDE Connect clipboard packets, shared by the desktop service and the phone transport.
//!
//! Pure functions over the JSON body of a packet; this crate does not depend on the transport.
//! `kdeconnect.clipboard` carries `{content}`; `kdeconnect.clipboard.connect` carries
//! `{content, timestamp}` (epoch milliseconds, 0 = unknown) and is sent once when a link comes up.
//! See `docs/architecture.md` for what stock peers do with them.
use serde_json::{Map, Value};

pub const CLIPBOARD: &str = "kdeconnect.clipboard";
pub const CLIPBOARD_CONNECT: &str = "kdeconnect.clipboard.connect";

pub fn is_clipboard_kind(kind: &str) -> bool {
    kind == CLIPBOARD || kind == CLIPBOARD_CONNECT
}

pub fn update_body(content: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("content".into(), Value::String(content.to_owned()));
    body
}

pub fn connect_body(content: &str, timestamp_ms: i64) -> Map<String, Value> {
    let mut body = update_body(content);
    body.insert("timestamp".into(), Value::from(timestamp_ms));
    body
}

#[derive(Clone, PartialEq, Eq)]
pub enum RemoteClip {
    /// A live copy on the peer.
    Update { content: String },
    /// State the peer announced when the link came up. The caller applies
    /// [`crate::clipboard::kde_connect_should_apply`] to `timestamp_ms`.
    Connect { content: String, timestamp_ms: i64 },
}

impl std::fmt::Debug for RemoteClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Update { content } => write!(f, "Update(<redacted {} bytes>)", content.len()),
            Self::Connect {
                content,
                timestamp_ms,
            } => write!(
                f,
                "Connect(<redacted {} bytes>, {timestamp_ms})",
                content.len()
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    NotClipboard,
    MissingContent,
    BadTimestamp,
    TooLarge,
    BadRequestId,
    UnsupportedType,
}

/// Parse a received packet body. `content` longer than `max_text_bytes` is refused here, before any
/// copy is made.
pub fn parse(
    kind: &str,
    body: &Map<String, Value>,
    max_text_bytes: usize,
) -> Result<RemoteClip, WireError> {
    if !is_clipboard_kind(kind) {
        return Err(WireError::NotClipboard);
    }
    let content = body
        .get("content")
        .and_then(Value::as_str)
        .ok_or(WireError::MissingContent)?;
    if content.len() > max_text_bytes {
        return Err(WireError::TooLarge);
    }
    if kind == CLIPBOARD {
        return Ok(RemoteClip::Update {
            content: content.to_owned(),
        });
    }
    let timestamp_ms = body
        .get("timestamp")
        .and_then(Value::as_i64)
        .ok_or(WireError::BadTimestamp)?;
    Ok(RemoteClip::Connect {
        content: content.to_owned(),
        timestamp_ms,
    })
}

// ---------------------------------------------------------------------------- pull protocol
//
// GeekBridge-to-GeekBridge packets (stock peers ignore them; they are only sent when the peer
// advertised them). The phone never pushes its clipboard on its own: the computer *pulls*, and the
// phone answers only with an item copied within the freshness window while it is unlocked.
//
//   computer -> phone   geekbridge.clipboard.pull   {request_id, max_age_ms}
//   phone -> computer   geekbridge.clipboard.item   {request_id?, kind, text?, html?, mime?, size?, age_ms}
//                       (images carry the bytes as a KDE payload; `request_id` is absent when the user
//                        explicitly sent the item from the phone)
//   phone -> computer   geekbridge.clipboard.none   {request_id, reason}
//   computer -> phone   geekbridge.clipboard.item   same shape, pushed when the computer clipboard changes

pub const PULL: &str = "geekbridge.clipboard.pull";
pub const ITEM: &str = "geekbridge.clipboard.item";
pub const NONE: &str = "geekbridge.clipboard.none";

pub fn is_pull_protocol_kind(kind: &str) -> bool {
    kind == PULL || kind == ITEM || kind == NONE
}

/// Default and permitted range of the freshness window.
pub const DEFAULT_WINDOW_MS: u64 = 30_000;
pub const MIN_WINDOW_MS: u64 = 5_000;
pub const MAX_WINDOW_MS: u64 = 10 * 60 * 1000;
/// Images (PNG/JPEG) are streamed, never inline.
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const HARD_MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_REQUEST_ID_BYTES: usize = 64;
/// HTML travels inline next to the plain text; same bound as plain text.
pub const MAX_HTML_BYTES: usize = crate::clipboard::DEFAULT_MAX_TEXT_BYTES;
/// Longest packet line either side must accept for clipboard traffic (two 64 KiB fields, worst-case
/// JSON escaping, envelope).
pub const MAX_ITEM_LINE_BYTES: usize = 1024 * 1024;

pub fn clamp_window_ms(ms: u64) -> u64 {
    ms.clamp(MIN_WINDOW_MS, MAX_WINDOW_MS)
}
pub fn clamp_image_bytes(n: u64) -> u64 {
    n.clamp(64 * 1024, HARD_MAX_IMAGE_BYTES)
}

/// Is an item copied `age_ms` ago still inside `window_ms`?
pub fn within_window(age_ms: u64, window_ms: u64) -> bool {
    age_ms <= clamp_window_ms(window_ms)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Text,
    Url,
    Html,
    Image,
}
impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Url => "url",
            Self::Html => "html",
            Self::Image => "image",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => Self::Text,
            "url" => Self::Url,
            "html" => Self::Html,
            "image" => Self::Image,
            _ => return None,
        })
    }
}

/// Why a peer has no item to give. Fixed vocabulary, no content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoneReason {
    /// The phone is locked; nothing is read or sent while locked.
    Locked,
    /// The latest item is older than the freshness window.
    Expired,
    Empty,
    /// Android lets an app read the clipboard only when it has focus: the user must tap the
    /// notification / tile on the phone.
    NeedsGesture,
    Disabled,
    TooLarge,
    Unsupported,
    Sensitive,
}
impl NoneReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Expired => "expired",
            Self::Empty => "empty",
            Self::NeedsGesture => "needs_gesture",
            Self::Disabled => "disabled",
            Self::TooLarge => "too_large",
            Self::Unsupported => "unsupported",
            Self::Sensitive => "sensitive",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "locked" => Self::Locked,
            "expired" => Self::Expired,
            "empty" => Self::Empty,
            "needs_gesture" => Self::NeedsGesture,
            "disabled" => Self::Disabled,
            "too_large" => Self::TooLarge,
            "unsupported" => Self::Unsupported,
            "sensitive" => Self::Sensitive,
            _ => return None,
        })
    }
}

pub fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REQUEST_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn pull_body(request_id: &str, max_age_ms: u64) -> Map<String, Value> {
    let mut b = Map::new();
    b.insert("request_id".into(), Value::from(request_id));
    b.insert(
        "max_age_ms".into(),
        Value::from(clamp_window_ms(max_age_ms)),
    );
    b
}

pub fn none_body(request_id: &str, reason: NoneReason) -> Map<String, Value> {
    let mut b = Map::new();
    b.insert("request_id".into(), Value::from(request_id));
    b.insert("reason".into(), Value::from(reason.as_str()));
    b
}

/// Inline item (text, URL or HTML with plain-text fallback).
pub fn text_item_body(
    kind: ItemKind,
    text: &str,
    html: Option<&str>,
    request_id: Option<&str>,
    age_ms: u64,
) -> Map<String, Value> {
    let mut b = Map::new();
    b.insert("kind".into(), Value::from(kind.as_str()));
    b.insert("text".into(), Value::from(text));
    if let Some(h) = html {
        b.insert("html".into(), Value::from(h));
    }
    if let Some(r) = request_id {
        b.insert("request_id".into(), Value::from(r));
    }
    b.insert("age_ms".into(), Value::from(age_ms));
    b
}

/// Metadata of an image item; the caller attaches `payloadSize`/`payloadTransferInfo` to the packet.
pub fn image_item_body(
    mime: &str,
    size: u64,
    request_id: Option<&str>,
    age_ms: u64,
) -> Map<String, Value> {
    let mut b = Map::new();
    b.insert("kind".into(), Value::from("image"));
    b.insert("mime".into(), Value::from(mime));
    b.insert("size".into(), Value::from(size));
    if let Some(r) = request_id {
        b.insert("request_id".into(), Value::from(r));
    }
    b.insert("age_ms".into(), Value::from(age_ms));
    b
}

#[derive(Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_text_bytes: usize,
    pub max_image_bytes: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Item {
    Text {
        kind: ItemKind,
        text: String,
        html: Option<String>,
    },
    /// Bytes follow as a payload of exactly `size` bytes.
    Image { mime: &'static str, size: u64 },
}
impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { kind, text, .. } => {
                write!(f, "Text({kind:?}, <redacted {} bytes>)", text.len())
            }
            Self::Image { mime, size } => write!(f, "Image({mime}, {size} bytes)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedItem {
    pub request_id: Option<String>,
    pub age_ms: u64,
    pub item: Item,
}

pub fn image_mime(m: &str) -> Option<&'static str> {
    match m {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        _ => None,
    }
}

/// Parse a `geekbridge.clipboard.item` body. Bounded before anything is copied.
pub fn parse_item(body: &Map<String, Value>, limits: &Limits) -> Result<ParsedItem, WireError> {
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .and_then(ItemKind::parse)
        .ok_or(WireError::MissingContent)?;
    let request_id = match body.get("request_id") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .filter(|s| valid_request_id(s))
                .ok_or(WireError::BadRequestId)?
                .to_owned(),
        ),
    };
    let age_ms = body.get("age_ms").and_then(Value::as_u64).unwrap_or(0);
    let item = if kind == ItemKind::Image {
        let mime = body
            .get("mime")
            .and_then(Value::as_str)
            .and_then(image_mime)
            .ok_or(WireError::UnsupportedType)?;
        let size = body
            .get("size")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or(WireError::MissingContent)?;
        if size > clamp_image_bytes(limits.max_image_bytes) {
            return Err(WireError::TooLarge);
        }
        Item::Image { mime, size }
    } else {
        let text = body
            .get("text")
            .and_then(Value::as_str)
            .ok_or(WireError::MissingContent)?;
        if text.is_empty() || text.contains('\0') {
            return Err(WireError::MissingContent);
        }
        if text.len() > limits.max_text_bytes {
            return Err(WireError::TooLarge);
        }
        let html = match body.get("html") {
            None => None,
            Some(h) => {
                let h = h.as_str().ok_or(WireError::MissingContent)?;
                if h.len() > MAX_HTML_BYTES.min(limits.max_text_bytes) {
                    return Err(WireError::TooLarge);
                }
                Some(h.to_owned())
            }
        };
        if kind == ItemKind::Html && html.is_none() {
            return Err(WireError::MissingContent);
        }
        if kind == ItemKind::Url && !looks_like_url(text) {
            return Err(WireError::UnsupportedType);
        }
        Item::Text {
            kind,
            text: text.to_owned(),
            html,
        }
    };
    Ok(ParsedItem {
        request_id,
        age_ms,
        item,
    })
}

pub fn parse_none(body: &Map<String, Value>) -> Result<(String, NoneReason), WireError> {
    let id = body
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| valid_request_id(s))
        .ok_or(WireError::BadRequestId)?;
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .and_then(NoneReason::parse)
        .ok_or(WireError::MissingContent)?;
    Ok((id.to_owned(), reason))
}

pub fn parse_pull(body: &Map<String, Value>) -> Result<(String, u64), WireError> {
    let id = body
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| valid_request_id(s))
        .ok_or(WireError::BadRequestId)?;
    let max_age = body
        .get("max_age_ms")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_WINDOW_MS, clamp_window_ms);
    Ok((id.to_owned(), max_age))
}

/// A single http(s) URL: no whitespace, scheme, something after it. Deliberately not a full parser.
pub fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("https://") || s.starts_with("http://"))
        && s.len() <= 4096
        && s.len() > 8
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Classify plain text from a clipboard.
pub fn text_kind(text: &str) -> ItemKind {
    if looks_like_url(text) {
        ItemKind::Url
    } else {
        ItemKind::Text
    }
}

/// Recognise PNG/JPEG by magic bytes; the declared MIME is never trusted.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_and_connect_round_trip() {
        let b = update_body("héllo");
        assert_eq!(
            parse(CLIPBOARD, &b, 64),
            Ok(RemoteClip::Update {
                content: "héllo".into()
            })
        );
        let b = connect_body("x", 1_700_000_000_000);
        assert_eq!(
            parse(CLIPBOARD_CONNECT, &b, 64),
            Ok(RemoteClip::Connect {
                content: "x".into(),
                timestamp_ms: 1_700_000_000_000
            })
        );
    }

    #[test]
    fn malformed_and_oversized_bodies_are_refused() {
        assert_eq!(
            parse("kdeconnect.ping", &Map::new(), 8),
            Err(WireError::NotClipboard)
        );
        assert_eq!(
            parse(CLIPBOARD, &Map::new(), 8),
            Err(WireError::MissingContent)
        );
        let mut b = Map::new();
        b.insert("content".into(), Value::from(5));
        assert_eq!(parse(CLIPBOARD, &b, 8), Err(WireError::MissingContent));
        assert_eq!(
            parse(CLIPBOARD, &update_body("123456789"), 8),
            Err(WireError::TooLarge)
        );
        // Limit is in bytes: five two-byte characters do not fit in 8.
        assert_eq!(
            parse(CLIPBOARD, &update_body("ééééé"), 8),
            Err(WireError::TooLarge)
        );
        // `.connect` needs an integer timestamp.
        assert_eq!(
            parse(CLIPBOARD_CONNECT, &update_body("x"), 8),
            Err(WireError::BadTimestamp)
        );
        let mut b = update_body("x");
        b.insert("timestamp".into(), Value::from("soon"));
        assert_eq!(
            parse(CLIPBOARD_CONNECT, &b, 8),
            Err(WireError::BadTimestamp)
        );
    }

    #[test]
    fn debug_hides_content() {
        let c = RemoteClip::Update {
            content: "TOPSECRET".into(),
        };
        assert!(!format!("{c:?}").contains("TOPSECRET"));
    }

    fn lim() -> Limits {
        Limits {
            max_text_bytes: 1024,
            max_image_bytes: 1 << 20,
        }
    }

    #[test]
    fn items_round_trip_and_are_bounded() {
        let b = text_item_body(
            ItemKind::Html,
            "plain",
            Some("<b>plain</b>"),
            Some("r-1"),
            900,
        );
        let p = parse_item(&b, &lim()).unwrap();
        assert_eq!(p.request_id.as_deref(), Some("r-1"));
        assert_eq!(p.age_ms, 900);
        assert!(matches!(
            p.item,
            Item::Text {
                kind: ItemKind::Html,
                ..
            }
        ));
        let url = text_item_body(ItemKind::Url, "https://example.org/a?b=1", None, None, 0);
        assert!(parse_item(&url, &lim()).is_ok());
        let bad_url = text_item_body(ItemKind::Url, "javascript:alert(1)", None, None, 0);
        assert_eq!(
            parse_item(&bad_url, &lim()),
            Err(WireError::UnsupportedType)
        );
        let big = text_item_body(ItemKind::Text, &"a".repeat(1025), None, None, 0);
        assert_eq!(parse_item(&big, &lim()), Err(WireError::TooLarge));
        let html_without = text_item_body(ItemKind::Html, "x", None, None, 0);
        assert_eq!(
            parse_item(&html_without, &lim()),
            Err(WireError::MissingContent)
        );
        let nul = text_item_body(ItemKind::Text, "a\0b", None, None, 0);
        assert_eq!(parse_item(&nul, &lim()), Err(WireError::MissingContent));
        let bad_id = text_item_body(ItemKind::Text, "x", None, Some("bad id!"), 0);
        assert_eq!(parse_item(&bad_id, &lim()), Err(WireError::BadRequestId));
    }

    #[test]
    fn images_are_png_or_jpeg_and_size_capped() {
        let ok = image_item_body("image/png", 1000, Some("r"), 5);
        assert_eq!(
            parse_item(&ok, &lim()).unwrap().item,
            Item::Image {
                mime: "image/png",
                size: 1000
            }
        );
        assert_eq!(
            parse_item(&image_item_body("image/gif", 10, None, 0), &lim()),
            Err(WireError::UnsupportedType)
        );
        assert_eq!(
            parse_item(
                &image_item_body("image/png", (1 << 20) + 1, None, 0),
                &lim()
            ),
            Err(WireError::TooLarge)
        );
        assert_eq!(
            parse_item(&image_item_body("image/png", 0, None, 0), &lim()),
            Err(WireError::MissingContent)
        );
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a"), None);
    }

    #[test]
    fn pull_none_and_window() {
        let b = pull_body("abc-1", 1);
        assert_eq!(parse_pull(&b), Ok(("abc-1".into(), MIN_WINDOW_MS)));
        assert_eq!(parse_pull(&Map::new()), Err(WireError::BadRequestId));
        for r in [
            NoneReason::Locked,
            NoneReason::Expired,
            NoneReason::Empty,
            NoneReason::NeedsGesture,
            NoneReason::Disabled,
            NoneReason::TooLarge,
            NoneReason::Unsupported,
            NoneReason::Sensitive,
        ] {
            assert_eq!(parse_none(&none_body("x", r)), Ok(("x".into(), r)));
        }
        let mut weird = none_body("x", NoneReason::Empty);
        weird.insert("reason".into(), Value::from("because"));
        assert_eq!(parse_none(&weird), Err(WireError::MissingContent));
        assert!(within_window(30_000, DEFAULT_WINDOW_MS));
        assert!(!within_window(30_001, DEFAULT_WINDOW_MS));
        // The window itself is clamped: a huge setting cannot disable the limit.
        assert!(!within_window(MAX_WINDOW_MS + 1, u64::MAX));
    }

    #[test]
    fn url_detection() {
        assert_eq!(text_kind("https://a.example/x"), ItemKind::Url);
        assert_eq!(text_kind("see https://a.example"), ItemKind::Text);
        assert_eq!(text_kind("https://a b"), ItemKind::Text);
    }
}
