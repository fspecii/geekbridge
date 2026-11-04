// SPDX-License-Identifier: GPL-3.0-or-later
//! Call audio: putting a phone call's voice on this computer's headphones and microphone.
//!
//! The phone is the Bluetooth hands-free audio gateway; PipeWire carries the voice link (SCO).
//! While the link is open PipeWire shows the call as two streams (see [`model`]); WirePlumber
//! links them to the default devices and [`CallAudio`] moves them to the ones the user picked,
//! asks the phone for the link when the user answered here, mutes the real microphone, checks
//! that audio flows and undoes every change when the call ends (also after a crash).
//!
//! * [`model`]: plain data of one audio-server read; how a phone call looks on PipeWire.
//! * [`route`]: pure decisions (find the call, resolve devices, name problems with fixes).
//! * [`controller`]: the per-call state machine.
//! * [`prefs`]: the user's device pick and the restore file.
//! * [`AudioServer`]: the narrow server interface; [`PulseServer`] on Linux (`pactl`/`parec` against
//!   `pipewire-pulse`), [`UnsupportedAudioServer`] elsewhere.

pub mod controller;
pub mod model;
pub mod prefs;
pub mod route;
pub mod server;
mod unsupported;

#[cfg(target_os = "linux")]
mod pulse;

pub use controller::{AudioStatus, CallAudio, Options, PhoneInput, Place, Request, Want};
pub use model::{BtAddress, ProfileKind, ServerIdentity, ServerSnapshot};
pub use prefs::Choice;
pub use route::{Device, DeviceList, FlowProblem, Role};
pub use server::{AudioServer, PeakTarget, ServerError};
pub use unsupported::UnsupportedAudioServer;

#[cfg(target_os = "linux")]
pub use pulse::{PulseOptions, PulseServer};

/// The audio server type for this platform.
#[cfg(target_os = "linux")]
pub type PlatformAudioServer = PulseServer;
/// The audio server type for this platform.
#[cfg(not(target_os = "linux"))]
pub type PlatformAudioServer = UnsupportedAudioServer;

/// Connect to this platform's audio server. Never starts one.
#[cfg(target_os = "linux")]
pub fn connect_platform() -> Result<PlatformAudioServer, ServerError> {
    PulseServer::connect(PulseOptions::default())
}

/// Connect to this platform's audio server. Never starts one.
#[cfg(not(target_os = "linux"))]
pub fn connect_platform() -> Result<PlatformAudioServer, ServerError> {
    Ok(UnsupportedAudioServer::new(std::env::consts::OS))
}
