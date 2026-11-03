// SPDX-License-Identifier: GPL-3.0-or-later
//! The computer's polite goodbye and the listening-port hint, against stand-in phones built on `bridge_kde`.
//! A GeekBridge phone that declares `geekbridge.goodbye` is told why the computer hangs up (user disconnect,
//! shutdown); one that does not declare it, and any stock peer, is not sent the packet. The secure identity
//! carries the computer's real listening port for phones that only know its address.

mod support;

use bridge_core::goodbye::{GOODBYE, Reason};
use bridge_core::ipc::Request;
use bridge_kde as kde;
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;

fn stand_in_phone(goodbye: bool, geekbridge: bool) -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut incoming = vec!["kdeconnect.ping".to_string()];
    if geekbridge {
        incoming.push(bridge_core::heartbeat::MARKER.to_string());
    }
    if goodbye {
        incoming.push(GOODBYE.to_string());
    }
    let outgoing: Vec<String> = incoming.iter().filter(|c| *c != GOODBYE).cloned().collect();
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        incoming,
        outgoing,
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

/// Everything the phone receives until the computer closes the link (or a short silence).
async fn until_closed(link: &mut kde::TrustedLink) -> (Vec<kde::Packet>, bool) {
    let mut got = Vec::new();
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match link.recv().await {
                Ok(kde::LinkEvent::Packet(p)) => got.push(p),
                Ok(_) => {}
                Err(_) => return true,
            }
        }
    })
    .await
    .unwrap_or(false);
    (got, closed)
}

fn goodbyes(packets: &[kde::Packet]) -> Vec<kde::Packet> {
    packets
        .iter()
        .filter(|p| p.kind == GOODBYE)
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_user_disconnect_tells_a_geekbridge_phone_why_before_closing() {
    let rig = Rig::start().await;
    let phone = stand_in_phone(true, true);
    let mut link = pair_phone(&rig, &phone).await;
    rig.ok(Request::Disconnect {
        device_id: phone.id(),
    })
    .await;
    let (got, closed) = until_closed(&mut link).await;
    assert!(closed, "the computer closes after the goodbye");
    let bye = goodbyes(&got);
    assert_eq!(bye.len(), 1, "{got:?}");
    assert_eq!(
        bridge_core::goodbye::parse(&bye[0].body),
        Ok(Reason::UserDisconnect)
    );
}

/// The next goodbye the phone receives within a few seconds, ignoring other traffic.
async fn next_goodbye(link: &mut kde::TrustedLink) -> Option<kde::Packet> {
    timeout(Duration::from_secs(5), async {
        loop {
            match link.recv().await {
                Ok(kde::LinkEvent::Packet(p)) if p.kind == GOODBYE => return Some(p),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn a_service_shutdown_says_shutdown_to_every_geekbridge_phone() {
    let rig = Rig::start().await;
    let a = stand_in_phone(true, true);
    let mut link_a = pair_phone(&rig, &a).await;
    let b = stand_in_phone(true, true);
    let mut link_b = pair_phone(&rig, &b).await;
    rig.daemon.goodbye_all().await;
    for link in [&mut link_a, &mut link_b] {
        let bye = next_goodbye(link).await.expect("a goodbye");
        assert_eq!(bridge_core::goodbye::parse(&bye.body), Ok(Reason::Shutdown));
    }
}

#[tokio::test]
async fn a_phone_that_does_not_declare_goodbye_is_never_sent_one() {
    // A GeekBridge phone of an older build (heartbeat marker only) and a plain peer.
    for (goodbye, geekbridge) in [(false, true), (false, false)] {
        let rig = Rig::start().await;
        let phone = stand_in_phone(goodbye, geekbridge);
        let mut link = pair_phone(&rig, &phone).await;
        rig.daemon.goodbye_all().await;
        rig.ok(Request::Disconnect {
            device_id: phone.id(),
        })
        .await;
        let (got, closed) = until_closed(&mut link).await;
        assert!(closed);
        assert!(goodbyes(&got).is_empty(), "{got:?}");
    }
}

#[tokio::test]
async fn the_goodbye_follows_the_declared_capability_alone() {
    // Declaring the type is what opens the door: the packet follows the negotiated capabilities, so a peer that
    // lists it as incoming gets it, and nothing else changes for the peer's other packets.
    let rig = Rig::start().await;
    let phone = stand_in_phone(true, false);
    let mut link = pair_phone(&rig, &phone).await;
    rig.daemon.goodbye_all().await;
    assert!(next_goodbye(&mut link).await.is_some());
}

#[tokio::test]
async fn the_secure_identity_carries_the_real_listening_port_to_a_phone_that_dials_in() {
    let mut rig = Rig::start().await;
    let inbound = rig.listen_inbound().await;
    let phone = stand_in_phone(true, true);
    let pc = rig.status().await.device_id;
    let pc = kde::DeviceId::parse(&pc).unwrap();
    drop(pair_phone(&rig, &phone).await);
    rig.wait("link gone", |s| s.links.is_empty()).await;
    // The phone dials in knowing nothing but the address: the computer's identity says where it listens.
    let hs = timeout(
        Duration::from_secs(15),
        phone.endpoint.connect(inbound, &pc),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(hs.info().peer.geekbridge_port, Some(inbound.port()));
}

#[tokio::test]
async fn the_port_hint_is_also_sent_when_the_computer_dials_the_phone() {
    let mut rig = Rig::start().await;
    let inbound = rig.listen_inbound().await;
    let phone = stand_in_phone(true, true);
    let first = pair_phone(&rig, &phone).await;
    // pair_phone happened over a computer-initiated dial: its secure identity already had the port.
    assert_eq!(first.info().peer.geekbridge_port, Some(inbound.port()));
}
