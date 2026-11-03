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

/// Wait after a silent loss before the computer dials a phone it has not heard announcing. The phone's own redial
/// (2 s, jittered) normally lands first; this one is for when the phone cannot: its timers stand still while it
/// sleeps (screen locked), so neither its redial nor its announcement goes out, and an incoming dial is what wakes it.
pub const FALLBACK_GRACE: Duration = Duration::from_secs(4);
/// Most phones a computer remembers a dial-back address for (paired phones only, so this is a safety cap).
const FALLBACK_KNOWN_MAX: usize = 64;

#[derive(Debug)]
struct FallbackEntry {
    tries: u32,
    next_at: Instant,
}

/// Dial-back without an announcement: after a paired phone's link is lost *silently* (reset, timeout; not a clean
/// close, a goodbye, a disconnect or an unpair), dial the address it was last reached on, with the same doubling
/// back-off and budget as [`Book`]. An announcing phone is left to [`Book`].
#[derive(Debug, Default)]
pub struct Fallback {
    known: HashMap<String, (std::net::IpAddr, u16)>,
    lost: HashMap<String, FallbackEntry>,
}

impl Fallback {
    /// Where `id` can be dialed: its announcement, the endpoint we dialed, or its address plus the port its secure
    /// identity declared.
    pub fn learn(&mut self, id: &str, ip: std::net::IpAddr, port: u16) {
        if self.known.len() < FALLBACK_KNOWN_MAX || self.known.contains_key(id) {
            self.known.insert(id.to_owned(), (ip, port));
        }
    }

    /// The link to `id` was lost without a goodbye or a clean close: start dialing it back after [`FALLBACK_GRACE`].
    pub fn lost(&mut self, id: &str, now: Instant) {
        if self.known.contains_key(id) {
            self.lost.insert(
                id.to_owned(),
                FallbackEntry {
                    tries: 0,
                    next_at: now + FALLBACK_GRACE,
                },
            );
        }
    }

    /// Linked again, announcing (the normal redial takes over), disconnected or unpaired: stop dialing back.
    pub fn settled(&mut self, id: &str) {
        self.lost.remove(id);
    }

    /// Unpaired: forget the address as well.
    pub fn forget(&mut self, id: &str) {
        self.lost.remove(id);
        self.known.remove(id);
    }

    /// Phones with a dial-back in progress.
    pub fn pending(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lost.keys().cloned().collect();
        v.sort();
        v
    }

    /// The endpoint to dial now, if a dial-back try is due. Records the try.
    pub fn due(&mut self, id: &str, now: Instant) -> Option<(std::net::IpAddr, u16)> {
        let addr = *self.known.get(id)?;
        let e = self.lost.get_mut(id)?;
        if e.tries >= MAX_TRIES || now < e.next_at {
            return None;
        }
        e.next_at = now
            + FALLBACK_GRACE
                .saturating_mul(1u32 << e.tries.min(10))
                .min(MAX_DELAY);
        e.tries += 1;
        Some(addr)
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
    fn a_silent_loss_is_dialed_back_after_a_grace_with_back_off_and_a_budget() {
        let mut f = Fallback::default();
        let t0 = Instant::now();
        f.lost("p", t0);
        assert!(f.pending().is_empty(), "no address known: nothing to do");
        f.learn("p", addr(5).0, 1716);
        f.lost("p", t0);
        assert_eq!(f.due("p", t0), None, "the phone's own redial goes first");
        assert_eq!(f.due("p", t0 + FALLBACK_GRACE), Some(addr(5)));
        assert_eq!(f.due("p", t0 + FALLBACK_GRACE), None);
        let mut t = t0 + FALLBACK_GRACE;
        let mut dials = 1;
        for _ in 0..10_000 {
            t += Duration::from_secs(1);
            if f.due("p", t).is_some() {
                dials += 1;
            }
        }
        assert_eq!(
            dials, MAX_TRIES,
            "bounded: a phone that is gone is not dialed forever"
        );
        // Linked again: a later loss starts afresh.
        f.settled("p");
        assert_eq!(f.due("p", t), None);
        f.lost("p", t);
        assert!(f.due("p", t + FALLBACK_GRACE).is_some());
        // Unpaired: the address goes too.
        f.forget("p");
        f.lost("p", t);
        assert!(f.pending().is_empty());
    }

    #[test]
    fn dial_back_gaps_double_from_the_grace_up_to_a_minute() {
        let mut f = Fallback::default();
        f.learn("p", addr(5).0, 1716);
        let t0 = Instant::now();
        f.lost("p", t0);
        let mut t = t0;
        let mut at = vec![];
        while at.len() < 8 {
            t += Duration::from_millis(500);
            if f.due("p", t).is_some() {
                at.push(t.duration_since(t0).as_secs());
            }
        }
        let gaps: Vec<u64> = at.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(at[0], 4);
        assert_eq!(gaps, vec![4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn the_address_book_is_capped() {
        let mut f = Fallback::default();
        for n in 0..1000 {
            f.learn(&format!("p{n}"), addr(5).0, 1716);
        }
        assert_eq!(f.known.len(), FALLBACK_KNOWN_MAX);
        f.learn("p0", addr(6).0, 1717); // known phones keep updating
        assert_eq!(f.known["p0"], (addr(6).0, 1717));
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
