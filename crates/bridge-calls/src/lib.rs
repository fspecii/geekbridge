// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth cellular-call backends for GeekBridge.
//!
//! The Linux adapter drives PipeWire's `org.pipewire.Telephony` D-Bus service, which makes this
//! computer the HFP hands-free unit for a paired phone (the phone being the audio gateway). Its
//! API is undocumented and unstable; every capability is derived from live introspection and
//! reported as *declared*, never *verified*. Other platforms compile to
//! [`UnavailableBackend`], which fails every operation with an explicit reason.
//!
//! * [`CallBackend`]: read ([`Snapshot`]) and explicitly act ([`Action`]).
//! * [`Session`]: turns snapshots into the neutral `bridge_core::calls::CallEvent` stream and
//!   gates `CallCommand`s.
//! * [`probe`]: read-only inventory used by the `geekbridge-call-probe` binary.
//!
//! * [`audio`]: call-audio routing (phone HFP voice <-> local speakers/microphone), a separate
//!   explicit step after call control.
//!
//! See `docs/call-control.md` and `docs/call-audio-options.md`.

pub mod audio;
pub mod backend;
pub mod declared;
pub mod error;
pub mod guard;
pub mod hear;
pub mod hfp_audio;
pub mod model;
mod private_file;
pub mod probe;
pub mod readiness;
mod session;

#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
mod probe_linux;
#[cfg(target_os = "linux")]
pub mod setup;

pub use backend::{CallBackend, UnavailableBackend, default_backend};
pub use declared::{
    DeclaredApi, DeclaredInterface, FeatureEvidence, MethodDeclaration, MethodSpec,
};
pub use error::{BackendError, ControlError, RemoteErrorKind};
pub use model::{
    Action, AudioCodecEvidence, CallFingerprint, CallObservation, CallRef, CallSnapshot, DeviceRef,
    PhoneSnapshot, ServiceStatus, Snapshot, TransportSnapshot, TransportState,
};
pub use session::{Session, SessionEvent};
