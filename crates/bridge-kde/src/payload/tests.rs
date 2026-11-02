// SPDX-License-Identifier: GPL-3.0-or-later
//! Payload socket tests with generated credentials over loopback.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::share::{self, FileOffer};

fn id(c: char) -> DeviceId {
    DeviceId::parse(&c.to_string().repeat(32)).unwrap()
}

fn creds(c: char) -> Arc<DeviceCredentials> {
    Arc::new(DeviceCredentials::generate_with_id(&id(c)).unwrap())
}

fn cfg() -> PayloadConfig {
    PayloadConfig {
        any_port: true,
        accept_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(3),
        ..PayloadConfig::default()
    }
}

/// `me` talking to `peer`, pinned to the peer's real certificate.
fn ctx(me: &Arc<DeviceCredentials>, peer: &Arc<DeviceCredentials>) -> PayloadContext {
    PayloadContext {
        creds: me.clone(),
        peer_id: peer.device_id().clone(),
        peer_cert: peer.cert().clone(),
        peer_ip: Ipv4Addr::LOCALHOST.into(),
        local_ip: None,
        versions: TlsVersions::Tls12And13,
        config: cfg(),
    }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

#[tokio::test]
async fn round_trip_with_progress() {
    let (a, b) = (creds('a'), creds('b'));
    let (sender, receiver) = (ctx(&a, &b), ctx(&b, &a));
    let payload = data(300_000);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let src = payload.clone();
    let c2 = cancel.clone();
    let send = tokio::spawn(async move {
        let mut last = 0;
        let r = listener
            .send(Cursor::new(src), 300_000, &mut |n| last = n, &c2)
            .await;
        (r, last)
    });
    let seen = AtomicU64::new(0);
    let mut out = Vec::new();
    let n = receiver
        .receive(
            port,
            Some(300_000),
            1 << 30,
            &mut out,
            &mut |n| seen.store(n, Ordering::SeqCst),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(n, 300_000);
    assert_eq!(out, payload);
    assert_eq!(seen.load(Ordering::SeqCst), 300_000);
    let (r, last) = send.await.unwrap();
    assert_eq!(r.unwrap(), 300_000);
    assert_eq!(last, 300_000);
}

#[tokio::test]
async fn stranger_cannot_take_the_payload_but_the_peer_still_can() {
    let (a, b, evil) = (creds('a'), creds('b'), creds('e'));
    let sender = ctx(&a, &b);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(data(1000)), 1000, &mut |_| {}, &c2)
            .await
    });
    // Evil presents its own certificate while claiming to be b.
    let mut evil_ctx = ctx(&evil, &a);
    evil_ctx.peer_id = a.device_id().clone();
    let mut sink = Vec::new();
    let got = evil_ctx
        .receive(port, Some(1000), 1 << 20, &mut sink, &mut |_| {}, &cancel)
        .await;
    // The evil client sees the real server certificate (a) and may even finish its half of the
    // handshake, but the server pins b and must refuse it, so no payload is delivered.
    assert!(got.is_err() || sink.is_empty(), "stranger received data");
    let mut out = Vec::new();
    ctx(&b, &a)
        .receive(port, Some(1000), 1 << 20, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap();
    assert_eq!(out, data(1000));
    send.await.unwrap().unwrap();
}

#[tokio::test]
async fn receiver_rejects_a_server_with_the_wrong_certificate() {
    let (a, b, evil) = (creds('a'), creds('b'), creds('e'));
    // The listener belongs to evil but b believes it is dialling a.
    let sender = ctx(&evil, &b);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let _send = tokio::spawn(async move {
        let _ = listener
            .send(Cursor::new(data(10)), 10, &mut |_| {}, &c2)
            .await;
    });
    let mut out = Vec::new();
    let e = ctx(&b, &a)
        .receive(port, Some(10), 100, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap_err();
    assert!(out.is_empty());
    assert!(matches!(e, PayloadError::Link(_)), "{e:?}");
    cancel.cancel();
}

#[tokio::test]
async fn enforces_size_limit_and_port_range() {
    let (a, b) = (creds('a'), creds('b'));
    let receiver = ctx(&b, &a);
    let cancel = CancelToken::new();
    let mut out = Vec::new();
    // Announced size above the limit never even connects.
    let e = receiver
        .receive(9, Some(11), 10, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(e, PayloadError::TooLarge { limit: 10 }));
    // Real configuration only dials the KDE payload range.
    let mut strict = receiver.clone();
    strict.config.any_port = false;
    for port in [0u16, 22, 1716, 1738, 1765, 65535] {
        let e = strict
            .receive(port, Some(1), 10, &mut out, &mut |_| {}, &cancel)
            .await
            .unwrap_err();
        assert!(matches!(e, PayloadError::PortRejected(_)), "{port}");
    }
    // A sender that lies about the size is cut off at the limit (unknown length case).
    let sender = ctx(&a, &b);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        let _ = listener
            .send(Cursor::new(data(5000)), 5000, &mut |_| {}, &c2)
            .await;
    });
    let mut out = Vec::new();
    let e = receiver
        .receive(port, None, 1000, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(e, PayloadError::TooLarge { .. }), "{e:?}");
    assert!(out.len() <= 1000 + CHUNK);
}

#[tokio::test]
async fn short_payload_is_reported_as_truncated() {
    let (a, b) = (creds('a'), creds('b'));
    let listener = ctx(&a, &b).listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        // Sends 100 bytes although 200 were announced, then closes.
        let _ = listener
            .send(Cursor::new(data(100)), 100, &mut |_| {}, &c2)
            .await;
    });
    let mut out = Vec::new();
    let e = ctx(&b, &a)
        .receive(port, Some(200), 1000, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            PayloadError::Truncated {
                received: 100,
                expected: 200
            }
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn sender_times_out_when_nobody_connects() {
    let (a, b) = (creds('a'), creds('b'));
    let mut c = ctx(&a, &b);
    c.config.accept_timeout = Duration::from_millis(200);
    let listener = c.listen().await.unwrap();
    let e = listener
        .send(Cursor::new(data(10)), 10, &mut |_| {}, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(e, PayloadError::Timeout(_)), "{e:?}");
}

#[tokio::test]
async fn cancel_stops_a_running_transfer() {
    let (a, b) = (creds('a'), creds('b'));
    let listener = ctx(&a, &b).listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    // A source that never ends keeps the receiver busy until cancelled.
    let (mut w, r) = tokio::io::duplex(64);
    tokio::spawn(async move {
        let _ = listener.send(r, u64::MAX / 2, &mut |_| {}, &c2).await;
    });
    tokio::spawn(async move {
        loop {
            if w.write_all(&[7u8; 64]).await.is_err() {
                break;
            }
        }
    });
    let c3 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c3.cancel();
    });
    let mut out = Vec::new();
    let e = ctx(&b, &a)
        .receive(
            port,
            Some(u64::MAX / 4),
            u64::MAX,
            &mut out,
            &mut |_| {},
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, PayloadError::Cancelled), "{e:?}");
}

#[tokio::test]
async fn receive_file_is_safe_and_atomic() {
    let (a, b) = (creds('a'), creds('b'));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("evil.txt"), b"existing").unwrap();
    let payload = data(70_000);
    for expect in ["evil (1).txt", "evil (2).txt"] {
        let listener = ctx(&a, &b).listen().await.unwrap();
        let port = listener.port();
        let cancel = CancelToken::new();
        let (src, c2) = (payload.clone(), cancel.clone());
        let send = tokio::spawn(async move {
            listener
                .send(Cursor::new(src), 70_000, &mut |_| {}, &c2)
                .await
        });
        let offer = FileOffer {
            name: share::sanitize_filename("../../evil.txt"),
            size: Some(70_000),
            port,
            last_modified_ms: Some(1_600_000_000_000),
            number_of_files: None,
        };
        let path = share::receive_file(
            &ctx(&b, &a),
            &offer,
            dir.path(),
            1 << 30,
            &mut |_| {},
            &cancel,
        )
        .await
        .unwrap();
        send.await.unwrap().unwrap();
        assert_eq!(path, dir.path().join(expect));
        assert_eq!(std::fs::read(&path).unwrap(), payload);
    }
    assert_eq!(
        std::fs::read(dir.path().join("evil.txt")).unwrap(),
        b"existing"
    );
    // A failed transfer leaves nothing behind.
    let before = std::fs::read_dir(dir.path()).unwrap().count();
    let offer = FileOffer {
        name: "gone.bin".into(),
        size: Some(10),
        port: 1,
        last_modified_ms: None,
        number_of_files: None,
    };
    let e = share::receive_file(
        &ctx(&b, &a),
        &offer,
        dir.path(),
        1 << 30,
        &mut |_| {},
        &CancelToken::new(),
    )
    .await;
    assert!(e.is_err());
    let after = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(before, after);
}

#[test]
fn only_the_paired_host_gets_a_handshake_slot() {
    let peer: IpAddr = "192.168.1.20".parse().unwrap();
    assert!(same_host(peer, peer));
    assert!(same_host("::ffff:192.168.1.20".parse().unwrap(), peer));
    assert!(!same_host("192.168.1.21".parse().unwrap(), peer));
    assert!(!same_host("fe80::1".parse().unwrap(), peer));
    assert!(PayloadConfig::default().handshake_timeout <= Duration::from_secs(5));
}

/// The sender only accepts the paired peer's IP. A receiver that knows its control-link address
/// binds to it, so it is accepted even when the OS would route the dial from another source.
// Needs 127.0.0.2/3 as extra local addresses, which only Linux routes.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn receiver_binds_its_control_link_address() {
    let (a, b) = (creds('a'), creds('b'));
    // `a` believes `b` is 127.0.0.2; `b` reaches `a` at 127.0.0.1 (OS would pick source 127.0.0.1).
    let mut sender = ctx(&a, &b);
    sender.peer_ip = Ipv4Addr::new(127, 0, 0, 2).into();
    let mut receiver = ctx(&b, &a);
    receiver.local_ip = Some(Ipv4Addr::new(127, 0, 0, 2).into());
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(data(5000)), 5000, &mut |_| {}, &c2)
            .await
    });
    let mut out = Vec::new();
    let n = receiver
        .receive(port, Some(5000), 1 << 20, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap();
    assert_eq!(n, 5000);
    assert_eq!(send.await.unwrap().unwrap(), 5000);

    // Without the bound address the sender discards the connection (the receiver sees an EOF).
    let mut sender = ctx(&a, &b);
    sender.peer_ip = Ipv4Addr::new(127, 0, 0, 2).into();
    sender.config.accept_timeout = Duration::from_millis(600);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let c3 = cancel.clone();
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(data(10)), 10, &mut |_| {}, &c3)
            .await
    });
    let unbound = ctx(&b, &a);
    let r = unbound
        .receive(
            port,
            Some(10),
            1 << 20,
            &mut Vec::new(),
            &mut |_| {},
            &cancel,
        )
        .await;
    assert!(r.is_err());
    let _ = send.await;
}

/// E2E-5: a desktop running with the development switch (`any_port`) used to announce a random
/// ephemeral port. A phone with the real configuration only dials 1739-1764 and silently failed
/// with `port_rejected`, leaving the desktop at "Waiting 0/N". The announced port must stay inside
/// the standard range whenever a range port is free, so a strict receiver can always dial it.
#[tokio::test]
async fn development_listener_announces_a_port_a_strict_receiver_accepts() {
    let (a, b) = (creds('a'), creds('b'));
    let sender = ctx(&a, &b);
    assert!(sender.config.any_port);
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    if !PAYLOAD_PORTS.contains(&port) {
        // Another process (a second desktop service, an emulator run) may legitimately hold every
        // range port right now; only a free range port makes this a failure.
        let free = PAYLOAD_PORTS
            .clone()
            .find(|p| std::net::TcpListener::bind(("0.0.0.0", *p)).is_ok());
        assert!(
            free.is_none(),
            "announced {port} although range port {free:?} was free ({PAYLOAD_PORTS:?})"
        );
        return;
    }
    let mut strict = ctx(&b, &a);
    strict.config.any_port = false;
    let payload = data(70_000);
    let cancel = CancelToken::new();
    let (src, c2) = (payload.clone(), cancel.clone());
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(src), 70_000, &mut |_| {}, &c2)
            .await
    });
    let mut out = Vec::new();
    strict
        .receive(port, Some(70_000), 1 << 30, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap();
    assert_eq!(out, payload);
    send.await.unwrap().unwrap();
}

/// E2E-14: the OS-chosen route comes first, the control link's address only after it, and a slow
/// network still gets the full budget at the end.
#[test]
fn the_plain_route_is_tried_before_the_control_link_address() {
    let addr = SocketAddr::new(Ipv4Addr::new(10, 0, 2, 2).into(), 1743);
    let full = Duration::from_secs(10);
    let eth0: IpAddr = Ipv4Addr::new(10, 0, 2, 15).into();
    assert_eq!(
        dial_plan(addr, Some(eth0), full),
        vec![
            (None, ROUTE_TRIAL_TIMEOUT),
            (Some(eth0), ROUTE_TRIAL_TIMEOUT),
            (None, full)
        ]
    );
    assert_eq!(dial_plan(addr, None, full), vec![(None, full)]);
    // A source of the other family could never connect.
    let v6: IpAddr = Ipv6Addr::LOCALHOST.into();
    assert_eq!(dial_plan(addr, Some(v6), full), vec![(None, full)]);
}

/// A route that connects and then goes silent or resets must not end the transfer: the receiver moves
/// on inside its handshake budget. The "NAT" kills what comes from the bound address, as the emulator's
/// default route does; the plain route reaches the sender.
// Needs 127.0.0.2/3 as extra local addresses, which only Linux routes.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_failing_route_moves_on_only_after_a_real_handshake_attempt() {
    // The other order, plain route dead and bound source alive, is the multi-homed case.
    let (a, b) = (creds('a'), creds('b'));
    let mut sender = ctx(&a, &b);
    sender.peer_ip = Ipv4Addr::new(127, 0, 0, 3).into();
    let mut receiver = ctx(&b, &a);
    receiver.local_ip = Some(Ipv4Addr::new(127, 0, 0, 3).into());
    let listener = sender.listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(data(1000)), 1000, &mut |_| {}, &c2)
            .await
    });
    let started = std::time::Instant::now();
    let mut out = Vec::new();
    receiver
        .receive(port, Some(1000), 1 << 20, &mut out, &mut |_| {}, &cancel)
        .await
        .unwrap();
    assert_eq!(out, data(1000));
    send.await.unwrap().unwrap();
    // The sender refused the plain route by closing, so nothing waited for a timeout.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A pin failure is a verdict, not a routing problem: no other route is tried.
#[tokio::test]
async fn a_pin_mismatch_is_final_and_reported_as_such() {
    let (a, b, evil) = (creds('a'), creds('b'), creds('e'));
    let listener = ctx(&evil, &b).listen().await.unwrap();
    let port = listener.port();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        let _ = listener
            .send(Cursor::new(data(10)), 10, &mut |_| {}, &c2)
            .await;
    });
    let mut receiver = ctx(&b, &a);
    receiver.local_ip = Some(Ipv4Addr::new(127, 0, 0, 1).into());
    let started = std::time::Instant::now();
    let e = receiver
        .receive(port, Some(10), 100, &mut Vec::new(), &mut |_| {}, &cancel)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "pin_mismatch", "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    cancel.cancel();
}

/// `send` owns the listener: however it ends, the port is free again and nothing answers on it.
#[tokio::test]
async fn the_listener_is_closed_when_a_send_is_cancelled_or_times_out() {
    let (a, b) = (creds('a'), creds('b'));
    async fn closed(port: u16) -> bool {
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err()
    }

    let listener = ctx(&a, &b).listen().await.unwrap();
    let port = listener.port();
    assert!(!closed(port).await);
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let send = tokio::spawn(async move {
        listener
            .send(Cursor::new(data(10)), 10, &mut |_| {}, &c2)
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    assert!(matches!(
        send.await.unwrap().unwrap_err(),
        PayloadError::Cancelled
    ));
    assert!(closed(port).await, "cancelled: port {port} still answers");

    let mut c = ctx(&a, &b);
    c.config.accept_timeout = Duration::from_millis(100);
    let listener = c.listen().await.unwrap();
    let port = listener.port();
    let e = listener
        .send(Cursor::new(data(10)), 10, &mut |_| {}, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(e, PayloadError::Timeout(_)), "{e:?}");
    assert!(closed(port).await, "timed out: port {port} still answers");
}
