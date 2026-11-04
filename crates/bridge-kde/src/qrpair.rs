// SPDX-License-Identifier: GPL-3.0-or-later
//! QR / link pairing: one-time tokens and the payload a computer shows and a phone scans.
//!
//! The QR code carries what the phone needs to *pin* the computer before it talks to it: the
//! device id, the SHA-256 of the computer's TLS certificate, the LAN addresses and a short-lived
//! random token. The phone dials, checks that the certificate on the connection is the pinned one
//! (a man in the middle fails here, before the token is sent), then sends an ordinary pair request
//! carrying the token. The computer redeems the token (unknown, expired and already-used tokens are
//! refused) and treats a valid one as its own approval, because the user put the code on the
//! computer's screen on purpose. The phone still asks its user, with no code to compare.
//!
//! Everything here is pure (time is passed in) so expiry, reuse and mismatch are unit-tested.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::cert::{hex, random_bytes, unix_now};
use crate::error::Result;
use crate::identity::DeviceId;

/// Payload scheme and host; the whole string is what the QR code encodes.
pub const SCHEME: &str = "geekbridge://pair";
pub const PAYLOAD_VERSION: u32 = 1;
/// How long a freshly issued token can be redeemed.
pub const TOKEN_TTL: Duration = Duration::from_secs(120);
/// Tokens that may be outstanding at once (each "show QR" issues one).
const MAX_OUTSTANDING: usize = 4;
/// When a new code is issued the earlier ones stay valid for this long and no longer (a scan that was already
/// under way when the code renewed itself), so two codes are never both good for minutes
/// (docs/reports/security-review-7.md S7-4).
pub const REPLACED_GRACE_MS: u64 = 5_000;
/// Wrong tokens tolerated from one source before that source is refused until the next QR code.
/// Other sources and the outstanding tokens are not affected, so a stranger on the network cannot
/// revoke the code its owner is about to scan (docs/reports/security-review-2.md S2-2).
const MAX_BAD_ATTEMPTS: u32 = 5;
/// Sources remembered for the count above.
const MAX_SOURCES: usize = 32;
/// Remembered used tokens, so reuse is reported as reuse and not as "unknown".
const MAX_USED: usize = 16;
const MAX_PAYLOAD_BYTES: usize = 700;
const MAX_ADDRESSES: usize = 8;
pub const TOKEN_HEX_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedeemError {
    /// Not a token this computer issued (wrong, mistyped or revoked).
    Unknown,
    Expired,
    /// Valid once, already used.
    AlreadyUsed,
    /// Too many wrong tokens from this source.
    LockedOut,
}

impl RedeemError {
    /// Plain-language reason, safe to show.
    pub fn message(self) -> &'static str {
        match self {
            Self::Unknown => {
                "That pairing code was not recognised. Show a new QR code and scan it again."
            }
            Self::Expired => "That QR code has expired. Show a new one and scan it again.",
            Self::AlreadyUsed => "That QR code was already used. Show a new one and scan it again.",
            Self::LockedOut => "Too many wrong codes. Show a new QR code to try again.",
        }
    }
}

struct Issued {
    digest: [u8; 32],
    expires_ms: u64,
}

/// Outstanding one-time tokens. Only SHA-256 digests are kept.
#[derive(Default)]
pub struct TokenBook {
    live: Vec<Issued>,
    used: Vec<[u8; 32]>,
    /// Wrong tokens per source address (`None` = source unknown), oldest source first.
    bad: Vec<(Option<IpAddr>, u32)>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Comparison that does not stop at the first differing byte.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl TokenBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a token valid for `ttl` from `now_ms`. Returns the token and its expiry.
    pub fn issue(&mut self, now_ms: u64, ttl: Duration) -> Result<(String, u64)> {
        let mut b = [0u8; TOKEN_HEX_LEN / 2];
        random_bytes(&mut b)?;
        Ok(self.issue_with(&hex(&b), now_ms, ttl))
    }

    /// [`issue`](Self::issue) with a caller-chosen token (tests).
    pub fn issue_with(&mut self, token: &str, now_ms: u64, ttl: Duration) -> (String, u64) {
        self.live.retain(|t| t.expires_ms > now_ms);
        for t in &mut self.live {
            t.expires_ms = t.expires_ms.min(now_ms + REPLACED_GRACE_MS);
        }
        // A new QR replaces the oldest one: a bounded number of live secrets.
        while self.live.len() >= MAX_OUTSTANDING {
            self.live.remove(0);
        }
        let expires_ms = now_ms + ttl.as_millis() as u64;
        self.live.push(Issued {
            digest: digest(token),
            expires_ms,
        });
        self.bad.clear();
        (token.to_owned(), expires_ms)
    }

    /// Redeem `presented` once. On success the token is gone for good.
    pub fn redeem(&mut self, presented: &str, now_ms: u64) -> std::result::Result<(), RedeemError> {
        self.redeem_from(presented, now_ms, None)
    }

    /// [`redeem`](Self::redeem) for a request that came from `source`: wrong tokens are counted
    /// per source, and only that source is locked out.
    pub fn redeem_from(
        &mut self,
        presented: &str,
        now_ms: u64,
        source: Option<IpAddr>,
    ) -> std::result::Result<(), RedeemError> {
        let source = source.map(|ip| match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        });
        if self.bad_count(source) >= MAX_BAD_ATTEMPTS {
            return Err(RedeemError::LockedOut);
        }
        let d = digest(presented);
        // Look at every entry, so timing does not reveal which one matched.
        let mut hit = None;
        for (i, t) in self.live.iter().enumerate() {
            if same(&t.digest, &d) {
                hit = Some(i);
            }
        }
        if let Some(i) = hit {
            let t = self.live.remove(i);
            if t.expires_ms <= now_ms {
                return Err(RedeemError::Expired);
            }
            self.used.push(t.digest);
            if self.used.len() > MAX_USED {
                self.used.remove(0);
            }
            self.bad.retain(|(s, _)| *s != source);
            return Ok(());
        }
        if self.used.iter().any(|u| same(u, &d)) {
            return Err(RedeemError::AlreadyUsed);
        }
        self.note_bad(source);
        Err(RedeemError::Unknown)
    }

    fn bad_count(&self, source: Option<IpAddr>) -> u32 {
        self.bad
            .iter()
            .find(|(s, _)| *s == source)
            .map_or(0, |(_, n)| *n)
    }

    fn note_bad(&mut self, source: Option<IpAddr>) {
        match self.bad.iter_mut().find(|(s, _)| *s == source) {
            Some((_, n)) => *n += 1,
            None => {
                if self.bad.len() >= MAX_SOURCES {
                    self.bad.remove(0);
                }
                self.bad.push((source, 1));
            }
        }
    }

    /// Drop every outstanding token (the user closed the QR dialog).
    pub fn revoke_all(&mut self) {
        self.live.clear();
    }

    pub fn outstanding(&self, now_ms: u64) -> usize {
        self.live.iter().filter(|t| t.expires_ms > now_ms).count()
    }
}

/// Is `s` shaped like a token (guards what goes into a packet).
pub fn token_shape_ok(s: &str) -> bool {
    s.len() == TOKEN_HEX_LEN && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn now_ms() -> u64 {
    unix_now().max(0) as u64 * 1000
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrPayload {
    pub device_id: String,
    pub name: String,
    /// SHA-256 of the computer's TLS certificate, lower-case hex.
    pub cert_sha256: String,
    pub addresses: Vec<SocketAddr>,
    pub token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadError {
    NotGeekBridge,
    Version,
    Malformed,
}

impl PayloadError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotGeekBridge => "That is not a GeekBridge pairing code.",
            Self::Version => {
                "That pairing code is from a different GeekBridge version. Update both apps."
            }
            Self::Malformed => "That pairing code is damaged. Show a new one and try again.",
        }
    }
}

fn encode_component(s: &str, out: &mut String) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

fn decode_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Format characters that draw nothing or reorder text (bidi controls, zero-width, joiners, BOM).
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

impl QrPayload {
    pub fn encode(&self) -> String {
        let mut s = format!(
            "{SCHEME}?v={PAYLOAD_VERSION}&id={}&fp={}&t={}&a=",
            self.device_id, self.cert_sha256, self.token
        );
        for (i, a) in self.addresses.iter().take(MAX_ADDRESSES).enumerate() {
            if i > 0 {
                s.push(',');
            }
            encode_component(&a.to_string(), &mut s);
        }
        s.push_str("&n=");
        encode_component(&self.name, &mut s);
        s
    }

    pub fn parse(text: &str) -> std::result::Result<Self, PayloadError> {
        let text = text.trim();
        if text.len() > MAX_PAYLOAD_BYTES {
            return Err(PayloadError::Malformed);
        }
        let rest = text
            .strip_prefix(SCHEME)
            .and_then(|r| r.strip_prefix('?'))
            .ok_or(PayloadError::NotGeekBridge)?;
        let (mut v, mut id, mut fp, mut t, mut a, mut n) = (None, None, None, None, None, None);
        for pair in rest.split('&') {
            let (k, val) = pair.split_once('=').ok_or(PayloadError::Malformed)?;
            let slot = match k {
                "v" => &mut v,
                "id" => &mut id,
                "fp" => &mut fp,
                "t" => &mut t,
                "a" => &mut a,
                "n" => &mut n,
                _ => continue, // unknown keys from a newer version are ignored
            };
            if slot.replace(val).is_some() {
                return Err(PayloadError::Malformed);
            }
        }
        if v.ok_or(PayloadError::Malformed)?.parse::<u32>() != Ok(PAYLOAD_VERSION) {
            return Err(PayloadError::Version);
        }
        let device_id = id.ok_or(PayloadError::Malformed)?;
        DeviceId::parse(device_id).map_err(|_| PayloadError::Malformed)?;
        let fp = fp.ok_or(PayloadError::Malformed)?;
        if fp.len() != 64 || !fp.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PayloadError::Malformed);
        }
        let token = t.ok_or(PayloadError::Malformed)?;
        if !token_shape_ok(token) {
            return Err(PayloadError::Malformed);
        }
        let mut addresses = Vec::new();
        for part in a.ok_or(PayloadError::Malformed)?.split(',') {
            let part = decode_component(part).ok_or(PayloadError::Malformed)?;
            addresses.push(
                part.parse::<SocketAddr>()
                    .map_err(|_| PayloadError::Malformed)?,
            );
            if addresses.len() > MAX_ADDRESSES {
                return Err(PayloadError::Malformed);
            }
        }
        if addresses.is_empty() {
            return Err(PayloadError::Malformed);
        }
        let name = decode_component(n.unwrap_or("")).ok_or(PayloadError::Malformed)?;
        if name.chars().any(char::is_control) {
            return Err(PayloadError::Malformed);
        }
        // The name is chosen by whoever made the code: direction overrides and zero-width characters
        // could make "Connect to <name>?" read as something else.
        let name: String = name.chars().filter(|c| !is_invisible_format(*c)).collect();
        Ok(Self {
            device_id: device_id.to_owned(),
            name: name.chars().take(64).collect(),
            cert_sha256: fp.to_ascii_lowercase(),
            addresses,
            token: token.to_ascii_lowercase(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(120);
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn payload() -> QrPayload {
        QrPayload {
            device_id: "0123456789abcdef0123456789abcdef".into(),
            name: "Ana's laptop / 1".into(),
            cert_sha256: "ab".repeat(32),
            addresses: vec![
                "192.168.1.20:1716".parse().unwrap(),
                "[fe80::1]:1716".parse().unwrap(),
            ],
            token: TOKEN.into(),
        }
    }

    #[test]
    fn token_redeems_once() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 1_000, TTL);
        assert_eq!(book.redeem(TOKEN, 2_000), Ok(()));
        assert_eq!(book.redeem(TOKEN, 2_100), Err(RedeemError::AlreadyUsed));
        assert_eq!(book.outstanding(2_100), 0);
    }

    #[test]
    fn token_expires() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 1_000, TTL);
        assert_eq!(
            book.redeem(TOKEN, 1_000 + 120_000),
            Err(RedeemError::Expired)
        );
        // An expired token is consumed too: it cannot come back to life.
        assert_ne!(book.redeem(TOKEN, 1_500), Ok(()));
    }

    #[test]
    fn token_just_before_expiry_still_works() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 0, TTL);
        assert_eq!(book.redeem(TOKEN, 119_999), Ok(()));
    }

    #[test]
    fn wrong_token_is_refused_and_does_not_burn_the_real_one() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 0, TTL);
        assert_eq!(book.redeem(&"f".repeat(32), 10), Err(RedeemError::Unknown));
        assert_eq!(book.redeem(TOKEN, 20), Ok(()));
    }

    #[test]
    fn repeated_wrong_tokens_lock_everything_out() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 0, TTL);
        for i in 0..MAX_BAD_ATTEMPTS {
            assert_eq!(
                book.redeem(&format!("{i:032x}"), 10),
                Err(RedeemError::Unknown)
            );
        }
        assert_eq!(book.redeem(TOKEN, 20), Err(RedeemError::LockedOut));
        // A freshly shown QR starts over.
        book.issue_with("11111111111111111111111111111111", 30, TTL);
        assert_eq!(book.redeem("11111111111111111111111111111111", 40), Ok(()));
    }

    #[test]
    fn a_strangers_wrong_tokens_do_not_lock_out_the_owner() {
        let a: IpAddr = "192.168.1.66".parse().unwrap();
        let b: IpAddr = "192.168.1.20".parse().unwrap();
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 0, TTL);
        for i in 0..(MAX_BAD_ATTEMPTS + 3) {
            let _ = book.redeem_from(&format!("{i:032x}"), 10, Some(a));
        }
        assert_eq!(
            book.redeem_from(TOKEN, 20, Some(a)),
            Err(RedeemError::LockedOut)
        );
        // The code on the screen still works for everyone else.
        assert_eq!(book.redeem_from(TOKEN, 30, Some(b)), Ok(()));
    }

    #[test]
    fn issuing_is_bounded_and_random() {
        let mut book = TokenBook::new();
        let mut seen = Vec::new();
        for _ in 0..(MAX_OUTSTANDING + 3) {
            let (t, _) = book.issue(0, TTL).unwrap();
            assert!(token_shape_ok(&t));
            assert!(!seen.contains(&t));
            seen.push(t);
        }
        assert_eq!(book.outstanding(0), MAX_OUTSTANDING);
        // The oldest were dropped.
        assert_eq!(book.redeem(&seen[0], 1), Err(RedeemError::Unknown));
        // Only the newest is still good once the grace has run out.
        assert_eq!(book.outstanding(REPLACED_GRACE_MS + 1), 1);
    }

    /// Security review 7: a refreshed code must not leave the previous one valid for its whole lifetime
    /// (two live tokens for up to two minutes). The old one gets a few seconds of grace for a scan that
    /// was already under way, then it is dead.
    #[test]
    fn a_new_code_ends_the_previous_one_after_a_short_grace() {
        let mut book = TokenBook::new();
        let (old, _) = book.issue(0, TTL).unwrap();
        let (new, _) = book.issue(60_000, TTL).unwrap();
        assert_eq!(book.outstanding(60_000 + REPLACED_GRACE_MS + 1), 1);
        assert_eq!(
            book.redeem(&old, 60_000 + REPLACED_GRACE_MS + 1),
            Err(RedeemError::Expired)
        );
        assert_eq!(book.redeem(&new, 70_000), Ok(()));
        // Within the grace the old code still works once (a scan that began just before the refresh).
        let (old2, _) = book.issue(100_000, TTL).unwrap();
        let (_new2, _) = book.issue(101_000, TTL).unwrap();
        assert_eq!(book.redeem(&old2, 101_000 + REPLACED_GRACE_MS - 1), Ok(()));
    }

    #[test]
    fn a_payload_name_loses_direction_overrides_and_zero_width_characters() {
        let mut p = payload();
        p.name = "Ana\u{202E}gpj.exe\u{200B}\u{2066}".into();
        let parsed = QrPayload::parse(&p.encode()).unwrap();
        assert_eq!(parsed.name, "Anagpj.exe");
    }

    #[test]
    fn revoke_all_kills_tokens() {
        let mut book = TokenBook::new();
        book.issue_with(TOKEN, 0, TTL);
        book.revoke_all();
        assert_eq!(book.redeem(TOKEN, 1), Err(RedeemError::Unknown));
    }

    #[test]
    fn payload_round_trips() {
        let p = payload();
        let text = p.encode();
        assert!(text.len() < MAX_PAYLOAD_BYTES);
        assert_eq!(QrPayload::parse(&text), Ok(p));
    }

    #[test]
    fn payload_rejects_garbage() {
        let good = payload().encode();
        assert_eq!(
            QrPayload::parse("https://example.com"),
            Err(PayloadError::NotGeekBridge)
        );
        assert_eq!(
            QrPayload::parse(&good.replace("v=1", "v=2")),
            Err(PayloadError::Version)
        );
        assert_eq!(
            QrPayload::parse(&good.replace(&"ab".repeat(32), "abcd")),
            Err(PayloadError::Malformed)
        );
        assert_eq!(
            QrPayload::parse(&good.replace(TOKEN, "short")),
            Err(PayloadError::Malformed)
        );
        assert_eq!(
            QrPayload::parse(&good.replace("192.168.1.20", "not-an-ip")),
            Err(PayloadError::Malformed)
        );
        assert_eq!(
            QrPayload::parse(&format!("{good}&t={TOKEN}")),
            Err(PayloadError::Malformed)
        );
        assert_eq!(
            QrPayload::parse(&format!("{good}{}", "x".repeat(800))),
            Err(PayloadError::Malformed)
        );
        assert_eq!(
            QrPayload::parse(&good.replace("n=Ana", "n=%00Ana")),
            Err(PayloadError::Malformed)
        );
    }

    #[test]
    fn newer_unknown_keys_are_ignored() {
        let text = format!("{}&z=future", payload().encode());
        assert_eq!(QrPayload::parse(&text), Ok(payload()));
    }
}
