// SPDX-License-Identifier: GPL-3.0-or-later
//! Which paired computers may use the phone-side opt-ins (call control, contacts, calendar, calendar editing, SMS, battery, notification mirroring).
//!
//! The app's switches say *what* the phone may share; this says *with which computer*. Until the app sends a
//! list nothing is restricted (`None`); once it has, a feature with no entry for a computer is closed to it:
//! its requests are refused or dropped, and nothing of it is sent there (not even the call state replayed to
//! a newly connected computer).
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use super::{Core, Ctl, Phase};

pub(in crate::transport) const FEATURES: [&str; 9] = [
    "calls",
    "contacts",
    "calendar",
    "calendar_edit",
    "sms",
    "battery",
    "notifications",
    "photos",
    // EXPERIMENTAL call audio over Wi-Fi; only a debug build with the `callwifi` feature ever acts on it.
    "callaudio",
];

/// feature -> device ids allowed to use it.
pub(in crate::transport) type PermitMap = HashMap<String, HashSet<String>>;

#[derive(Default)]
pub(in crate::transport) struct Permits(Mutex<Option<PermitMap>>);

impl Permits {
    /// Replaces the list; returns what it was (`None` = nothing had been set, so nobody was restricted).
    pub(in crate::transport) fn set(&self, map: PermitMap) -> Option<PermitMap> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(map)
    }

    pub(in crate::transport) fn allows(&self, feature: &str, device: &str) -> bool {
        match &*self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            None => true,
            Some(m) => m.get(feature).is_some_and(|s| s.contains(device)),
        }
    }
}

impl Core {
    pub(in crate::transport) fn set_permits(&self, map: PermitMap) {
        let before = self.permits.set(map);
        // A computer whose grant was withdrawn loses a live call-audio session at once.
        self.callwifi_revalidate();
        // A computer that was just allowed call control is told what the phone is doing now. Its own request for the
        // state (made when it switched Telephony on) was refused while it was not allowed and is not repeated, and an
        // idle or ringing phone has no change to report, so without this it saw nothing until the next call event.
        if let Some(before) = before {
            let newly = self.newly_permitted("calls", &before);
            self.calls_state_to(&newly);
        }
    }

    /// Trusted connections whose computer may use `feature` now but was not in `before`.
    fn newly_permitted(&self, feature: &str, before: &PermitMap) -> Vec<u64> {
        let st = self.lock();
        st.conns
            .values()
            .filter(|c| c.phase == Phase::Trusted)
            .filter(|c| self.permits.allows(feature, c.peer_id.as_str()))
            .filter(|c| {
                !before
                    .get(feature)
                    .is_some_and(|s| s.contains(c.peer_id.as_str()))
            })
            .map(|c| c.id)
            .collect()
    }

    /// May the computer on this connection use `feature`?
    pub(in crate::transport) fn permitted(&self, feature: &str, connection_id: u64) -> bool {
        let device = self
            .lock()
            .conns
            .get(&connection_id)
            .map(|c| c.peer_id.to_string());
        device.is_some_and(|d| self.permits.allows(feature, &d))
    }

    /// Does the computer on this connection declare the debug packets (only a computer built to read them)?
    pub(in crate::transport) fn peer_debug(&self, connection_id: u64) -> bool {
        self.lock()
            .conns
            .get(&connection_id)
            .is_some_and(|c| c.peer_debug && c.phase == Phase::Trusted)
    }

    /// Control handles of the trusted connections that declared the debug packets (optionally one connection).
    pub(super) fn debug_ctls(&self, only: Option<u64>) -> Vec<tokio::sync::mpsc::Sender<Ctl>> {
        let st = self.lock();
        st.conns
            .values()
            .filter(|c| {
                c.phase == Phase::Trusted && c.peer_debug && only.is_none_or(|id| id == c.id)
            })
            .map(|c| c.ctl.clone())
            .collect()
    }

    /// Control handles of the trusted connections allowed to use `feature` that also declared they accept
    /// `kind` (optionally only one connection).
    pub(super) fn permitted_ctls(
        &self,
        feature: &str,
        kind: &str,
        only: Option<u64>,
    ) -> Vec<tokio::sync::mpsc::Sender<Ctl>> {
        let st = self.lock();
        st.conns
            .values()
            .filter(|c| c.phase == Phase::Trusted && only.is_none_or(|id| id == c.id))
            .filter(|c| self.permits.allows(feature, c.peer_id.as_str()))
            .filter(|c| c.peer_accepts.iter().any(|a| a == kind))
            .map(|c| c.ctl.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrestricted_until_set_then_closed_by_default() {
        let p = Permits::default();
        assert!(p.allows("sms", "a"));
        p.set(PermitMap::from([(
            "sms".into(),
            HashSet::from(["a".to_owned()]),
        )]));
        assert!(p.allows("sms", "a"));
        assert!(!p.allows("sms", "b"));
        assert!(!p.allows("calls", "a"));
    }
}
