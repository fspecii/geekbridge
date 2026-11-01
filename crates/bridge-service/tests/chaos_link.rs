//! Failure injection on the phone link: pairing and reconnecting through a fault proxy, a
//! 100-cycle reconnect storm with descriptor / thread / memory growth checks, the dead-link
//! settings, and call control while the link flaps.
//!
//! Short mode (default) is CI friendly; `GEEKBRIDGE_CHAOS=soak` multiplies the work.

mod support;

use bridge_core::calls_link::{self as calls_link, LinkAction, LinkCallReport, PhoneState};
use bridge_core::ipc::{CallAction, EventKind, Feature, LinkPhase, PairMode, Request, Response};
use bridge_kde as kde;
use std::sync::Arc;
use std::{sync::OnceLock, time::Duration};
use support::chaos::*;
use support::*;
use tokio::time::timeout;
use tokio::{sync::Mutex, time::sleep};

/// These tests read process-wide counters; run them one at a time.
fn serial() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn settle_fds(limit: usize) -> usize {
    let mut now = fd_count();
    for _ in 0..300 {
        if now <= limit {
            break;
        }
        sleep(Duration::from_millis(25)).await;
        now = fd_count();
    }
    now
}

async fn no_links(rig: &Rig) {
    rig.wait("no links", |s| s.links.is_empty()).await;
}

#[tokio::test]
async fn pairing_survives_latency_loss_and_fragmentation() {
    let _g = serial().lock().await;
    let variants: Vec<(&str, Faults)> = vec![
        (
            "latency+loss",
            Faults {
                latency: Duration::from_millis(15),
                loss: 0.3,
                loss_penalty: Duration::from_millis(60),
                ..Faults::default()
            },
        ),
        (
            "byte-at-a-time",
            Faults {
                fragment: true,
                ..Faults::default()
            },
        ),
        (
            "slow-handshake",
            Faults {
                handshake_delay: Duration::from_secs(3),
                ..Faults::default()
            },
        ),
        (
            "bandwidth-cap",
            Faults {
                bandwidth: Some(8 * 1024),
                latency: Duration::from_millis(30),
                ..Faults::default()
            },
        ),
    ];
    for (i, (name, faults)) in variants.into_iter().enumerate() {
        let rig = Rig::start().await;
        let phone = Phone::new();
        let mut server = PhoneServer::start(&phone, faults, 7 + i as u64).await;
        let link = pair_through(&rig, &phone, &mut server).await;
        assert_eq!(rig.trust_len(), 1, "{name}: pairing must persist");
        let status = rig.status().await;
        assert!(status.challenges.is_empty(), "{name}: {status:#?}");
        drop(link);
        no_links(&rig).await;
    }
}

#[tokio::test]
async fn a_stalled_or_reset_pairing_fails_cleanly_and_a_retry_works() {
    let _g = serial().lock().await;
    let rig = Rig::start().await;
    let phone = Phone::new();

    // 1. The TLS handshake takes longer than its 10 s deadline: the dial must fail, leave nothing
    //    behind, and not wedge the next attempt.
    let slow = PhoneServer::start(
        &phone,
        Faults {
            handshake_delay: Duration::from_secs(14),
            ..Faults::default()
        },
        3,
    )
    .await;
    dial(&rig, &phone, &slow).await;
    let started = std::time::Instant::now();
    no_links(&rig).await;
    assert!(
        started.elapsed() < Duration::from_secs(14),
        "a stalled handshake must hit the service's deadline, not the proxy's"
    );
    assert!(
        rig.has_event(&phone.id(), |k| !matches!(k, EventKind::Paired))
            .await
    );
    drop(slow);

    // 2. A reset in the middle of the pairing exchange.
    let cut = PhoneServer::start(
        &phone,
        Faults {
            reset_after_bytes: Some(2500),
            ..Faults::default()
        },
        4,
    )
    .await;
    dial(&rig, &phone, &cut).await;
    timeout(Duration::from_secs(20), async {
        loop {
            let s = rig.status().await;
            if s.links.is_empty() {
                break;
            }
            if let Some(c) = s.challenges.first() {
                let _ = rig
                    .call(Request::Approve {
                        challenge_id: c.challenge_id.clone(),
                    })
                    .await;
            }
            if link_of(&s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired) {
                let _ = rig
                    .call(Request::Pair {
                        device_id: phone.id(),
                        mode: PairMode::Request,
                    })
                    .await;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a reset link must be torn down");
    no_links(&rig).await;
    assert!(
        rig.status().await.challenges.is_empty(),
        "no stale challenge"
    );
    assert_eq!(
        rig.trust_len(),
        0,
        "a half-finished pairing must not be trusted"
    );

    // 3. A clean path afterwards: the same device pairs normally.
    let mut ok = PhoneServer::start(&phone, Faults::default(), 5).await;
    let link = pair_through(&rig, &phone, &mut ok).await;
    assert_eq!(rig.trust_len(), 1);
    drop(link);
}

#[tokio::test]
async fn reconnect_storm_leaks_no_fds_threads_or_memory() {
    let _g = serial().lock().await;
    let rig = Rig::start().await;
    let phone = Phone::new();
    let mut server = PhoneServer::start(&phone, Faults::default(), 11).await;
    drop(pair_through(&rig, &phone, &mut server).await);
    no_links(&rig).await;

    let total = scaled(100, 1000);
    let warmup = (total / 10).max(10);
    let mut baseline = None;
    let mut rng = Rng(0x5eed);
    for i in 0..total {
        // A different failure each cycle.
        let mode = rng.below(5);
        server.proxy.set(Faults {
            reset_after_bytes: (mode == 3).then(|| 800 + rng.below(2000) as u64),
            fragment: mode == 4,
            latency: Duration::from_millis(rng.below(4) as u64),
            ..Faults::default()
        });
        dial(&rig, &phone, &server).await;
        match mode {
            3 => {} // dies on its own mid-handshake
            _ => {
                let link = server.next_link(Duration::from_secs(20)).await;
                match mode {
                    0 => drop(link),
                    1 => {
                        server.proxy.reset_all();
                        drop(link);
                    }
                    2 => {
                        rig.ok(Request::Disconnect {
                            device_id: phone.id(),
                        })
                        .await;
                        drop(link);
                    }
                    _ => drop(link),
                }
            }
        }
        no_links(&rig).await;
        if i + 1 == warmup {
            settle_fds(0).await;
            baseline = Some((fd_count(), thread_count(), rss_kib()));
        }
    }
    let (fds0, threads0, rss0) = baseline.unwrap();
    server.proxy.set(Faults::default());
    let fds = settle_fds(fds0 + 6).await;
    let (threads, rss) = (thread_count(), rss_kib());
    eprintln!(
        "storm cycles={total}: fds {fds0}->{fds}, threads {threads0}->{threads}, rss {rss0}->{rss} KiB, proxy accepted {}",
        server.proxy.accepted()
    );
    assert!(fds <= fds0 + 6, "fd leak {fds0} -> {fds}");
    assert!(
        threads <= threads0 + 16,
        "thread growth {threads0} -> {threads}"
    );
    assert!(rss <= rss0 + 8 * 1024, "memory growth {rss0} -> {rss} KiB");
    assert_eq!(server.proxy.live(), 0, "proxy connections must all be gone");
    let status = rig.status().await;
    assert!(status.links.is_empty() && status.challenges.is_empty());
    assert!(rig.events().await.events.len() <= 256);
}

/// The service must notice a vanished peer in about a minute, and the kernel does that
/// (keepalive 30 s + 3 x 10 s, user timeout 60 s). The options are private to bridge-kde, so read
/// the kernel's own view: every established link socket must have its keepalive timer armed with
/// no more than the 30 s idle time left.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn link_sockets_have_the_keepalive_timer_armed() {
    let _g = serial().lock().await;
    let rig = Rig::start().await;
    let phone = Phone::new();
    let mut server = PhoneServer::start(&phone, Faults::default(), 1).await;
    let link = pair_through(&rig, &phone, &mut server).await;
    let port = server.proxy.port();
    // The service dialled the proxy port; the proxy dialled the phone. Both ends of the first hop
    // belong to this process, as do both ends of the second hop. The first hop is the service's.
    let rows = tcp_rows();
    let service_side: Vec<_> = rows
        .iter()
        .filter(|r| r.state == 1 && (r.local_port == port || r.remote_port == port))
        .filter(|r| r.remote_port == port) // the dialling end: the service's socket
        .collect();
    assert_eq!(service_side.len(), 1, "{rows:#?}");
    let r = service_side[0];
    assert_eq!(
        r.timer, 2,
        "keepalive timer not armed on the service socket: {r:?}"
    );
    assert!(
        r.when_cs <= 3_000 + 100,
        "first probe more than 30 s away: {r:?}"
    );
    drop(link);
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct TcpRow {
    local_port: u16,
    remote_port: u16,
    state: u8,
    timer: u8,
    when_cs: u64,
}

#[cfg(target_os = "linux")]
fn tcp_rows() -> Vec<TcpRow> {
    let text = std::fs::read_to_string("/proc/net/tcp").unwrap_or_default();
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let port = |s: &str| u16::from_str_radix(s.rsplit(':').next()?, 16).ok();
            let (tr, when) = f.get(5)?.split_once(':')?;
            Some(TcpRow {
                local_port: port(f.get(1)?)?,
                remote_port: port(f.get(2)?)?,
                state: u8::from_str_radix(f.get(3)?, 16).ok()?,
                timer: u8::from_str_radix(tr, 16).ok()?,
                when_cs: u64::from_str_radix(when, 16).ok()?,
            })
        })
        .collect()
}

#[tokio::test]
async fn call_control_during_link_flaps_never_hangs_or_goes_stale() {
    let _g = serial().lock().await;
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut server = PhoneServer::start(&phone, Faults::default(), 21).await;
    let mut link = pair_through(&rig, &phone, &mut server).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;

    let rounds = scaled(5, 40);
    for round in 0..rounds {
        link.send(&report(PhoneState::Ringing)).await.unwrap();
        let c = wait_calls(&rig, "ringing", |c| c.view.ringing().is_some()).await;
        let answer = c.view.ringing().unwrap().answer.clone().unwrap();
        // The link dies at the moment the user presses Answer.
        let req = rig.call(Request::CallAction {
            target: answer,
            action: CallAction::Answer,
        });
        let kill = async {
            if round % 2 == 0 {
                server.proxy.reset_all();
            } else {
                sleep(Duration::from_millis(5)).await;
                server.proxy.reset_all();
            }
        };
        let (res, ()) = tokio::join!(timeout(Duration::from_secs(10), req), kill);
        assert!(res.is_ok(), "round {round}: the control call hung");
        drop(link);
        no_links(&rig).await;
        // A ringing call from a dead link must not stay on screen.
        let c = wait_calls(&rig, "stale rows cleared", |c| c.view.ringing().is_none()).await;
        assert!(
            c.view.rows.is_empty(),
            "round {round}: stale call rows {c:?}"
        );
        // Acting on the stale row now is refused, not queued for the next link.
        dial(&rig, &phone, &server).await;
        link = server.next_link(Duration::from_secs(15)).await;
        wait_connected(&rig, &phone).await;
        link.send(&report(PhoneState::Ringing)).await.unwrap();
        let c = wait_calls(&rig, "ringing again", |c| c.view.ringing().is_some()).await;
        let answer = c.view.ringing().unwrap().answer.clone().unwrap();
        rig.ok(Request::CallAction {
            target: answer,
            action: CallAction::Answer,
        })
        .await;
        let (id, action) = next_request(&mut link).await;
        assert_eq!(
            action,
            LinkAction::Answer,
            "round {round}: only the fresh request arrives"
        );
        link.send(&kde::Packet::new(
            calls_link::RESULT,
            calls_link::result_body(&id, Ok(())),
        ))
        .await
        .unwrap();
        link.send(&report(PhoneState::Idle)).await.unwrap();
        wait_calls(&rig, "idle", |c| c.view.rows.is_empty()).await;
    }
}

fn calls_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into(), calls_link::REQUEST.into()],
        vec![
            "kdeconnect.ping".into(),
            calls_link::STATE.into(),
            calls_link::RESULT.into(),
        ],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn report(state: PhoneState) -> kde::Packet {
    let ringing = matches!(state, PhoneState::Ringing);
    let r = LinkCallReport {
        state,
        number: ringing.then(|| bridge_core::calls::Sensitive::new("+15550100")),
        name: ringing.then(|| bridge_core::calls::Sensitive::new("Ada")),
        can_answer: true,
        can_end: true,
    };
    kde::Packet::new(calls_link::STATE, r.body())
}

async fn wait_calls(
    rig: &Rig,
    what: &str,
    cond: impl Fn(&bridge_core::ipc::CallsStatus) -> bool,
) -> bridge_core::ipc::CallsStatus {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let c = match rig.call(Request::Calls).await.unwrap() {
            Response::Calls(c) => *c,
            other => panic!("expected calls, got {other:?}"),
        };
        if cond(&c) {
            return c;
        }
        assert!(
            std::time::Instant::now() < end,
            "timed out waiting for {what}: {c:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn next_request(link: &mut kde::TrustedLink) -> (String, LinkAction) {
    loop {
        let event = timeout(Duration::from_secs(5), link.recv())
            .await
            .expect("request not sent")
            .unwrap();
        if let kde::LinkEvent::Packet(p) = event
            && p.kind == calls_link::REQUEST
        {
            return calls_link::parse_request(&p.body).unwrap();
        }
    }
}
