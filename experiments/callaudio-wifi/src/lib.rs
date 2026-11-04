// SPDX-License-Identifier: GPL-3.0-or-later
//! **EXPERIMENTAL, off by default, not wired into the service.** Phone-call audio between a GeekBridge phone and
//! this computer over the existing TLS link, for computers without Bluetooth.
//!
//! On the phone the audio comes from (and, where the device allows it, goes into) the call through a helper
//! process running as the `shell` uid, started over Wireless debugging. What that helper can and cannot do is in
//! `docs/design/call-audio-over-wifi.md`; the measured state is in `docs/reports/call-audio-wifi-prototype.md`.
//!
//! * [`wire`]: the `geekbridge.callaudio.*` packets.
//! * [`gate`]: the three consents and the session rules.
//! * [`codec`]: Opus 16 kHz mono, 20 ms (pure Rust).
//! * [`jitter`]: the adaptive jitter buffer.
//! * [`helper`]: framing to and from the shell-uid helper.
//! * [`pcm`]: PipeWire playback and capture on the computer.
//! * [`lab`]: phone and computer in one process over the real link code, for tests and evidence.
//!
//! Privacy: audio is never written to disk or logged; analysis keeps numbers only (level, frequency, counts).
#![forbid(unsafe_code)]

pub mod codec;
pub mod gate;
pub mod helper;
pub mod jitter;
pub mod lab;
pub mod pcm;
pub mod tone;
pub mod wire;
