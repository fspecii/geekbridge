// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain-language help for first-time pairing: what a failure means in terms of the user's Wi-Fi,
//! and the verdict of the "Test connection" check. Pure functions over measured facts, so every
//! wording rule is unit-tested and the service, the apps and `geekbridge-ctl` all say the same thing.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// One line of the diagnostic: what was checked and what was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestStep {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

/// The "Test connection" answer. `headline` is one sentence for a big label; `advice` is a short
/// list of things to try, most likely first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTest {
    pub ok: bool,
    pub headline: String,
    pub steps: Vec<TestStep>,
    pub advice: Vec<String>,
}

/// What dialling the phone's link port did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Not tried: no address is known for the phone yet.
    NotTried,
    Connected,
    /// The phone's network answered "nothing listens here".
    Refused,
    /// No answer at all. Windows reports a refused connect this way too (it retries for about two
    /// seconds, then times out), so this also covers "the app is not running on the phone".
    TimedOut,
    /// The route does not exist ("network unreachable", "no route to host").
    NoRoute,
    Other,
}

/// Everything the check measured.
#[derive(Clone, Debug)]
pub struct Measured {
    /// This computer accepts link connections.
    pub listening: bool,
    /// This computer's LAN addresses (loopback excluded).
    pub own_addresses: Vec<IpAddr>,
    /// Prefix length of each of those addresses (`(address, bits)`); an address missing here is
    /// compared as a /24 (IPv4) or /64 (IPv6).
    pub own_prefixes: Vec<(IpAddr, u8)>,
    pub peer_address: Option<IpAddr>,
    pub probe: Probe,
    /// An interface that looks like a VPN is up (tun/wg/tailscale/ppp, ...).
    pub vpn_interface: bool,
}

/// Same network, by the real prefix when known. `None` when the families differ: a phone seen over
/// IPv4 says nothing about this computer's IPv6 addresses, and the other way round.
fn same_subnet(a: IpAddr, prefixes: &[(IpAddr, u8)], b: IpAddr) -> Option<bool> {
    let default = if a.is_ipv4() { 24 } else { 64 };
    let bits = prefixes
        .iter()
        .find(|(ip, _)| *ip == a)
        .map_or(default, |(_, p)| *p);
    match (a, b) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = u32::MAX
                .checked_shl(32 - u32::from(bits.min(32)))
                .unwrap_or(0);
            Some(u32::from(a) & mask == u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = u128::MAX
                .checked_shl(128 - u32::from(bits.min(128)))
                .unwrap_or(0);
            Some(u128::from(a) & mask == u128::from(b) & mask)
        }
        _ => None,
    }
}

fn step(label: &str, ok: bool, detail: impl Into<String>) -> TestStep {
    TestStep {
        label: label.into(),
        ok,
        detail: detail.into(),
    }
}

const WIFI: &str =
    "Make sure the phone and this computer are on the same Wi-Fi network (not a guest network).";
const GUEST: &str = "Guest and public Wi-Fi often block devices from talking to each other (\"client isolation\"). Use your main Wi-Fi.";
const VPN: &str = "Turn off any VPN on the phone and on this computer while linking.";
const FIREWALL: &str = "Allow GeekBridge through this computer's firewall on your home/private network (TCP and UDP 1716-1764).";
const OPEN_APP: &str = "Open GeekBridge on the phone and keep the screen on while linking.";

pub fn build_report(m: &Measured) -> ConnectionTest {
    let mut steps = Vec::new();
    let mut advice: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !advice.iter().any(|a| a == s) {
            advice.push(s.to_owned());
        }
    };

    let has_lan = !m.own_addresses.is_empty();
    steps.push(step(
        "This computer is on a network",
        has_lan,
        if has_lan {
            m.own_addresses
                .iter()
                .map(IpAddr::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "No local network address found".into()
        },
    ));
    steps.push(step(
        "This computer accepts connections",
        m.listening,
        if m.listening {
            "Ready"
        } else {
            "GeekBridge is not listening for phones"
        },
    ));

    // Only addresses of the phone's family can tell: all of them must disagree.
    let different_network = match m.peer_address {
        Some(peer) if has_lan => {
            let verdicts: Vec<bool> = m
                .own_addresses
                .iter()
                .filter_map(|a| same_subnet(*a, &m.own_prefixes, peer))
                .collect();
            !verdicts.is_empty() && !verdicts.contains(&true)
        }
        _ => false,
    };
    if let Some(peer) = m.peer_address {
        steps.push(step(
            "Phone is on the same network",
            !different_network,
            if different_network {
                format!("The phone is at {peer}, which is not next to this computer's address")
            } else {
                format!("Phone seen at {peer}")
            },
        ));
    } else {
        steps.push(step(
            "Phone has been seen",
            false,
            "No announcement from a phone yet",
        ));
    }
    let probe_ok = m.probe == Probe::Connected;
    if m.peer_address.is_some() {
        steps.push(step(
            "Phone answers",
            probe_ok,
            match m.probe {
                Probe::Connected => "Connected",
                Probe::Refused => "The phone refused: GeekBridge is not running there",
                Probe::TimedOut if !different_network => {
                    "No answer, although the phone's announcement was heard: GeekBridge closed on the phone, Wi-Fi client isolation or a firewall"
                }
                Probe::TimedOut => "No answer",
                Probe::NoRoute => "No route to the phone",
                Probe::NotTried => "Not tried",
                Probe::Other => "Could not connect",
            },
        ));
    }
    if m.vpn_interface {
        steps.push(step(
            "No VPN in the way",
            false,
            "A VPN-like network interface is active on this computer",
        ));
    }

    let headline;
    if !has_lan {
        headline = "This computer is not connected to a network".to_owned();
        add("Connect this computer to your Wi-Fi or Ethernet.");
    } else if !m.listening {
        headline = "This computer is not accepting connections".to_owned();
        add(
            "Restart GeekBridge on this computer (log out and in again, or run geekbridge-service).",
        );
        add(FIREWALL);
    } else if m.peer_address.is_none() {
        headline = "No phone has been found yet".to_owned();
        add(OPEN_APP);
        add(WIFI);
        add(GUEST);
        if m.vpn_interface {
            add(VPN);
        }
        add(FIREWALL);
    } else if probe_ok {
        headline = "Your phone is reachable - you can link now".to_owned();
    } else if m.probe == Probe::Refused {
        headline = "The phone is reachable, but GeekBridge is not listening on it".to_owned();
        add(OPEN_APP);
        add(
            "In the phone app, tap \"Look for computers\" (or Start link) so it accepts connections.",
        );
    } else if different_network || m.probe == Probe::NoRoute {
        headline = "The phone seems to be on a different network".to_owned();
        add(WIFI);
        add(GUEST);
        if m.vpn_interface {
            add(VPN);
        }
    } else {
        // The phone's broadcast got through but a connection did not, on the same subnet: that is
        // what a router's client isolation does (broadcast allowed, client-to-client TCP dropped),
        // or a firewall. Say so first.
        headline = if m.probe == Probe::TimedOut {
            "The phone can be seen but does not accept connections (app closed, client isolation or a firewall)"
        } else {
            "The phone does not answer"
        }
        .to_owned();
        add(GUEST);
        add(OPEN_APP);
        add(FIREWALL);
        if m.vpn_interface {
            add(VPN);
        }
        add(WIFI);
    }
    ConnectionTest {
        ok: probe_ok && m.listening && has_lan,
        headline,
        steps,
        advice,
    }
}

/// A short, friendly sentence plus one thing to try, for a failure `reason` from the service or
/// the Android core (a machine code such as `pairing_timeout`, or an error text).
pub struct Explained {
    pub headline: &'static str,
    pub advice: &'static str,
}

pub fn explain_failure(reason: &str) -> Explained {
    let r = reason.to_ascii_lowercase();
    let has = |needle: &str| r.contains(needle);
    let (headline, advice) = if has("expired")
        || has("already used")
        || has("not recognised")
        || has("qr code")
    {
        (
            "That QR code can't be used.",
            "Show a new QR code on the computer and scan it again.",
        )
    } else if has("pin") && (has("mismatch") || has("does not match"))
        || has("pairing_binding_changed")
        || has("certificate_mismatch")
    {
        (
            "This is not the computer you scanned.",
            "Nothing was linked. Check you are on your own Wi-Fi, then scan the code again.",
        )
    } else if has("clock") {
        (
            "The clocks on the two devices disagree.",
            "Turn on automatic date and time on the phone and the computer, then try again.",
        )
    } else if has("rejected_locally") || has("rejected locally") || has("declined") {
        (
            "Linking was declined on this device.",
            "Start again and tap Link when you are ready.",
        )
    } else if has("rejected_by_peer")
        || has("rejected or cancelled")
        || has("pairing_rejected_by_peer")
    {
        (
            "The other device declined to link.",
            "Start again and confirm on both devices.",
        )
    } else if has("pairing_timeout") || has("pairing request timed out") {
        (
            "Linking timed out before it was confirmed.",
            "Start again and confirm on both devices within half a minute.",
        )
    } else if has("refused") || has("connection_refused") {
        (
            "The other device is reachable but GeekBridge is not listening there.",
            "Open GeekBridge on the other device and start it, then try again.",
        )
    } else if has("timed out") || has("timeout") || has("connect_timeout") {
        (
            "The other device did not answer.",
            "Same Wi-Fi? Guest Wi-Fi and VPNs block this. Allow GeekBridge through the firewall (TCP and UDP 1716-1764). Still stuck? Run geekbridge-ctl doctor on the computer.",
        )
    } else if has("unreachable") || has("no route") || has("address policy") || has("not permitted")
    {
        (
            "The other device is on a different network.",
            "Put both devices on the same Wi-Fi, switch off VPNs and avoid guest networks.",
        )
    } else if has("link_closed") || has("peer_closed") || has("closed") || has("park_timeout") {
        (
            "The connection closed before linking finished.",
            "Keep both apps open and the screens on, then try again.",
        )
    } else {
        (
            "Linking did not work.",
            "Run \"Test connection\" for a step-by-step check.",
        )
    };
    Explained { headline, advice }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_phone_points_at_the_firewall_and_the_doctor_check() {
        let e = explain_failure("connect timed out");
        assert!(e.advice.contains("firewall"));
        assert!(e.advice.contains("geekbridge-ctl doctor"));
    }

    #[test]
    fn an_expired_code_says_to_scan_the_new_one() {
        let e = explain_failure("That QR code has expired.");
        assert!(e.advice.contains("new QR code"));
    }

    fn m(probe: Probe) -> Measured {
        Measured {
            listening: true,
            own_addresses: vec!["192.168.1.20".parse().unwrap()],
            own_prefixes: vec![],
            peer_address: Some("192.168.1.35".parse().unwrap()),
            probe,
            vpn_interface: false,
        }
    }

    #[test]
    fn reachable_phone_is_ok() {
        let r = build_report(&m(Probe::Connected));
        assert!(r.ok);
        assert!(r.advice.is_empty());
        assert!(r.steps.iter().all(|s| s.ok));
    }

    #[test]
    fn refused_means_the_phone_app_is_not_listening() {
        let r = build_report(&m(Probe::Refused));
        assert!(!r.ok);
        assert!(r.headline.contains("not listening"));
    }

    #[test]
    fn timeout_on_the_same_subnet_points_at_firewall_and_isolation() {
        let r = build_report(&m(Probe::TimedOut));
        assert!(!r.ok);
        assert!(r.advice.iter().any(|a| a.contains("firewall")));
        assert!(r.advice.iter().any(|a| a.contains("Guest")));
    }

    #[test]
    fn announcement_heard_but_tcp_blocked_says_client_isolation_first() {
        let r = build_report(&m(Probe::TimedOut));
        assert!(r.headline.contains("client isolation"), "{}", r.headline);
        assert!(r.advice[0].contains("client isolation"), "{:?}", r.advice);
        assert!(r.advice[0].contains("main Wi-Fi"));
        // Windows cannot tell a refused connect from a dropped one, so the app-closed case must be
        // named and its fix offered.
        assert!(r.headline.contains("app closed"), "{}", r.headline);
        assert!(
            r.advice
                .iter()
                .any(|a| a.contains("Open GeekBridge on the phone"))
        );
        assert!(r.advice.iter().any(|a| a.contains("firewall")));
        let answers = r.steps.iter().find(|s| s.label == "Phone answers").unwrap();
        assert!(answers.detail.contains("client isolation"));
        // A refusal is a different thing: the phone's network answered.
        assert!(
            !build_report(&m(Probe::Refused))
                .headline
                .contains("isolation")
        );
    }

    #[test]
    fn a_slash_16_or_23_home_network_is_not_a_different_network() {
        let mut x = m(Probe::TimedOut);
        x.own_addresses = vec!["10.20.0.10".parse().unwrap()];
        x.own_prefixes = vec![("10.20.0.10".parse().unwrap(), 16)];
        x.peer_address = Some("10.20.7.3".parse().unwrap());
        assert!(!build_report(&x).headline.contains("different network"));
        // With no prefix known the old /24 guess applies.
        x.own_prefixes.clear();
        assert!(build_report(&x).headline.contains("different network"));
        // 192.168.0.0/23 holds 192.168.1.x; 192.168.2.x is another network.
        x.own_addresses = vec!["192.168.0.9".parse().unwrap()];
        x.own_prefixes = vec![("192.168.0.9".parse().unwrap(), 23)];
        x.peer_address = Some("192.168.1.77".parse().unwrap());
        assert!(!build_report(&x).headline.contains("different network"));
        x.peer_address = Some("192.168.2.77".parse().unwrap());
        assert!(build_report(&x).headline.contains("different network"));
    }

    #[test]
    fn ipv6_prefixes_are_judged_and_mixed_families_are_not() {
        let mut x = m(Probe::TimedOut);
        x.own_addresses = vec!["fd00:1:2:3::5".parse().unwrap()];
        x.peer_address = Some("fd00:1:2:3::99".parse().unwrap());
        assert!(!build_report(&x).headline.contains("different network"));
        x.peer_address = Some("fd00:1:2:4::99".parse().unwrap());
        assert!(build_report(&x).headline.contains("different network"));
        // A v4 phone against a v6-only computer cannot be judged either way.
        x.peer_address = Some("192.168.1.9".parse().unwrap());
        assert!(!build_report(&x).headline.contains("different network"));
        // But a dual-stack computer still sees that a v4 phone is on another v4 network.
        x.own_addresses = vec![
            "192.168.1.20".parse().unwrap(),
            "fd00:1:2:3::5".parse().unwrap(),
        ];
        x.peer_address = Some("10.0.0.7".parse().unwrap());
        assert!(build_report(&x).headline.contains("different network"));
        x.peer_address = Some("192.168.1.9".parse().unwrap());
        assert!(!build_report(&x).headline.contains("different network"));
    }

    #[test]
    fn other_subnet_is_called_out() {
        let mut x = m(Probe::TimedOut);
        x.peer_address = Some("10.0.0.7".parse().unwrap());
        let r = build_report(&x);
        assert!(r.headline.contains("different network"));
        assert!(
            r.steps
                .iter()
                .any(|s| !s.ok && s.label.contains("same network"))
        );
    }

    #[test]
    fn vpn_is_mentioned_only_when_present() {
        let plain = build_report(&m(Probe::TimedOut));
        assert!(!plain.advice.iter().any(|a| a.contains("VPN")));
        let mut x = m(Probe::TimedOut);
        x.vpn_interface = true;
        assert!(build_report(&x).advice.iter().any(|a| a.contains("VPN")));
    }

    #[test]
    fn no_phone_seen_and_not_listening_and_offline() {
        let mut x = m(Probe::NotTried);
        x.peer_address = None;
        assert!(build_report(&x).headline.contains("No phone"));
        x.listening = false;
        assert!(build_report(&x).headline.contains("not accepting"));
        x.own_addresses.clear();
        assert!(
            build_report(&x)
                .headline
                .contains("not connected to a network")
        );
    }

    #[test]
    fn failures_get_plain_words() {
        for (reason, word) in [
            ("pairing_timeout", "timed out"),
            ("connect refused (os error 111)", "not listening"),
            ("timed out: tcp connect", "did not answer"),
            ("Network is unreachable", "different network"),
            ("That QR code has expired. Show a new one", "qr code"),
            ("clock_skew", "clocks"),
            ("pairing_binding_changed", "not the computer"),
            ("???", "did not work"),
        ] {
            let e = explain_failure(reason);
            assert!(
                e.headline.to_lowercase().contains(word),
                "{reason}: {}",
                e.headline
            );
        }
    }
}
