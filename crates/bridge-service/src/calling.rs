// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only view of the host's Bluetooth calling backend (`bridge-calls`).
//!
//! Only counts and a verdict are reported: no phone numbers, caller names or Bluetooth addresses
//! leave the probe, and no control method (answer, dial, hang up) is reachable from here.

use bridge_core::ipc::CallingStatus;

pub trait CallingProbe: Send + Sync {
    /// Blocking; called on a blocking thread with a deadline in the caller.
    fn probe(&self) -> CallingStatus;
}

/// Probes the real host through `bridge_calls::probe::run` (D-Bus introspection and property
/// reads on Linux; an explicit "unavailable" report elsewhere).
pub struct HostCallingProbe;

impl CallingProbe for HostCallingProbe {
    fn probe(&self) -> CallingStatus {
        let report = bridge_calls::probe::run(&bridge_calls::probe::ProbeOptions::default());
        let verdict = serde_json::to_value(&report.summary.verdict)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        CallingStatus {
            platform: report.platform,
            backend: report.backend.to_owned(),
            verdict,
            telephony_service_present: report.summary.telephony_service_present,
            bluetooth_controllers: report.summary.bluetooth_controllers,
            phones: report.summary.phones,
            calls: report.summary.calls,
            read_only: report.read_only,
        }
    }
}

/// For deployments and tests that must not touch the system bus.
pub struct DisabledCallingProbe;

impl CallingProbe for DisabledCallingProbe {
    fn probe(&self) -> CallingStatus {
        CallingStatus {
            platform: std::env::consts::OS.to_owned(),
            backend: "disabled".into(),
            verdict: "platform_unavailable".into(),
            telephony_service_present: false,
            bluetooth_controllers: None,
            phones: 0,
            calls: 0,
            read_only: true,
        }
    }
}
