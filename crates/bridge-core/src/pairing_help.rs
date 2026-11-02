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
    /// No answer at all.
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
    pub peer_address: Option<IpAddr>,
    pub probe: Probe,
    /// An interface that looks like a VPN is up (tun/wg/tailscale/ppp, ...).
    pub vpn_interface: bool,
}

fn same_subnet24(a: IpAddr, b: IpAddr) -> bool {
    match (a, b) {
        (IpAddr::V4(a), IpAddr::V4(b)) => a.octets()[..3] == b.octets()[..3],
        _ => true, // not judged for IPv6
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
const VPN: &str = "Turn off any VPN on the phone and on this computer while pairing.";
const FIREWALL: &str = "Allow GeekBridge through this computer's firewall on your home/private network (TCP and UDP 1716-1764).";
const OPEN_APP: &str = "Open GeekBridge on the phone and keep the screen on while pairing.";

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

    let different_network = match m.peer_address {
        Some(peer) if has_lan => !m.own_addresses.iter().any(|a| same_subnet24(*a, peer)),
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
        headline = "Your phone is reachable - you can pair now".to_owned();
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
        headline = "The phone does not answer".to_owned();
        add(OPEN_APP);
        add(GUEST);
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
            "Nothing was paired. Check you are on your own Wi-Fi, then scan the code again.",
        )
    } else if has("clock") {
        (
            "The clocks on the two devices disagree.",
            "Turn on automatic date and time on the phone and the computer, then try again.",
        )
    } else if has("rejected_locally") || has("rejected locally") || has("declined") {
        (
            "Pairing was declined on this device.",
            "Start again and tap Pair when you are ready.",
        )
    } else if has("rejected_by_peer")
        || has("rejected or cancelled")
        || has("pairing_rejected_by_peer")
    {
        (
            "The other device declined the pairing.",
            "Start again and confirm on both devices.",
        )
    } else if has("pairing_timeout") || has("pairing request timed out") {
        (
            "Pairing timed out before it was confirmed.",
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
            "Same Wi-Fi? Guest Wi-Fi and VPNs block this. Allow GeekBridge through the firewall (TCP and UDP 1716-1764).",
        )
    } else if has("unreachable") || has("no route") || has("address policy") || has("not permitted")
    {
        (
            "The other device is on a different network.",
            "Put both devices on the same Wi-Fi, switch off VPNs and avoid guest networks.",
        )
    } else if has("link_closed") || has("peer_closed") || has("closed") || has("park_timeout") {
        (
            "The connection closed before pairing finished.",
            "Keep both apps open and the screens on, then try again.",
        )
    } else {
        (
            "Pairing did not work.",
            "Run \"Test connection\" for a step-by-step check.",
        )
    };
    Explained { headline, advice }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(probe: Probe) -> Measured {
        Measured {
            listening: true,
            own_addresses: vec!["192.168.1.20".parse().unwrap()],
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
