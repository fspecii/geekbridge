// SPDX-License-Identifier: GPL-3.0-or-later
//! Simulated phone for end-to-end call testing without Bluetooth.
//!
//! * [`modem`] (portable): the [`Modem`] / [`RemoteParty`] traits, an in-memory [`FakeModem`] and
//!   an [`AdbModem`] that drives an Android emulator's telephony stack through `adb`.
//! * `sim` / `ag` (Linux only): a service that claims `org.pipewire.Telephony` on a D-Bus bus
//!   and mirrors a [`Modem`]'s call state into the declared PipeWire telephony API
//!   (`AudioGateway1`, `AudioGatewayTransport1`, `Call1`), so the real call stack in
//!   `bridge-calls` can be exercised end to end.
//!
//! Only signalling is simulated. No Bluetooth link, SCO audio or HFP AT command exchange exists
//! here, so nothing in this crate verifies hardware or audio behaviour.

pub mod data;
pub mod modem;

#[cfg(target_os = "linux")]
mod ag;
#[cfg(target_os = "linux")]
mod sim;

pub use modem::{
    AdbModem, FakeModem, Modem, ModemError, ModemState, RemoteParty, parse_registry_call_state,
    redact_numbers, validate_number,
};
#[cfg(target_os = "linux")]
pub use sim::{Sim, SimConfig, SimError};
