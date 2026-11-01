//! The Windows implementation of [`CallsPlatform`] over `Windows.ApplicationModel.Calls`.
//!
//! Everything here runs on the service's actor thread inside a multithreaded WinRT apartment that
//! [`WinRtPlatform::new`] initialises and drops. Asynchronous operations are awaited with a
//! deadline (see `wait`); a deadline on a mutating operation surfaces as
//! [`PlatformErrorKind::TimedOut`], which the engine reports as *uncertain*.
//!
//! No Limited Access Feature is unlocked, no token or attestation is supplied, and an access-denied
//! HRESULT is returned as [`PlatformErrorKind::AccessDenied`] and nowhere worked around. Nothing in
//! this module has run on a Windows machine.
mod devices;
mod env;
mod lines;
mod signals;
mod wait;

use crate::model::*;
use crate::platform::{CallsPlatform, Deadline, SignalSink};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

/// Keeps the thread's WinRT apartment initialised.
struct Apartment;
impl Apartment {
    fn enter() -> Result<Self, PlatformError> {
        // SAFETY: balanced by `RoUninitialize` in `Drop`; called once on the owning thread.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|e| wait::classify(&e, "RoInitialize(MTA)"))?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `RoInitialize` in `enter` on this same thread.
        unsafe { RoUninitialize() };
    }
}

pub struct WinRtPlatform {
    // Declared first so subscriptions are removed before the apartment is torn down.
    signals: Option<signals::Signals>,
    _apartment: Apartment,
}

impl WinRtPlatform {
    /// Initialise the apartment on the current thread and build the port. Call from the thread
    /// that will own it (the service factory does).
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self {
            signals: None,
            _apartment: Apartment::enter()?,
        })
    }
}

impl CallsPlatform for WinRtPlatform {
    fn environment(&mut self, _: Deadline) -> Step<EnvironmentFacts> {
        Ok(env::environment())
    }
    fn list_transports(&mut self, deadline: Deadline) -> Step<Vec<TransportRecord>> {
        devices::list_transports(deadline)
    }
    fn transport_info(&mut self, id: &TransportDeviceId, _: Deadline) -> Step<TransportInfo> {
        devices::transport_info(id)
    }
    fn list_lines(&mut self, deadline: Deadline) -> Step<Vec<LineRecord>> {
        lines::list_lines(deadline)
    }
    fn list_calls(&mut self, line: &LineId, deadline: Deadline) -> Step<CallListing> {
        lines::list_calls(line, deadline)
    }

    fn start_signals(&mut self, sink: SignalSink) -> Step<()> {
        self.signals = None;
        self.signals = Some(signals::Signals::start(sink)?);
        Ok(())
    }
    fn stop_signals(&mut self) {
        self.signals = None;
    }

    fn request_access(&mut self, id: &TransportDeviceId, deadline: Deadline) -> Step<AccessStatus> {
        devices::request_access(id, deadline)
    }
    fn register(&mut self, id: &TransportDeviceId) -> Step<()> {
        devices::register(id)
    }
    fn unregister(&mut self, id: &TransportDeviceId) -> Step<()> {
        devices::unregister(id)
    }
    fn connect(&mut self, id: &TransportDeviceId, deadline: Deadline) -> Step<bool> {
        devices::connect(id, deadline)
    }
    fn dial(&mut self, line: &LineId, number: &str, deadline: Deadline) -> Step<DialResult> {
        lines::dial(line, number, deadline)
    }
    fn call_op(
        &mut self,
        call: &NativeCallId,
        op: CallOp,
        deadline: Deadline,
    ) -> Step<OperationStatus> {
        lines::call_op(call, op, deadline)
    }
}
