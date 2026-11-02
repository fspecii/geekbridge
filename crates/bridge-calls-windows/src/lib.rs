// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows 11 cellular-call adapter over the public `Windows.ApplicationModel.Calls` phone-line
//! APIs (Bluetooth Hands-Free Profile, the PC as hands-free unit and the Android phone as audio
//! gateway).
//!
//! Windows owns the Bluetooth link and the call audio: this crate controls calls and asks Windows
//! to route audio, nothing more. It implements no HFP, SCO or PCM handling, uses no Microsoft
//! Limited Access Feature key (Windows 10 is reported unsupported) and treats `access denied` as a
//! typed result rather than something to work around. Nothing in it has run against real hardware;
//! every capability is at most *declared* (`Support::Declared`), never `Verified`.
//!
//! # Layers
//!
//! | module | role | host |
//! | --- | --- | --- |
//! | [`model`] | identifiers, native vocabulary, facts | any |
//! | [`readiness`] | ordered [`readiness::Blocker`]s (contract, package identity, privacy, access, registration, line, audio route) | any |
//! | [`platform`] | the mockable port [`platform::CallsPlatform`] | any |
//! | `native` | the WinRT implementation of the port | Windows only |
//! | [`engine`] | selection, refresh, gating, dispatch, epochs | any |
//! | [`project`] | mapping to/from `bridge_core::calls` | any |
//! | [`service`] | actor thread, deadlines, subscriptions | any |
//! | [`report`] | redacted JSON/text rendering | any |
//! | [`testing`] | in-memory port for tests | any |
//!
//! # Side effects
//!
//! Reading ([`engine::Engine::refresh`], [`service::CallsHandle::refresh`], selection) calls only
//! the read methods of the port. Requesting access, registering, connecting, unregistering, dialing
//! and every call operation require their own explicit call and re-validate against freshly read
//! state first.
#![deny(unsafe_code)]

pub mod engine;
pub mod model;
pub mod platform;
pub mod project;
pub mod readiness;
pub mod report;
pub mod service;
pub mod testing;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod native;

use platform::CallsPlatform;

/// Start the calling service on the best port for this host: the WinRT port on Windows, a port
/// that reports `NotWindows` elsewhere. The COM apartment is initialised on the actor thread.
pub fn spawn_default(
    cfg: service::ServiceConfig,
) -> Result<service::CallsService, model::PlatformError> {
    #[cfg(windows)]
    {
        service::CallsService::spawn(native::WinRtPlatform::new, cfg)
    }
    #[cfg(not(windows))]
    {
        service::CallsService::spawn(
            || Ok(platform::UnsupportedPlatform) as Result<platform::UnsupportedPlatform, _>,
            cfg,
        )
    }
}

/// Compile-time check that the port is object-safe enough to be boxed by integrators.
#[allow(dead_code)]
fn _port_is_usable(_: &mut dyn CallsPlatform) {}
