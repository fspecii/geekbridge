// SPDX-License-Identifier: GPL-3.0-or-later
//! Opus 16 kHz mono, 20 ms frames, via the pure-Rust `opus-pure` crate (EXPERIMENTAL).
//!
//! Settings: VoIP application (SILK for speech), 24 kbit/s, in-band FEC on with a 10 % expected loss so a lost
//! frame can be rebuilt from the next one, DTX off (a constant 50 frames/s keeps the jitter estimate honest).
use opus_pure::{Application, OpusDecoder, OpusEncoder};

use crate::jitter::Out;
use crate::wire::{FRAME_SAMPLES, MAX_OPUS_BYTES, RATE};

pub const BITRATE: i32 = 24_000;

#[derive(Debug)]
pub struct CodecError(pub String);

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "opus: {}", self.0)
    }
}

impl std::error::Error for CodecError {}

fn err(e: opus_pure::Error) -> CodecError {
    CodecError(e.to_string())
}

pub struct Encoder {
    inner: OpusEncoder,
    out: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Result<Self, CodecError> {
        let mut inner = OpusEncoder::new(RATE as i32, 1, Application::Voip).map_err(err)?;
        inner.bitrate_bps = BITRATE;
        inner.use_inband_fec = true;
        inner.packet_loss_perc = 10;
        inner.use_dtx = false;
        inner.complexity = 5;
        Ok(Self {
            inner,
            out: vec![0u8; MAX_OPUS_BYTES],
        })
    }

    /// Samples of delay the encoder adds (its look-ahead).
    pub fn lookahead(&self) -> usize {
        self.inner.lookahead()
    }

    /// Encode exactly one 20 ms frame ([`FRAME_SAMPLES`] samples).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, CodecError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(CodecError(format!("frame of {} samples", pcm.len())));
        }
        let n = self
            .inner
            .encode_s16(pcm, FRAME_SAMPLES, &mut self.out)
            .map_err(err)?;
        Ok(self.out[..n].to_vec())
    }
}

pub struct Decoder {
    inner: OpusDecoder,
    pcm: Vec<i16>,
}

impl Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            inner: OpusDecoder::new(RATE as i32, 1).map_err(err)?,
            pcm: vec![0; FRAME_SAMPLES],
        })
    }

    /// Turn one jitter-buffer slot into exactly [`FRAME_SAMPLES`] samples. A frame the decoder rejects (corrupt,
    /// or not 20 ms) is concealed instead, so hostile input never changes the output length.
    pub fn render(&mut self, slot: &Out) -> &[i16] {
        let got = match slot {
            Out::Idle => {
                self.pcm.fill(0);
                return &self.pcm;
            }
            Out::Packet(p) => self.inner.decode_s16(p, FRAME_SAMPLES, &mut self.pcm),
            Out::Conceal { fec: Some(next) } => {
                self.inner
                    .decode_fec_s16(next, FRAME_SAMPLES, &mut self.pcm)
            }
            Out::Conceal { fec: None } => self.inner.decode_s16(&[], FRAME_SAMPLES, &mut self.pcm),
        };
        match got {
            Ok(n) if n == FRAME_SAMPLES => {}
            _ => {
                if self
                    .inner
                    .decode_s16(&[], FRAME_SAMPLES, &mut self.pcm)
                    .ok()
                    != Some(FRAME_SAMPLES)
                {
                    self.pcm.fill(0);
                }
            }
        }
        &self.pcm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone;

    #[test]
    fn a_tone_survives_encode_and_decode() {
        let mut e = Encoder::new().unwrap();
        let mut d = Decoder::new().unwrap();
        let mut g = tone::Tone::new(700.0, 0.3);
        let mut out = Vec::new();
        let mut bytes = 0;
        for _ in 0..50 {
            let f = g.frame();
            let pkt = e.encode(&f).unwrap();
            assert!(!pkt.is_empty() && pkt.len() <= MAX_OPUS_BYTES);
            bytes += pkt.len();
            out.extend_from_slice(d.render(&Out::Packet(pkt)));
        }
        assert!(
            bytes / 50 < 100,
            "about 60 bytes a frame at 24 kbit/s, got {}",
            bytes / 50
        );
        let f = tone::dominant_hz(&out[out.len() / 2..], RATE);
        assert!((f - 700.0).abs() < 15.0, "{f}");
    }

    #[test]
    fn garbage_and_loss_still_give_whole_frames() {
        let mut d = Decoder::new().unwrap();
        for slot in [
            Out::Idle,
            Out::Packet(vec![0xFF; 3]),
            Out::Packet(vec![]),
            Out::Conceal { fec: None },
            Out::Conceal {
                fec: Some(vec![1, 2, 3]),
            },
        ] {
            assert_eq!(d.render(&slot).len(), FRAME_SAMPLES);
        }
        assert!(Encoder::new().unwrap().encode(&[0; 10]).is_err());
    }
}
