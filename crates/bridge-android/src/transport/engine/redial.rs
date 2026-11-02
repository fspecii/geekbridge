// SPDX-License-Identifier: GPL-3.0-or-later
//! The phone's side of reconnecting: when a trusted link is lost, dial the computer again at the
//! address it was last reached on, with a growing delay. Pure bookkeeping; the engine owns the
//! dialing, checks that the device is still paired and goes through `begin_attempt`/`admit`, so a
//! redial that crosses the computer's own redial still leaves exactly one connection.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

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
    /// An inbound connection only tells us the computer's IP, so the port we already know (or the
    /// default) is kept.
    pub(super) fn connected(
        &mut self,
        device: &DeviceId,
        peer_ip: std::net::IpAddr,
        dialed: Option<SocketAddr>,
    ) {
        let e = self.entries.entry(device.clone()).or_default();
        e.tries = 0;
        let addr = match dialed {
            Some(a) => a,
            None => SocketAddr::new(peer_ip, e.addr.map_or(DEFAULT_PORT, |a| a.port())),
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

    /// Wait before the next try, or `None` once the budget is spent.
    pub(super) fn next_delay(&mut self, device: &DeviceId) -> Option<Duration> {
        let e = self.entries.get_mut(device)?;
        if e.tries >= MAX_TRIES {
            return None;
        }
        let delay = FIRST_DELAY
            .saturating_mul(1u32 << e.tries.min(10))
            .min(MAX_DELAY);
        e.tries += 1;
        Some(delay)
    }

    pub(super) fn forget(&mut self, device: &DeviceId) {
        self.entries.remove(device);
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
        b.connected(&id('a'), ip(150), None);
        let got: Vec<u64> = std::iter::from_fn(|| b.next_delay(&id('a')))
            .map(|d| d.as_secs())
            .collect();
        assert_eq!(got.len(), MAX_TRIES as usize);
        assert_eq!(&got[..6], &[2, 4, 8, 16, 32, 60]);
        assert!(got[6..].iter().all(|s| *s == 60));
        // Any new connection starts the budget afresh.
        b.connected(&id('a'), ip(150), None);
        assert_eq!(b.next_delay(&id('a')), Some(Duration::from_secs(2)));
    }

    #[test]
    fn an_inbound_connection_keeps_the_known_port_and_follows_the_ip() {
        let mut b = Book::default();
        let dialed: SocketAddr = "192.168.1.150:17400".parse().unwrap();
        b.connected(&id('a'), ip(150), Some(dialed));
        b.connected(&id('a'), ip(151), None);
        assert_eq!(
            b.addr(&id('a')),
            Some("192.168.1.151:17400".parse().unwrap())
        );
        b.connected(&id('b'), ip(7), None);
        assert_eq!(b.addr(&id('b')), Some(SocketAddr::new(ip(7), DEFAULT_PORT)));
    }

    #[test]
    fn one_redial_task_per_device_and_nothing_for_strangers() {
        let mut b = Book::default();
        assert!(!b.begin(&id('a')));
        b.connected(&id('a'), ip(1), None);
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
        );
        first.connected(&id('y'), std::net::Ipv4Addr::LOCALHOST.into(), None);
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
}
