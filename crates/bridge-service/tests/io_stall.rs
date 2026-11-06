// SPDX-License-Identifier: GPL-3.0-or-later
//! A saturated disk (every durable state write takes seconds) must slow the writes down, not the
//! service: soak 5 saw a 35 s control stall with the async workers inside fsync. The runtime here has
//! the service's two workers, the writes are slowed by the test hook in `statefile`, and the
//! control API, link accept and call handling must keep answering while they are in flight.
//!
//! The delay is process-wide, so this file holds one slow test at a time.

mod support;

use bridge_core::calls_link::{self as link, LinkCallReport, PhoneState};
use bridge_core::ipc::{ErrorCode, EventKind, Feature, LinkPhase, Request, Response};
use bridge_kde::{self as kde, TrustStore as _};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Phone, Rig, link_of, pair_phone};
use tokio::time::{sleep, timeout};

static ONE_SLOW_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What "still answers" means: well inside one human-noticeable pause.
const ANSWER_WITHIN: Duration = Duration::from_millis(200);

fn calls_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into(), link::REQUEST.into()],
        vec![
            "kdeconnect.ping".into(),
            link::STATE.into(),
            link::RESULT.into(),
        ],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn ringing() -> kde::Packet {
    let mut r = LinkCallReport::idle(true, true);
    r.state = PhoneState::Ringing;
    r.number = Some(bridge_core::calls::Sensitive::new("+15550100"));
    r.name = Some(bridge_core::calls::Sensitive::new("Ada"));
    kde::Packet::new(link::STATE, r.body())
}

async fn timed<T>(what: &str, limit: Duration, f: impl std::future::Future<Output = T>) -> T {
    let started = Instant::now();
    let out = f.await;
    let took = started.elapsed();
    assert!(took < limit, "{what} took {took:?} (limit {limit:?})");
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_state_writes_do_not_stall_the_control_api_links_or_calls() {
    let _one = ONE_SLOW_TEST.lock().await;
    let names = tempfile::tempdir().unwrap(); // private (0700), like the state folder
    let name_dir = names.path().join("n");
    kde::fsutil::ensure_private_dir(&name_dir).unwrap();
    let name_file = name_dir.join("device-name");
    let rig = Rig::start_with(|o| o.device_name_file = Some(name_file.clone())).await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    let second = calls_phone();
    let l = support::listener().await;
    let second_endpoint = support::endpoint_of(&l);
    let _hold = second.answer(l);

    // From here on every durable write sleeps 2 s before its rename.
    bridge_core::statefile::set_write_delay_ms(2000);
    let started = Instant::now();
    let rig = Arc::new(rig);
    let mut writers: Vec<tokio::task::JoinHandle<_>> = Vec::new();
    for request in [
        Request::SetConsent {
            device_id: phone.id(),
            feature: Feature::Clipboard,
            enabled: true,
        },
        Request::SetDeviceName {
            name: "Slow disk desktop".into(),
        },
        // Two at once: written inline they would hold both of the service's workers.
        Request::SetDeviceName {
            name: "Slow disk desktop".into(),
        },
    ] {
        let rig = rig.clone();
        writers.push(tokio::spawn(async move { rig.call(request).await }));
    }
    sleep(Duration::from_millis(300)).await;
    let mut done = Vec::new();
    for w in &mut writers {
        done.push(if w.is_finished() {
            Some(w.await.unwrap())
        } else {
            None
        });
    }
    assert!(
        done.iter().all(Option::is_none),
        "the writes must all still be in flight (the slow-disk hook is not active): finished {done:?}"
    );

    // The control API answers.
    for _ in 0..3 {
        let status = timed("status", ANSWER_WITHIN, rig.status()).await;
        assert!(link_of(&status, &phone.id()).is_some());
        timed("ping", ANSWER_WITHIN, async {
            rig.ok(Request::Ping {
                device_id: phone.id(),
            })
            .await
        })
        .await;
        timed("calls", ANSWER_WITHIN, rig.call(Request::Calls))
            .await
            .unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // A link is accepted and reaches the pairing prompt.
    timed("link accept", Duration::from_millis(1000), async {
        rig.ok(Request::Connect {
            device_id: second.id(),
            endpoint: Some(second_endpoint),
        })
        .await;
        rig.wait("second link", |s| {
            link_of(s, &second.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
        })
        .await
    })
    .await;

    // A ring from the phone shows up on the call list.
    phone_link.send(&ringing()).await.unwrap();
    let rung = Instant::now();
    loop {
        let calls = match rig.call(Request::Calls).await.unwrap() {
            Response::Calls(c) => *c,
            other => panic!("expected calls, got {other:?}"),
        };
        if calls.view.ringing().is_some() {
            break;
        }
        assert!(
            rung.elapsed() < Duration::from_millis(1000),
            "ring not shown"
        );
        sleep(Duration::from_millis(25)).await;
    }
    assert!(
        writers.iter().any(|w| !w.is_finished()),
        "the measurements must have run while the writes were in flight"
    );

    // The writes themselves still finish, and are durable.
    for w in writers {
        let r = timeout(Duration::from_secs(60), w).await.unwrap().unwrap();
        assert!(matches!(r, Ok(Response::Ack)), "{r:?}");
    }
    assert!(started.elapsed() >= Duration::from_secs(2));
    bridge_core::statefile::set_write_delay_ms(0);
    let consent = std::fs::read_to_string(rig.state_dir.join("consent.json")).unwrap();
    assert!(consent.contains(&phone.id()), "{consent}");
    assert!(rig.trust_len() == 1);
    assert_eq!(
        std::fs::read_to_string(&name_file).unwrap(),
        "Slow disk desktop"
    );
    let _ = phone_link;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpair_that_outlives_the_control_deadline_still_finishes_and_blocks_a_new_link() {
    let _one = ONE_SLOW_TEST.lock().await;
    let rig = Arc::new(Rig::start().await);
    let phone = calls_phone();
    let _phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;

    // Each removal rewrites the primary file, the backup and the backup again: past the 8 s deadline.
    bridge_core::statefile::set_write_delay_ms(2700);
    let unpair = {
        let (rig, id) = (rig.clone(), phone.id());
        tokio::spawn(async move { rig.call(Request::Unpair { device_id: id }).await })
    };
    sleep(Duration::from_millis(500)).await;

    // While it runs, a new link to the same phone is refused as busy, and the API answers.
    timed("status", ANSWER_WITHIN, rig.status()).await;
    let l = support::listener().await;
    let busy = rig
        .refused(Request::Connect {
            device_id: phone.id(),
            endpoint: Some(support::endpoint_of(&l)),
        })
        .await;
    assert_eq!(busy, ErrorCode::Busy);

    // The client gives up at the exchange deadline; the unpair does not.
    let _ = timeout(Duration::from_secs(30), unpair).await.unwrap();
    let end = Instant::now() + Duration::from_secs(40);
    loop {
        let consent =
            std::fs::read_to_string(rig.state_dir.join("consent.json")).unwrap_or_default();
        let unpaired = rig
            .has_event(&phone.id(), |k| matches!(k, EventKind::Unpaired))
            .await;
        if rig.trust_len() == 0 && !consent.contains(&phone.id()) && unpaired {
            break;
        }
        assert!(
            Instant::now() < end,
            "unpair did not finish: trust {} consent {consent}",
            rig.trust_len()
        );
        sleep(Duration::from_millis(200)).await;
    }
    bridge_core::statefile::set_write_delay_ms(0);
    // And the phone can be linked again afterwards.
    let l = support::listener().await;
    let endpoint = support::endpoint_of(&l);
    let _answer = phone.answer(l);
    // The guard that refuses new links drops a moment after the Unpaired event.
    let end = Instant::now() + Duration::from_secs(5);
    while rig
        .call(Request::Connect {
            device_id: phone.id(),
            endpoint: Some(endpoint.clone()),
        })
        .await
        .is_err()
    {
        assert!(Instant::now() < end, "still busy");
        sleep(Duration::from_millis(50)).await;
    }
    rig.wait("a new link", |s| link_of(s, &phone.id()).is_some())
        .await;
    let _ = kde::FileTrustStore::open(&rig.state_dir).map(|s| s.list());
}
