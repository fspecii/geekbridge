// SPDX-License-Identifier: GPL-3.0-or-later
//! Android companion native library: a bounded JSON request dispatcher over shared
//! `bridge-core` rules, plus (Android builds only) the JNI export Kotlin calls.
//!
//! Scope: stateless validation plus an opt-in phone transport. Nothing here sends an SMS, reads or
//! writes the clipboard, or places a call. Validation success means "this input satisfies the
//! shared rules", never "paired", "trusted" or "call-ready". Networking starts only after an
//! explicit `transport_start` request and runs on its own runtime, never on the calling thread.
//!
//! Request/response schema: `docs/android-contract.md`; validation behaviour and limits:
//! `docs/android-native.md`; transport: `docs/android-transport.md`.
#![deny(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod dispatch;
#[cfg(target_os = "android")]
mod jni_export;
pub mod mutf8;
mod transport;

pub use dispatch::{
    Dispatcher, InputError, MAX_DATA_REQUEST_BYTES, MAX_REQUEST_BYTES, REQUEST_VERSION,
    handle_request, input_error_response,
};
