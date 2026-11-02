//! Failure injection on file transfers: a 200 MB file cancelled from either side, a peer that goes
//! silent or resets mid-stream, two transfers at once, and the guarantees that no partial file or
//! descriptor survives. Payload sockets go through the fault proxy.
//!
//! Short mode streams less data; `GEEKBRIDGE_CHAOS=soak` uses the full sizes and more rounds.

mod support;

use bridge_core::ipc::{ErrorCode, Request, Response};
use bridge_core::share::{ShareStatus, TransferInfo, TransferState};
use bridge_kde::{self as kde, share as kshare};
use std::{
    path::PathBuf,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use support::chaos::*;
use support::*;
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::Mutex,
    task::JoinHandle,
    time::sleep,
};

const SHARE: &str = "kdeconnect.share.request";
/// 200 MB in soak mode, 48 MB in the short CI mode (the cancel / flap logic does not care). The
/// payload is generated (`Pattern`), only what the receiver writes touches the disk.
fn big() -> u64 {
    if soak() { 200 << 20 } else { 48 << 20 }
}

fn serial() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Deterministic bytes without holding them in memory.
struct Pattern {
    pos: u64,
    len: u64,
}
impl Pattern {
    fn new(len: u64) -> Self {
        Self { pos: 0, len }
    }
}
fn at(i: u64) -> u8 {
    (i.wrapping_mul(13) % 251) as u8
}
impl AsyncRead for Pattern {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let n = (buf.remaining() as u64).min(self.len - self.pos) as usize;
        let start = self.pos;
        let out = buf.initialize_unfilled_to(n);
        for (k, b) in out.iter_mut().enumerate() {
            *b = at(start + k as u64);
        }
        buf.advance(n);
        self.pos += n as u64;
        Poll::Ready(Ok(()))
    }
}

fn verify(path: &std::path::Path, len: u64) {
    use std::io::Read;
    let mut f = std::fs::File::open(path).unwrap();
    assert_eq!(f.metadata().unwrap().len(), len);
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for (k, b) in buf[..n].iter().enumerate() {
            assert_eq!(*b, at(pos + k as u64), "corrupt byte at {}", pos + k as u64);
        }
        pos += n as u64;
    }
    assert_eq!(pos, len);
}

struct World {
    _tmp: tempfile::TempDir,
    rig: Rig,
    save_dir: PathBuf,
    phone: Arc<Phone>,
    server: PhoneServer,
    link: kde::TrustedLink,
}

fn sharing_phone(idle: Duration) -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    config.payload.any_port = true;
    config.payload.accept_timeout = Duration::from_secs(30);
    config.payload.idle_timeout = idle;
    let caps = vec!["kdeconnect.ping".to_string(), SHARE.to_string()];
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

impl World {
    /// Skips (true) when the scratch dir cannot hold `bytes`: an environment limit, not a bug.
    fn short_of_room(&self, bytes: u64) -> bool {
        space::skip_without_room(self._tmp.path(), bytes)
    }

    async fn start(idle: Duration, max_file: Option<u64>) -> World {
        let tmp = space::tempdir();
        let save_dir = tmp.path().join("GeekBridge");
        let sd = save_dir.clone();
        let rig = Rig::start_with(move |o| {
            o.share.save_dir = sd.clone();
            o.share.settings_path = None;
            if let Some(m) = max_file {
                o.share.max_file_bytes = m;
            }
            o.transport.payload.any_port = true;
            o.transport.payload.idle_timeout = idle;
            o.transport.payload.accept_timeout = Duration::from_secs(30);
        })
        .await;
        let phone = sharing_phone(idle);
        let mut server = PhoneServer::start(&phone, Faults::default(), 1).await;
        let link = pair_through(&rig, &phone, &mut server).await;
        rig.ok(Request::ShareSettings {
            device_id: phone.id(),
            receive: Some(true),
            auto_accept: Some(true),
        })
        .await;
        World {
            _tmp: tmp,
            rig,
            save_dir,
            phone,
            server,
            link,
        }
    }

    async fn share(&self) -> ShareStatus {
        match self.rig.call(Request::ShareStatus).await {
            Ok(Response::Share(s)) => *s,
            other => panic!("share status: {other:?}"),
        }
    }

    async fn wait_transfer(&self, what: &str, f: impl Fn(&TransferInfo) -> bool) -> TransferInfo {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let s = self.share().await;
            if let Some(t) = s.transfers.iter().find(|t| f(t)) {
                return t.clone();
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s:#?}");
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// The phone offers `size` pattern bytes behind a fault proxy; returns the proxy and the
    /// serving task.
    async fn offer(
        &mut self,
        name: &str,
        size: u64,
        faults: Faults,
    ) -> (FaultProxy, JoinHandle<Result<u64, kde::PayloadError>>) {
        let ctx = self.link.payload_context();
        let l = ctx.listen().await.unwrap();
        let target: std::net::SocketAddr = ([127, 0, 0, 1], l.port()).into();
        let proxy = FaultProxy::start_with(target, faults, 9).await;
        let packet = kshare::file_packet(name, size, None, proxy.port());
        self.link.send_with_payload(&packet).await.unwrap();
        let serve = tokio::spawn(async move {
            l.send(
                Pattern::new(size),
                size,
                &mut |_| {},
                &kde::CancelToken::new(),
            )
            .await
        });
        (proxy, serve)
    }

    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(&self.save_dir)
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn assert_clean(&self, what: &str) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let left = self.leftovers();
            if left.is_empty() {
                return;
            }
            assert!(
                Instant::now() < end,
                "{what}: partial files left behind: {left:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }
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

fn slow() -> Faults {
    Faults {
        bandwidth: Some(6 * 1024 * 1024),
        ..Faults::default()
    }
}

#[tokio::test]
async fn cancel_midway_from_the_receiver_cleans_up() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(20), None).await;
    if w.short_of_room(big().min(32 << 20)) {
        return;
    }
    let base = settle_fds(0).await;
    let rounds = scaled(2, 10);
    for round in 0..rounds {
        let (proxy, serve) = w.offer("big.bin", big(), slow()).await;
        let t = w
            .wait_transfer("bytes flowing", |t| t.bytes > 4 << 20)
            .await;
        assert_eq!(t.state, TransferState::Active);
        w.rig
            .ok(Request::ShareCancel {
                transfer_id: t.transfer_id,
            })
            .await;
        let t = w
            .wait_transfer("cancelled", |x| {
                x.transfer_id == t.transfer_id && x.state != TransferState::Active
            })
            .await;
        assert_eq!(t.state, TransferState::Cancelled, "round {round}");
        w.assert_clean("receiver cancel").await;
        // The sender notices the closed socket and stops; it must not run on to 200 MB.
        let r = tokio::time::timeout(Duration::from_secs(20), serve)
            .await
            .expect("sender hung after cancel");
        assert!(
            r.unwrap().is_err(),
            "sender should see the receiver go away"
        );
        assert!(proxy.forwarded() < big() / 2);
        drop(proxy);
        w.rig
            .ok(Request::ShareDismiss {
                transfer_id: t.transfer_id,
            })
            .await;
    }
    let end = settle_fds(base + 4).await;
    assert!(end <= base + 4, "fd leak {base} -> {end}");
}

#[tokio::test]
async fn sender_vanishing_mid_transfer_fails_fast_and_leaves_no_partial() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(3), None).await;
    // 1. The phone's app is killed: its payload task disappears and the proxy resets.
    let (proxy, serve) = w.offer("a.bin", big(), slow()).await;
    let t = w
        .wait_transfer("bytes flowing", |t| t.bytes > 2 << 20)
        .await;
    serve.abort();
    proxy.reset_all();
    let t = w
        .wait_transfer("failed", |x| {
            x.transfer_id == t.transfer_id && x.state != TransferState::Active
        })
        .await;
    assert_eq!(t.state, TransferState::Failed);
    w.assert_clean("sender killed").await;
    drop(proxy);

    // 2. Half-open: the network goes silent with the sockets still up. The idle timeout (3 s here,
    //    30 s by default) must end it.
    let (proxy, serve) = w.offer("b.bin", big(), slow()).await;
    let t = w
        .wait_transfer("flowing", |t| t.name == "b.bin" && t.bytes > 1 << 20)
        .await;
    let started = Instant::now();
    proxy.blackhole(true);
    let t = w
        .wait_transfer("timed out", |x| {
            x.transfer_id == t.transfer_id && x.state != TransferState::Active
        })
        .await;
    assert_eq!(t.state, TransferState::Failed);
    assert_eq!(t.code.as_deref(), Some("timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    w.assert_clean("half-open").await;
    serve.abort();
    drop(proxy);

    // 3. A reset in the middle of the stream.
    let (proxy, _serve) = w
        .offer(
            "c.bin",
            big(),
            Faults {
                reset_after_bytes: Some(3 << 20),
                ..Faults::default()
            },
        )
        .await;
    let t = w
        .wait_transfer("reset", |t| {
            t.name == "c.bin" && t.state == TransferState::Failed
        })
        .await;
    assert!(t.bytes < big());
    w.assert_clean("reset").await;
    drop(proxy);
}

#[tokio::test]
async fn two_concurrent_transfers_one_cancelled_one_complete_intact() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(20), None).await;
    let keep = if soak() { big() } else { 16 << 20 };
    if w.short_of_room(keep + (16 << 20)) {
        return;
    }
    let (p1, s1) = w
        .offer(
            "keep.bin",
            keep,
            Faults {
                latency: Duration::from_millis(2),
                ..Faults::default()
            },
        )
        .await;
    let (p2, s2) = w.offer("drop.bin", big(), slow()).await;
    let doomed = w
        .wait_transfer("second flowing", |t| {
            t.name == "drop.bin" && t.bytes > 1 << 20
        })
        .await;
    w.rig
        .ok(Request::ShareCancel {
            transfer_id: doomed.transfer_id,
        })
        .await;
    let done = w
        .wait_transfer("first done", |t| {
            t.name == "keep.bin" && t.state == TransferState::Done
        })
        .await;
    assert_eq!(done.bytes, keep);
    verify(&PathBuf::from(done.path.unwrap()), keep);
    s1.await.unwrap().unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), s2)
        .await
        .expect("cancelled sender hung");
    let names = w.leftovers();
    assert_eq!(
        names,
        vec!["keep.bin".to_string()],
        "only the finished file may remain: {names:?}"
    );
    drop((p1, p2));
}

#[tokio::test]
async fn a_full_200mb_transfer_through_a_degraded_link_is_byte_exact() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(20), None).await;
    let size = if soak() { big() } else { 16 << 20 };
    if w.short_of_room(size) {
        return;
    }
    let faults = Faults {
        latency: Duration::from_millis(3),
        loss: 0.02,
        loss_penalty: Duration::from_millis(40),
        ..Faults::default()
    };
    let (proxy, serve) = w.offer("full.bin", size, faults).await;
    let t = w
        .wait_transfer("done", |t| {
            t.state == TransferState::Done || t.state == TransferState::Failed
        })
        .await;
    if t.state == TransferState::Failed
        && t.code.as_deref() == Some("io_error")
        && w.short_of_room(size)
    {
        return; // the disk filled up under the receiver (environment), not a product failure
    }
    assert_eq!(t.state, TransferState::Done, "{t:?}");
    verify(&PathBuf::from(t.path.unwrap()), size);
    serve.await.unwrap().unwrap();
    drop(proxy);
}

#[tokio::test]
async fn sending_to_the_phone_survives_cancel_and_peer_loss() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(3), None).await;
    let dir = space::tempdir();
    let file = dir.path().join("out.bin");
    // Sparse: the length is real, the content is zeros that cost no disk (the test cancels
    // mid-stream and never checks the bytes).
    let size = 64u64 << 20;
    std::fs::File::create(&file)
        .and_then(|f| f.set_len(size))
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let Ok(Response::TransferStarted { transfer_id }) = w
            .rig
            .call(Request::ShareSendFile {
                device_id: w.phone.id(),
                path: file.display().to_string(),
            })
            .await
        else {
            panic!("send not started")
        };
        ids.push(transfer_id);
    }
    // The phone gets both offers; it fetches the first slowly through a proxy... the offer port is
    // the service's, so fault it by simply not reading the second.
    let mut offers = Vec::new();
    while offers.len() < 2 {
        match tokio::time::timeout(Duration::from_secs(10), w.link.recv())
            .await
            .unwrap()
            .unwrap()
        {
            kde::LinkEvent::Packet(p) if p.kind == SHARE => {
                if let Ok(kshare::ShareContent::File(o)) = kshare::parse_share(&p, 1 << 40) {
                    offers.push(o);
                }
            }
            _ => {}
        }
    }
    // Cancel the second from the service while the phone never connects.
    w.rig
        .ok(Request::ShareCancel {
            transfer_id: ids[1],
        })
        .await;
    // The first: the phone connects, reads a little, then vanishes without closing (idle timeout).
    let ctx = w.link.payload_context();
    let out = space::tempdir();
    let cancel = kde::CancelToken::new();
    let c2 = cancel.clone();
    let first = offers[0].clone();
    let recv = tokio::spawn(async move {
        kshare::receive_file(&ctx, &first, out.path(), 1 << 40, &mut |_| {}, &c2)
            .await
            .map(|p| (p, out))
    });
    w.wait_transfer("outgoing flowing", |t| {
        t.transfer_id == ids[0] && t.bytes > 0
    })
    .await;
    cancel.cancel();
    assert!(recv.await.unwrap().is_err());
    let t = w
        .wait_transfer("send ended", |t| {
            t.transfer_id == ids[0] && t.state != TransferState::Active
        })
        .await;
    assert_eq!(t.state, TransferState::Failed, "{t:?}");
    assert!(t.bytes < size);
    let t2 = w
        .wait_transfer("cancelled", |t| t.transfer_id == ids[1])
        .await;
    assert_eq!(t2.state, TransferState::Cancelled);
    // Cancelling something already finished is refused, not a crash.
    assert_eq!(
        w.rig
            .refused(Request::ShareCancel {
                transfer_id: ids[1]
            })
            .await,
        ErrorCode::Busy
    );
}

#[tokio::test]
async fn link_flap_during_transfer_does_not_leak_the_transfer() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(5), None).await;
    let (proxy, serve) = w.offer("flap.bin", big(), slow()).await;
    let t = w.wait_transfer("flowing", |t| t.bytes > 2 << 20).await;
    // The control link drops (the payload socket is separate).
    w.server.proxy.reset_all();
    w.rig.wait("link gone", |s| s.links.is_empty()).await;
    let t = w
        .wait_transfer("transfer resolved", |x| {
            x.transfer_id == t.transfer_id && x.state != TransferState::Active
        })
        .await;
    assert!(
        matches!(
            t.state,
            TransferState::Failed | TransferState::Cancelled | TransferState::Done
        ),
        "{t:?}"
    );
    serve.abort();
    drop(proxy);
    // The payload socket is separate from the control link, so the file may well finish; what must
    // never happen is a partial file or a leftover temporary.
    match t.state {
        TransferState::Done => {
            verify(&PathBuf::from(t.path.clone().unwrap()), big());
            assert_eq!(w.leftovers(), vec!["flap.bin".to_string()]);
        }
        _ => w.assert_clean("link flap").await,
    }
}

// ------------------------------------------------------------------ disk full

/// Disk-full is simulated with RLIMIT_FSIZE (a write past the limit fails like ENOSPC would): no
/// root, tmpfs or loop device needed. The limit is process-wide, so the real test re-runs this
/// binary as a child with the limit applied and checks the child passed.
#[cfg(unix)]
#[test]
fn disk_full_on_the_receiver_fails_cleanly() {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "disk_full_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("GEEKBRIDGE_DISKFULL_CHILD", "1")
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{text}");
    assert!(
        text.contains("disk_full_child ... ok") || text.contains("disk full verified"),
        "{text}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn disk_full_child() {
    if std::env::var_os("GEEKBRIDGE_DISKFULL_CHILD").is_none() {
        return;
    }
    const LIMIT: u64 = 6 << 20;
    // SAFETY: plain libc calls with valid arguments, made before any file is written.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let lim = libc::rlimit {
            rlim_cur: LIMIT,
            rlim_max: LIMIT,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &lim), 0, "setrlimit");
    }
    let mut w = World::start(Duration::from_secs(5), None).await;
    let base = settle_fds(0).await;
    let (proxy, serve) = w.offer("huge.bin", 32 << 20, Faults::default()).await;
    let t = w
        .wait_transfer("failed", |t| {
            t.state == TransferState::Failed || t.state == TransferState::Done
        })
        .await;
    assert_eq!(t.state, TransferState::Failed, "{t:?}");
    assert_eq!(t.code.as_deref(), Some("io_error"));
    w.assert_clean("disk full").await;
    assert!(
        serve.await.unwrap().is_err(),
        "the sender must learn the receiver gave up"
    );
    drop(proxy);
    // The service is still healthy: a file that fits is received afterwards.
    let (proxy, serve) = w.offer("small.bin", 1 << 20, Faults::default()).await;
    let t = w
        .wait_transfer("small done", |t| {
            t.name == "small.bin" && t.state == TransferState::Done
        })
        .await;
    verify(&PathBuf::from(t.path.unwrap()), 1 << 20);
    serve.await.unwrap().unwrap();
    drop(proxy);
    let end = settle_fds(base + 4).await;
    assert!(end <= base + 4, "fd leak {base} -> {end}");
    eprintln!("disk full verified");
}

#[tokio::test]
async fn an_unfetched_outgoing_offer_does_not_outlive_its_link() {
    let _g = serial().lock().await;
    let mut w = World::start(Duration::from_secs(5), None).await;
    let dir = space::tempdir();
    let file = dir.path().join("offer.bin");
    std::fs::write(&file, vec![1u8; 1 << 20]).unwrap();
    let Ok(Response::TransferStarted { transfer_id }) = w
        .rig
        .call(Request::ShareSendFile {
            device_id: w.phone.id(),
            path: file.display().to_string(),
        })
        .await
    else {
        panic!("send not started")
    };
    // The phone receives the offer and then vanishes without ever connecting to the payload port.
    loop {
        match tokio::time::timeout(Duration::from_secs(10), w.link.recv())
            .await
            .unwrap()
            .unwrap()
        {
            kde::LinkEvent::Packet(p) if p.kind == SHARE => break,
            _ => {}
        }
    }
    w.server.proxy.reset_all();
    w.rig.wait("link gone", |s| s.links.is_empty()).await;
    let started = Instant::now();
    // The listener waits for a phone that is gone; it must be released soon, not after the 120 s
    // default accept timeout (30 s in this rig), and must not keep the slot as "active" meanwhile.
    let t = w
        .wait_transfer("offer resolved", |x| {
            x.transfer_id == transfer_id
                && matches!(x.state, TransferState::Failed | TransferState::Cancelled)
        })
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}: {t:?}",
        started.elapsed()
    );
}
