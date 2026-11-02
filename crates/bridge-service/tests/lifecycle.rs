//! End-to-end lifecycle tests: the service core against phones that speak the real transport over
//! loopback TLS. Covers pair, reconnect, unpair (both directions), dead-link detection and hostile
//! input on established and half-open connections. The leak assertions live in `leaks.rs`, which
//! must not share a process with parallel tests.

mod support;

use bridge_core::ipc::{EventKind, LinkPhase, Request};
use bridge_kde as kde;
use serde_json::{Map, Value, json};
use std::time::Duration;
use support::*;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{sleep, timeout},
};

#[tokio::test]
async fn pair_reconnect_disconnect_and_unpair_in_one_session() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    let link = pair_phone(&rig, &phone).await;
    assert_eq!(rig.trust_len(), 1);

    // Phone vanishes: the service reports the loss and keeps the pairing.
    drop(link);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::LinkLost { .. }))
            .await
    );
    assert_eq!(rig.trust_len(), 1);

    // Reconnect authenticates against the pinned certificate; no challenge appears.
    let link = reconnect(&rig, &phone).await;
    assert!(rig.status().await.challenges.is_empty());

    // Local disconnect ends the link, keeps the pairing, and the phone sees the stream end.
    rig.ok(Request::Disconnect {
        device_id: phone.id(),
    })
    .await;
    rig.wait("disconnected", |s| s.links.is_empty()).await;
    assert_eq!(rig.trust_len(), 1);
    drop(link);

    // Local unpair tells the phone and forgets everything; a second unpair is an error.
    let mut link = reconnect(&rig, &phone).await;
    rig.ok(Request::Unpair {
        device_id: phone.id(),
    })
    .await;
    let event = timeout(Duration::from_secs(5), link.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, kde::LinkEvent::PeerUnpaired));
    rig.wait("unpaired", |s| s.links.is_empty() && s.paired.is_empty())
        .await;
    assert_eq!(rig.trust_len(), 0);
    assert_eq!(
        rig.refused(Request::Unpair {
            device_id: phone.id()
        })
        .await,
        bridge_core::ipc::ErrorCode::NotFound
    );

    // The same phone can pair again from scratch afterwards.
    let _link = pair_phone(&rig, &phone).await;
    assert_eq!(rig.trust_len(), 1);
}

#[tokio::test]
async fn the_phone_unpairing_removes_the_pairing_and_it_can_be_redone() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    let link = pair_phone(&rig, &phone).await;
    link.unpair().await.unwrap();
    rig.wait("unpaired by phone", |s| {
        s.links.is_empty() && s.paired.is_empty()
    })
    .await;
    assert_eq!(rig.trust_len(), 0);
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::PeerUnpaired))
            .await
    );
    let _link = pair_phone(&rig, &phone).await;
}

/// A peer that accepts the TCP connection and then says nothing must neither hang the dial
/// forever nor keep the link slot.
#[tokio::test]
async fn a_silent_peer_during_the_handshake_is_dropped_and_frees_the_slot() {
    let rig = Rig::start_with(|o| {
        o.transport.timeouts.plaintext_identity = Duration::from_millis(400);
        o.transport.timeouts.tls_handshake = Duration::from_millis(400);
        o.transport.timeouts.secure_identity = Duration::from_millis(400);
    })
    .await;
    let phone = Phone::new();
    let silent = listener().await;
    let endpoint = endpoint_of(&silent);
    let held = tokio::spawn(async move {
        let (stream, _) = silent.accept().await.unwrap();
        sleep(Duration::from_secs(30)).await;
        drop(stream);
    });
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    rig.wait("slot freed", |s| s.links.is_empty()).await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(
            k,
            EventKind::ConnectFailed { .. } | EventKind::LinkLost { .. }
        ))
        .await,
        "the failure must be reported, not swallowed"
    );
    held.abort();
    // And the same device can be dialled again right away.
    let _link = pair_phone(&rig, &phone).await;
}

/// A link whose peer dies without a FIN (power loss, Wi-Fi gone) can only be found by TCP
/// keepalive. We cannot black-hole packets without privileges, so assert the mechanism: every
/// established link socket has the keepalive timer armed.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn established_links_arm_tcp_keepalive_for_dead_peer_detection() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    let _link = pair_phone(&rig, &phone).await;
    // /proc/net/tcp: st=01 is ESTABLISHED; tr=02 means the keepalive timer is running.
    let table = std::fs::read_to_string("/proc/net/tcp").unwrap();
    let established: Vec<(&str, &str)> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            (f.get(3) == Some(&"01")).then(|| (f[1], f[5]))
        })
        .collect();
    let link_sockets: Vec<_> = established
        .iter()
        .filter(|(local, _)| !local.ends_with(&format!(":{:04X}", rig.control_port)))
        .collect();
    assert!(
        link_sockets
            .iter()
            .any(|(_, timer)| timer.starts_with("02:")),
        "no ESTABLISHED socket has the keepalive timer armed: {link_sockets:?}"
    );
}

#[derive(Clone, Copy)]
enum Outcome {
    /// The service ends the link (and the phone can reconnect).
    Dropped,
    /// The sending transport refuses the frame locally; nothing reaches the service.
    RefusedByPeerSide,
    /// Counted as an unnegotiated packet; the link stays up.
    Ignored,
}

fn body_with(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.into(), value);
    m
}

fn nested(depth: usize) -> Value {
    (0..depth).fold(json!(1), |inner, _| json!({ "n": inner }))
}

/// Packets that are valid frames of invalid packets. Each must end that link (or be counted and
/// ignored) without killing the service, losing the pairing, or poisoning the next connection.
#[tokio::test]
async fn hostile_packets_on_an_established_link_never_hurt_the_service() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    let mut link = pair_phone(&rig, &phone).await;

    let mut cases: Vec<(&str, kde::Packet, Outcome)> = Vec::new();
    cases.push((
        "type longer than the cap",
        kde::Packet::new(&"k".repeat(300), Map::new()),
        Outcome::Dropped,
    ));
    cases.push((
        "body nested past the depth limit",
        kde::Packet::new("kdeconnect.ping", body_with("n", nested(200))),
        Outcome::Dropped,
    ));
    let mut negative = kde::Packet::new("kdeconnect.ping", Map::new());
    negative.payload_size = Some(-9);
    negative.payload_transfer_info = Some(Map::new());
    cases.push((
        "negative payload size",
        negative,
        Outcome::RefusedByPeerSide,
    ));
    let mut stray_payload = kde::Packet::new("kdeconnect.ping", Map::new());
    stray_payload.payload_size = Some(1 << 30);
    stray_payload.payload_transfer_info = Some(body_with("port", json!(1)));
    cases.push((
        "unsolicited payload announcement",
        stray_payload,
        Outcome::RefusedByPeerSide,
    ));
    cases.push((
        "line over the packet limit",
        kde::Packet::new(
            "kdeconnect.ping",
            body_with(
                "pad",
                json!("x".repeat(2 * 1024 * 1024 + 1024)), // daemon::MAX_PACKET_LINE + a bit
            ),
        ),
        Outcome::Dropped,
    ));
    cases.push((
        "unknown packet type",
        kde::Packet::new("kdeconnect.no.such.type", body_with("a", json!(1))),
        Outcome::Ignored,
    ));

    for (name, packet, expect) in cases {
        let sent = link.send(&packet).await;
        // Whether the send itself fails (sender-side limit) or the receiver drops us, the service
        // must stay answerable and keep the pairing.
        let status = rig.status().await;
        assert_eq!(status.paired.len(), 1, "{name}: pairing lost");
        if sent.is_ok() {
            // Give the service time to either count or drop it.
            sleep(Duration::from_millis(150)).await;
        }
        let status = rig.status().await;
        assert_eq!(status.paired.len(), 1, "{name}: pairing lost");
        match expect {
            Outcome::Dropped => {
                assert!(sent.is_ok(), "{name}: should reach the service");
                assert!(
                    link_of(&status, &phone.id()).is_none(),
                    "{name}: not dropped"
                );
            }
            Outcome::RefusedByPeerSide => {
                assert!(
                    sent.is_err(),
                    "{name}: the transport should refuse to send it"
                );
            }
            Outcome::Ignored => {
                assert!(sent.is_ok(), "{name}");
                assert!(link_of(&status, &phone.id()).is_some(), "{name}: dropped");
            }
        }
        if link_of(&status, &phone.id()).is_none() {
            link = reconnect(&rig, &phone).await;
        } else {
            // Still linked: the service must still answer a ping on it.
            assert_eq!(
                link_of(&status, &phone.id()).unwrap().phase,
                LinkPhase::Connected,
                "{name}"
            );
        }
        let _ = rig.status().await;
    }
    // After everything the link works end to end.
    rig.ok(Request::Ping {
        device_id: phone.id(),
    })
    .await;
    let event = timeout(Duration::from_secs(5), link.recv()).await;
    assert!(event.is_ok(), "the service can still talk on the link");
}

/// Connections that never complete, or send junk, to the inbound listener are dropped within the
/// handshake deadline and do not starve the 16-permit accept gate.
#[tokio::test]
async fn junk_and_silent_inbound_connections_are_dropped_and_do_not_starve_real_phones() {
    let mut rig = Rig::start_with(|o| {
        o.transport.timeouts.plaintext_identity = Duration::from_millis(500);
        o.transport.timeouts.tls_handshake = Duration::from_millis(500);
        o.transport.timeouts.secure_identity = Duration::from_millis(500);
    })
    .await;
    let addr = rig.listen_inbound().await;
    let mut rng = Rng(0xbad5_eed5_0000_0001);

    let mut conns = Vec::new();
    for i in 0..48 {
        let mut s = TcpStream::connect(addr).await.unwrap();
        match i % 4 {
            0 => {} // silent
            1 => {
                let junk: Vec<u8> = (0..rng.below(2000)).map(|_| rng.next() as u8).collect();
                let _ = s.write_all(&junk).await;
            }
            2 => {
                // identity line far past the 8 KiB cap, no newline
                let _ = s.write_all(&vec![b'a'; 64 * 1024]).await;
            }
            _ => {
                let _ = s
                    .write_all(b"{\"id\":1,\"type\":\"kdeconnect.identity\",\"body\":{}}\n")
                    .await;
            }
        }
        conns.push(s);
    }
    // Every one of them is closed by the service well within a few handshake deadlines.
    for mut s in conns {
        let mut sink = vec![0u8; 4096];
        let closed = timeout(Duration::from_secs(10), async {
            loop {
                match s.read(&mut sink).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "a junk connection was left open");
    }
    let status = rig.status().await;
    assert!(status.links.is_empty(), "junk created a link: {status:#?}");
    assert!(status.paired.is_empty());

    // A real phone can still get in afterwards, and its pairing still needs explicit approval.
    let phone = Phone::new();
    let service_id = kde::DeviceId::parse(&rig.status().await.device_id).unwrap();
    let dial = {
        let phone = phone.clone();
        tokio::spawn(async move {
            let handshake = phone.endpoint.connect(addr, &service_id).await.unwrap();
            if let kde::Handshake::Pending(p) = handshake {
                let _ = p.pair(kde::PairMode::Request, &mut Yes).await;
            }
        })
    };
    let status = rig
        .wait("incoming challenge", |s| !s.challenges.is_empty())
        .await;
    rig.ok(Request::Deny {
        challenge_id: status.challenges[0].challenge_id.clone(),
    })
    .await;
    let _ = timeout(Duration::from_secs(10), dial).await;
    assert_eq!(rig.trust_len(), 0);
}

/// The phone forgot the pairing while no link was up, so the computer was never told and still
/// trusts it. The phone's next pairing request must not die on the computer's trusted link: it
/// is raised as an ordinary incoming challenge, and approving it pairs on the first try.
#[tokio::test]
async fn a_phone_that_forgot_the_pairing_silently_can_re_pair_first_try() {
    let mut rig = Rig::start().await;
    let inbound = rig.listen_inbound().await;
    let phone = Phone::new();
    let link = pair_phone(&rig, &phone).await;
    drop(link);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    assert_eq!(rig.trust_len(), 1, "the computer still trusts the phone");

    // Same identity, empty trust store: what the phone looks like after its local unpair.
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let ping = vec!["kdeconnect.ping".to_string()];
    let forgetful = kde::Endpoint::new(
        phone.creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        ping.clone(),
        ping,
        std::sync::Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    let service_id = kde::DeviceId::parse(&rig.status().await.device_id).unwrap();
    let dial = tokio::spawn(async move {
        match forgetful.connect(inbound, &service_id).await.unwrap() {
            kde::Handshake::Pending(p) => p.pair(kde::PairMode::Request, &mut Yes).await,
            kde::Handshake::Trusted(_) => panic!("the phone forgot the computer"),
        }
    });
    let status = rig
        .wait("re-pair challenge", |s| !s.challenges.is_empty())
        .await;
    rig.ok(Request::Approve {
        challenge_id: status.challenges[0].challenge_id.clone(),
    })
    .await;
    let result = timeout(Duration::from_secs(10), dial)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "first re-pair attempt failed: {:?}",
        result.err()
    );
    wait_connected(&rig, &phone).await;
    assert_eq!(rig.trust_len(), 1);
}

#[tokio::test]
async fn renaming_the_computer_is_validated_saved_and_used_by_the_next_connection() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let file = dir.path().join("device-name");
    let path = file.clone();
    let rig = Rig::start_with(move |o| o.device_name_file = Some(path)).await;
    assert_eq!(rig.status().await.device_name, "Test Desktop");

    for bad in ["", "   ", "tab\there", "esc\u{1b}[2J", &"n".repeat(33)] {
        assert_eq!(
            rig.refused(Request::SetDeviceName { name: bad.into() })
                .await,
            bridge_core::ipc::ErrorCode::BadRequest,
            "{bad:?}"
        );
    }
    assert_eq!(rig.status().await.device_name, "Test Desktop");
    assert!(!file.exists());

    rig.ok(Request::SetDeviceName {
        name: " Study PC. ".into(),
    })
    .await;
    assert_eq!(rig.status().await.device_name, "Study PC");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Study PC");

    // A phone that connects afterwards is told the new name.
    let phone = Phone::new();
    let link = pair_phone(&rig, &phone).await;
    assert_eq!(link.info().peer.device_name, "Study PC");
}

#[tokio::test]
async fn a_saved_computer_name_is_used_at_start_and_junk_in_the_file_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("device-name");
    std::fs::write(&file, "Saved Name\n").unwrap();
    let path = file.clone();
    let rig = Rig::start_with(move |o| o.device_name_file = Some(path)).await;
    assert_eq!(rig.status().await.device_name, "Saved Name");
    drop(rig);

    std::fs::write(&file, "bad\u{1b}[31m").unwrap();
    let path = file.clone();
    let rig = Rig::start_with(move |o| o.device_name_file = Some(path)).await;
    assert_eq!(rig.status().await.device_name, "Test Desktop");
}
