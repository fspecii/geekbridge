// SPDX-License-Identifier: GPL-3.0-or-later
//! The service against a stand-in for the REAL stock KDE Connect Android app: a genuine `bridge_kde`
//! endpoint whose capability lists are copied from the identity captured from 1.37.0 (F-Droid), not
//! hand-written. Pins what a stock peer must never see or cause: our own capability lists carry nothing
//! it would act on wrongly, no GeekBridge-only packet is sent to it, and its unasked traffic (battery
//! without consent, packets we never declared) neither takes the link down nor leaks.
//!
//! This is a pinned-capture check; the live one needs a stock KDE Connect tree (`scripts/e2e/coexist.sh`).

mod support;

use bridge_core::ipc::{ErrorCode, Feature, Request};
use bridge_kde::{self as kde, Identity, Packet};
use bridge_service::gate;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;
use support::{Phone, Rig, link_of, pair_phone};
use tokio::time::timeout;

fn stock_identity() -> Identity {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bridge-kde/tests/fixtures/kdeconnect-android-real/android-identity-secure.json"
    );
    let line = std::fs::read(path).unwrap();
    Identity::from_packet(&Packet::from_line(&line[..line.len() - 1]).unwrap()).unwrap()
}

fn stock_phone() -> Arc<Phone> {
    let id = stock_identity();
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Stock Phone",
        kde::DeviceType::Phone,
        id.incoming_capabilities,
        id.outgoing_capabilities,
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn battery(level: i64) -> Packet {
    let Value::Object(m) =
        json!({"currentCharge": level, "isCharging": false, "thresholdEvent": 0})
    else {
        unreachable!()
    };
    Packet::new("kdeconnect.battery", m)
}

/// Nothing but (at most) a manual ping reaches the stock phone within a short window.
async fn quiet(link: &mut kde::TrustedLink) -> Vec<String> {
    let mut seen = Vec::new();
    let _ = timeout(Duration::from_millis(500), async {
        while let Ok(ev) = link.recv().await {
            if let kde::LinkEvent::Packet(p) = ev {
                seen.push(p.kind);
            }
        }
    })
    .await;
    seen
}

#[test]
fn our_identity_declares_nothing_a_stock_peer_would_misread() {
    let stock = stock_identity();
    // The heartbeat ping would show as a "Ping!" notification on a stock peer: it is not one.
    assert!(!bridge_core::heartbeat::is_geekbridge_peer(
        &stock.incoming_capabilities,
        &stock.outgoing_capabilities
    ));
    // Every stock-namespace type we declare is one with working code behind it; none of the
    // telephony / dial families a stock phone has plugins for.
    for kind in gate::LOCAL_INCOMING.iter().chain(&gate::LOCAL_OUTGOING) {
        for banned in [
            "kdeconnect.telephony",
            "kdeconnect.mpris",
            "kdeconnect.runcommand",
            "kdeconnect.sftp",
            "kdeconnect.findmyphone",
            "kdeconnect.connectivity_report",
            "kdeconnect.photo",
            "kdeconnect.presenter",
            "kdeconnect.sms.dial",
            "kdeconnect.dial",
        ] {
            assert!(!kind.starts_with(banned), "{kind}");
        }
    }
    let no_dups = |l: &[&str]| {
        let mut v = l.to_vec();
        v.sort_unstable();
        v.dedup();
        v.len() == l.len()
    };
    assert!(no_dups(&gate::LOCAL_INCOMING) && no_dups(&gate::LOCAL_OUTGOING));
    // Everything GeekBridge-only, in both directions, is invisible to a stock phone's negotiation.
    for kind in gate::negotiated_incoming(&stock)
        .iter()
        .chain(&gate::negotiated_outgoing(&stock))
    {
        assert!(!kind.starts_with("geekbridge."), "{kind}");
    }
}

#[tokio::test]
async fn stock_phone_gets_no_geekbridge_packet_and_no_dial_even_with_every_consent_on() {
    let rig = Rig::start().await;
    let phone = stock_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    // What the stock phone was told about us at the handshake.
    let ours = &phone_link.info().peer;
    assert!(
        ours.incoming_capabilities
            .iter()
            .any(|c| c == "kdeconnect.ping")
    );
    assert!(
        !ours
            .incoming_capabilities
            .iter()
            .chain(&ours.outgoing_capabilities)
            .any(|c| c.starts_with("kdeconnect.telephony") || c.contains("dial")),
        "{ours:?}"
    );
    for feature in [
        Feature::Telephony,
        Feature::Clipboard,
        Feature::Battery,
        Feature::Notifications,
        Feature::Sms,
        Feature::Contacts,
        Feature::Calendar,
        Feature::Debug,
        Feature::Photos,
    ] {
        rig.ok(Request::SetConsent {
            device_id: phone.id(),
            feature,
            enabled: true,
        })
        .await;
    }
    // A dial cannot even be aimed at it: it never declared the GeekBridge call packet.
    let source = format!("link:{}", phone.id());
    assert_eq!(
        rig.refused(Request::CallDial {
            source: source.clone(),
            number: bridge_core::calls::Sensitive::new("+15550100"),
        })
        .await,
        ErrorCode::NotFound
    );
    let seen = quiet(&mut phone_link).await;
    // With consent the stock phone is asked for the data it declared it serves (verified live), and for
    // nothing else: no GeekBridge extension, no telephony/dial, no ping, no battery request.
    let negotiated = gate::negotiated_outgoing(&stock_identity());
    assert!(
        seen.iter().all(|k| negotiated.contains(k)
            && k != "kdeconnect.ping"
            && !k.contains("battery")
            && !k.contains("photos")),
        "stock phone received {seen:?}"
    );
    // A user-initiated ping is the only ping it gets (the heartbeat is for GeekBridge peers).
    rig.ok(Request::Ping {
        device_id: phone.id(),
    })
    .await;
    assert_eq!(quiet(&mut phone_link).await, ["kdeconnect.ping"]);
}

#[tokio::test]
async fn stock_battery_is_dropped_without_consent_and_never_spends_the_misbehaviour_budget() {
    let rig = Rig::start().await;
    let phone = stock_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    // Stock sends its level on every change; far more than the rejection budget over a day.
    for i in 0..150 {
        phone_link.send(&battery(90 - i % 50)).await.unwrap();
    }
    phone_link
        .send(&Packet::new("kdeconnect.ping", Map::new()))
        .await
        .unwrap();
    let s = rig
        .wait("ping after battery flood", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.pings_received == 1)
        })
        .await;
    let l = link_of(&s, &phone.id()).unwrap();
    assert_eq!(
        l.rejected_packets, 0,
        "unconsented battery is not misbehaviour"
    );
    assert!(l.battery.is_none(), "no reading is kept without consent");
    // With consent the same packets are shown.
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Battery,
        enabled: true,
    })
    .await;
    phone_link.send(&battery(64)).await.unwrap();
    let s = rig
        .wait("battery shown", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.battery.is_some())
        })
        .await;
    assert_eq!(
        link_of(&s, &phone.id()).unwrap().battery.unwrap().percent,
        64
    );
    // A stock "unknown level" (-1) is not a reading and not an offence.
    phone_link.send(&battery(-1)).await.unwrap();
    phone_link
        .send(&Packet::new("kdeconnect.ping", Map::new()))
        .await
        .unwrap();
    let s = rig
        .wait("second ping", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.pings_received == 2)
        })
        .await;
    assert_eq!(link_of(&s, &phone.id()).unwrap().rejected_packets, 0);
}

#[tokio::test]
async fn packets_we_never_declared_are_dropped_unread_and_the_link_survives() {
    let rig = Rig::start().await;
    let phone = stock_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    const SECRET: &str = "SECRET-NOTIFICATION-TEXT";
    // Mirrored notifications are negotiated with a stock phone but only read with the `notifications`
    // consent: without it they are dropped unread and are not misbehaviour (stock sends one per
    // notification, far more than the budget).
    let mirrored = gate::negotiated_incoming(&stock_identity())
        .iter()
        .any(|k| k == "kdeconnect.notification");
    // Not negotiated (mirroring not built in): one stray packet, counted like any other.
    for i in 0..if mirrored { 150 } else { 1 } {
        let mut b = Map::new();
        b.insert("id".into(), format!("n{i}").into());
        b.insert("appName".into(), "App".into());
        b.insert("title".into(), SECRET.into());
        b.insert("text".into(), SECRET.into());
        phone_link
            .send(&Packet::new("kdeconnect.notification", b))
            .await
            .unwrap();
    }
    for kind in [
        "kdeconnect.telephony",
        "kdeconnect.connectivity_report",
        "kdeconnect.mpris",
        "kdeconnect.something.new",
    ] {
        let mut b = Map::new();
        b.insert("text".into(), SECRET.into());
        phone_link.send(&Packet::new(kind, b)).await.unwrap();
    }
    let s = rig
        .wait("dropped", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.rejected_packets == 4 + u64::from(!mirrored))
        })
        .await;
    assert_eq!(
        link_of(&s, &phone.id()).unwrap().phase,
        bridge_core::ipc::LinkPhase::Connected
    );
    let all = format!(
        "{}{}",
        serde_json::to_string(&s).unwrap(),
        serde_json::to_string(&rig.events().await).unwrap()
    );
    assert!(!all.contains(SECRET));
    // Only one event for the whole burst: the user is told once, not once per packet.
    assert_eq!(all.matches("packet_rejected").count(), 1);
}
