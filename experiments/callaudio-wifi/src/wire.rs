// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge.callaudio.*` packets (EXPERIMENTAL). Inside TLS only, sent only to a peer that declared the
//! packet kind as an incoming capability, and only while both standing consents are on (see [`crate::gate`]).
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`REQUEST`] | computer to phone | `session` (the user pressed "call audio here" on the computer) |
//! | [`OFFER`] | phone to computer | `session`, `dirs`, `codec`, `rate`, `frameMs`, `uplink` |
//! | [`ANSWER`] | computer to phone | `session`, `accept`, `dirs`, optional `reason` |
//! | [`FRAME`] | both | `session`, `dir`, `seq`, `ts`, `cap`, `opus` (base64, one 20 ms Opus packet) |
//! | [`STATS`] | both | `session`, `dir`, `got`, `lost`, `late`, `depthMs` (receiver report, once a second) |
//! | [`STOP`] | both | `session`, `reason` |
//!
//! Parsing is strict about every field the receiver acts on and lenient about extra keys (ignored), the same
//! rule as the rest of the `geekbridge.*` family. Nothing in a packet is ever logged or written to disk.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Map, Value};

pub const REQUEST: &str = "geekbridge.callaudio.request";
pub const OFFER: &str = "geekbridge.callaudio.offer";
pub const ANSWER: &str = "geekbridge.callaudio.answer";
pub const FRAME: &str = "geekbridge.callaudio.frame";
pub const STATS: &str = "geekbridge.callaudio.stats";
pub const STOP: &str = "geekbridge.callaudio.stop";
pub const ALL: [&str; 6] = [REQUEST, OFFER, ANSWER, FRAME, STATS, STOP];

/// What the phone declares as incoming (computer declares the mirror image). Kept out of the plaintext identity
/// like every `geekbridge.*` name.
pub const PHONE_INCOMING: [&str; 5] = [REQUEST, ANSWER, FRAME, STATS, STOP];
pub const COMPUTER_INCOMING: [&str; 4] = [OFFER, FRAME, STATS, STOP];

pub const CODEC: &str = "opus";
pub const RATE: u32 = 16_000;
pub const FRAME_MS: u32 = 20;
/// Samples per frame at [`RATE`].
pub const FRAME_SAMPLES: usize = (RATE / 1000 * FRAME_MS) as usize;
/// Largest Opus packet accepted. 20 ms of 16 kHz speech at 24 kbit/s is about 60 bytes; 400 leaves room for a
/// 160 kbit/s burst and keeps the JSON line under 1 KiB.
pub const MAX_OPUS_BYTES: usize = 400;
pub const MAX_ID: u64 = (1 << 53) - 1;
pub const MAX_REASON_CHARS: usize = 32;

pub fn is_callaudio_kind(kind: &str) -> bool {
    ALL.contains(&kind)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Far party to the computer's headset (phone captures VOICE_DOWNLINK).
    Down,
    /// Computer's microphone into the call (phone injects; see the design doc for which route).
    Up,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "down" => Some(Self::Down),
            "up" => Some(Self::Up),
            _ => None,
        }
    }
}

/// How the phone believes it can put the computer's microphone into the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uplink {
    /// Shell-uid AudioTrack to the TELEPHONY_TX ("in-call music") output. Device dependent.
    TelephonyTx,
    /// Shell-uid AudioPolicy loop-back mix into VOICE_COMMUNICATION recorders (VoIP apps only, not cellular).
    VoipMix,
    /// No route: listen-only (the user speaks into the phone).
    None,
}

impl Uplink {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TelephonyTx => "telephony_tx",
            Self::VoipMix => "voip_mix",
            Self::None => "none",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "telephony_tx" => Self::TelephonyTx,
            "voip_mix" => Self::VoipMix,
            "none" => Self::None,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub session: u64,
    pub down: bool,
    pub up: bool,
    pub uplink: Uplink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub session: u64,
    pub accept: bool,
    pub down: bool,
    pub up: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub session: u64,
    pub dir: Dir,
    /// Frame counter, +1 per 20 ms frame, wraps at 2^32.
    pub seq: u32,
    /// Sample index of the first sample at [`RATE`], wraps at 2^32 (74 hours).
    pub ts: u32,
    /// Sender's monotonic capture time in ms (wraps); only differences between two of the sender's own values
    /// mean anything. Used for end-to-end latency when both ends share a clock (tests, the lab).
    pub cap: u32,
    pub opus: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    pub session: u64,
    pub dir: Dir,
    pub got: u64,
    pub lost: u64,
    pub late: u64,
    pub depth_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Request {
        session: u64,
    },
    Offer(Offer),
    Answer(Answer),
    Frame(Frame),
    Stats(Stats),
    Stop {
        session: u64,
        reason: Option<String>,
    },
}

impl Msg {
    pub fn session(&self) -> u64 {
        match self {
            Self::Request { session } | Self::Stop { session, .. } => *session,
            Self::Offer(o) => o.session,
            Self::Answer(a) => a.session,
            Self::Frame(f) => f.session,
            Self::Stats(s) => s.session,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Request { .. } => REQUEST,
            Self::Offer(_) => OFFER,
            Self::Answer(_) => ANSWER,
            Self::Frame(_) => FRAME,
            Self::Stats(_) => STATS,
            Self::Stop { .. } => STOP,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    UnknownKind,
    Malformed(&'static str),
}

pub fn valid_reason(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REASON_CHARS
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn dirs_value(down: bool, up: bool) -> Value {
    let mut v = Vec::new();
    if down {
        v.push(Value::from("down"));
    }
    if up {
        v.push(Value::from("up"));
    }
    Value::Array(v)
}

/// Body of a packet (the `type` is [`Msg::kind`]).
pub fn body(msg: &Msg) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("session".into(), Value::from(msg.session()));
    match msg {
        Msg::Request { .. } => {}
        Msg::Offer(o) => {
            m.insert("dirs".into(), dirs_value(o.down, o.up));
            m.insert("codec".into(), Value::from(CODEC));
            m.insert("rate".into(), Value::from(RATE));
            m.insert("frameMs".into(), Value::from(FRAME_MS));
            m.insert("uplink".into(), Value::from(o.uplink.as_str()));
        }
        Msg::Answer(a) => {
            m.insert("accept".into(), Value::from(a.accept));
            m.insert("dirs".into(), dirs_value(a.down, a.up));
            if let Some(r) = a.reason.as_deref().filter(|r| valid_reason(r)) {
                m.insert("reason".into(), Value::from(r));
            }
        }
        Msg::Frame(f) => {
            m.insert("dir".into(), Value::from(f.dir.as_str()));
            m.insert("seq".into(), Value::from(f.seq));
            m.insert("ts".into(), Value::from(f.ts));
            m.insert("cap".into(), Value::from(f.cap));
            m.insert("opus".into(), Value::from(B64.encode(&f.opus)));
        }
        Msg::Stats(s) => {
            m.insert("dir".into(), Value::from(s.dir.as_str()));
            m.insert("got".into(), Value::from(s.got.min(MAX_ID)));
            m.insert("lost".into(), Value::from(s.lost.min(MAX_ID)));
            m.insert("late".into(), Value::from(s.late.min(MAX_ID)));
            m.insert("depthMs".into(), Value::from(s.depth_ms));
        }
        Msg::Stop { reason, .. } => {
            if let Some(r) = reason.as_deref().filter(|r| valid_reason(r)) {
                m.insert("reason".into(), Value::from(r));
            }
        }
    }
    m
}

fn id(body: &Map<String, Value>, key: &'static str) -> Result<u64, WireError> {
    body.get(key)
        .and_then(Value::as_u64)
        .filter(|n| (1..=MAX_ID).contains(n))
        .ok_or(WireError::Malformed(key))
}

fn count(body: &Map<String, Value>, key: &'static str) -> Result<u64, WireError> {
    body.get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n <= MAX_ID)
        .ok_or(WireError::Malformed(key))
}

fn u32_of(body: &Map<String, Value>, key: &'static str) -> Result<u32, WireError> {
    body.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(WireError::Malformed(key))
}

fn dir(body: &Map<String, Value>) -> Result<Dir, WireError> {
    body.get("dir")
        .and_then(Value::as_str)
        .and_then(Dir::from_name)
        .ok_or(WireError::Malformed("dir"))
}

/// `dirs`: a non-empty array of distinct known names.
fn dirs(body: &Map<String, Value>) -> Result<(bool, bool), WireError> {
    let arr = body
        .get("dirs")
        .and_then(Value::as_array)
        .ok_or(WireError::Malformed("dirs"))?;
    if arr.is_empty() || arr.len() > 2 {
        return Err(WireError::Malformed("dirs"));
    }
    let (mut down, mut up) = (false, false);
    for v in arr {
        match v.as_str().and_then(Dir::from_name) {
            Some(Dir::Down) if !down => down = true,
            Some(Dir::Up) if !up => up = true,
            _ => return Err(WireError::Malformed("dirs")),
        }
    }
    Ok((down, up))
}

fn reason(body: &Map<String, Value>) -> Option<String> {
    body.get("reason")
        .and_then(Value::as_str)
        .filter(|s| valid_reason(s))
        .map(str::to_owned)
}

pub fn parse(kind: &str, body: &Map<String, Value>) -> Result<Msg, WireError> {
    if !is_callaudio_kind(kind) {
        return Err(WireError::UnknownKind);
    }
    let session = id(body, "session")?;
    Ok(match kind {
        REQUEST => Msg::Request { session },
        OFFER => {
            let (down, up) = dirs(body)?;
            if body.get("codec").and_then(Value::as_str) != Some(CODEC) {
                return Err(WireError::Malformed("codec"));
            }
            if body.get("rate").and_then(Value::as_u64) != Some(u64::from(RATE)) {
                return Err(WireError::Malformed("rate"));
            }
            if body.get("frameMs").and_then(Value::as_u64) != Some(u64::from(FRAME_MS)) {
                return Err(WireError::Malformed("frameMs"));
            }
            let uplink = body
                .get("uplink")
                .and_then(Value::as_str)
                .and_then(Uplink::from_name)
                .ok_or(WireError::Malformed("uplink"))?;
            if up && uplink == Uplink::None {
                return Err(WireError::Malformed("up without an uplink route"));
            }
            Msg::Offer(Offer {
                session,
                down,
                up,
                uplink,
            })
        }
        ANSWER => {
            let accept = body
                .get("accept")
                .and_then(Value::as_bool)
                .ok_or(WireError::Malformed("accept"))?;
            let (down, up) = if accept { dirs(body)? } else { (false, false) };
            Msg::Answer(Answer {
                session,
                accept,
                down,
                up,
                reason: reason(body),
            })
        }
        FRAME => {
            let text = body
                .get("opus")
                .and_then(Value::as_str)
                .ok_or(WireError::Malformed("opus"))?;
            // Check the encoded length first so a huge string is never decoded.
            if text.is_empty() || text.len() > MAX_OPUS_BYTES.div_ceil(3) * 4 {
                return Err(WireError::Malformed("opus size"));
            }
            let opus = B64
                .decode(text)
                .map_err(|_| WireError::Malformed("opus base64"))?;
            if opus.is_empty() || opus.len() > MAX_OPUS_BYTES {
                return Err(WireError::Malformed("opus size"));
            }
            Msg::Frame(Frame {
                session,
                dir: dir(body)?,
                seq: u32_of(body, "seq")?,
                ts: u32_of(body, "ts")?,
                cap: u32_of(body, "cap")?,
                opus,
            })
        }
        STATS => Msg::Stats(Stats {
            session,
            dir: dir(body)?,
            got: count(body, "got")?,
            lost: count(body, "lost")?,
            late: count(body, "late")?,
            depth_ms: u32_of(body, "depthMs")?,
        }),
        STOP => Msg::Stop {
            session,
            reason: reason(body),
        },
        _ => return Err(WireError::UnknownKind),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn samples() -> Vec<Msg> {
        vec![
            Msg::Request { session: 1 },
            Msg::Offer(Offer {
                session: 2,
                down: true,
                up: true,
                uplink: Uplink::TelephonyTx,
            }),
            Msg::Offer(Offer {
                session: 2,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
            Msg::Answer(Answer {
                session: 3,
                accept: true,
                down: true,
                up: false,
                reason: Some("no_mic".into()),
            }),
            Msg::Answer(Answer {
                session: 3,
                accept: false,
                down: false,
                up: false,
                reason: Some("no_consent".into()),
            }),
            Msg::Frame(Frame {
                session: 4,
                dir: Dir::Up,
                seq: u32::MAX,
                ts: 7,
                cap: 9,
                opus: vec![0xF8, 1, 2, 3],
            }),
            Msg::Stats(Stats {
                session: 5,
                dir: Dir::Down,
                got: 100,
                lost: 2,
                late: 1,
                depth_ms: 60,
            }),
            Msg::Stop {
                session: MAX_ID,
                reason: Some("call_ended".into()),
            },
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for m in samples() {
            assert_eq!(parse(m.kind(), &body(&m)), Ok(m.clone()), "{m:?}");
        }
    }

    #[test]
    fn hostile_bodies_are_errors() {
        let bad = [
            (REQUEST, json!({})),
            (REQUEST, json!({"session": 0})),
            (REQUEST, json!({"session": "1"})),
            (REQUEST, json!({"session": 9007199254740992u64})),
            (
                OFFER,
                json!({"session": 1, "dirs": [], "codec": "opus", "rate": 16000, "frameMs": 20, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["down","down"], "codec": "opus", "rate": 16000, "frameMs": 20, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["down"], "codec": "pcm", "rate": 16000, "frameMs": 20, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["down"], "codec": "opus", "rate": 48000, "frameMs": 20, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["down"], "codec": "opus", "rate": 16000, "frameMs": 10, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["up"], "codec": "opus", "rate": 16000, "frameMs": 20, "uplink": "none"}),
            ),
            (
                OFFER,
                json!({"session": 1, "dirs": ["side"], "codec": "opus", "rate": 16000, "frameMs": 20, "uplink": "none"}),
            ),
            (ANSWER, json!({"session": 1, "accept": "yes"})),
            (ANSWER, json!({"session": 1, "accept": true})),
            (
                FRAME,
                json!({"session": 1, "dir": "down", "seq": 1, "ts": 1, "cap": 1, "opus": ""}),
            ),
            (
                FRAME,
                json!({"session": 1, "dir": "down", "seq": 1, "ts": 1, "cap": 1, "opus": "!!!"}),
            ),
            (
                FRAME,
                json!({"session": 1, "dir": "down", "seq": 4294967296u64, "ts": 1, "cap": 1, "opus": "AA=="}),
            ),
            (
                FRAME,
                json!({"session": 1, "dir": "down", "seq": -1, "ts": 1, "cap": 1, "opus": "AA=="}),
            ),
            (
                FRAME,
                json!({"session": 1, "dir": "sideways", "seq": 1, "ts": 1, "cap": 1, "opus": "AA=="}),
            ),
            (
                FRAME,
                json!({"session": 1, "dir": "down", "seq": 1, "ts": 1, "cap": 1, "opus": B64.encode(vec![0u8; MAX_OPUS_BYTES + 1])}),
            ),
            (
                STATS,
                json!({"session": 1, "dir": "down", "got": -1, "lost": 0, "late": 0, "depthMs": 0}),
            ),
            ("geekbridge.callaudio.other", json!({"session": 1})),
        ];
        for (k, v) in bad {
            assert!(parse(k, &obj(v.clone())).is_err(), "{k} {v}");
        }
    }

    #[test]
    fn a_refusal_needs_no_dirs_and_odd_reasons_are_dropped() {
        let a = parse(
            ANSWER,
            &obj(json!({"session": 9, "accept": false, "reason": "Not OK\n"})),
        )
        .unwrap();
        assert_eq!(
            a,
            Msg::Answer(Answer {
                session: 9,
                accept: false,
                down: false,
                up: false,
                reason: None
            })
        );
        let s = Msg::Stop {
            session: 1,
            reason: Some("x y".into()),
        };
        assert!(body(&s).get("reason").is_none());
    }

    #[test]
    fn the_largest_frame_fits_and_kinds_are_geekbridge_only() {
        let f = Msg::Frame(Frame {
            session: 1,
            dir: Dir::Down,
            seq: 1,
            ts: 1,
            cap: 1,
            opus: vec![0xAB; MAX_OPUS_BYTES],
        });
        assert_eq!(parse(FRAME, &body(&f)), Ok(f));
        assert!(ALL.iter().all(|k| k.starts_with("geekbridge.callaudio.")));
        assert!(!is_callaudio_kind("kdeconnect.telephony"));
        assert_eq!(FRAME_SAMPLES, 320);
    }
}
