// SPDX-License-Identifier: GPL-3.0-or-later
//! Security review 2 (docs/reports/security-review-2.md, S2-1): strangers on the LAN must not be able to
//! use up the service's link table with unpaired connections. One source address may hold only a
//! few unpaired links, and a paired phone that reconnects always gets in, even when the table is
//! full of strangers from many addresses.

// Dials from several 127/8 source addresses, which only Linux routes (macOS has just 127.0.0.1).
#![cfg(target_os = "linux")]

use bridge_core::{
    Snapshot,
    ipc::{LinkPhase, Request, Response},
};
use bridge_kde::{self as kde, qrpair::QrPayload};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    control::Control,
    daemon::{Daemon, DaemonOptions, MAX_LINKS},
    prepare_control, serve_inbound,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, net::TcpSocket, sync::RwLock, task::JoinHandle};

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
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
    let mut options = DaemonOptions::new("Starved Desktop", Arc::new(DisabledCallingProbe));
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
        "Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into()],
        vec!["kdeconnect.ping".into()],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap()
}

/// Dial the service from the loopback address `127.0.<hi>.<lo>` (every 127/8 address is local).
async fn dial_from(
    phone: &kde::Endpoint,
    source: Ipv4Addr,
    to: SocketAddr,
    target: &str,
) -> kde::Result<kde::Handshake> {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(SocketAddr::new(IpAddr::V4(source), 0)).unwrap();
    let stream = socket.connect(to).await.unwrap();
    phone
        .initiate(stream, to.ip(), &kde::DeviceId::parse(target).unwrap())
        .await
}

async fn qr(rig: &Rig) -> QrPayload {
    match rig.daemon.handle(Request::PairQr).await {
        Response::PairQr(q) => QrPayload::parse(&q.payload).unwrap(),
        other => panic!("expected a QR, got {other:?}"),
    }
}

fn service_id(rig: &Rig) -> String {
    rig.daemon.status().unwrap().device_id
}

#[tokio::test]
async fn one_address_cannot_fill_the_link_table_with_strangers() {
    let rig = rig().await;
    let target = service_id(&rig);
    let mut held = Vec::new();
    for _ in 0..8 {
        let stranger = phone();
        if let Ok(h) = dial_from(&stranger, Ipv4Addr::new(127, 0, 9, 9), rig.addr, &target).await {
            held.push((stranger, h));
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let links = rig.daemon.status().unwrap().links.len();
    assert!(
        links <= 3,
        "one source address holds {links} unpaired links"
    );
}

#[tokio::test]
async fn a_paired_phone_gets_in_when_strangers_fill_the_table() {
    let rig = rig().await;
    let target = service_id(&rig);

    // Pair a phone through a QR code, then let it go.
    let payload = qr(&rig).await;
    let paired_phone = phone();
    let Ok(kde::Handshake::Pending(link)) = dial_from(
        &paired_phone,
        Ipv4Addr::new(127, 0, 1, 1),
        rig.addr,
        &target,
    )
    .await
    else {
        panic!("expected a pending link");
    };
    let trusted = link
        .with_pair_token(payload.token.clone())
        .pair(kde::PairMode::Request, &mut Yes)
        .await
        .expect("pairs");
    drop(trusted);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Strangers from many addresses take every slot.
    let mut held = Vec::new();
    for i in 0..(MAX_LINKS as u8 + 4) {
        let stranger = phone();
        if let Ok(h) = dial_from(
            &stranger,
            Ipv4Addr::new(127, 0, 2, i + 1),
            rig.addr,
            &target,
        )
        .await
        {
            held.push((stranger, h));
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The paired phone comes back.
    let again = dial_from(
        &paired_phone,
        Ipv4Addr::new(127, 0, 1, 1),
        rig.addr,
        &target,
    )
    .await
    .expect("handshake");
    assert!(matches!(again, kde::Handshake::Trusted(_)));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let connected = rig
        .daemon
        .status()
        .unwrap()
        .links
        .iter()
        .any(|l| l.phase == LinkPhase::Connected);
    assert!(
        connected,
        "a paired phone was refused because strangers held the link table"
    );
}

#[tokio::test]
async fn a_strangers_wrong_tokens_do_not_revoke_the_code_on_screen() {
    let rig = rig().await;
    let target = service_id(&rig);
    let payload = qr(&rig).await;

    // A stranger tries wrong tokens, one connection each, until it is locked out.
    for i in 0..7u8 {
        let stranger = phone();
        let Ok(kde::Handshake::Pending(link)) =
            dial_from(&stranger, Ipv4Addr::new(127, 0, 8, 8), rig.addr, &target).await
        else {
            continue;
        };
        let wrong = format!("{:032x}", u128::from(i) + 1);
        let _ = link
            .with_pair_token(wrong)
            .pair(kde::PairMode::Request, &mut Yes)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The owner scans the code that has been on the screen all along.
    let owner = phone();
    let Ok(kde::Handshake::Pending(link)) =
        dial_from(&owner, Ipv4Addr::new(127, 0, 1, 1), rig.addr, &target).await
    else {
        panic!("expected a pending link");
    };
    let paired = link
        .with_pair_token(payload.token.clone())
        .pair(kde::PairMode::Request, &mut Yes)
        .await;
    assert!(
        paired.is_ok(),
        "the owner's scan was refused: {:?}",
        paired.err()
    );
}
