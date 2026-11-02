// SPDX-License-Identifier: GPL-3.0-or-later
//! Leak and growth assertions across many reconnect / pair / junk cycles.
//!
//! The service core and the stand-in phones share this test process, so after each cycle every
//! socket on both sides must be closed again: the process fd count has to return to its baseline
//! and resident memory has to stay flat after a warm-up. Tests here take a lock so no other test
//! in this binary perturbs the process-wide counters. `GEEKBRIDGE_SOAK_CYCLES` scales the cycles
//! (scripts/soak.sh sets it high and runs in release mode).

mod support;

use bridge_core::ipc::Request;
use bridge_kde as kde;
use std::{sync::OnceLock, time::Duration};
use support::*;
use tokio::{io::AsyncWriteExt, net::TcpStream, sync::Mutex, time::sleep};

fn serial() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn cycles(default: usize) -> usize {
    std::env::var("GEEKBRIDGE_SOAK_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Sockets close asynchronously; wait for the count to settle at or below `limit`.
async fn settle_fds(limit: usize) -> usize {
    let mut now = fd_count();
    for _ in 0..200 {
        if now <= limit {
            break;
        }
        sleep(Duration::from_millis(25)).await;
        now = fd_count();
    }
    now
}

struct Sample {
    fds: usize,
    rss_kib: usize,
}

fn sample() -> Sample {
    Sample {
        fds: fd_count(),
        rss_kib: rss_kib(),
    }
}

/// RSS is allocator- and test-profile dependent, so allow generous but bounded growth: a real leak
/// of a few KiB per cycle shows up over hundreds of cycles; noise does not reach this bound.
const RSS_GROWTH_BUDGET_KIB: usize = if cfg!(debug_assertions) {
    6 * 1024
} else {
    1536
};
const FD_SLACK: usize = 4;

#[tokio::test]
async fn many_reconnect_cycles_leak_neither_fds_nor_memory() {
    let _guard = serial().lock().await;
    let rig = Rig::start().await;
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    rig.wait("first link gone", |s| s.links.is_empty()).await;

    let total = cycles(80);
    let warmup = (total / 10).max(10);
    let mut baseline = None;
    for i in 0..total {
        let link = reconnect(&rig, &phone).await;
        if i % 2 == 0 {
            drop(link); // phone vanishes
        } else {
            rig.ok(Request::Disconnect {
                device_id: phone.id(),
            })
            .await; // service hangs up
            drop(link);
        }
        rig.wait("link gone", |s| s.links.is_empty()).await;
        if i + 1 == warmup {
            settle_fds(0).await;
            baseline = Some(sample());
        }
    }
    let base = baseline.expect("warm-up ran");
    let end_fds = settle_fds(base.fds + FD_SLACK).await;
    let end = sample();
    eprintln!(
        "reconnect cycles={total}: fds {} -> {end_fds}, rss {} KiB -> {} KiB",
        base.fds, base.rss_kib, end.rss_kib
    );
    assert!(
        end_fds <= base.fds + FD_SLACK,
        "fd leak: {} -> {end_fds} over {} cycles",
        base.fds,
        total - warmup
    );
    assert!(
        end.rss_kib <= base.rss_kib + RSS_GROWTH_BUDGET_KIB,
        "memory growth: {} KiB -> {} KiB over {} cycles",
        base.rss_kib,
        end.rss_kib,
        total - warmup
    );
    // Bounded bookkeeping: the event log is a ring, the link table is empty, one pairing remains.
    let status = rig.status().await;
    assert!(status.links.is_empty() && status.challenges.is_empty());
    assert_eq!(status.paired.len(), 1);
    assert!(rig.events().await.events.len() <= 256);
}

#[tokio::test]
async fn pair_unpair_cycles_leak_nothing_and_leave_no_state_behind() {
    let _guard = serial().lock().await;
    let rig = Rig::start().await;
    let mut files_after_first = 0;
    let total = cycles(80) / 4 + 5;
    let warmup = 5.min(total / 2);
    let mut baseline = None;
    for i in 0..total {
        // A fresh phone identity each time, like many different devices over the years.
        let phone = Phone::new();
        let mut link = pair_phone(&rig, &phone).await;
        rig.ok(Request::Unpair {
            device_id: phone.id(),
        })
        .await;
        let event = tokio::time::timeout(Duration::from_secs(5), link.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, kde::LinkEvent::PeerUnpaired));
        drop(link);
        rig.wait("all gone", |s| s.links.is_empty() && s.paired.is_empty())
            .await;
        if i == 0 {
            // Stores create their files on first use; after that the set must not grow.
            files_after_first = rig.state_files();
        }
        if i + 1 == warmup {
            settle_fds(0).await;
            baseline = Some(sample());
        }
    }
    let base = baseline.unwrap();
    let end_fds = settle_fds(base.fds + FD_SLACK).await;
    let end = sample();
    eprintln!(
        "pair/unpair cycles={total}: fds {} -> {end_fds}, rss {} KiB -> {} KiB",
        base.fds, base.rss_kib, end.rss_kib
    );
    assert!(end_fds <= base.fds + FD_SLACK, "fd leak");
    assert!(
        end.rss_kib <= base.rss_kib + RSS_GROWTH_BUDGET_KIB,
        "memory growth"
    );
    assert_eq!(rig.trust_len(), 0);
    assert_eq!(rig.state_files(), files_after_first, "state directory grew");
}

#[tokio::test]
async fn junk_connection_churn_leaks_nothing() {
    let _guard = serial().lock().await;
    let mut rig = Rig::start_with(|o| {
        o.transport.timeouts.plaintext_identity = Duration::from_millis(200);
        o.transport.timeouts.tls_handshake = Duration::from_millis(200);
        o.transport.timeouts.secure_identity = Duration::from_millis(200);
    })
    .await;
    let addr = rig.listen_inbound().await;
    let mut rng = Rng(0x5eed_0000_dead_0001);
    let total = cycles(80) * 4;
    let warmup = total / 10;
    let mut baseline = None;
    for i in 0..total {
        let mut s = TcpStream::connect(addr).await.unwrap();
        match rng.below(3) {
            0 => {}
            1 => {
                let junk: Vec<u8> = (0..rng.below(600)).map(|_| rng.next() as u8).collect();
                let _ = s.write_all(&junk).await;
            }
            _ => {
                let _ = s.write_all(&vec![b'z'; 20 * 1024]).await;
            }
        }
        drop(s);
        if i % 16 == 15 {
            sleep(Duration::from_millis(250)).await; // let handshake deadlines pass
        }
        if i + 1 == warmup {
            sleep(Duration::from_millis(400)).await;
            settle_fds(0).await;
            baseline = Some(sample());
        }
    }
    sleep(Duration::from_millis(400)).await;
    let base = baseline.unwrap();
    let end_fds = settle_fds(base.fds + FD_SLACK).await;
    let end = sample();
    eprintln!(
        "junk connections={total}: fds {} -> {end_fds}, rss {} KiB -> {} KiB",
        base.fds, base.rss_kib, end.rss_kib
    );
    assert!(end_fds <= base.fds + FD_SLACK, "fd leak");
    assert!(
        end.rss_kib <= base.rss_kib + RSS_GROWTH_BUDGET_KIB,
        "memory growth"
    );
    let status = rig.status().await;
    assert!(status.links.is_empty() && status.paired.is_empty());
}
