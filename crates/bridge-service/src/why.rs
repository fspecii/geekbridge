// SPDX-License-Identifier: GPL-3.0-or-later
//! The service-side inputs of the "why is the phone not connected" answer (`bridge_core::why_disconnected`):
//! when each paired phone last announced itself and from where, how its last link ended, and what this
//! computer's own network looks like. Everything here is in memory and holds no phone content.
use crate::lan_select::Iface;
use bridge_core::ipc::{Event, EventKind};
use bridge_core::why_disconnected::LinkEnd;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

/// Last announcement per device: where from, and when (unix ms).
#[derive(Debug, Default)]
pub struct Sightings {
    last: HashMap<String, (IpAddr, u64)>,
}

impl Sightings {
    /// Keep the newest sighting; discovery repeats old ones until they expire.
    pub fn record(&mut self, id: &str, ip: IpAddr, at_ms: u64) {
        match self.last.get_mut(id) {
            Some(e) if e.1 >= at_ms => {}
            Some(e) => *e = (ip, at_ms),
            None => {
                // Bounded by the number of distinct announcers on the LAN; cap it anyway.
                if self.last.len() < 256 {
                    self.last.insert(id.to_owned(), (ip, at_ms));
                }
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<(IpAddr, u64)> {
        self.last.get(id).copied()
    }
}

/// How the newest link of `id` ended, if its last recorded event is an ending. A later `Connected`
/// means the link is up (or was replaced), so nothing is reported then.
pub fn last_end(events: &VecDeque<Event>, id: &str) -> Option<LinkEnd> {
    for e in events.iter().rev() {
        if e.device_id.as_deref() != Some(id) {
            continue;
        }
        match &e.kind {
            EventKind::LinkLost { reason } => return Some(LinkEnd::Lost(reason.clone())),
            EventKind::ConnectFailed { reason } => {
                return Some(LinkEnd::ConnectFailed(reason.clone()));
            }
            EventKind::PeerUnpaired | EventKind::Unpaired => return Some(LinkEnd::Unpaired),
            EventKind::Disconnected => return Some(LinkEnd::Disconnected),
            EventKind::Connected => return None,
            _ => {}
        }
    }
    None
}

/// Is any non-loopback, non-link-local interface up? Without one nothing can reach this computer.
pub fn computer_online(ifaces: &[Iface]) -> bool {
    ifaces.iter().any(|i| i.up && usable(i.ip))
}

fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => !a.is_loopback() && !a.is_link_local() && !a.is_unspecified(),
        IpAddr::V6(a) => {
            !a.is_loopback() && !a.is_unspecified() && (a.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

/// Was `phone` last seen on one of this computer's IPv4 networks? A /24 match is a good-enough proxy
/// for "same Wi-Fi". `None` when it cannot be said (IPv6, or no IPv4 interface to compare with).
pub fn on_our_network(ifaces: &[Iface], phone: IpAddr) -> Option<bool> {
    let IpAddr::V4(p) = phone else { return None };
    let mut any = false;
    for i in ifaces.iter().filter(|i| i.up && usable(i.ip)) {
        if let IpAddr::V4(ours) = i.ip {
            any = true;
            if ours.octets()[..3] == p.octets()[..3] {
                return Some(true);
            }
        }
    }
    any.then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, id: &str, kind: EventKind) -> Event {
        Event {
            seq,
            at_unix_ms: seq,
            device_id: Some(id.into()),
            kind,
        }
    }

    #[test]
    fn last_end_reads_the_newest_ending_for_that_device_only() {
        let mut q = VecDeque::new();
        q.push_back(ev(1, "a", EventKind::Connected));
        q.push_back(ev(
            2,
            "a",
            EventKind::LinkLost {
                reason: "timed out".into(),
            },
        ));
        q.push_back(ev(3, "b", EventKind::Disconnected));
        assert_eq!(last_end(&q, "a"), Some(LinkEnd::Lost("timed out".into())));
        assert_eq!(last_end(&q, "b"), Some(LinkEnd::Disconnected));
        assert_eq!(last_end(&q, "c"), None);
        q.push_back(ev(4, "a", EventKind::Connected));
        assert_eq!(last_end(&q, "a"), None);
        q.push_back(ev(5, "a", EventKind::PeerUnpaired));
        assert_eq!(last_end(&q, "a"), Some(LinkEnd::Unpaired));
        q.push_back(ev(6, "a", EventKind::ConnectFailed { reason: "x".into() }));
        assert_eq!(last_end(&q, "a"), Some(LinkEnd::ConnectFailed("x".into())));
    }

    #[test]
    fn network_facts_from_interfaces() {
        let down = [
            Iface::new("wlan0", "192.168.1.5", false),
            Iface::new("lo", "127.0.0.1", true),
        ];
        assert!(!computer_online(&down));
        let up = [Iface::new("wlan0", "192.168.1.5", true)];
        assert!(computer_online(&up));
        assert!(!computer_online(&[Iface::new("wlan0", "fe80::1", true)]));
        assert_eq!(
            on_our_network(&up, "192.168.1.77".parse().unwrap()),
            Some(true)
        );
        assert_eq!(
            on_our_network(&up, "10.0.0.9".parse().unwrap()),
            Some(false)
        );
        assert_eq!(on_our_network(&up, "fd00::9".parse().unwrap()), None);
        assert_eq!(on_our_network(&down, "10.0.0.9".parse().unwrap()), None);
    }

    #[test]
    fn sightings_keep_the_newest() {
        let mut s = Sightings::default();
        let a: IpAddr = "192.168.1.9".parse().unwrap();
        let b: IpAddr = "192.168.1.10".parse().unwrap();
        s.record("p", a, 100);
        s.record("p", b, 50);
        assert_eq!(s.get("p"), Some((a, 100)));
        s.record("p", b, 200);
        assert_eq!(s.get("p"), Some((b, 200)));
    }
}
