// SPDX-License-Identifier: GPL-3.0-or-later
//! **EXPERIMENTAL, off by default.** The parts of "phone call audio over Wi-Fi" that both the phone engine and the
//! computer service share. Bluetooth hands-free stays the primary call-audio path.
//!
//! * [`wire`]: the `geekbridge.callaudio.*` packets.
//! * [`gate`]: the three consents and the session rules.
//! * [`codec`]: Opus 16 kHz mono, 20 ms (pure Rust).
//! * [`jitter`]: the adaptive jitter buffer.
//! * [`helper`]: framing to and from the shell-uid helper on the phone.
//! * [`selftest`]: what the phone's self-test result means, in plain words.
//!
//! Privacy: audio is never written to disk or logged; analysis keeps numbers only (level, counts).
#![forbid(unsafe_code)]

pub mod codec;
pub mod gate;
pub mod helper;
pub mod jitter;
pub mod selftest;
pub mod tone;
pub mod wire;
