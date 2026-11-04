// SPDX-License-Identifier: GPL-3.0-or-later
//! The backend contract and the explicit "nothing here" backend.

use std::time::Duration;

use bridge_core::calls::{
    CallBackend as CallBackendKind, CallCapabilities, HfpHost, UnsupportedReason,
};

use crate::error::{BackendError, ControlError};
use crate::model::{Action, Snapshot};

/// A source of phone/call state and an executor of explicit control requests.
///
/// Reading ([`snapshot`](Self::snapshot), [`wait_for_change`](Self::wait_for_change)) is always
/// side-effect free. [`invoke`](Self::invoke) is the only method that can change a call, and it
/// does exactly what the [`Action`] names after re-validating it against fresh state.
pub trait CallBackend: Send + Sync {
    /// Stable machine-readable backend name.
    fn name(&self) -> &'static str;
    /// Read the current phones and calls. An absent service is `Ok` with
    /// [`ServiceStatus::Absent`](crate::ServiceStatus::Absent).
    fn snapshot(&self) -> Result<Snapshot, BackendError>;
    /// Execute one control request. `Ok(())` means the service accepted it, **not** that the phone
    /// has acted: the effect arrives later as a state change (see [`crate::Session`]).
    fn invoke(&self, action: &Action) -> Result<(), ControlError>;
    /// Block up to `timeout` until the service reports any change. `Ok(true)` if something
    /// arrived; callers re-read with [`snapshot`](Self::snapshot).
    fn wait_for_change(&self, timeout: Duration) -> Result<bool, BackendError>;
}

/// The backend for platforms (and failures) where no real adapter exists. Every operation fails
/// with an explicit reason; it never shells out and never pretends a phone exists.
#[derive(Clone, Debug)]
pub struct UnavailableBackend {
    platform: &'static str,
    reason: String,
}

impl UnavailableBackend {
    pub fn new(platform: &'static str, reason: impl Into<String>) -> Self {
        Self {
            platform,
            reason: reason.into(),
        }
    }
    /// The backend this build uses when the platform has no adapter yet.
    pub fn for_this_platform() -> Self {
        let platform = std::env::consts::OS;
        Self::new(platform_label(platform), unavailable_reason(platform))
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn platform(&self) -> &'static str {
        self.platform
    }
    /// Every feature explicitly unsupported (not merely unknown).
    pub fn capabilities(&self) -> CallCapabilities {
        let host = match self.platform {
            "windows" => HfpHost::Windows,
            "macos" => HfpHost::MacOs,
            _ => HfpHost::LinuxPipeWire,
        };
        CallCapabilities::unsupported(
            CallBackendKind::BluetoothHfp { host },
            UnsupportedReason::NotImplemented,
        )
    }
}

/// Why there is no Bluetooth call backend, in words for the system it is read on. Nothing here
/// names a Linux tool unless the system is Linux-like.
pub fn unavailable_reason(os: &str) -> String {
    match os {
        "windows" => "Windows carries call audio itself (pair the phone in Settings > Bluetooth & devices and turn on Phone calls); GeekBridge shows and answers calls through the phone link".into(),
        "macos" => "a Mac cannot carry the voice of an Android phone's call; GeekBridge shows and answers calls through the phone link, and you talk on the phone".into(),
        other => format!("no Bluetooth call adapter is implemented for {other} (Linux uses PipeWire's org.pipewire.Telephony)"),
    }
}

fn platform_label(os: &str) -> &'static str {
    match os {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => "other",
    }
}

impl CallBackend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "unavailable"
    }
    fn snapshot(&self) -> Result<Snapshot, BackendError> {
        Err(BackendError::Unavailable(self.reason.clone()))
    }
    fn invoke(&self, _action: &Action) -> Result<(), ControlError> {
        Err(ControlError::Unavailable(self.reason.clone()))
    }
    fn wait_for_change(&self, _timeout: Duration) -> Result<bool, BackendError> {
        Err(BackendError::Unavailable(self.reason.clone()))
    }
}

/// The backend for this build: PipeWire telephony over D-Bus on Linux, otherwise the explicit
/// unavailable backend. A Linux machine whose bus cannot be reached also yields
/// [`UnavailableBackend`], with the connection error as its reason.
pub fn default_backend() -> Box<dyn CallBackend> {
    #[cfg(target_os = "linux")]
    match crate::pipewire::PipeWireBackend::connect(crate::pipewire::PipeWireConfig::default()) {
        Ok(b) => Box::new(b),
        Err(e) => Box::new(UnavailableBackend::new("linux", e.to_string())),
    }
    #[cfg(not(target_os = "linux"))]
    Box::new(UnavailableBackend::for_this_platform())
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_and_macos_reasons_name_no_linux_tool() {
        for os in ["windows", "macos"] {
            let r = super::unavailable_reason(os);
            for linux in ["PipeWire", "pactl", "D-Bus", "bluetoothctl", "BlueZ"] {
                assert!(!r.contains(linux), "{os}: {r}");
            }
        }
        assert!(super::unavailable_reason("windows").contains("Phone calls"));
        assert!(super::unavailable_reason("macos").contains("talk on the phone"));
    }

    use super::*;
    use bridge_core::calls::{CallFeature, Sensitive, Support};

    #[test]
    fn unavailable_backend_refuses_everything_with_a_reason() {
        let b = UnavailableBackend::new("windows", "no adapter");
        assert!(matches!(b.snapshot(), Err(BackendError::Unavailable(r)) if r == "no adapter"));
        assert!(matches!(
            b.wait_for_change(Duration::ZERO),
            Err(BackendError::Unavailable(_))
        ));
        let dev = crate::DeviceRef {
            owner: ":1.1".into(),
            path: "/x".into(),
            address: Sensitive::new("AA"),
        };
        let err = b
            .invoke(&Action::Dial {
                device: dev,
                number: Sensitive::new("123"),
            })
            .unwrap_err();
        assert!(matches!(err, ControlError::Unavailable(_)));
        assert!(!err.outcome_unknown());
        let caps = b.capabilities();
        for f in CallFeature::ALL {
            assert!(
                matches!(caps.support(f), Support::Unsupported { .. }),
                "{f:?}"
            );
        }
    }

    #[test]
    fn this_platform_label_is_never_empty() {
        let b = UnavailableBackend::for_this_platform();
        assert!(!b.reason().is_empty());
        assert!(!b.platform().is_empty());
    }
}
