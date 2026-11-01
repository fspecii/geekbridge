//! macOS Bluetooth cellular-call adapter: the Mac acts as the Hands-Free (HF) unit and the Android
//! phone as the Audio Gateway (AG), through `IOBluetoothHandsFreeDevice`.
//!
//! # Layers
//!
//! * Platform-independent (compiled and tested on every host): [`model`], [`tracker`] (HFP
//!   indicators to calls), [`plan`] (command to native operation or refusal), [`controller`]
//!   (generation-bound lifecycle, confirmation, timeouts), [`route`] (explicit, reversible
//!   audio-default changes), [`caps`], [`protocol`] and [`helper`] (JSON-lines helper logic).
//! * macOS only: `apple` — Objective-C bindings through `objc2` for IOBluetooth, CoreAudio and the
//!   main run loop. Every other host builds [`unsupported`] ports that fail explicitly.
//!
//! The adapter's output is the neutral `bridge_core::calls::CallEvent` stream plus
//! [`controller::AdapterEvent`]s; it never claims `Verified` support or "duplex" audio.
//! See `docs/architecture.md` for evidence levels and daemon integration.
pub mod caps;
pub mod cli;
pub mod controller;
pub mod error;
pub mod helper;
pub mod model;
pub mod plan;
pub mod port;
pub mod protocol;
pub mod route;
pub mod tracker;
pub mod unsupported;

#[cfg(target_os = "macos")]
pub mod apple;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use caps::MacCapabilities;
pub use controller::{AdapterEvent, Config, Controller, Outcome, Snapshot, Ticket};
pub use error::AdapterError;
pub use model::{BtAddress, Generation, Inventory, PhoneRecord};
pub use plan::{AudioStrategy, MacCommand, Refusal};
pub use route::{AudioProof, RouteRequest};
