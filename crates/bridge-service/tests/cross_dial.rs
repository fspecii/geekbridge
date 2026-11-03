// SPDX-License-Identifier: GPL-3.0-or-later
//! Both sides dial each other at the same moment (the computer's redial after a drop, the phone's redial
//! or its reconnect after a capability toggle). The phone keeps the connection opened by the lexicographically
//! smaller device id; the computer must make the same choice, or each end closes the other's connection and the
//! link is gone until the next redial. docs/reports/redial-collision.md.

mod support;

use bridge_core::ipc::{LinkPhase, LinkRole, Request};
use bridge_kde as kde;
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::{sleep, timeout};

/// A stand-in phone whose id is above or below `pc_id`; a GeekBridge one declares the heartbeat marker, as the
/// real app does.
fn stand_in_phone(pc_id: &str, above: bool, geekbridge: bool) -> Arc<Phone> {
    loop {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        if (creds.device_id().as_str() > pc_id) != above {
            continue;
        }
        let mut caps = vec!["kdeconnect.ping".to_string()];
        if geekbridge {
            caps.push(bridge_core::heartbeat::MARKER.to_string());
            caps.push(bridge_core::goodbye::GOODBYE.to_string());
        }
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            caps.clone(),
            caps,
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        return Arc::new(Phone { creds, endpoint });
    }
}

/// Pair, drop the link, then make the computer dial the phone (A) while the phone dials the computer (B)
/// before the phone has even looked at A. Returns the phone's handshake results for B and A.
async fn cross_dial(
    above: bool,
    geekbridge: bool,
) -> (
    Rig,
    Arc<Phone>,
    kde::TrustedLink,
    Result<kde::Handshake, kde::KdeError>,
) {
    cross_dial_after(above, geekbridge, false).await
}

/// `polite`: the link ends with the user's Disconnect (and its goodbye) instead of the phone just dropping it.
async fn cross_dial_after(
    above: bool,
    geekbridge: bool,
    polite: bool,
) -> (
    Rig,
    Arc<Phone>,
    kde::TrustedLink,
    Result<kde::Handshake, kde::KdeError>,
) {
    let mut rig = Rig::start().await;
    let inbound = rig.listen_inbound().await;
    let pc_id = rig.status().await.device_id;
    let phone = stand_in_phone(&pc_id, above, geekbridge);
    let first = pair_phone(&rig, &phone).await;
    if polite {
        rig.ok(Request::Disconnect {
            device_id: phone.id(),
        })
        .await;
    }
    drop(first);
    rig.wait("link gone", |s| s.links.is_empty()).await;

    // A: the computer dials; the phone's listener holds the connection without answering yet.
    let l = listener().await;
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint_of(&l)),
    })
    .await;
    rig.wait("dial in flight", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connecting)
    })
    .await;
    // B: the phone dials the computer and finishes its handshake first.
    let pc = kde::DeviceId::parse(&pc_id).unwrap();
    let b = timeout(
        Duration::from_secs(15),
        phone.endpoint.connect(inbound, &pc),
    )
    .await
    .unwrap()
    .unwrap();
    let b = match b {
        kde::Handshake::Trusted(link) => link,
        kde::Handshake::Pending(_) => panic!("a paired phone must be trusted"),
    };
    sleep(Duration::from_millis(300)).await;
    // Now the phone gets to A.
    let (stream, _) = timeout(Duration::from_secs(5), l.accept())
        .await
        .unwrap()
        .unwrap();
    let a = timeout(Duration::from_secs(15), phone.endpoint.accept(stream))
        .await
        .unwrap();
    (rig, phone, b, a)
}

#[tokio::test]
async fn when_the_computer_has_the_smaller_id_its_own_dial_survives_the_phones_cross_dial() {
    // phone id above the computer's: the connection the computer opened wins on both ends.
    let (rig, phone, mut b, a) = cross_dial(true, true).await;
    let mut a = match a.expect("the computer's dial must complete") {
        kde::Handshake::Trusted(link) => link,
        kde::Handshake::Pending(_) => panic!("trusted"),
    };
    let s = rig
        .wait("one connected link", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    assert_eq!(
        link_of(&s, &phone.id()).unwrap().role,
        Some(LinkRole::Initiator),
        "the computer's own connection is the one kept"
    );
    // the phone's connection was closed by the computer, the kept one carries traffic
    assert!(
        timeout(Duration::from_secs(5), b.recv())
            .await
            .unwrap()
            .is_err()
    );
    a.send(&kde::Packet::new("kdeconnect.ping", Default::default()))
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;
    let s = rig.status().await;
    assert_eq!(link_of(&s, &phone.id()).unwrap().pings_received, 1);
}

#[tokio::test]
async fn when_the_phone_has_the_smaller_id_its_connection_replaces_the_computers_dial() {
    let (rig, phone, mut b, a) = cross_dial(false, true).await;
    // the computer's dial was given up for the phone's connection
    assert!(a.is_err(), "the computer's dial must have been abandoned");
    let s = rig
        .wait("one connected link", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    assert_eq!(
        link_of(&s, &phone.id()).unwrap().role,
        Some(LinkRole::Acceptor)
    );
    b.send(&kde::Packet::new("kdeconnect.ping", Default::default()))
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;
    let s = rig.status().await;
    assert_eq!(link_of(&s, &phone.id()).unwrap().pings_received, 1);
}

#[tokio::test]
async fn a_stock_phone_keeps_the_newest_connection_whatever_the_ids() {
    // Stock KDE Connect replaces the old connection with the newest; the computer does the same for it.
    let (rig, phone, mut b, a) = cross_dial(true, false).await;
    assert!(a.is_err(), "the computer's dial is replaced by the phone's");
    let s = rig
        .wait("one connected link", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    assert_eq!(
        link_of(&s, &phone.id()).unwrap().role,
        Some(LinkRole::Acceptor)
    );
    b.send(&kde::Packet::new("kdeconnect.ping", Default::default()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_goodbye_does_not_disturb_the_tie_break_when_both_ends_dial_again() {
    // After the user's disconnect the phone is told to stay away, then the user taps Connect on the phone while
    // the computer dials as well: exactly one link survives, the same rule for either id order.
    for above in [true, false] {
        let (rig, phone, _b, _a) = cross_dial_after(above, true, true).await;
        let s = rig
            .wait("one connected link", |s| {
                link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
            })
            .await;
        assert_eq!(s.links.len(), 1);
    }
}
