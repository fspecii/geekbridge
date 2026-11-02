// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard sync under rapid updates and a degraded link: bursts of local copies collapse (the
//! watcher polls), the phone only ever sees real changes, and echoes that arrive late - after a
//! newer local copy - do not start a send loop.

mod support;

use bridge_core::{
    clipboard_wire::{self, CLIPBOARD},
    ipc::{ClipboardStatus, Feature, Request, Response},
};
use bridge_kde as kde;
use bridge_service::clipboard::{ClipboardBackend, MemoryClipboard};
use std::{sync::Arc, time::Duration};
use support::chaos::*;
use support::*;
use tokio::time::{sleep, timeout};

fn stock_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let caps: Vec<String> = ["kdeconnect.ping", CLIPBOARD, "kdeconnect.clipboard.connect"]
        .map(String::from)
        .into();
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
    Arc::new(Phone { creds, endpoint })
}

async fn clipboard(rig: &Rig) -> ClipboardStatus {
    match rig.call(Request::ClipboardStatus).await {
        Ok(Response::ClipboardStatus(c)) => *c,
        other => panic!("clipboard status: {other:?}"),
    }
}

/// Everything the phone received in `window`, in order.
async fn drain(link: &mut kde::TrustedLink, window: Duration) -> Vec<String> {
    let mut got = Vec::new();
    let end = tokio::time::Instant::now() + window;
    while let Ok(ev) = timeout(
        end.saturating_duration_since(tokio::time::Instant::now()),
        link.recv(),
    )
    .await
    {
        if let Ok(kde::LinkEvent::Packet(p)) = ev
            && p.kind == CLIPBOARD
        {
            got.push(p.body["content"].as_str().unwrap_or_default().to_owned());
        }
        if tokio::time::Instant::now() >= end {
            break;
        }
    }
    got
}

async fn setup(
    faults: Faults,
) -> (
    Rig,
    Arc<MemoryClipboard>,
    Arc<Phone>,
    PhoneServer,
    kde::TrustedLink,
) {
    let clip = Arc::new(MemoryClipboard::default());
    let c2 = clip.clone();
    let rig = Rig::start_with(move |o| {
        o.clipboard = Some(c2.clone() as Arc<dyn ClipboardBackend>);
    })
    .await;
    let phone = stock_phone();
    let mut server = PhoneServer::start(&phone, faults, 5).await;
    let link = pair_through(&rig, &phone, &mut server).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Clipboard,
        enabled: true,
    })
    .await;
    // Let the watcher start and take its baseline.
    sleep(Duration::from_millis(2500)).await;
    (rig, clip, phone, server, link)
}

#[tokio::test]
async fn rapid_local_copies_collapse_and_the_last_value_wins() {
    let faults = Faults {
        latency: Duration::from_millis(10),
        fragment: true,
        ..Faults::default()
    };
    let (rig, clip, _phone, _server, mut link) = setup(faults).await;
    let bursts = scaled(5, 40);
    let mut last = String::new();
    for b in 0..bursts {
        for i in 0..50 {
            last = format!("burst {b} item {i}");
            clip.user_copy(&last, false);
            sleep(Duration::from_millis(3)).await;
        }
        let got = drain(&mut link, Duration::from_secs(4)).await;
        assert!(!got.is_empty(), "burst {b}: nothing reached the phone");
        assert!(
            got.len() <= 8,
            "burst {b}: 50 copies produced {} sends",
            got.len()
        );
        assert_eq!(
            got.last().unwrap(),
            &last,
            "burst {b}: the phone must end on the newest value"
        );
        let mut seen = std::collections::HashSet::new();
        assert!(
            got.iter().all(|g| seen.insert(g.clone())),
            "burst {b}: duplicate sends {got:?}"
        );
    }
    let d = clipboard(&rig).await.devices.remove(0);
    assert!(d.sent as usize <= bursts * 8, "{d:?}");
}

#[tokio::test]
async fn late_echoes_do_not_start_a_send_loop() {
    let (rig, clip, _phone, _server, mut link) = setup(Faults {
        latency: Duration::from_millis(20),
        ..Faults::default()
    })
    .await;
    let rounds = scaled(4, 30);
    for r in 0..rounds {
        let a = format!("A{r}");
        let b = format!("B{r}");
        clip.user_copy(&a, false);
        let got = drain(&mut link, Duration::from_secs(3)).await;
        assert_eq!(got.last(), Some(&a), "round {r}");
        // The user copies B; the phone's (late) echo of A arrives after it.
        clip.user_copy(&b, false);
        link.send(&kde::Packet::new(
            CLIPBOARD,
            clipboard_wire::update_body(&a),
        ))
        .await
        .unwrap();
        link.send(&kde::Packet::new(
            CLIPBOARD,
            clipboard_wire::update_body(&a),
        ))
        .await
        .unwrap();
        let got = drain(&mut link, Duration::from_secs(4)).await;
        // B may be sent once; A must never be sent back, and nothing may repeat.
        assert!(
            !got.contains(&a),
            "round {r}: echo of {a} was bounced back: {got:?}"
        );
        assert!(got.len() <= 1, "round {r}: {got:?}");
    }
    // Quiescence: with no new copies nothing is sent at all.
    let before = clipboard(&rig).await.devices[0].sent;
    let got = drain(&mut link, Duration::from_secs(4)).await;
    assert!(got.is_empty(), "the system must go quiet, got {got:?}");
    assert_eq!(clipboard(&rig).await.devices[0].sent, before);
}

#[tokio::test]
async fn a_phone_update_flood_is_applied_in_order_and_survives_a_link_flap() {
    let (rig, clip, phone, mut server, link) = setup(Faults::default()).await;
    let mut link = link;
    for i in 0..200 {
        link.send(&kde::Packet::new(
            CLIPBOARD,
            clipboard_wire::update_body(&format!("flood {i}")),
        ))
        .await
        .unwrap();
    }
    // Generous: a loaded macOS runner applied only 173 of the 200 updates in 10 s. The loop exits
    // as soon as the last one lands, so the bound costs nothing when the machine is healthy.
    for _ in 0..1200 {
        if clip.text().as_deref() == Some("flood 199") {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        clip.text().as_deref(),
        Some("flood 199"),
        "the last update must win"
    );
    assert_eq!(clipboard(&rig).await.devices[0].received, 200);

    // The link drops; the clipboard keeps working on the next link and the flood is not re-sent.
    server.proxy.reset_all();
    drop(link);
    rig.wait("link gone", |s| s.links.is_empty()).await;
    dial(&rig, &phone, &server).await;
    let mut link = server.next_link(Duration::from_secs(15)).await;
    wait_connected(&rig, &phone).await;
    // The watcher takes its baseline at link-up (C-10), so a copy made well inside the first poll
    // interval is still a change and must be sent, while the stale flood text is not.
    sleep(Duration::from_millis(300)).await;
    clip.user_copy("after the flap", false);
    let got = drain(&mut link, Duration::from_secs(5)).await;
    assert!(got.contains(&"after the flap".to_string()), "{got:?}");
    assert!(
        !got.iter().any(|g| g.starts_with("flood")),
        "stale content resent: {got:?}"
    );
}
