// SPDX-License-Identifier: GPL-3.0-or-later
//! Application-level heartbeat for a trusted link, shared by the computer and the phone.
//!
//! A ping every [`EVERY`] keeps NAT / Wi-Fi access-point idle timers and the phone's network-idle
//! handling from treating the connection as dead, and lets each side notice a silent peer faster than
//! kernel keepalive alone. The peer's silence only counts once it has proved it heartbeats itself
//! (stock KDE Connect peers send no periodic ping), so an old or foreign peer is never cut off.
use std::time::{Duration, Instant};

/// Gap between our own pings.
pub const EVERY: Duration = Duration::from_secs(20);
/// A heartbeating peer that has said nothing at all for this long is considered gone (3+ missed pings).
pub const PEER_SILENCE_LIMIT: Duration = Duration::from_secs(70);

/// How early a timer tick may be and still count as an interval.
const TICK_SLACK: Duration = Duration::from_secs(1);
/// Gaps between two pings that look like a heartbeat (ours is [`EVERY`], twice that when a tick was missed).
const HEARTBEAT_GAP_MIN: Duration = Duration::from_secs(10);
const HEARTBEAT_GAP_MAX: Duration = Duration::from_secs(50);

#[derive(Debug, Clone)]
pub struct Heartbeat {
    last_sent: Instant,
    last_heard: Instant,
    last_ping: Option<Instant>,
    peer_pings: bool,
}

impl Heartbeat {
    pub fn new(now: Instant) -> Self {
        Self {
            last_sent: now,
            last_heard: now,
            last_ping: None,
            peer_pings: false,
        }
    }

    /// Any packet from the peer is proof of life. Two pings a heartbeat apart mark it as a heartbeating peer; one
    /// ping (stock KDE Connect's "send ping" button) or a burst of them does not.
    pub fn heard(&mut self, now: Instant, was_ping: bool) {
        self.last_heard = now;
        if was_ping {
            if let Some(prev) = self.last_ping {
                let gap = now.saturating_duration_since(prev);
                if (HEARTBEAT_GAP_MIN..=HEARTBEAT_GAP_MAX).contains(&gap) {
                    self.peer_pings = true;
                }
            }
            self.last_ping = Some(now);
        }
    }

    /// We wrote something (any packet counts as traffic for the idle timers).
    pub fn sent(&mut self, now: Instant) {
        self.last_sent = now;
    }

    /// Time to send our ping? The timer that asks runs on its own schedule while `sent` is stamped after the
    /// write, so a tick can land a few milliseconds short of [`EVERY`]; that must not skip a whole interval.
    pub fn ping_due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_sent) + TICK_SLACK >= EVERY
    }

    /// The peer heartbeats and has been silent for too long.
    pub fn peer_silent(&self, now: Instant) -> bool {
        self.peer_pings && now.saturating_duration_since(self.last_heard) > PEER_SILENCE_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn pings_are_due_every_interval_and_traffic_defers_them() {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        assert!(!h.ping_due(at(t0, 18)));
        assert!(h.ping_due(at(t0, 20)));
        h.sent(at(t0, 20));
        assert!(!h.ping_due(at(t0, 38)));
        assert!(h.ping_due(at(t0, 40)));
    }

    #[test]
    fn a_silent_peer_that_never_pinged_is_never_cut_off() {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        h.heard(t0, false);
        assert!(!h.peer_silent(at(t0, 3600)));
    }

    #[test]
    fn a_heartbeating_peer_times_out_after_the_limit_and_traffic_resets_it() {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        h.heard(at(t0, 5), true);
        h.heard(at(t0, 25), true);
        assert!(!h.peer_silent(at(t0, 95)));
        assert!(h.peer_silent(at(t0, 96)));
        h.heard(at(t0, 96), false); // any packet is proof of life
        assert!(!h.peer_silent(at(t0, 160)));
        assert!(h.peer_silent(at(t0, 167)));
    }

    #[test]
    fn review3_ticks_that_land_a_little_early_still_ping_every_interval() {
        // The timer fires on its own schedule while `sent` is stamped after the write: a tick 20 s after the
        // previous one sees slightly less than EVERY elapsed. It must not skip a whole interval.
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        let mut pings = 0;
        for n in 1..=6u64 {
            let tick = at(t0, 20 * n) + Duration::from_millis(2);
            if h.ping_due(tick) {
                pings += 1;
                h.sent(tick + Duration::from_millis(30)); // the write took 30 ms
            }
        }
        assert_eq!(pings, 6, "one ping per tick, not one per two");
    }

    #[test]
    fn review3_one_manual_ping_does_not_make_a_stock_peer_a_heartbeating_one() {
        // Stock KDE Connect sends a ping only when the user taps "send ping"; that single ping must not arm
        // the silence limit and cut the link 70 s later.
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        h.heard(at(t0, 5), true);
        assert!(!h.peer_silent(at(t0, 3600)));
        // Two quick manual pings are not a heartbeat either.
        h.heard(at(t0, 6), true);
        assert!(!h.peer_silent(at(t0, 3600)));
    }
}
