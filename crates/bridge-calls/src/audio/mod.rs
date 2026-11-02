// SPDX-License-Identifier: GPL-3.0-or-later
//! Call-audio routing: connecting the phone's duplex HFP/SCO audio to this computer's speakers
//! and microphone.
//!
//! This is the layer *after* call control. Orchestration, in order (details and the exact service
//! integration are in `docs/call-audio-options.md`):
//!
//! 1. The call adapter (`CallBackend`) invokes `AudioGatewayTransport1.Activate` (the explicit
//!    `ConnectAudio` action). That only asks PipeWire to open the SCO/eSCO transport. It does not
//!    create microphone audio and it does not route anything.
//! 2. The adapter reports `TransportSnapshot { state: Active, codec }`. The service passes that as
//!    a [`ControlEvidence`].
//! 3. WirePlumber switches the phone's card to a duplex voice profile and exposes the voice
//!    source/sink. [`AudioRouteManager::assess`] reports [`Blocker::CardNotInVoiceProfile`] /
//!    [`Blocker::VoiceEndpointsMissing`] until it has; the service re-assesses on a bounded
//!    schedule. Card profiles are never switched from here.
//! 4. Only then does the service call [`AudioRouteManager::start`] with a prepared [`RoutePlan`].
//!
//! Platform support: [`AudioServer`] is implemented for PulseAudio / `pipewire-pulse` on Linux
//! ([`PulseServer`]); other platforms get [`UnsupportedAudioServer`], which fails every operation
//! with an explicit reason.

pub mod inventory;
pub mod manager;
pub mod model;
pub mod plan;
pub mod server;
pub mod spec;
mod unsupported;

#[cfg(target_os = "linux")]
mod pulse;

pub use inventory::{AudioInventory, LocalEndpoint, OrphanModule, PhoneAudio};
pub use manager::{
    ActiveRoute, AudioRouteManager, EchoFailure, EchoOutcome, Health, ModuleRole, OwnedModule,
    RecoverReport, RecoverRoute, StartError, StartFailure, StartOptions, StartReport, StopReason,
    StopReport, TeardownReport,
};
pub use model::{BtAddress, EndpointId, ProfileKind, ServerIdentity, ServerSnapshot, VoiceCodec};
pub use plan::{
    Assessment, Blocker, ControlEvidence, DeclaredFeasibility, EchoPolicy, HardwareEvidence,
    LocalSelection, RoutePlan, RouteRequest, Warning,
};
pub use server::{AudioServer, ServerError};
pub use spec::{EchoMethod, PathDirection, SessionId};
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
