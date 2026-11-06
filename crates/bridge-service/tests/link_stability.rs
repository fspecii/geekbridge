// SPDX-License-Identifier: GPL-3.0-or-later
//! Link lifecycle faults on the computer's side (docs/reports/link-stability-review-2.md): the gap between two phone
//! announcements must neither undo a manual disconnect nor reset the redial back-off, a phone whose announcements do
//! not arrive (asleep, filtered broadcast) is dialed back after a silent loss, and a phone that comes back from a new
//! address replaces the half-open link the computer still holds.

mod support;

use bridge_chaos::{FaultProxy, Faults};
use bridge_core::ipc::{EventKind, LinkPhase, Request};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};

/// Make the service hear `phone` announce from `addr`, `age_ms` ago (the UDP listener stamps arrival time).
async fn announce_aged(rig: &Rig, phone: &Phone, addr: SocketAddr, age_ms: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let device = bridge_core::Device {
        id: phone.id(),
        name: "Test Phone".into(),
        device_type: "phone".into(),
        address: addr.ip(),
        tcp_port: addr.port(),
        protocol_version: 8,
        incoming_capabilities: vec![],
        outgoing_capabilities: vec![],
        state: bridge_core::ConnectionState::Discovered,
        last_seen_unix_ms: now - age_ms,
    };
    let mut snap = rig.discovery.write().await;
    snap.devices.retain(|d| d.id != phone.id());
    bridge_service::record_device(&mut snap, device);
}

/// A listener that counts connection attempts and hangs up on each (a phone whose TCP side is unusable).
async fn counting_refuser() -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let c = n.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            // Count only the service's dial; a stray local probe of this port is not a dial.
            let c = c.clone();
            tokio::spawn(async move {
                if opens_like_kde_peer(&s).await {
                    c.fetch_add(1, Ordering::SeqCst);
                }
                drop(s);
            });
        }
    });
    (addr, n)
}

async fn settle(rig: &Rig) {
    rig.wait("no links", |s| s.links.is_empty()).await;
}

/// The phone announces every 30 s and the computer counted a phone as "announcing" only within 20 s of the last one,
/// so every gap between two announcements dropped the phone's redial entry, and with it a manual disconnect: the
/// next announcement then made the computer dial the phone the user had just disconnected.
#[tokio::test]
async fn a_manual_disconnect_survives_the_gap_between_two_announcements() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    let link = pair_phone(&rig, &phone).await;
    rig.ok(Request::Disconnect {
        device_id: phone.id(),
    })
    .await;
    drop(link);
    settle(&rig).await;
    let (addr, dials) = counting_refuser().await;
    // 25 s after the last announcement (between two of them), then the next one arrives.
    announce_aged(&rig, &phone, addr, 25_000).await;
    rig.daemon.reconnect_tick().await;
    announce_aged(&rig, &phone, addr, 0).await;
    rig.daemon.reconnect_tick().await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        dials.load(Ordering::SeqCst),
        0,
        "redialed a disconnected phone"
    );
    assert!(rig.status().await.links.is_empty());
}

/// The redial pass only needs its 2 s timer while a redial is under way; with nothing to dial it waits long and is
/// woken by announcements and link events instead (idle wake-ups, docs/performance.md).
#[tokio::test]
async fn the_redial_pass_waits_long_unless_a_redial_is_under_way() {
    use bridge_service::reconnect::{BUSY_EVERY, IDLE_EVERY};
    let rig = Rig::start().await;
    assert_eq!(
        rig.daemon.reconnect_tick().await,
        IDLE_EVERY,
        "nobody paired"
    );
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    settle(&rig).await;
    let (addr, dials) = counting_refuser().await;
    announce_aged(&rig, &phone, addr, 0).await;
    assert_eq!(
        rig.daemon.reconnect_tick().await,
        BUSY_EVERY,
        "a dial was made and its back-off is running"
    );
    timeout(Duration::from_secs(10), async {
        while dials.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// Same gap, back-off: a phone that announces but cannot be reached over TCP must be dialed with a growing delay, not
/// once per announcement (each try is a TLS handshake and a radio wake-up on the phone).
#[tokio::test]
async fn the_gap_between_announcements_does_not_reset_the_back_off() {
    let rig = Rig::start().await;
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    settle(&rig).await;
    let (addr, dials) = counting_refuser().await;
    announce_aged(&rig, &phone, addr, 0).await;
    rig.daemon.reconnect_tick().await;
    timeout(Duration::from_secs(10), async {
        while dials.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    settle(&rig).await;
    for _ in 0..3 {
        announce_aged(&rig, &phone, addr, 25_000).await;
        rig.daemon.reconnect_tick().await;
        announce_aged(&rig, &phone, addr, 0).await;
        rig.daemon.reconnect_tick().await;
        sleep(Duration::from_millis(100)).await;
        settle(&rig).await;
    }
    assert_eq!(
        dials.load(Ordering::SeqCst),
        1,
        "the back-off restarted with every announcement"
    );
}

/// The phone dialed in (so the computer never dialed it), then the link dies silently (a reset; the phone is asleep
/// and its announcements do not arrive). The computer dials it back at its address and the port its secure identity
/// declared, without waiting for an announcement.
#[tokio::test]
async fn a_phone_that_cannot_announce_is_dialed_back_after_a_silent_loss() {
    let mut rig = Rig::start().await;
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    settle(&rig).await;
    let inbound = rig.listen_inbound().await;
    let proxy = FaultProxy::start_with(inbound, Faults::default(), 21).await;
    // The phone listens and says so in its identity.
    let back = listener().await;
    phone
        .endpoint
        .set_listen_port(back.local_addr().unwrap().port());
    let target = kde_id(&rig);
    let bridge_kde::Handshake::Trusted(mut link) = phone
        .endpoint
        .connect(proxy.addr(), &target)
        .await
        .expect("phone dials in")
    else {
        panic!("expected a trusted link");
    };
    wait_connected(&rig, &phone).await;
    let answered = phone.answer(back);
    // The real loop, started while all is well so that it is in its 30 s idle wait: the loss event must wake it, and
    // the dial-back then keeps it at 2 s.
    let redial = tokio::spawn(rig.daemon.clone().redial_forever());
    sleep(Duration::from_millis(300)).await;
    // Silent loss: the path resets the flow (RST both ways) on the next packet; no goodbye, no announcement.
    proxy.update(|f| f.reset_after_bytes = Some(0));
    let _ = link
        .send(&bridge_kde::Packet::new(
            "kdeconnect.ping",
            serde_json::Map::new(),
        ))
        .await;
    rig.wait("loss seen", |s| {
        s.links
            .iter()
            .all(|l| l.device_id != phone.id() || l.phase != LinkPhase::Connected)
    })
    .await;
    drop(link);
    let dialed_back = async {
        while !answered.is_finished() {
            sleep(Duration::from_millis(100)).await;
        }
    };
    let late = timeout(Duration::from_secs(15), dialed_back).await.is_err();
    redial.abort();
    if late {
        panic!(
            "not dialed back: {:#?} {:#?}",
            rig.events().await,
            rig.status().await
        );
    }
    wait_connected(&rig, &phone).await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::LinkLost { .. }))
            .await
    );
}

/// A clean close from the phone (its own restart, its user's disconnect) is not dialed back: the phone redials by
/// itself when it means to, and its user must be able to disconnect.
#[tokio::test]
async fn a_clean_close_from_the_phone_is_not_dialed_back() {
    let mut rig = Rig::start().await;
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    settle(&rig).await;
    let inbound = rig.listen_inbound().await;
    let (back, dials) = counting_refuser().await;
    phone.endpoint.set_listen_port(back.port());
    let link = phone
        .endpoint
        .connect(inbound, &kde_id(&rig))
        .await
        .expect("phone dials in");
    wait_connected(&rig, &phone).await;
    drop(link);
    settle(&rig).await;
    for _ in 0..30 {
        rig.daemon.reconnect_tick().await;
        sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(dials.load(Ordering::SeqCst), 0);
}

/// IP change mid-session: the computer still holds the old link (half-open: the phone's address is gone, nothing
/// answers and nothing resets) when the phone comes back from its new address. The newest connection wins at once;
/// the computer does not wait for its own dead-link detection.
#[tokio::test]
async fn a_phone_back_from_a_new_address_replaces_the_half_open_link() {
    let mut rig = Rig::start().await;
    let phone = Phone::new();
    drop(pair_phone(&rig, &phone).await);
    settle(&rig).await;
    let inbound = rig.listen_inbound().await;
    let old_path = FaultProxy::start_with(inbound, Faults::default(), 22).await;
    let target = kde_id(&rig);
    let _old = phone
        .endpoint
        .connect(old_path.addr(), &target)
        .await
        .unwrap();
    wait_connected(&rig, &phone).await;
    let first = link_of(&rig.status().await, &phone.id())
        .unwrap()
        .connection_id;
    old_path.blackhole(true);
    // Beyond the cross-dial window: this is a reconnect, not a collision.
    sleep(bridge_kde::tiebreak::CROSS_DIAL_WINDOW + Duration::from_millis(500)).await;
    let bridge_kde::Handshake::Trusted(mut fresh) =
        phone.endpoint.connect(inbound, &target).await.unwrap()
    else {
        panic!("expected a trusted reconnect");
    };
    let st = rig
        .wait("the new link", |s| {
            link_of(s, &phone.id())
                .is_some_and(|l| l.phase == LinkPhase::Connected && l.connection_id != first)
        })
        .await;
    assert_eq!(st.links.len(), 1);
    // The new link carries traffic.
    fresh
        .send(&bridge_kde::Packet::new(
            "kdeconnect.ping",
            serde_json::Map::new(),
        ))
        .await
        .unwrap();
}

fn kde_id(rig: &Rig) -> bridge_kde::DeviceId {
    bridge_kde::DeviceId::parse(&rig_id(rig)).unwrap()
}

fn rig_id(rig: &Rig) -> String {
    rig.daemon.status().unwrap().device_id
}
