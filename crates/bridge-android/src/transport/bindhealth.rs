// SPDX-License-Identifier: GPL-3.0-or-later
//! Safety net for binding the core's sockets to the Wi-Fi address (docs/reports/wifi-binding-safety.md).
//!
//! A socket bound to an address the default network does not own can connect and then die (E2E-14), which a
//! connect-time fallback cannot see. Two signals say the bound path is bad: a bound dial whose TLS/identity
//! handshake does not finish within [`BOUND_BUDGET`], and a bound link that dies within [`EARLY_DEATH`] without
//! the computer ever having been heard. Either marks that Wi-Fi address unhealthy for [`UNHEALTHY_FOR`] (memory
//! only): dials and announcements leave unbound meanwhile, so a flapping network is not tried again every redial.
//!
//! A persistent switch (`bind_wifi`, pushed by the app) and `GB_BIND_WIFI=0` (host tests) turn binding off.
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A bound dial must connect and finish the TLS/identity handshake within this, or the bound path is blamed.
pub(super) const BOUND_BUDGET: Duration = Duration::from_secs(6);
/// A bound link that closes within this of starting, having heard nothing from the computer, is blamed.
pub(super) const EARLY_DEATH: Duration = Duration::from_secs(10);
/// How long an address stays unhealthy.
pub(super) const UNHEALTHY_FOR: Duration = Duration::from_secs(10 * 60);

/// Reason codes (also the `code` of the `wifi_bind_fallback` event; the debug "Link health" section shows them).
pub(super) const REASON_STALLED: &str = "bound_stalled";
pub(super) const REASON_IO: &str = "bound_io";
pub(super) const REASON_EARLY_DEATH: &str = "bound_early_death";

/// Unhealthy Wi-Fi addresses and when each was marked.
#[derive(Default)]
pub(super) struct Health {
    marked: HashMap<Ipv4Addr, Instant>,
}

impl Health {
    pub(super) fn mark(&mut self, ip: Ipv4Addr, now: Instant) {
        self.marked
            .retain(|_, at| now.saturating_duration_since(*at) < UNHEALTHY_FOR);
        self.marked.insert(ip, now);
    }

    pub(super) fn unhealthy(&self, ip: Ipv4Addr, now: Instant) -> bool {
        self.marked
            .get(&ip)
            .is_some_and(|at| now.saturating_duration_since(*at) < UNHEALTHY_FOR)
    }

    /// Seconds left of the mark on `ip`.
    #[cfg(test)]
    pub(super) fn left(&self, ip: Ipv4Addr, now: Instant) -> Option<Duration> {
        let at = self.marked.get(&ip)?;
        UNHEALTHY_FOR.checked_sub(now.saturating_duration_since(*at))
    }
}

static HEALTH: LazyLock<Mutex<Health>> = LazyLock::new(Mutex::default);

pub(super) fn mark_unhealthy(ip: Ipv4Addr) {
    HEALTH
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .mark(ip, Instant::now());
}

pub(super) fn is_unhealthy(ip: Ipv4Addr) -> bool {
    HEALTH
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unhealthy(ip, Instant::now())
}

/// Forget every mark (a test hook; a restart of the process does the same).
#[cfg(test)]
pub(super) fn clear() {
    *HEALTH.lock().unwrap_or_else(PoisonError::into_inner) = Health::default();
}

/// Tests share this process-wide state: they take this lock first.
#[cfg(test)]
pub(super) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `GB_BIND_WIFI` value that switches binding off.
fn env_disables(v: Option<&str>) -> bool {
    v.is_some_and(|v| matches!(v.trim(), "0" | "false" | "off" | "no"))
}

static ENABLED: LazyLock<AtomicBool> =
    LazyLock::new(|| AtomicBool::new(!env_disables(std::env::var("GB_BIND_WIFI").ok().as_deref())));

/// Is binding allowed at all? Default on; the app's `bind_wifi` preference and `GB_BIND_WIFI=0` turn it off.
pub(super) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub(super) fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Should a bound link that ended after `age` with `quiet` of silence and `code` count against the bound path?
/// It must be young, must never have heard the computer, and must not be an orderly close by the computer
/// (the cross-dial rule closes the loser that way).
pub(super) fn early_death(age: Duration, quiet: Duration, code: Option<&str>) -> bool {
    age < EARLY_DEATH
        && quiet + Duration::from_secs(1) >= age
        && code.is_some_and(|c| c != "peer_closed" && c != "goodbye")
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);
    const OTHER: Ipv4Addr = Ipv4Addr::new(192, 168, 7, 7);

    #[test]
    fn a_mark_lasts_ten_minutes_for_that_address_only() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert!(!h.unhealthy(IP, t0));
        h.mark(IP, t0);
        assert!(h.unhealthy(IP, t0));
        assert!(h.unhealthy(IP, t0 + Duration::from_secs(599)));
        assert!(!h.unhealthy(IP, t0 + Duration::from_secs(601)));
        assert!(!h.unhealthy(OTHER, t0), "another network starts clean");
        assert_eq!(
            h.left(IP, t0 + Duration::from_secs(100)),
            Some(Duration::from_secs(500))
        );
    }

    #[test]
    fn marking_again_prunes_expired_entries_and_restarts_the_window() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.mark(IP, t0);
        let later = t0 + Duration::from_secs(700);
        h.mark(OTHER, later);
        assert_eq!(h.marked.len(), 1, "the expired mark is gone");
        h.mark(OTHER, later + Duration::from_secs(500));
        assert!(h.unhealthy(OTHER, later + Duration::from_secs(1000)));
    }

    #[test]
    fn the_environment_switch() {
        for off in ["0", "false", "off", "no", " 0 "] {
            assert!(env_disables(Some(off)), "{off}");
        }
        for on in ["1", "true", "", "yes"] {
            assert!(!env_disables(Some(on)), "{on}");
        }
        assert!(!env_disables(None), "unset: binding stays on");
    }

    #[test]
    fn only_a_young_silent_failed_link_counts() {
        let s = Duration::from_secs;
        assert!(early_death(s(3), s(3), Some("conn_reset")));
        assert!(early_death(s(9), s(8), Some("tcp_timeout")));
        assert!(!early_death(s(11), s(11), Some("conn_reset")), "too old");
        assert!(
            !early_death(s(5), s(1), Some("conn_reset")),
            "the computer was heard"
        );
        assert!(
            !early_death(s(3), s(3), Some("peer_closed")),
            "an orderly close"
        );
        assert!(!early_death(s(3), s(3), None));
    }
}
