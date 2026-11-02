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
}

impl Iface {
    pub fn new(name: &str, ip: &str, up: bool) -> Self {
        Self {
            name: name.to_owned(),
            ip: ip.parse().expect("test address"),
            up,
        }
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
/// IPv6 only appears when there is no IPv4 candidate at all (older phone builds and many routers are
/// IPv4-first, and a bracketed v6 literal is the least tested path).
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
        out.retain(IpAddr::is_ipv4);
    }
    out.truncate(max);
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
            out.push(Iface {
                name: CStr::from_ptr(e.ifa_name).to_string_lossy().into_owned(),
                ip,
                up,
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
        assert!(got.iter().all(IpAddr::is_ipv4), "v6 only without any v4");
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
}
