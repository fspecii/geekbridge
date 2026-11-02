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
/// Gap between our own pings while the phone screen is off and nobody is using the link. Still well inside
/// typical AP/NAT idle timeouts (minutes); the peer keeps its own cadence, so silence detection is unchanged.
pub const IDLE_EVERY: Duration = Duration::from_secs(60);
/// A heartbeating peer that has said nothing at all for this long is considered gone (3+ missed pings).
pub const PEER_SILENCE_LIMIT: Duration = Duration::from_secs(70);

/// How early a timer tick may be and still count as an interval.
const TICK_SLACK: Duration = Duration::from_secs(1);
/// Gaps between two pings that look like a heartbeat (ours is [`EVERY`], twice that when a tick was missed).
const HEARTBEAT_GAP_MIN: Duration = Duration::from_secs(10);
const HEARTBEAT_GAP_MAX: Duration = Duration::from_secs(50);

/// Does this peer take part in our heartbeat? A stock KDE Connect peer shows every `kdeconnect.ping` as a
/// "Ping!" notification, so the periodic ping only goes to peers that declared a `geekbridge.*` capability.
pub fn is_geekbridge_peer(incoming: &[String], outgoing: &[String]) -> bool {
    incoming
        .iter()
        .chain(outgoing)
        .any(|c| c.starts_with("geekbridge."))
}

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
        self.ping_due_idle(now, false)
    }

    /// As [`Self::ping_due`], at the slower [`IDLE_EVERY`] cadence while `idle` (screen off). Going back to
    /// active makes a ping due on the next tick when the last one is older than [`EVERY`].
    pub fn ping_due_idle(&self, now: Instant, idle: bool) -> bool {
        let every = if idle { IDLE_EVERY } else { EVERY };
        now.saturating_duration_since(self.last_sent) + TICK_SLACK >= every
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
    fn idle_pings_are_three_times_rarer_and_wake_up_on_the_next_tick() {
        let t0 = Instant::now();
        let h = Heartbeat::new(t0);
        assert!(!h.ping_due_idle(at(t0, 20), true));
        assert!(!h.ping_due_idle(at(t0, 40), true));
        assert!(h.ping_due_idle(at(t0, 60), true));
        // Screen on again 45 s after the last ping: due at once, not 15 s later.
        assert!(h.ping_due_idle(at(t0, 45), false));
        assert_eq!(
            IDLE_EVERY.as_secs() % EVERY.as_secs(),
            0,
            "idle cadence is a whole number of ticks"
        );
        assert!(
            IDLE_EVERY < PEER_SILENCE_LIMIT,
            "an idle phone must still be heard within the peer's limit"
        );
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

    #[test]
    fn only_peers_that_declare_a_geekbridge_capability_get_the_heartbeat() {
        let caps = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let stock = caps(&[
            "kdeconnect.ping",
            "kdeconnect.clipboard",
            "kdeconnect.share.request",
        ]);
        assert!(!is_geekbridge_peer(&stock, &stock));
        assert!(is_geekbridge_peer(
            &stock,
            &caps(&["kdeconnect.ping", "geekbridge.clipboard.pull"])
        ));
        assert!(is_geekbridge_peer(&caps(&["geekbridge.debug.log"]), &stock));
    }
}
