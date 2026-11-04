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

/// Was `phone` last seen on one of this computer's IPv4 networks? Compared with each interface's real
/// prefix (a /16 home network is one network; a /25 is two). `None` when it cannot be said (IPv6, or
/// no IPv4 interface to compare with).
pub fn on_our_network(ifaces: &[Iface], phone: IpAddr) -> Option<bool> {
    let IpAddr::V4(p) = phone else { return None };
    let mut any = false;
    for i in ifaces.iter().filter(|i| i.up && usable(i.ip)) {
        if i.ip.is_ipv4() {
            any = true;
            if i.contains(IpAddr::V4(p)) {
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

    // ---- test-gaps-4

    #[test]
    fn sightings_are_capped_but_known_phones_keep_updating() {
        let mut s = Sightings::default();
        for i in 0..300u32 {
            s.record(&format!("d{i}"), "192.168.1.9".parse().unwrap(), 10);
        }
        assert!(s.get("d0").is_some() && s.get("d255").is_some());
        assert!(
            s.get("d256").is_none(),
            "newcomers beyond the cap are not kept"
        );
        let b: IpAddr = "192.168.1.77".parse().unwrap();
        s.record("d0", b, 20);
        assert_eq!(
            s.get("d0"),
            Some((b, 20)),
            "a known phone still updates at the cap"
        );
        // Equal timestamps keep the first address.
        s.record("d0", "10.0.0.1".parse().unwrap(), 20);
        assert_eq!(s.get("d0"), Some((b, 20)));
    }

    #[test]
    fn events_of_other_kinds_or_without_a_device_do_not_hide_how_the_link_ended() {
        let mut q = VecDeque::new();
        q.push_back(ev(
            1,
            "a",
            EventKind::LinkLost {
                reason: "reset".into(),
            },
        ));
        q.push_back(ev(2, "a", EventKind::Connecting));
        q.push_back(Event {
            seq: 3,
            at_unix_ms: 3,
            device_id: None,
            kind: EventKind::Connected,
        });
        q.push_back(ev(4, "b", EventKind::Connected));
        assert_eq!(last_end(&q, "a"), Some(LinkEnd::Lost("reset".into())));
        // An unpair ends it too, and the newest ending wins over an older one.
        q.push_back(ev(5, "a", EventKind::Unpaired));
        assert_eq!(last_end(&q, "a"), Some(LinkEnd::Unpaired));
        assert_eq!(last_end(&VecDeque::new(), "a"), None);
    }

    #[test]
    fn usable_addresses_exclude_loopback_link_local_and_unspecified() {
        for ip in [
            "127.0.0.1",
            "169.254.3.4",
            "0.0.0.0",
            "::1",
            "::",
            "fe80::1",
            "febf::1",
        ] {
            assert!(!computer_online(&[Iface::new("x", ip, true)]), "{ip}");
        }
        for ip in [
            "192.168.1.2",
            "10.1.2.3",
            "fd00::5",
            "2001:db8::1",
            "fec0::1",
        ] {
            assert!(computer_online(&[Iface::new("x", ip, true)]), "{ip}");
        }
        // One good interface among bad ones is enough; a down one never counts.
        assert!(computer_online(&[
            Iface::new("lo", "127.0.0.1", true),
            Iface::new("eth0", "192.168.1.2", true)
        ]));
        assert!(!computer_online(&[Iface::new(
            "eth0",
            "192.168.1.2",
            false
        )]));
        assert!(!computer_online(&[]));
    }

    #[test]
    fn same_network_follows_the_interface_prefix_on_any_usable_up_interface() {
        let ifaces = [
            Iface::new("lo", "127.0.0.1", true),
            Iface::new("docker0", "172.17.0.1", false),
            Iface::new("wlan0", "192.168.1.5", true),
            Iface::new("eth0", "10.0.0.2", true),
        ];
        let at = |ip: &str| on_our_network(&ifaces, ip.parse().unwrap());
        assert_eq!(at("192.168.1.200"), Some(true));
        assert_eq!(at("10.0.0.99"), Some(true), "any interface counts");
        assert_eq!(
            at("192.168.2.5"),
            Some(false),
            "a different third octet is another network"
        );
        assert_eq!(
            at("172.17.0.9"),
            Some(false),
            "a down interface is not ours"
        );
        assert_eq!(at("127.0.0.9"), Some(false), "loopback is not a network");
        // A /16 or /23 home network is one network, not 256 or two.
        let wide = [
            Iface::new("wlan0", "10.20.0.10", true).with_prefix(16),
            Iface::new("eth0", "192.168.0.7", true).with_prefix(23),
        ];
        let wide_at = |ip: &str| on_our_network(&wide, ip.parse().unwrap());
        assert_eq!(wide_at("10.20.7.3"), Some(true));
        assert_eq!(wide_at("10.21.0.3"), Some(false));
        assert_eq!(wide_at("192.168.1.9"), Some(true));
        assert_eq!(wide_at("192.168.2.9"), Some(false));
        // Only IPv6 interfaces: cannot say either way.
        let v6 = [Iface::new("wlan0", "fd00::2", true)];
        assert_eq!(on_our_network(&v6, "192.168.1.5".parse().unwrap()), None);
        assert_eq!(on_our_network(&[], "192.168.1.5".parse().unwrap()), None);
        // A mixed host compares IPv4 only.
        let mixed = [
            Iface::new("a", "fd00::2", true),
            Iface::new("b", "192.168.1.5", true),
        ];
        assert_eq!(
            on_our_network(&mixed, "192.168.9.9".parse().unwrap()),
            Some(false)
        );
    }
}
