// SPDX-License-Identifier: GPL-3.0-or-later
//! The port between the calling engine and the operating system.
//!
//! [`CallsPlatform`] is the only seam to Windows. The real implementation
//! (`native::WinRtPlatform`, Windows only) talks to `Windows.ApplicationModel.Calls`; tests and
//! other hosts use [`crate::testing::MockPlatform`] or [`UnsupportedPlatform`].
//!
//! The methods are split by side effect. Reads may be called at any time, including from the
//! default diagnose path. [`CallsPlatform::request_access`], [`CallsPlatform::register`],
//! [`CallsPlatform::unregister`], [`CallsPlatform::connect`], [`CallsPlatform::dial`] and
//! [`CallsPlatform::call_op`] change Windows or phone state and are reached only through an
//! explicit engine action.
use crate::model::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A point in time after which a step must give up. Every blocking port call receives one.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);
impl Deadline {
    pub fn after(timeout: Duration) -> Self {
        Self(Instant::now() + timeout)
    }
    pub fn remaining(&self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }
    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
    /// The earlier of this deadline and `step` from now, so one slow step cannot consume the
    /// whole budget of a multi-step operation.
    pub fn capped(&self, step: Duration) -> Self {
        Self(self.0.min(Instant::now() + step))
    }
    pub fn instant(&self) -> Instant {
        self.0
    }
}

/// Something changed in Windows; the engine reacts by re-reading, never by trusting the signal's
/// payload. Signals carry no identity on purpose (`PhoneCallManager.CallStateChanged` has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    CallsChanged,
    LinesChanged,
    TransportsChanged,
}

/// Callback invoked from an operating-system thread. It must not block.
pub type SignalSink = Arc<dyn Fn(Signal) + Send + Sync>;

pub trait CallsPlatform {
    // ---- read-only -------------------------------------------------------------------------

    /// OS build, contract versions, package identity and the Phone calls privacy state.
    fn environment(&mut self, deadline: Deadline) -> Step<EnvironmentFacts>;

    /// Bluetooth phone-line transports Windows knows about (paired phones). Capped at
    /// [`MAX_DEVICES`].
    fn list_transports(&mut self, deadline: Deadline) -> Step<Vec<TransportRecord>>;

    /// Registration, routing and ringing state of one transport.
    fn transport_info(&mut self, id: &TransportDeviceId, deadline: Deadline)
    -> Step<TransportInfo>;

    /// Snapshot of the phone lines visible to this app. A phone's line is visible only while the
    /// app is registered for its transport and the link is up. Capped at [`MAX_LINES`].
    fn list_lines(&mut self, deadline: Deadline) -> Step<Vec<LineRecord>>;

    /// Active calls of one line.
    fn list_calls(&mut self, line: &LineId, deadline: Deadline) -> Step<CallListing>;

    // ---- change-notification (read-only) ---------------------------------------------------

    /// Start forwarding OS change notifications to `sink`. Idempotent: a second call replaces the
    /// sink. Observation only.
    fn start_signals(&mut self, sink: SignalSink) -> Step<()>;
    fn stop_signals(&mut self);

    // ---- explicit actions ------------------------------------------------------------------

    /// `PhoneLineTransportDevice.RequestAccessAsync`. May involve the operating system's consent
    /// logic, so it is never part of a probe.
    fn request_access(&mut self, id: &TransportDeviceId, deadline: Deadline) -> Step<AccessStatus>;

    /// `PhoneLineTransportDevice.RegisterApp` for this app only.
    fn register(&mut self, id: &TransportDeviceId) -> Step<()>;

    /// `PhoneLineTransportDevice.UnregisterApp` for this app only.
    fn unregister(&mut self, id: &TransportDeviceId) -> Step<()>;

    /// `PhoneLineTransportDevice.ConnectAsync`; `true` when Windows reports the connection made.
    fn connect(&mut self, id: &TransportDeviceId, deadline: Deadline) -> Step<bool>;

    /// `PhoneLine.DialWithResultAsync`.
    fn dial(&mut self, line: &LineId, number: &str, deadline: Deadline) -> Step<DialResult>;

    /// An operation on one call. The platform resolves the call afresh by id; it keeps no handle
    /// that could outlive the call.
    fn call_op(
        &mut self,
        call: &NativeCallId,
        op: CallOp,
        deadline: Deadline,
    ) -> Step<OperationStatus>;
}

/// Port for hosts without the Windows APIs. Every read reports a non-Windows environment and
/// nothing else is available, so diagnostics render a typed `NotWindows` blocker.
#[derive(Debug, Default)]
pub struct UnsupportedPlatform;

impl UnsupportedPlatform {
    fn unsupported<T>(context: &'static str) -> Step<T> {
        Err(PlatformError::new(PlatformErrorKind::Unsupported, context))
    }
}

impl CallsPlatform for UnsupportedPlatform {
    fn environment(&mut self, _: Deadline) -> Step<EnvironmentFacts> {
        Ok(EnvironmentFacts::not_windows())
    }
    fn list_transports(&mut self, _: Deadline) -> Step<Vec<TransportRecord>> {
        Self::unsupported("list_transports")
    }
    fn transport_info(&mut self, _: &TransportDeviceId, _: Deadline) -> Step<TransportInfo> {
        Self::unsupported("transport_info")
    }
    fn list_lines(&mut self, _: Deadline) -> Step<Vec<LineRecord>> {
        Self::unsupported("list_lines")
    }
    fn list_calls(&mut self, _: &LineId, _: Deadline) -> Step<CallListing> {
        Self::unsupported("list_calls")
    }
    fn start_signals(&mut self, _: SignalSink) -> Step<()> {
        Self::unsupported("start_signals")
    }
    fn stop_signals(&mut self) {}
    fn request_access(&mut self, _: &TransportDeviceId, _: Deadline) -> Step<AccessStatus> {
        Self::unsupported("request_access")
    }
    fn register(&mut self, _: &TransportDeviceId) -> Step<()> {
        Self::unsupported("register")
    }
    fn unregister(&mut self, _: &TransportDeviceId) -> Step<()> {
        Self::unsupported("unregister")
    }
    fn connect(&mut self, _: &TransportDeviceId, _: Deadline) -> Step<bool> {
        Self::unsupported("connect")
    }
    fn dial(&mut self, _: &LineId, _: &str, _: Deadline) -> Step<DialResult> {
        Self::unsupported("dial")
    }
    fn call_op(&mut self, _: &NativeCallId, _: CallOp, _: Deadline) -> Step<OperationStatus> {
        Self::unsupported("call_op")
    }
}
