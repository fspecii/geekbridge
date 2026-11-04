// SPDX-License-Identifier: GPL-3.0-or-later
//! Which of this computer's addresses go into the pairing QR code, and in which order.
//!
//! A machine with Docker, libvirt, Tailscale and a VPN has half a dozen addresses and only one of them
//! is on the network the phone is on. The phone dials the QR's addresses in order, each with its own
//! timeout, so the likeliest one must be first and the unreachable ones must not be there at all.
//!
//! [`rank`] is pure (a list of interfaces in, a list of addresses out) so it is tested with a fake
//! interface list; [`system_interfaces`] fills that list from the OS.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One address on one network interface, as the OS reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub ip: IpAddr,
    /// Link is up (IFF_UP and IFF_RUNNING on Unix).
    pub up: bool,
    /// Length of the network prefix (the netmask as a bit count): 24 for 255.255.255.0.
    pub prefix: u8,
}

impl Iface {
    pub fn new(name: &str, ip: &str, up: bool) -> Self {
        Self {
            name: name.to_owned(),
            ip: ip.parse().expect("test address"),
            up,
            prefix: if ip.contains(':') { 64 } else { 24 },
        }
    }

    pub fn with_prefix(mut self, prefix: u8) -> Self {
        self.prefix = prefix;
        self
    }

    /// Is `other` on the network this address belongs to (same family, same prefix)?
    pub fn contains(&self, other: IpAddr) -> bool {
        same_network(self.ip, self.prefix, other)
    }

    /// The directed broadcast address of this interface's IPv4 network (`10.20.255.255` for
    /// `10.20.0.10/16`), the destination a datagram needs to reach every host on that network
    /// and only that one. `None` for IPv6, and for /31 and /32 (point-to-point, no broadcast).
    pub fn directed_broadcast(&self) -> Option<Ipv4Addr> {
        let IpAddr::V4(v4) = self.ip else { return None };
        if self.prefix > 30 {
            return None;
        }
        let host_bits = u32::MAX >> self.prefix;
        Some(Ipv4Addr::from(u32::from(v4) | host_bits))
    }
}

/// Do `a` (with its `prefix`) and `b` share a network? Different families never do.
pub fn same_network(a: IpAddr, prefix: u8, b: IpAddr) -> bool {
    match (a, b) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let bits = u32::from(prefix.min(32));
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let bits = u32::from(prefix.min(128));
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            (u128::from(a) & mask) == (u128::from(b) & mask)
        }
        _ => false,
    }
}

/// Interface names that carry containers and virtual machines: the phone is never on them.
const VIRTUAL_PREFIXES: &[&str] = &[
    "docker",
    "br-",
    "veth",
    "virbr",
    "vmnet",
    "vboxnet",
    "lxc",
    "lxd",
    "podman",
    "cni",
    "flannel",
    "cali",
    "cilium",
    "kube",
    "weave",
    "vnet",
    "tap",
    "vEthernet",
    "bridge",
];
/// Tunnels: addresses the phone can only reach if it is on the same VPN, which a QR cannot assume.
const TUNNEL_PREFIXES: &[&str] = &[
    "tun",
    "wg",
    "tailscale",
    "utun",
    "zt",
    "ppp",
    "nordlynx",
    "proton",
    "mullvad",
    "ipsec",
    "gre",
    "sit",
    "ip6tnl",
];

fn has_prefix(name: &str, prefixes: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    prefixes
        .iter()
        .any(|p| lower.starts_with(&p.to_ascii_lowercase()))
}

/// 100.64.0.0/10, carrier-grade NAT (also what Tailscale hands out).
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 64
}

/// Docker's default bridge network. Only ever a last resort: a real LAN could in theory use it.
fn is_docker_default(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 172 && o[1] == 17
}

fn is_v6_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn is_v6_ula(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// Never put this in a QR: it cannot be the phone's way to this computer.
fn never(i: &Iface) -> bool {
    if !i.up || has_prefix(&i.name, VIRTUAL_PREFIXES) || has_prefix(&i.name, TUNNEL_PREFIXES) {
        return true;
    }
    match i.ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || is_cgnat(v4)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || is_v6_link_local(v6)
        }
    }
}

/// Lower is likelier. `default_ips` are the source addresses the OS would use to leave for the
/// internet, i.e. the address on the default-route interface.
fn score(i: &Iface, default_ips: &[IpAddr]) -> u8 {
    let on_default = default_ips.contains(&i.ip);
    match i.ip {
        IpAddr::V4(v4) if v4.is_private() && !is_docker_default(v4) => {
            if on_default {
                0
            } else {
                1 // a second NIC (wired + Wi-Fi): the phone may be on that one
            }
        }
        IpAddr::V4(v4) if is_docker_default(v4) => 4,
        IpAddr::V4(_) => 2, // public address on a directly attached interface
        IpAddr::V6(v6) if is_v6_ula(v6) => 5,
        IpAddr::V6(_) => 6,
    }
}

/// The addresses for the QR, most likely first, without duplicates, at most `max`.
/// IPv4 comes first. IPv6 follows it only as a unique-local (`fd..`) address, the fallback for a phone
/// whose Wi-Fi has no IPv4 (the phone tries addresses in order, so a working IPv4 never waits for it);
/// global IPv6 appears only when there is no IPv4 candidate at all (a bracketed v6 literal is the
/// least tested path, and a global address is rarely what a LAN peer should dial).
pub fn rank(ifaces: &[Iface], default_ips: &[IpAddr], max: usize) -> Vec<IpAddr> {
    let mut v: Vec<(u8, usize, IpAddr)> = ifaces
        .iter()
        .enumerate()
        .filter(|(_, i)| !never(i))
        .map(|(n, i)| (score(i, default_ips), n, i.ip))
        .collect();
    v.sort();
    let mut out: Vec<IpAddr> = Vec::new();
    for (_, _, ip) in &v {
        if !out.contains(ip) {
            out.push(*ip);
        }
    }
    if out.iter().any(IpAddr::is_ipv4) {
        out.retain(|ip| match ip {
            IpAddr::V4(_) => true,
            IpAddr::V6(v6) => is_v6_ula(*v6),
        });
    }
    out.truncate(max);
    out
}

/// [`rank`], but returning the interfaces' addresses with their prefix lengths.
pub fn rank_ifaces(ifaces: &[Iface], default_ips: &[IpAddr], max: usize) -> Vec<Iface> {
    rank(ifaces, default_ips, max)
        .into_iter()
        .filter_map(|ip| ifaces.iter().find(|i| i.ip == ip && !never(i)).cloned())
        .collect()
}

/// Where this computer's announcement goes: the limited broadcast (the default-route interface only,
/// as the kernel routes it) plus the directed broadcast of every LAN-like interface, computed from
/// its real netmask. Never a tunnel or container bridge ([`rank`] drops those).
pub fn announce_targets(lan: &[Iface], port: u16) -> Vec<std::net::SocketAddr> {
    let mut out = vec![std::net::SocketAddr::from((Ipv4Addr::BROADCAST, port))];
    for b in lan.iter().filter_map(Iface::directed_broadcast) {
        let t = std::net::SocketAddr::from((b, port));
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Every address of every interface that is up. Unix only; elsewhere the caller relies on the
/// route-probe addresses alone.
#[cfg(unix)]
pub fn system_interfaces() -> Vec<Iface> {
    use std::ffi::CStr;
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we only read and then release with freeifaddrs.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let e = &*cur;
            cur = e.ifa_next;
            if e.ifa_addr.is_null() {
                continue;
            }
            let up = (e.ifa_flags & libc::IFF_UP as libc::c_uint) != 0
                && (e.ifa_flags & libc::IFF_RUNNING as libc::c_uint) != 0;
            let ip = match i32::from((*e.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let sin = &*(e.ifa_addr as *const libc::sockaddr_in);
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let sin6 = &*(e.ifa_addr as *const libc::sockaddr_in6);
                    IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            let prefix = if e.ifa_netmask.is_null() {
                if ip.is_ipv4() { 24 } else { 64 }
            } else {
                match ip {
                    IpAddr::V4(_) => {
                        let m = &*(e.ifa_netmask as *const libc::sockaddr_in);
                        u32::from_be(m.sin_addr.s_addr).leading_ones() as u8
                    }
                    IpAddr::V6(_) => {
                        let m = &*(e.ifa_netmask as *const libc::sockaddr_in6);
                        u128::from_be_bytes(m.sin6_addr.s6_addr).leading_ones() as u8
                    }
                }
            };
            out.push(Iface {
                name: CStr::from_ptr(e.ifa_name).to_string_lossy().into_owned(),
                ip,
                up,
                prefix,
            });
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(unix))]
pub fn system_interfaces() -> Vec<Iface> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This developer machine plus the interfaces other people's machines have.
    fn busy_machine() -> Vec<Iface> {
        vec![
            Iface::new("lo", "127.0.0.1", true),
            Iface::new("lo", "::1", true),
            Iface::new("docker0", "172.17.0.1", true),
            Iface::new("docker0", "fe80::42:acff:fe11:1", true),
            Iface::new("virbr0", "192.168.122.1", true),
            Iface::new("br-3f2a1b", "172.18.0.1", true),
            Iface::new("veth1a2b3c", "169.254.7.7", true),
            Iface::new("tailscale0", "100.101.102.103", true),
            Iface::new("tailscale0", "fd7a:115c:a1e0::1", true),
            Iface::new("wg0", "10.66.66.2", true),
            Iface::new("enp5s0", "192.168.1.150", true),
            Iface::new("enp5s0", "fe80::8363:bc65:289e:a359", true),
            Iface::new("wlan0", "192.168.1.77", true),
        ]
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn default_route_private_address_comes_first_and_junk_is_dropped() {
        let got = rank(&busy_machine(), &ips(&["192.168.1.150"]), 4);
        assert_eq!(got, ips(&["192.168.1.150", "192.168.1.77"]));
    }

    #[test]
    fn never_loopback_link_local_cgnat_containers_or_tunnels() {
        let got = rank(&busy_machine(), &[], 8);
        for bad in [
            "127.0.0.1",
            "172.17.0.1",
            "172.18.0.1",
            "192.168.122.1",
            "169.254.7.7",
            "100.101.102.103",
            "10.66.66.2",
        ] {
            assert!(!got.contains(&bad.parse().unwrap()), "{bad} in {got:?}");
        }
        assert!(
            got.iter().all(IpAddr::is_ipv4),
            "no v6 on this machine is a LAN address"
        );
    }

    #[test]
    fn second_nic_follows_the_default_one() {
        // Wired is the default route, Wi-Fi is another private network: both are offered, wired first.
        let list = vec![
            Iface::new("wlan0", "10.0.5.20", true),
            Iface::new("enp5s0", "192.168.1.150", true),
        ];
        assert_eq!(
            rank(&list, &ips(&["192.168.1.150"]), 4),
            ips(&["192.168.1.150", "10.0.5.20"])
        );
        // ...and the reverse when Wi-Fi is the default route.
        assert_eq!(
            rank(&list, &ips(&["10.0.5.20"]), 4),
            ips(&["10.0.5.20", "192.168.1.150"])
        );
    }

    #[test]
    fn down_interfaces_are_skipped() {
        let list = vec![
            Iface::new("enp5s0", "192.168.1.150", false),
            Iface::new("wlan0", "192.168.1.77", true),
        ];
        assert_eq!(rank(&list, &[], 4), ips(&["192.168.1.77"]));
    }

    #[test]
    fn full_tunnel_vpn_does_not_hijack_the_choice() {
        // The VPN owns the default route (its address is the "default" source) but is a tunnel.
        let list = vec![
            Iface::new("wg0", "10.8.0.2", true),
            Iface::new("wlan0", "192.168.0.23", true),
        ];
        assert_eq!(rank(&list, &ips(&["10.8.0.2"]), 4), ips(&["192.168.0.23"]));
    }

    #[test]
    fn docker_default_range_is_a_last_resort_on_an_unknown_interface_name() {
        let list = vec![
            Iface::new("eth9", "172.17.0.5", true),
            Iface::new("eth0", "192.168.1.9", true),
        ];
        assert_eq!(rank(&list, &[], 4), ips(&["192.168.1.9", "172.17.0.5"]));
    }

    #[test]
    fn a_unique_local_address_trails_ipv4_but_a_global_one_does_not() {
        let list = [
            Iface::new("enp5s0", "192.168.1.150", true),
            Iface::new("enp5s0", "fd12:3456::10", true),
            Iface::new("enp5s0", "2001:db8::10", true),
            Iface::new("tailscale0", "fd7a:115c:a1e0::1", true),
        ];
        assert_eq!(
            rank(&list, &["192.168.1.150".parse().unwrap()], 8),
            ips(&["192.168.1.150", "fd12:3456::10"])
        );
    }

    #[test]
    fn ipv6_only_when_there_is_no_ipv4() {
        let list = vec![
            Iface::new("enp5s0", "fe80::1", true),
            Iface::new("enp5s0", "fd12:3456::10", true),
            Iface::new("enp5s0", "2001:db8::10", true),
        ];
        assert_eq!(rank(&list, &[], 4), ips(&["fd12:3456::10", "2001:db8::10"]));
    }

    #[test]
    fn nothing_usable_is_an_empty_list() {
        let list = vec![
            Iface::new("lo", "127.0.0.1", true),
            Iface::new("docker0", "172.17.0.1", true),
        ];
        assert!(rank(&list, &[], 4).is_empty());
    }

    #[test]
    fn respects_the_limit_and_removes_duplicates() {
        let list = vec![
            Iface::new("a0", "192.168.1.1", true),
            Iface::new("a1", "192.168.1.1", true),
            Iface::new("a2", "192.168.2.1", true),
            Iface::new("a3", "192.168.3.1", true),
        ];
        assert_eq!(rank(&list, &[], 2), ips(&["192.168.1.1", "192.168.2.1"]));
    }

    #[test]
    fn cgnat_boundaries() {
        assert!(is_cgnat("100.64.0.1".parse().unwrap()));
        assert!(is_cgnat("100.127.255.254".parse().unwrap()));
        assert!(!is_cgnat("100.63.255.255".parse().unwrap()));
        assert!(!is_cgnat("100.128.0.1".parse().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn system_interfaces_reports_loopback() {
        assert!(system_interfaces().iter().any(|i| i.ip.is_loopback()));
    }

    #[test]
    fn directed_broadcast_follows_the_real_netmask() {
        let b = |ip: &str, p: u8| {
            Iface::new("x", ip, true)
                .with_prefix(p)
                .directed_broadcast()
        };
        assert_eq!(
            b("192.168.1.10", 24),
            Some("192.168.1.255".parse().unwrap())
        );
        assert_eq!(b("10.20.0.10", 16), Some("10.20.255.255".parse().unwrap()));
        assert_eq!(
            b("192.168.0.10", 23),
            Some("192.168.1.255".parse().unwrap())
        );
        assert_eq!(b("10.1.2.3", 8), Some("10.255.255.255".parse().unwrap()));
        assert_eq!(
            b("192.168.1.130", 25),
            Some("192.168.1.255".parse().unwrap())
        );
        assert_eq!(b("10.0.0.1", 31), None);
        assert_eq!(b("10.0.0.1", 32), None);
        assert_eq!(b("fd00::1", 64), None);
    }

    #[test]
    fn announcements_go_to_each_lan_like_nic_and_not_to_vpn_or_docker() {
        let machine = vec![
            Iface::new("eth0", "192.168.1.10", true),
            Iface::new("wlan0", "10.20.0.10", true).with_prefix(16),
            Iface::new("docker0", "172.17.0.1", true).with_prefix(16),
            Iface::new("tailscale0", "100.101.0.1", true).with_prefix(32),
            Iface::new("wg0", "10.66.0.2", true),
        ];
        let lan = rank_ifaces(&machine, &["192.168.1.10".parse().unwrap()], 8);
        let t: Vec<String> = announce_targets(&lan, 1716)
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert_eq!(
            t,
            [
                "255.255.255.255:1716",
                "192.168.1.255:1716",
                "10.20.255.255:1716"
            ]
        );
    }

    #[test]
    fn same_network_uses_the_prefix_not_a_fixed_24() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(same_network(ip("10.20.0.10"), 16, ip("10.20.7.3")));
        assert!(!same_network(ip("10.20.0.10"), 24, ip("10.20.7.3")));
        assert!(same_network(ip("192.168.0.9"), 23, ip("192.168.1.200")));
        assert!(!same_network(ip("192.168.0.9"), 23, ip("192.168.2.1")));
        assert!(same_network(ip("fd00:1:2:3::5"), 64, ip("fd00:1:2:3::99")));
        assert!(!same_network(ip("fd00:1:2:3::5"), 64, ip("fd00:1:2:4::5")));
        assert!(!same_network(ip("10.0.0.1"), 8, ip("fd00::1")));
    }

    #[test]
    fn the_system_reports_a_real_prefix() {
        let lo = system_interfaces()
            .into_iter()
            .find(|i| i.ip == IpAddr::V4(Ipv4Addr::LOCALHOST));
        if let Some(lo) = lo {
            assert_eq!(lo.prefix, 8);
        }
    }
}
