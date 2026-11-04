// SPDX-License-Identifier: GPL-3.0-or-later
//! The local framing between the shell-uid helper (`app_process`, see
//! `android/app/src/debug/java/dev/geekbridge/android/callaudio/`) and its client (EXPERIMENTAL).
//!
//! In the product the client is the GeekBridge app on the same phone (abstract unix socket, peer uid checked).
//! In the lab and the emulator e2e it is `geekbridge-callaudio-lab` on the computer, reached through
//! `adb forward`, standing in for the app. The Kotlin twin is `HelperProtocol.kt`; both are tested against the
//! same byte vectors (`HELLO_VECTOR`, `PCM_VECTOR`).
//!
//! ```text
//! frame  = type:u8  flags:u8  len:u16be  payload[len]          (len <= MAX_PAYLOAD)
//! HELLO  (1)  helper -> client  UTF-8 JSON: {"v":1,"uid":2000,"sdk":36,"token":"…", …}
//! PCM    (2)  helper -> client  seq:u32be cap:u32be samples:s16le[320]   (flags: 0 = down, 1 = up)
//! STATUS (3)  helper -> client  UTF-8 JSON (probe results, errors)
//! INJECT (4)  client -> helper  same layout as PCM: the computer's microphone, to be played into the call
//! CMD    (5)  client -> helper  UTF-8 JSON: {"cmd":"start","source":"VOICE_DOWNLINK","token":"…"} | stop | probe
//! ```
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::wire::FRAME_SAMPLES;

pub const HELLO: u8 = 1;
pub const PCM: u8 = 2;
pub const STATUS: u8 = 3;
pub const INJECT: u8 = 4;
pub const CMD: u8 = 5;
pub const MAX_PAYLOAD: usize = 4096;
pub const PCM_PAYLOAD: usize = 8 + FRAME_SAMPLES * 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperFrame {
    Hello(String),
    Pcm {
        up: bool,
        seq: u32,
        cap: u32,
        samples: Vec<i16>,
    },
    Status(String),
    Inject {
        seq: u32,
        cap: u32,
        samples: Vec<i16>,
    },
    Cmd(String),
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn pcm_payload(seq: u32, cap: u32, samples: &[i16]) -> Vec<u8> {
    let mut p = Vec::with_capacity(PCM_PAYLOAD);
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&cap.to_be_bytes());
    for s in samples {
        p.extend_from_slice(&s.to_le_bytes());
    }
    p
}

pub fn encode(f: &HelperFrame) -> Vec<u8> {
    let (t, flags, payload) = match f {
        HelperFrame::Hello(s) => (HELLO, 0, s.as_bytes().to_vec()),
        HelperFrame::Status(s) => (STATUS, 0, s.as_bytes().to_vec()),
        HelperFrame::Cmd(s) => (CMD, 0, s.as_bytes().to_vec()),
        HelperFrame::Pcm {
            up,
            seq,
            cap,
            samples,
        } => (PCM, u8::from(*up), pcm_payload(*seq, *cap, samples)),
        HelperFrame::Inject { seq, cap, samples } => (INJECT, 0, pcm_payload(*seq, *cap, samples)),
    };
    assert!(payload.len() <= MAX_PAYLOAD, "helper frame too large");
    let mut out = Vec::with_capacity(4 + payload.len());
    out.push(t);
    out.push(flags);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Parse one frame body. Strict: unknown type, wrong PCM size, non-UTF-8 text are errors.
pub fn decode(t: u8, flags: u8, payload: &[u8]) -> io::Result<HelperFrame> {
    let text = || String::from_utf8(payload.to_vec()).map_err(|_| bad("helper text is not UTF-8"));
    let pcm = || -> io::Result<(u32, u32, Vec<i16>)> {
        if payload.len() != PCM_PAYLOAD {
            return Err(bad("helper PCM frame of the wrong size"));
        }
        let seq = u32::from_be_bytes(payload[0..4].try_into().expect("4"));
        let cap = u32::from_be_bytes(payload[4..8].try_into().expect("4"));
        let samples = payload[8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        Ok((seq, cap, samples))
    };
    if t != PCM && flags != 0 {
        return Err(bad("helper flags"));
    }
    Ok(match t {
        HELLO => HelperFrame::Hello(text()?),
        STATUS => HelperFrame::Status(text()?),
        CMD => HelperFrame::Cmd(text()?),
        PCM => {
            if flags > 1 {
                return Err(bad("helper PCM flags"));
            }
            let (seq, cap, samples) = pcm()?;
            HelperFrame::Pcm {
                up: flags == 1,
                seq,
                cap,
                samples,
            }
        }
        INJECT => {
            let (seq, cap, samples) = pcm()?;
            HelperFrame::Inject { seq, cap, samples }
        }
        _ => return Err(bad("unknown helper frame type")),
    })
}

/// Parse every whole frame at the start of `buf`; returns the frames and the bytes consumed (for the fuzz target
/// and for callers that read in chunks).
pub fn decode_all(buf: &[u8]) -> io::Result<(Vec<HelperFrame>, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while buf.len() - at >= 4 {
        let len = usize::from(u16::from_be_bytes([buf[at + 2], buf[at + 3]]));
        if len > MAX_PAYLOAD {
            return Err(bad("helper frame too large"));
        }
        if buf.len() - at - 4 < len {
            break;
        }
        out.push(decode(buf[at], buf[at + 1], &buf[at + 4..at + 4 + len])?);
        at += 4 + len;
    }
    Ok((out, at))
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<HelperFrame> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h).await?;
    let len = usize::from(u16::from_be_bytes([h[2], h[3]]));
    if len > MAX_PAYLOAD {
        return Err(bad("helper frame too large"));
    }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p).await?;
    decode(h[0], h[1], &p)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, f: &HelperFrame) -> io::Result<()> {
    w.write_all(&encode(f)).await?;
    w.flush().await
}

/// Shared test vectors with `HelperProtocolTest.kt`.
pub const HELLO_VECTOR: &[u8] = b"\x01\x00\x00\x07{\"v\":1}";

pub fn pcm_vector() -> Vec<u8> {
    let samples: Vec<i16> = (0..FRAME_SAMPLES as i16).map(|i| i * 3 - 400).collect();
    encode(&HelperFrame::Pcm {
        up: false,
        seq: 0x01020304,
        cap: 0xA0B0C0D0,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_match_the_kotlin_vectors() {
        assert_eq!(
            encode(&HelperFrame::Hello("{\"v\":1}".into())),
            HELLO_VECTOR
        );
        let v = pcm_vector();
        assert_eq!(
            &v[..12],
            &[2, 0, 0x02, 0x88, 1, 2, 3, 4, 0xA0, 0xB0, 0xC0, 0xD0]
        );
        // first sample -400 = 0xFE70 little-endian
        assert_eq!(&v[12..14], &[0x70, 0xFE]);
        let mut all = v.clone();
        all.extend(encode(&HelperFrame::Cmd("{\"cmd\":\"stop\"}".into())));
        all.extend(encode(&HelperFrame::Inject {
            seq: 1,
            cap: 2,
            samples: vec![5; FRAME_SAMPLES],
        }));
        all.extend_from_slice(&[3, 0, 0]); // a partial header stays unconsumed
        let (frames, used) = decode_all(&all).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(used, all.len() - 3);
        assert!(matches!(
            &frames[0],
            HelperFrame::Pcm {
                seq: 0x01020304,
                up: false,
                ..
            }
        ));
    }

    #[test]
    fn hostile_frames_are_errors() {
        assert!(decode(9, 0, b"").is_err());
        assert!(decode(CMD, 1, b"{}").is_err());
        assert!(decode(PCM, 0, &[0; 10]).is_err());
        assert!(decode(PCM, 2, &vec![0; PCM_PAYLOAD]).is_err());
        assert!(decode(STATUS, 0, &[0xFF, 0xFE]).is_err());
        assert!(decode_all(&[1, 0, 0xFF, 0xFF]).is_err());
    }

    #[tokio::test]
    async fn async_read_and_write() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let f = HelperFrame::Status("{\"ok\":true}".into());
        write_frame(&mut a, &f).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), f);
    }
}
