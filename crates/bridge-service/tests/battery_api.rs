// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone battery over the stock `kdeconnect.battery*` packets, against a real service and a stand-in
//! phone that is a genuine `bridge_kde` endpoint: consent on both ends, malformed readings, the unknown
//! level, withdrawal, and a fresh link starting without the old reading.

mod support;

use bridge_core::battery_link as bat;
use bridge_core::ipc::{Feature, Request};
use bridge_kde::{self as kde, Packet};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;
use support::{Phone, Rig, link_of, pair_phone, reconnect};
use tokio::time::timeout;

fn battery_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into(), bat::REQUEST.into()],
        vec!["kdeconnect.ping".into(), bat::BATTERY.into()],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn raw(body: Value) -> Packet {
    let Value::Object(m) = body else {
        unreachable!()
    };
    Packet::new(bat::BATTERY, m)
}

fn level(percent: u8, charging: bool, low: bool) -> Packet {
    Packet::new(bat::BATTERY, bat::body(percent, charging, low))
}

async fn consent(rig: &Rig, phone: &Phone, enabled: bool) {
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Battery,
        enabled,
    })
    .await;
}

/// All `kdeconnect.battery.request` packets the service sends within a short window.
async fn requests(link: &mut kde::TrustedLink, window: Duration) -> Vec<Map<String, Value>> {
    let mut seen = Vec::new();
    let _ = timeout(window, async {
        while let Ok(ev) = link.recv().await {
            if let kde::LinkEvent::Packet(p) = ev
                && p.kind == bat::REQUEST
            {
                seen.push(p.body);
            }
        }
    })
    .await;
    seen
}

/// A ping after a batch makes sure earlier packets were processed before the status is read.
async fn flush(rig: &Rig, phone: &Phone, link: &mut kde::TrustedLink, want_pings: u64) {
    link.send(&Packet::new("kdeconnect.ping", Map::new()))
        .await
        .unwrap();
    rig.wait("ping processed", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.pings_received >= want_pings)
    })
    .await;
}

#[tokio::test]
async fn the_reading_needs_consent_shows_every_field_and_withdrawal_erases_it() {
    let rig = Rig::start().await;
    let phone = battery_phone();
    let mut link = pair_phone(&rig, &phone).await;

    // No consent: nothing is asked of the phone and nothing is kept.
    assert!(
        requests(&mut link, Duration::from_millis(400))
            .await
            .is_empty()
    );
    link.send(&level(50, false, false)).await.unwrap();
    flush(&rig, &phone, &mut link, 1).await;
    assert!(
        link_of(&rig.status().await, &phone.id())
            .unwrap()
            .battery
            .is_none()
    );

    // Turning it on asks the phone once, with the stock request body.
    consent(&rig, &phone, true).await;
    let asked = requests(&mut link, Duration::from_secs(2)).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0], bat::request_body());

    // Edge values and flags pass through exactly.
    for (pct, charging, low) in [
        (0u8, false, true),
        (100, true, false),
        (15, false, true),
        (42, true, false),
    ] {
        link.send(&level(pct, charging, low)).await.unwrap();
        let s = rig
            .wait("reading shown", |s| {
                link_of(s, &phone.id())
                    .and_then(|l| l.battery)
                    .is_some_and(|b| b.percent == pct && b.charging == charging && b.low == low)
            })
            .await;
        let b = link_of(&s, &phone.id()).unwrap().battery.unwrap();
        assert!(b.text().starts_with(&format!("Battery {pct}%")));
        assert_eq!(b.text().contains("(charging)"), charging);
    }

    // Withdrawing erases what was shown, at once, and later packets are dropped unread.
    consent(&rig, &phone, false).await;
    assert!(
        link_of(&rig.status().await, &phone.id())
            .unwrap()
            .battery
            .is_none()
    );
    link.send(&level(77, false, false)).await.unwrap();
    flush(&rig, &phone, &mut link, 2).await;
    let s = rig.status().await;
    let l = link_of(&s, &phone.id()).unwrap();
    assert!(l.battery.is_none(), "dropped unread");
    assert_eq!(
        l.rejected_packets, 0,
        "an unconsented packet is not an offence"
    );
    // Switching back on asks again.
    consent(&rig, &phone, true).await;
    assert_eq!(requests(&mut link, Duration::from_secs(2)).await.len(), 1);
}

#[tokio::test]
async fn a_malformed_reading_is_an_offence_and_never_replaces_a_good_one() {
    let rig = Rig::start().await;
    let phone = battery_phone();
    let mut link = pair_phone(&rig, &phone).await;
    consent(&rig, &phone, true).await;
    link.send(&level(61, false, false)).await.unwrap();
    rig.wait("good reading", |s| {
        link_of(s, &phone.id())
            .and_then(|l| l.battery)
            .is_some_and(|b| b.percent == 61)
    })
    .await;

    let bad = [
        json!({"currentCharge": 101, "isCharging": false}),
        json!({"currentCharge": -2, "isCharging": false}),
        json!({"currentCharge": "50", "isCharging": false}),
        json!({"currentCharge": 50.5, "isCharging": false}),
        json!({"currentCharge": 50}),
        json!({"currentCharge": 50, "isCharging": "yes"}),
        json!({"isCharging": true}),
        json!({"currentCharge": 50, "isCharging": true, "thresholdEvent": "low"}),
        json!({}),
    ];
    let before = link_of(&rig.status().await, &phone.id())
        .unwrap()
        .rejected_packets;
    link.send(&raw(bad[0].clone())).await.unwrap();
    rig.wait("first offence counted", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.rejected_packets > before)
    })
    .await;
    // The link may be cut once the budget is spent; until then the good reading must stay.
    for b in &bad[1..4] {
        if link.send(&raw(b.clone())).await.is_err() {
            break;
        }
    }
    let s = rig.status().await;
    if let Some(l) = link_of(&s, &phone.id())
        && let Some(b) = l.battery
    {
        assert_eq!(b.percent, 61, "a bad packet never changes what is shown");
    }
}

#[tokio::test]
async fn an_unknown_level_is_not_an_offence_and_leaves_the_last_reading_in_place() {
    // Deliberate (docs/reports/code-review-4.md): -1 is "not a reading", not "clear the display".
    let rig = Rig::start().await;
    let phone = battery_phone();
    let mut link = pair_phone(&rig, &phone).await;
    consent(&rig, &phone, true).await;
    link.send(&level(33, true, false)).await.unwrap();
    rig.wait("reading", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.battery.is_some())
    })
    .await;
    for _ in 0..40 {
        link.send(&raw(
            json!({"currentCharge": -1, "isCharging": false, "thresholdEvent": 0}),
        ))
        .await
        .unwrap();
    }
    flush(&rig, &phone, &mut link, 1).await;
    let s = rig.status().await;
    let l = link_of(&s, &phone.id()).unwrap();
    assert_eq!(
        l.rejected_packets, 0,
        "stock sends -1 when it does not know"
    );
    assert_eq!(l.battery.unwrap().percent, 33);
}

#[tokio::test]
async fn extra_stock_fields_are_ignored_and_a_new_link_starts_without_the_old_reading() {
    let rig = Rig::start().await;
    let phone = battery_phone();
    let mut link = pair_phone(&rig, &phone).await;
    consent(&rig, &phone, true).await;
    link.send(&raw(json!({
        "currentCharge": 88, "isCharging": true, "thresholdEvent": 0, "unknownField": [1, 2], "x": null
    })))
    .await
    .unwrap();
    rig.wait("reading", |s| {
        link_of(s, &phone.id())
            .and_then(|l| l.battery)
            .is_some_and(|b| b.percent == 88 && b.charging)
    })
    .await;
    assert_eq!(
        link_of(&rig.status().await, &phone.id())
            .unwrap()
            .rejected_packets,
        0
    );

    // The link goes away and comes back: the old number is not shown as if it were current, and the
    // computer asks the phone again by itself because the consent is still on.
    drop(link);
    rig.wait("link gone", |s| {
        link_of(s, &phone.id()).is_none_or(|l| l.battery.is_none())
    })
    .await;
    let mut again = reconnect(&rig, &phone).await;
    assert!(
        link_of(&rig.status().await, &phone.id())
            .unwrap()
            .battery
            .is_none()
    );
    let asked = requests(&mut again, Duration::from_secs(2)).await;
    assert_eq!(asked.len(), 1, "asked once at link up: {asked:?}");
}
