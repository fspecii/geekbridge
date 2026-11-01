//! QR pairing against a real service: the phone side is a `bridge_kde` endpoint that dials the
//! service's inbound listener with the token it "scanned". Covers redemption, reuse, a wrong
//! token, revocation, and that a token-less request still needs the normal explicit approval.

use bridge_core::{
    Snapshot,
    ipc::{ErrorCode, EventKind, Request, Response},
};
use bridge_kde::{self as kde, qrpair::QrPayload};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    control::Control,
    daemon::{Daemon, DaemonOptions},
    prepare_control, serve_inbound,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, request: kde::ApprovalRequest) -> bool {
        assert!(request.qr_pinned || request.presented_token.is_none());
        true
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    daemon: Arc<Daemon>,
    addr: SocketAddr,
    inbound: JoinHandle<std::io::Result<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.inbound.abort();
    }
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
    let mut options = DaemonOptions::new("QR Desktop", Arc::new(DisabledCallingProbe));
    options.dev_loopback = true;
    let Control::Ready { daemon, .. } =
        prepare_control(&tmp.path().join("state"), discovery, options)
    else {
        panic!("private state should open");
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    daemon.set_inbound_listener(addr);
    let inbound = tokio::spawn(serve_inbound(listener, daemon.clone()));
    Rig {
        _tmp: tmp,
        daemon,
        addr,
        inbound,
    }
}

fn phone() -> kde::Endpoint {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    kde::Endpoint::new(
        creds,
        "Scanning Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into()],
        vec!["kdeconnect.ping".into()],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap()
}

async fn qr(rig: &Rig) -> QrPayload {
    match rig.daemon.handle(Request::PairQr).await {
        Response::PairQr(q) => {
            assert!(q.expires_in_ms > 0);
            QrPayload::parse(&q.payload).expect("the service writes a payload the phone can parse")
        }
        other => panic!("expected a QR, got {other:?}"),
    }
}

/// The phone side of a scan: dial, check the pinned certificate, send the token.
async fn scan(
    phone: &kde::Endpoint,
    payload: &QrPayload,
    token: &str,
    dial: SocketAddr,
) -> kde::Result<kde::TrustedLink> {
    let id = kde::DeviceId::parse(&payload.device_id).unwrap();
    let Ok(kde::Handshake::Pending(link)) = phone.connect(dial, &id).await else {
        panic!("expected a pending link");
    };
    assert_eq!(link.info().peer_cert.fingerprint_hex(), payload.cert_sha256);
    link.with_pair_token(token.to_owned())
        .pair(kde::PairMode::Request, &mut Yes)
        .await
}

async fn events(rig: &Rig) -> Vec<EventKind> {
    match rig.daemon.handle(Request::Events { after: 0 }).await {
        Response::Events(p) => p.events.into_iter().map(|e| e.kind).collect(),
        other => panic!("{other:?}"),
    }
}

async fn paired_count(rig: &Rig) -> usize {
    rig.daemon.status().unwrap().paired.len()
}

#[tokio::test]
async fn scanned_qr_pairs_with_no_code_comparison() {
    let rig = rig().await;
    let payload = qr(&rig).await;
    assert_eq!(payload.cert_sha256, rig.daemon.own_cert_sha256());
    assert_eq!(payload.name, "QR Desktop");
    let p = phone();
    let link = tokio::time::timeout(
        Duration::from_secs(20),
        scan(&p, &payload, &payload.token, rig.addr),
    )
    .await
    .unwrap()
    .expect("pairs");
    assert_eq!(link.info().peer.device_id.as_str(), payload.device_id);
    assert_eq!(paired_count(&rig).await, 1);
    assert!(
        rig.daemon.status().unwrap().challenges.is_empty(),
        "no challenge was ever shown"
    );
    assert!(
        events(&rig)
            .await
            .iter()
            .any(|k| matches!(k, EventKind::QrAccepted))
    );
}

#[tokio::test]
async fn a_token_works_once() {
    let rig = rig().await;
    let payload = qr(&rig).await;
    scan(&phone(), &payload, &payload.token, rig.addr)
        .await
        .expect("first scan");
    let second = scan(&phone(), &payload, &payload.token, rig.addr).await;
    assert!(second.is_err(), "reuse must fail");
    assert_eq!(paired_count(&rig).await, 1);
    let refusals: Vec<_> = events(&rig)
        .await
        .into_iter()
        .filter_map(|k| match k {
            EventKind::QrRefused { reason } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(refusals.len(), 1);
    assert!(refusals[0].contains("already used"), "{refusals:?}");
}

#[tokio::test]
async fn a_wrong_token_pairs_nothing() {
    let rig = rig().await;
    let payload = qr(&rig).await;
    let wrong = "f".repeat(32);
    assert!(scan(&phone(), &payload, &wrong, rig.addr).await.is_err());
    assert_eq!(paired_count(&rig).await, 0);
    // The real token still works afterwards: a stranger's guess does not burn it.
    scan(&phone(), &payload, &payload.token, rig.addr)
        .await
        .expect("real token");
}

#[tokio::test]
async fn a_cancelled_qr_is_dead() {
    let rig = rig().await;
    let payload = qr(&rig).await;
    assert!(matches!(
        rig.daemon.handle(Request::PairQrCancel).await,
        Response::Ack
    ));
    assert!(
        scan(&phone(), &payload, &payload.token, rig.addr)
            .await
            .is_err()
    );
    assert_eq!(paired_count(&rig).await, 0);
}

#[tokio::test]
async fn a_request_without_a_token_still_waits_for_an_explicit_approval() {
    let rig = rig().await;
    let p = phone();
    let id = kde::DeviceId::parse(&rig.daemon.status().unwrap().device_id).unwrap();
    let Ok(kde::Handshake::Pending(link)) = p.connect(rig.addr, &id).await else {
        panic!("pending");
    };
    let pairing = tokio::spawn(async move { link.pair(kde::PairMode::Request, &mut Yes).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let challenge = loop {
        if let Some(c) = rig.daemon.status().unwrap().challenges.first().cloned() {
            break c;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no challenge appeared"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(
        paired_count(&rig).await,
        0,
        "nothing is paired before approval"
    );
    assert!(matches!(
        rig.daemon
            .handle(Request::Approve {
                challenge_id: challenge.challenge_id
            })
            .await,
        Response::Ack
    ));
    pairing.await.unwrap().expect("paired after approval");
    assert_eq!(paired_count(&rig).await, 1);
}

#[tokio::test]
async fn the_qr_needs_a_listener() {
    let tmp = tempfile::tempdir().unwrap();
    let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
    let Control::Ready { daemon, .. } = prepare_control(
        &tmp.path().join("state"),
        discovery,
        DaemonOptions::new("No Listener", Arc::new(DisabledCallingProbe)),
    ) else {
        panic!()
    };
    match daemon.handle(Request::PairQr).await {
        Response::Error(e) => assert_eq!(e.code, ErrorCode::Unsupported),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn test_connection_tells_reachable_from_closed() {
    let rig = rig().await;
    let open = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let open_port = open.local_addr().unwrap().port();
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let probe = |port| Request::TestConnection {
        device_id: None,
        endpoint: Some(bridge_core::ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port,
        }),
    };
    let Response::ConnectionTest(t) = rig.daemon.handle(probe(open_port)).await else {
        panic!()
    };
    assert!(
        t.steps.iter().any(|s| s.label == "Phone answers" && s.ok),
        "{t:?}"
    );
    let Response::ConnectionTest(t) = rig.daemon.handle(probe(closed)).await else {
        panic!()
    };
    assert!(!t.ok);
    assert!(
        t.steps
            .iter()
            .any(|s| s.label == "Phone answers" && !s.ok && s.detail.contains("refused")),
        "{t:?}"
    );
}

#[tokio::test]
async fn announcement_names_the_computer_and_its_port() {
    let rig = rig().await;
    let line = rig.daemon.announcement(1717);
    let packet = kde::Packet::from_line(line.strip_suffix(b"\n").unwrap_or(&line)).unwrap();
    assert_eq!(packet.kind, "kdeconnect.identity");
    assert_eq!(packet.body["deviceName"], "QR Desktop");
    assert_eq!(packet.body["tcpPort"], 1717);
    assert_eq!(packet.body["deviceType"], "desktop");
}
