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

/// Declared by GeekBridge on the phone in every identity packet, whatever optional features are on, so the
/// computer can tell it from a stock KDE Connect phone and heartbeat it. Carries no packets.
pub const MARKER: &str = "geekbridge.heartbeat";

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

    /// How long since anything was last heard from the peer (for the "silent for" part of a loss report).
    pub fn silent_for(&self, now: Instant) -> std::time::Duration {
        now.saturating_duration_since(self.last_heard)
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

    #[test]
    fn the_marker_alone_makes_a_heartbeat_peer() {
        let m = vec![MARKER.to_owned()];
        assert!(is_geekbridge_peer(&m, &m));
    }

    // ---- test-gaps-4: screen-off cadence and the silence rules, simulated the way the engines drive them

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Run the timer loop both engines have: a tick every `EVERY` (a little late), a ping when due, the write
    /// stamped afterwards. `idle_at(t)` says whether the screen is off at second `t`. Returns ping times (s).
    fn simulate(minutes: u64, idle_at: impl Fn(u64) -> bool) -> Vec<u64> {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        let mut pings = Vec::new();
        let mut tick = t0;
        while tick < t0 + Duration::from_secs(minutes * 60) {
            tick += EVERY + ms(2); // timer lateness
            let secs = tick.duration_since(t0).as_secs();
            if h.ping_due_idle(tick, idle_at(secs)) {
                pings.push(secs);
                h.sent(tick + ms(30)); // the write took 30 ms
            }
        }
        pings
    }

    #[test]
    fn with_the_screen_on_a_ping_goes_out_every_tick_and_with_it_off_every_third_tick() {
        let on = simulate(10, |_| false);
        let off = simulate(10, |_| true);
        assert_eq!(on.len(), 30, "{on:?}");
        assert_eq!(off.len(), 10, "{off:?}");
        let gaps: Vec<u64> = off.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| (59..=62).contains(g)), "{gaps:?}");
        let on_gaps: Vec<u64> = on.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(on_gaps.iter().all(|g| (19..=21).contains(g)), "{on_gaps:?}");
    }

    #[test]
    fn the_idle_gap_stays_inside_the_peers_silence_limit_with_room_for_a_late_tick() {
        let off = simulate(30, |_| true);
        let worst = off.windows(2).map(|w| w[1] - w[0]).max().unwrap();
        // The peer checks on its own 20 s ticks, so the worst case it can see is one gap plus one tick.
        assert!(
            Duration::from_secs(worst) + EVERY < PEER_SILENCE_LIMIT + EVERY,
            "worst idle gap {worst} s"
        );
        assert!(Duration::from_secs(worst) < PEER_SILENCE_LIMIT - Duration::from_secs(5));
    }

    #[test]
    fn switching_the_screen_back_on_pings_at_the_next_tick_not_a_minute_later() {
        // Off until 4:50, on afterwards; the last idle ping was at 4:00.
        let pings = simulate(8, |t| t < 290);
        let after: Vec<u64> = pings.iter().copied().filter(|t| *t >= 290).collect();
        assert_eq!(
            after.first().copied().map(|t| t / 10),
            Some(30),
            "{pings:?}"
        ); // 300 s, the next tick
        assert!(after.windows(2).all(|w| w[1] - w[0] <= 21), "{after:?}");
        // And going quiet again after being active slows down to every third tick.
        let pings = simulate(8, |t| !(120..240).contains(&t));
        let quiet: Vec<u64> = pings.iter().copied().filter(|t| *t > 260).collect();
        assert!(quiet.windows(2).all(|w| w[1] - w[0] >= 59), "{quiet:?}");
    }

    #[test]
    fn the_idle_boundary_is_one_slack_before_the_interval() {
        let t0 = Instant::now();
        let h = Heartbeat::new(t0);
        assert!(!h.ping_due_idle(at(t0, 58), true));
        assert!(!h.ping_due_idle(t0 + Duration::from_millis(58_999), true));
        assert!(h.ping_due_idle(at(t0, 59), true));
        assert!(!h.ping_due_idle(at(t0, 18), false));
        assert!(h.ping_due_idle(at(t0, 19), false));
        // A clock that reads earlier than the last send never makes a ping due.
        let mut h = Heartbeat::new(at(t0, 100));
        h.sent(at(t0, 100));
        assert!(!h.ping_due_idle(t0, true) && !h.ping_due(t0));
    }

    #[test]
    fn other_traffic_defers_the_idle_ping_too() {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        h.sent(at(t0, 50));
        assert!(!h.ping_due_idle(at(t0, 100), true));
        assert!(!h.ping_due_idle(at(t0, 108), true));
        assert!(h.ping_due_idle(at(t0, 109), true));
        // Hearing the peer is not sending: it does not defer our ping.
        let mut h = Heartbeat::new(t0);
        h.heard(at(t0, 55), false);
        assert!(h.ping_due_idle(at(t0, 59), true));
    }

    #[test]
    fn a_peer_that_pinged_before_going_idle_is_still_judged_by_the_silence_limit() {
        // Screen on for two minutes (pings 20 s apart), then off (pings 60 s apart).
        let t0 = Instant::now();
        let mut peer = Heartbeat::new(t0);
        for t in (20..=120).step_by(20) {
            peer.heard(at(t0, t), true);
        }
        let mut last = 120;
        for _ in 0..20 {
            last += 60;
            peer.heard(at(t0, last), true);
            // Checked on the peer's own ticks, 20 s apart, all the way to the next ping.
            for probe in (last..last + 60).step_by(20) {
                assert!(
                    !peer.peer_silent(at(t0, probe)),
                    "cut off at {probe} after a ping at {last}"
                );
            }
        }
        // If it then really goes quiet, it is cut off once the limit has passed.
        assert!(!peer.peer_silent(at(t0, last + 70)));
        assert!(peer.peer_silent(at(t0, last + 71)));
    }

    #[test]
    fn a_peer_seen_only_pinging_a_minute_apart_is_never_cut_off() {
        // Pings 60 s apart are outside what counts as a heartbeat, so the peer is never marked and never
        // timed out here: dead-peer detection is left to the socket (fail safe, never a false cut-off).
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        for n in 1..=10 {
            h.heard(at(t0, 60 * n), true);
        }
        assert!(!h.peer_silent(at(t0, 100_000)));
    }

    #[test]
    fn the_heartbeat_gap_window_includes_both_ends_and_excludes_just_outside() {
        let armed = |gap_ms: u64| {
            let t0 = Instant::now();
            let mut h = Heartbeat::new(t0);
            h.heard(t0, true);
            h.heard(t0 + ms(gap_ms), true);
            h.peer_silent(t0 + ms(gap_ms) + PEER_SILENCE_LIMIT + ms(1))
        };
        assert!(armed(10_000) && armed(20_000) && armed(50_000));
        assert!(!armed(9_999) && !armed(50_001) && !armed(0) && !armed(60_000));
    }

    #[test]
    fn a_burst_of_pings_never_arms_the_silence_rule_but_a_later_real_pair_does() {
        let t0 = Instant::now();
        let mut h = Heartbeat::new(t0);
        for n in 0..200 {
            h.heard(t0 + ms(n * 500), true); // a ping every half second
        }
        assert!(!h.peer_silent(at(t0, 10_000)));
        // Other packets in between do not break the measuring of the gap between pings.
        h.heard(at(t0, 200), true);
        h.heard(at(t0, 210), false);
        h.heard(at(t0, 225), true); // 25 s after the previous ping
        assert!(h.peer_silent(at(t0, 225 + 71)));
    }

    #[test]
    fn capability_matching_is_by_the_geekbridge_prefix_only() {
        let caps = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(!is_geekbridge_peer(&[], &[]));
        assert!(!is_geekbridge_peer(
            &caps(&["geekbridge"]),
            &caps(&["Geekbridge.x", "xgeekbridge.x"])
        ));
        assert!(!is_geekbridge_peer(
            &caps(&["kdeconnect.geekbridge.x"]),
            &[]
        ));
        assert!(is_geekbridge_peer(&[], &caps(&["geekbridge.calls.state"])));
        assert!(is_geekbridge_peer(&caps(&["geekbridge."]), &[]));
        assert_eq!(MARKER, "geekbridge.heartbeat");
        assert!(MARKER.starts_with("geekbridge."));
    }

    #[test]
    fn the_constants_keep_their_documented_relations() {
        assert!(EVERY < IDLE_EVERY && IDLE_EVERY < PEER_SILENCE_LIMIT);
        assert!(PEER_SILENCE_LIMIT >= EVERY * 3, "three missed pings");
        assert!(HEARTBEAT_GAP_MIN < EVERY && EVERY < HEARTBEAT_GAP_MAX);
        assert!(TICK_SLACK < EVERY);
    }
}
