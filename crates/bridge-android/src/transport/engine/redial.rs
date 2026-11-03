// SPDX-License-Identifier: GPL-3.0-or-later
//! The phone's side of reconnecting: when a trusted link is lost, dial the computer again at the
//! address it was last reached on, with a growing delay. Pure bookkeeping; the engine owns the
//! dialing, checks that the device is still paired and goes through `begin_attempt`/`admit`, so a
//! redial that crosses the computer's own redial still leaves exactly one connection.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use bridge_kde::DeviceId;

/// KDE Connect's TCP port, used when a computer only ever dialed us and its listening port is unknown.
pub(super) const DEFAULT_PORT: u16 = 1716;
const FIRST_DELAY: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(60);
/// Failed tries in a row before this phone stops; a later connection, from either side, starts afresh.
pub(super) const MAX_TRIES: u32 = 12;

/// Where each computer was last reached, kept for the life of the process: a network change restarts the
/// transport (to announce on the new network), and the restarted transport dials these addresses again.
/// LAN addresses only; a loopback address (tests, the emulator's host alias) is never carried over.
fn remembered() -> &'static Mutex<HashMap<DeviceId, SocketAddr>> {
    static R: OnceLock<Mutex<HashMap<DeviceId, SocketAddr>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// Computers that said goodbye on purpose, and until when this phone stays off them. Process-wide like
/// [`remembered`]: a network change restarts the transport, which must not undo the computer's goodbye.
fn suppressed() -> &'static Mutex<HashMap<DeviceId, Instant>> {
    static S: OnceLock<Mutex<HashMap<DeviceId, Instant>>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// The computer asked, politely, to be left alone for `span` (see `bridge_core::goodbye`).
pub(super) fn suppress(device: &DeviceId, now: Instant, span: Duration) {
    suppressed()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(device.clone(), now + span);
}

/// How much longer to stay away from `device`; zero when it did not say goodbye or the time is up.
pub(super) fn suppressed_for(device: &DeviceId, now: Instant) -> Duration {
    let mut map = suppressed().lock().unwrap_or_else(PoisonError::into_inner);
    match map.get(device).copied() {
        Some(until) if until > now => until - now,
        Some(_) => {
            map.remove(device);
            Duration::ZERO
        }
        None => Duration::ZERO,
    }
}

/// The user connected by hand, or any link came up: the goodbye no longer applies.
pub(crate) fn unsuppress(device: &DeviceId) {
    suppressed()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(device);
}

/// The back-off before try number `tries` (0-based): 2 s doubling to 60 s.
fn nominal_delay(tries: u32) -> Duration {
    FIRST_DELAY
        .saturating_mul(1u32 << tries.min(10))
        .min(MAX_DELAY)
}

/// Jitter: `delay` scaled into 0.8..1.2 by `unit` (0..1). The computer redials on its own schedule too, and both ends
/// noticing the same loss at the same instant would otherwise dial each other at the same instants at every step (each
/// such cross-dial costs a TLS handshake that the tie-break then throws away); with several phones behind one computer
/// restarting (a router reboot), it also keeps them from arriving in lockstep.
pub(super) fn spread(delay: Duration, unit: f64) -> Duration {
    delay.mul_f64(0.8 + 0.4 * unit.clamp(0.0, 1.0))
}

/// A cheap uniform-ish number in 0..1, different on every call; std's per-process random hash keys are the source.
fn random_unit() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[derive(Debug, Default)]
struct Entry {
    addr: Option<SocketAddr>,
    tries: u32,
    /// A redial task is running for this device.
    active: bool,
}

#[derive(Debug, Default)]
pub(super) struct Book {
    entries: HashMap<DeviceId, Entry>,
}

impl Book {
    /// A connection to `device` was established; `dialed` is the endpoint we dialed, if we did.
    /// An inbound connection only tells us the computer's IP; its listening port comes from the GeekBridge
    /// field of its secure identity (`hint`), else the port we already know, else the default.
    pub(super) fn connected(
        &mut self,
        device: &DeviceId,
        peer_ip: std::net::IpAddr,
        dialed: Option<SocketAddr>,
        hint: Option<u16>,
    ) {
        let e = self.entries.entry(device.clone()).or_default();
        e.tries = 0;
        unsuppress(device);
        let addr = match dialed {
            Some(a) => a,
            None => SocketAddr::new(
                peer_ip,
                hint.or(e.addr.map(|a| a.port())).unwrap_or(DEFAULT_PORT),
            ),
        };
        e.addr = Some(addr);
        if !addr.ip().is_loopback() {
            remembered()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(device.clone(), addr);
        }
    }

    /// A restarted transport: take over the addresses of the previous run.
    pub(super) fn carry_over(&mut self) {
        let known = remembered().lock().unwrap_or_else(PoisonError::into_inner);
        for (device, addr) in known.iter() {
            self.entries.entry(device.clone()).or_default().addr = Some(*addr);
        }
    }

    /// Devices with an address to dial.
    pub(super) fn dialable(&self) -> Vec<DeviceId> {
        self.entries
            .iter()
            .filter(|(_, e)| e.addr.is_some())
            .map(|(d, _)| d.clone())
            .collect()
    }

    pub(super) fn addr(&self, device: &DeviceId) -> Option<SocketAddr> {
        self.entries.get(device).and_then(|e| e.addr)
    }

    /// Claim the single redial task for `device`; false if one already runs or there is nowhere to dial.
    pub(super) fn begin(&mut self, device: &DeviceId) -> bool {
        match self.entries.get_mut(device) {
            Some(e) if !e.active && e.addr.is_some() => {
                e.active = true;
                true
            }
            _ => false,
        }
    }

    pub(super) fn end(&mut self, device: &DeviceId) {
        if let Some(e) = self.entries.get_mut(device) {
            e.active = false;
        }
    }

    /// Wait before the next try, or `None` once the budget is spent. Spread by +-20 % (see [`spread`]).
    pub(super) fn next_delay(&mut self, device: &DeviceId) -> Option<Duration> {
        let e = self.entries.get_mut(device)?;
        if e.tries >= MAX_TRIES {
            return None;
        }
        let delay = nominal_delay(e.tries);
        e.tries += 1;
        Some(spread(delay, random_unit()))
    }

    /// A spent budget gets one more try (the app nudged us: screen on, Doze window). True if it was spent.
    pub(super) fn grant_one(&mut self, device: &DeviceId) -> bool {
        match self.entries.get_mut(device) {
            Some(e) if e.tries >= MAX_TRIES => {
                e.tries = MAX_TRIES - 1;
                true
            }
            _ => false,
        }
    }

    pub(super) fn forget(&mut self, device: &DeviceId) {
        self.entries.remove(device);
        unsuppress(device);
        remembered()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> DeviceId {
        DeviceId::parse(&c.to_string().repeat(32)).unwrap()
    }
    fn ip(last: u8) -> std::net::IpAddr {
        std::net::IpAddr::from([192, 168, 1, last])
    }

    #[test]
    fn delays_double_up_to_the_cap_then_the_budget_ends() {
        let mut b = Book::default();
        b.connected(&id('a'), ip(150), None, None);
        let got: Vec<Duration> = std::iter::from_fn(|| b.next_delay(&id('a'))).collect();
        assert_eq!(got.len(), MAX_TRIES as usize);
        let nominal: Vec<u64> = (0..MAX_TRIES).map(|n| nominal_delay(n).as_secs()).collect();
        assert_eq!(&nominal[..6], &[2, 4, 8, 16, 32, 60]);
        assert!(nominal[6..].iter().all(|s| *s == 60));
        for (d, n) in got.iter().zip(&nominal) {
            let n = Duration::from_secs(*n);
            assert!(
                *d >= n.mul_f64(0.8) && *d <= n.mul_f64(1.2),
                "{d:?} vs {n:?}"
            );
        }
        // Any new connection starts the budget afresh.
        b.connected(&id('a'), ip(150), None, None);
        let first = b.next_delay(&id('a')).unwrap();
        assert!(first >= Duration::from_millis(1600) && first <= Duration::from_millis(2400));
    }

    #[test]
    fn jitter_stays_in_its_band_and_actually_spreads() {
        let d = Duration::from_secs(10);
        assert_eq!(spread(d, 0.0), Duration::from_secs(8));
        assert_eq!(spread(d, 1.0), Duration::from_secs(12));
        assert_eq!(spread(d, 7.0), Duration::from_secs(12));
        assert_eq!(spread(d, -1.0), Duration::from_secs(8));
        let draws: Vec<f64> = (0..200).map(|_| random_unit()).collect();
        assert!(draws.iter().all(|u| (0.0..1.0).contains(u)));
        assert!(draws.iter().any(|u| *u < 0.3) && draws.iter().any(|u| *u > 0.7));
        // Two phones that lost the same computer at the same instant do not keep redialing in lockstep.
        let schedule = |b: &mut Book| -> Vec<Duration> {
            b.connected(&id('a'), ip(1), None, None);
            let mut at = Duration::ZERO;
            std::iter::from_fn(|| b.next_delay(&id('a')))
                .map(|d| {
                    at += d;
                    at
                })
                .collect()
        };
        let (x, y) = (
            schedule(&mut Book::default()),
            schedule(&mut Book::default()),
        );
        assert!(
            x.iter()
                .zip(&y)
                .filter(|(a, b)| a.abs_diff(**b) < Duration::from_millis(50))
                .count()
                < 3
        );
    }

    #[test]
    fn a_nudge_buys_exactly_one_more_try_once_the_budget_is_spent() {
        let mut b = Book::default();
        b.connected(&id('a'), ip(150), None, None);
        assert!(!b.grant_one(&id('a')), "budget not spent: nothing to grant");
        while b.next_delay(&id('a')).is_some() {}
        assert!(b.grant_one(&id('a')));
        assert!(b.next_delay(&id('a')).is_some());
        assert!(b.next_delay(&id('a')).is_none());
        assert!(!b.grant_one(&id('z')), "nothing for a stranger");
    }

    #[test]
    fn an_inbound_connection_keeps_the_known_port_and_follows_the_ip() {
        let mut b = Book::default();
        let dialed: SocketAddr = "192.168.1.150:17400".parse().unwrap();
        b.connected(&id('a'), ip(150), Some(dialed), None);
        b.connected(&id('a'), ip(151), None, None);
        assert_eq!(
            b.addr(&id('a')),
            Some("192.168.1.151:17400".parse().unwrap())
        );
        b.connected(&id('b'), ip(7), None, None);
        assert_eq!(b.addr(&id('b')), Some(SocketAddr::new(ip(7), DEFAULT_PORT)));
    }

    #[test]
    fn one_redial_task_per_device_and_nothing_for_strangers() {
        let mut b = Book::default();
        assert!(!b.begin(&id('a')));
        b.connected(&id('a'), ip(1), None, None);
        assert!(b.begin(&id('a')));
        assert!(!b.begin(&id('a')));
        b.end(&id('a'));
        assert!(b.begin(&id('a')));
        b.forget(&id('a'));
        assert!(b.addr(&id('a')).is_none() && b.next_delay(&id('a')).is_none());
    }

    #[test]
    fn a_restarted_transport_inherits_lan_addresses_but_not_loopback_or_unpaired() {
        let mut first = Book::default();
        first.connected(
            &id('x'),
            ip(150),
            Some("192.168.1.150:1716".parse().unwrap()),
            None,
        );
        first.connected(&id('y'), std::net::Ipv4Addr::LOCALHOST.into(), None, None);
        let mut second = Book::default();
        second.carry_over();
        assert_eq!(
            second.addr(&id('x')),
            Some("192.168.1.150:1716".parse().unwrap())
        );
        assert!(second.addr(&id('y')).is_none());
        second.forget(&id('x')); // unpairing forgets for good
        let mut third = Book::default();
        third.carry_over();
        assert!(third.addr(&id('x')).is_none());
    }

    #[test]
    fn the_computers_port_hint_beats_the_guess_but_not_a_dial_we_made() {
        let mut b = Book::default();
        // Only ever dialed in: without a hint the default is the guess, with one it is the real port.
        b.connected(&id('c'), ip(9), None, None);
        assert_eq!(b.addr(&id('c')), Some(SocketAddr::new(ip(9), DEFAULT_PORT)));
        b.connected(&id('c'), ip(9), None, Some(1730));
        assert_eq!(b.addr(&id('c')), Some(SocketAddr::new(ip(9), 1730)));
        // The computer moved to another port: the newest hint wins over what was remembered.
        b.connected(&id('c'), ip(9), None, Some(1731));
        assert_eq!(b.addr(&id('c')), Some(SocketAddr::new(ip(9), 1731)));
        // A hint never overrides the endpoint we dialed ourselves.
        let dialed: SocketAddr = "192.168.1.9:1740".parse().unwrap();
        b.connected(&id('c'), ip(9), Some(dialed), Some(1731));
        assert_eq!(b.addr(&id('c')), Some(dialed));
    }

    #[test]
    fn a_goodbye_suppresses_for_a_bounded_time_and_any_connection_ends_it() {
        let d = id('s');
        let t0 = Instant::now();
        assert_eq!(suppressed_for(&d, t0), Duration::ZERO);
        suppress(&d, t0, Duration::from_secs(300));
        assert_eq!(
            suppressed_for(&d, t0 + Duration::from_secs(100)),
            Duration::from_secs(200)
        );
        assert_eq!(
            suppressed_for(&d, t0 + Duration::from_secs(301)),
            Duration::ZERO
        );
        // Expired entries are gone.
        assert_eq!(suppressed_for(&d, t0), Duration::ZERO);
        // A connection from either side (the user's Connect, the computer's own dial) ends it at once.
        suppress(&d, t0, Duration::from_secs(300));
        let mut b = Book::default();
        b.connected(&d, ip(3), None, None);
        assert_eq!(suppressed_for(&d, t0), Duration::ZERO);
        // Unpairing forgets it too.
        suppress(&d, t0, Duration::from_secs(300));
        b.forget(&d);
        assert_eq!(suppressed_for(&d, t0), Duration::ZERO);
    }
}
