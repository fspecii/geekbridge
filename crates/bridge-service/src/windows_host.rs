// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Bluetooth hands-free calling for the call hub, through `bridge-calls-windows`.
//!
//! Windows owns the HFP link and the call audio; this adapter only observes (`watch`) and executes
//! the user's decisions as `bridge_core` commands. It never registers, connects or requests access:
//! those are explicit setup steps (see `packaging/windows/README.md`), so until
//! they are done the hub reports the first blocker in plain words instead of offering controls. The
//! crate compiles everywhere; off Windows its port reports `NotWindows`, and tests drive it with the
//! in-memory port.
use crate::call_control::HostCalls;
use bridge_calls_windows::{
    engine::Outcome,
    readiness::Blocker,
    service::{CallsHandle, CallsService, ServiceEvent},
};
use bridge_core::calls::{CallCapabilities, CallCommand, CallState};
use bridge_core::calls_view::SourceSnapshot;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub const SOURCE_KEY: &str = "hfp:windows";

pub struct WindowsHost {
    _service: CallsService,
    handle: CallsHandle,
    events: Receiver<ServiceEvent>,
    state: CallState,
    caps: Option<CallCapabilities>,
    label: String,
    /// What stops calls from working yet; `None` once a selected device is ready.
    blocked: Option<String>,
}

fn describe(b: Blocker) -> &'static str {
    match b {
        Blocker::NotWindows => "Windows calling is only available on Windows 11.",
        Blocker::UnsupportedOs { .. } => "Windows calling needs Windows 11.",
        Blocker::ContractMissing { .. } | Blocker::ContractUnknown => {
            "This Windows build does not offer the phone-call API."
        }
        Blocker::PackageIdentityMissing | Blocker::PackageIdentityUnknown => {
            "GeekBridge needs to be installed as a packaged app for Windows calling."
        }
        Blocker::PrivacyBlocked { .. } => {
            "Allow phone calls for GeekBridge in Settings > Privacy & security > Phone calls."
        }
        Blocker::NoPairedTransport => "Pair the phone with this PC over Bluetooth first.",
        Blocker::DeviceDisabled => "The phone's Bluetooth connection is disabled in Windows.",
        _ => "Windows calling is not set up for this phone yet.",
    }
}

impl WindowsHost {
    /// Reads the current state, selects the only paired phone (a read-only choice) and starts the
    /// read-only watcher.
    pub fn new(service: CallsService) -> Result<Self, String> {
        let handle = service.handle();
        let events = handle.subscribe();
        let snapshot = handle.refresh().map_err(|e| e.to_string())?;
        if snapshot.selected().is_none()
            && let [only] = snapshot.devices.as_slice()
        {
            handle.select(&only.id).map_err(|e| e.to_string())?;
        }
        handle.watch(true).map_err(|e| e.to_string())?;
        let mut host = Self {
            _service: service,
            handle,
            events,
            state: CallState::default(),
            caps: None,
            label: "Phone".into(),
            blocked: Some("Looking for a phone…".into()),
        };
        host.note_snapshot(&snapshot);
        Ok(host)
    }

    fn note_snapshot(&mut self, s: &bridge_calls_windows::engine::Snapshot) {
        self.blocked = match s.selected() {
            None if s.devices.is_empty() => Some(
                s.env_blockers
                    .first()
                    .map(|b| describe(*b))
                    .unwrap_or("No phone is paired with this PC over Bluetooth.")
                    .to_owned(),
            ),
            None => Some("Choose which paired phone to use for calls.".to_owned()),
            Some(d) => {
                self.label = d
                    .name
                    .as_ref()
                    .map(|n| n.expose().chars().take(80).collect())
                    .unwrap_or_else(|| "Phone".into());
                d.readiness
                    .control_blocker()
                    .map(|b| describe(b).to_owned())
            }
        };
    }
}

impl HostCalls for WindowsHost {
    fn label(&self) -> String {
        "Windows Bluetooth calling".into()
    }

    fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
        let mut next = self.events.recv_timeout(wait);
        loop {
            match next {
                Ok(ServiceEvent::Snapshot(s)) => self.note_snapshot(&s),
                Ok(ServiceEvent::Core {
                    events,
                    capabilities,
                }) => {
                    for e in events {
                        self.state.apply(e);
                    }
                    self.caps = Some(capabilities);
                }
                Ok(ServiceEvent::CoreReset { state }) => self.state = state,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("The Windows calling service stopped.".into());
                }
            }
            next = self.events.try_recv().map_err(|e| match e {
                std::sync::mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                std::sync::mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            });
        }
        if let Some(why) = &self.blocked {
            return Err(why.clone());
        }
        let Some(caps) = self.caps.clone() else {
            return Ok(Vec::new());
        };
        Ok(vec![SourceSnapshot {
            key: SOURCE_KEY.into(),
            label: self.label.clone(),
            caps,
            state: self.state.clone(),
        }])
    }

    fn execute(&mut self, _source: &str, command: &CallCommand) -> Result<(), String> {
        match self
            .handle
            .execute_core(command)
            .map_err(|e| e.to_string())?
            .outcome
        {
            Outcome::Succeeded => Ok(()),
            Outcome::Failed { .. } => Err("Windows refused the request.".into()),
            Outcome::Uncertain { .. } => {
                Err("Windows did not confirm the request; check the phone before retrying.".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_calls_windows::{
        model::NativeCallStatus,
        service::ServiceConfig,
        testing::{MockPlatform, call},
    };
    use bridge_core::calls::CallId;
    use bridge_core::calls_view;

    fn host(mock: &MockPlatform) -> WindowsHost {
        let m = mock.clone();
        let cfg = ServiceConfig {
            poll_interval: Duration::from_millis(40),
            debounce: Duration::from_millis(5),
            ..ServiceConfig::default()
        };
        WindowsHost::new(CallsService::spawn(move || Ok(m), cfg).unwrap()).unwrap()
    }

    fn poll_until(
        h: &mut WindowsHost,
        f: impl Fn(&Result<Vec<SourceSnapshot>, String>) -> bool,
    ) -> Result<Vec<SourceSnapshot>, String> {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let r = h.poll(Duration::from_millis(50));
            if f(&r) {
                return r;
            }
            assert!(std::time::Instant::now() < end, "timed out: {r:?}");
        }
    }

    #[test]
    fn an_unregistered_phone_reports_what_is_missing_instead_of_controls() {
        let mock = MockPlatform::fresh();
        let mut h = host(&mock);
        let r = poll_until(&mut h, |r| r.is_err());
        assert!(!r.unwrap_err().is_empty());
    }

    #[test]
    fn ringing_call_is_answerable_and_rejectable_through_the_core_commands() {
        let mock = MockPlatform::connected();
        let mut h = host(&mock);
        mock.set_calls(vec![call("c1", NativeCallStatus::Incoming)]);
        let snaps = poll_until(&mut h, |r| {
            r.as_ref()
                .is_ok_and(|s| s.first().is_some_and(|s| !s.state.calls.is_empty()))
        })
        .unwrap();
        let view = calls_view::build(&snaps);
        let row = view.ringing().expect("a ringing call");
        assert!(row.answer.is_some() && row.reject.is_some());
        assert_eq!(row.caller_label(), "Test Caller");

        let id = row.reject.as_ref().unwrap().call.clone();
        mock.clear_log();
        h.execute(SOURCE_KEY, &CallCommand::Reject { call: id.clone() })
            .unwrap();
        let log = mock.mutations();
        assert!(
            log.iter().any(|l| l.to_lowercase().contains("reject")),
            "{log:?}"
        );
        // An unknown call id is refused without touching the phone.
        mock.clear_log();
        assert!(
            h.execute(
                SOURCE_KEY,
                &CallCommand::Answer {
                    call: CallId::new("nope")
                }
            )
            .is_err()
        );
        assert!(mock.mutations().is_empty());
    }
}
