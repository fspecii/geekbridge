// SPDX-License-Identifier: GPL-3.0-or-later
//! When a paired phone's link drops, dial it again by ourselves. Only paired phones, only while the phone keeps
//! announcing itself, with a growing delay between tries and a hard cap, so a phone that is gone or has been told
//! to disconnect is never hammered. Pure bookkeeping here; the daemon owns the dialing and checks trust.
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

/// First wait after a failed try; doubled each time up to [`MAX_DELAY`].
const FIRST_DELAY: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(60);
/// After this many failed tries in a row we wait for the phone to disappear and announce again.
pub const MAX_TRIES: u32 = 12;

#[derive(Debug)]
struct Entry {
    tries: u32,
    next_at: Instant,
    /// Where the last try went; a new address is a new chance.
    last_addr: Option<(std::net::IpAddr, u16)>,
    /// The user disconnected this phone by hand: leave it alone until it has been silent and came back.
    paused: bool,
    /// A bounded pause (the phone was told goodbye and will stay away as long): ends by itself at this time.
    pause_until: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Book {
    entries: HashMap<String, Entry>,
}

impl Book {
    /// Is a dial to `addr` due now? Records the try and the next allowed time.
    pub fn due(&mut self, id: &str, addr: (std::net::IpAddr, u16), now: Instant) -> bool {
        let e = self.entries.entry(id.to_owned()).or_insert(Entry {
            tries: 0,
            next_at: now,
            last_addr: None,
            paused: false,
            pause_until: None,
        });
        if e.paused && e.pause_until.is_some_and(|u| now >= u) {
            *e = Entry {
                tries: 0,
                next_at: now,
                last_addr: None,
                paused: false,
                pause_until: None,
            };
        }
        if e.paused {
            return false;
        }
        if e.last_addr.is_some_and(|a| a != addr) && e.tries >= MAX_TRIES {
            // The phone moved after we gave up: two more chances, still spaced by the old wait. A phone that
            // announces from alternating addresses must not reset the back-off with every flip.
            e.tries = MAX_TRIES - 2;
        }
        if e.tries >= MAX_TRIES || now < e.next_at {
            return false;
        }
        e.last_addr = Some(addr);
        let delay = FIRST_DELAY
            .saturating_mul(1u32 << e.tries.min(10))
            .min(MAX_DELAY);
        e.tries += 1;
        e.next_at = now + delay;
        true
    }

    /// Connected (or unpaired): forget the history so the next drop starts with a quick retry.
    pub fn settled(&mut self, id: &str) {
        self.entries.remove(id);
    }

    /// The user disconnected this phone: no automatic redial until it has been silent for a while.
    pub fn pause(&mut self, id: &str, now: Instant) {
        self.entries.insert(
            id.to_owned(),
            Entry {
                tries: 0,
                next_at: now,
                last_addr: None,
                paused: true,
                pause_until: None,
            },
        );
    }

    /// Like [`Book::pause`] but only for `span`: used for a GeekBridge phone that was sent a goodbye and stays
    /// off the computer for [`bridge_core::goodbye::SUPPRESS_FOR`], so both ends resume at about the same time.
    pub fn pause_for(&mut self, id: &str, now: Instant, span: Duration) {
        self.pause(id, now);
        if let Some(e) = self.entries.get_mut(id) {
            e.pause_until = Some(now + span);
        }
    }

    /// Phones that are not announcing any more: drop their history (and a pause).
    pub fn keep_only(&mut self, announcing: &[&str]) {
        self.entries
            .retain(|id, _| announcing.contains(&id.as_str()));
    }

    /// Redial attempts made for this phone since its link dropped, and whether the user paused it.
    pub fn progress(&self, id: &str) -> (u32, bool) {
        self.entries
            .get(id)
            .map_or((0, false), |e| (e.tries, e.paused))
    }

    /// Phones we are still trying to reach, for the "Reconnecting" state.
    pub fn trying(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| !e.paused && e.tries > 0 && e.tries < MAX_TRIES)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addr(last: u8) -> (std::net::IpAddr, u16) {
        (std::net::IpAddr::from([192, 168, 1, last]), 1716)
    }

    #[test]
    fn progress_reports_tries_and_pause() {
        let mut b = Book::default();
        let t0 = Instant::now();
        assert_eq!(b.progress("p"), (0, false));
        assert!(b.due("p", addr(5), t0));
        assert_eq!(b.progress("p"), (1, false));
        b.pause("p", t0);
        assert_eq!(b.progress("p"), (0, true));
    }

    #[test]
    fn backs_off_then_gives_up_until_the_phone_moves() {
        let mut b = Book::default();
        let t0 = Instant::now();
        assert!(b.due("p", addr(5), t0));
        assert!(!b.due("p", addr(5), t0 + Duration::from_secs(1)));
        assert!(b.due("p", addr(5), t0 + Duration::from_secs(2)));
        assert!(!b.due("p", addr(5), t0 + Duration::from_secs(5)));
        let mut t = t0 + Duration::from_secs(6);
        let mut tries = 2;
        while tries < MAX_TRIES {
            t += Duration::from_secs(61);
            assert!(b.due("p", addr(5), t));
            tries += 1;
        }
        assert!(!b.due("p", addr(5), t + Duration::from_secs(3600)));
        assert!(b.due("p", addr(6), t + Duration::from_secs(3600)));
    }

    #[test]
    fn review3_a_phone_announcing_from_alternating_addresses_is_not_dialed_in_a_storm() {
        let mut b = Book::default();
        let t0 = Instant::now();
        let mut dials = 0;
        for n in 0..300u64 {
            // the daemon ticks every 2 s; the announced address flips each time (wifi + another interface)
            let a = addr(if n % 2 == 0 { 5 } else { 6 });
            if b.due("p", a, t0 + Duration::from_secs(2 * n)) {
                dials += 1;
            }
        }
        assert!(dials <= 25, "{dials} dials in ten minutes");
    }

    #[test]
    fn a_manual_disconnect_is_respected_until_silence() {
        let mut b = Book::default();
        let t = Instant::now();
        b.pause("p", t);
        assert!(!b.due("p", addr(5), t + Duration::from_secs(100)));
        b.keep_only(&[]);
        assert!(b.due("p", addr(5), t + Duration::from_secs(100)));
    }

    #[test]
    fn a_goodbye_pause_ends_when_the_phone_stops_staying_away() {
        let mut b = Book::default();
        let t = Instant::now();
        let span = bridge_core::goodbye::SUPPRESS_FOR;
        b.pause_for("p", t, span);
        assert!(!b.due("p", addr(5), t + span - Duration::from_secs(1)));
        assert_eq!(b.progress("p"), (0, true));
        // Same moment as the phone's own redial resumes: dial at once, with a fresh budget.
        assert!(b.due("p", addr(5), t + span));
        assert_eq!(b.progress("p"), (1, false));
        // A plain pause has no end of its own.
        b.pause("q", t);
        assert!(!b.due("q", addr(5), t + span * 10));
    }

    #[test]
    fn reports_who_is_being_retried() {
        let mut b = Book::default();
        let t = Instant::now();
        b.due("p", addr(5), t);
        assert_eq!(b.trying(), vec!["p".to_owned()]);
        b.settled("p");
        assert!(b.trying().is_empty());
    }
}
