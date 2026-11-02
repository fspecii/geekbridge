// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop -> phone payloads through a model of the emulator's two networks (docs/reports/e2e-findings.md E2E-14).
//!
//! The control link arrives on one interface (`adb emu redir` -> eth0, here a redirect proxy to the phone's
//! own loopback address) while the default route is another (wlan0). A payload socket bound to the control
//! link's address leaves through the default route with a source that network does not own, and the NAT
//! there kills it after the first data: an RST, or one with an out-of-window sequence number that the
//! phone's TCP ignores, which an application sees as silence. A socket on the plain route works.
//!
//! `GEEKBRIDGE_CHAOS=soak` uses 500 MiB instead of the short size.
// Needs several 127/8 addresses (the phone's "eth0" is its own loopback address), which only Linux routes.
#![cfg(target_os = "linux")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bridge_chaos::FaultProxy;
use bridge_kde::share;
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, CancelToken, DeviceCredentials, DeviceId, DeviceType,
    Endpoint, Handshake, LinkEvent, MemoryTrustStore, PairMode, PayloadConfig, TransportConfig,
    TrustedLink,
};
use geekbridge_android::Dispatcher;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

const MIB: u64 = 1024 * 1024;

fn big() -> u64 {
    if std::env::var("GEEKBRIDGE_CHAOS").is_ok_and(|v| v == "soak") {
        500 * MIB
    } else {
        32 * MIB
    }
}

/// Deterministic bytes without holding them in memory.
struct Pattern {
    pos: u64,
    len: u64,
}

fn pattern_at(i: u64) -> u8 {
    (i.wrapping_mul(13).wrapping_add(i >> 16) % 251) as u8
}

impl AsyncRead for Pattern {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let n = (buf.remaining() as u64).min(self.len - self.pos) as usize;
        let start = self.pos;
        for (k, b) in buf.initialize_unfilled_to(n).iter_mut().enumerate() {
            *b = pattern_at(start + k as u64);
        }
        buf.advance(n);
        self.pos += n as u64;
        Poll::Ready(Ok(()))
    }
}

fn pattern_sha(len: u64) -> Vec<u8> {
    let mut h = Sha256::new();
    let mut pos = 0;
    let mut chunk = vec![0u8; 256 * 1024];
    while pos < len {
        let n = (len - pos).min(chunk.len() as u64) as usize;
        for (k, b) in chunk[..n].iter_mut().enumerate() {
            *b = pattern_at(pos + k as u64);
        }
        h.update(&chunk[..n]);
        pos += n as u64;
    }
    h.finalize().to_vec()
}

fn file_sha(f: &mut std::fs::File) -> (u64, Vec<u8>) {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(0)).unwrap();
    let mut h = Sha256::new();
    let mut chunk = vec![0u8; 256 * 1024];
    let mut total = 0;
    loop {
        let n = f.read(&mut chunk).unwrap();
        if n == 0 {
            return (total, h.finalize().to_vec());
        }
        total += n as u64;
        h.update(&chunk[..n]);
    }
}

// ------------------------------------------------------------------------------- the two ends

struct Auto;
impl Approver for Auto {
    async fn approve(&mut self, _: ApprovalRequest) -> bool {
        true
    }
}

struct Phone {
    d: Dispatcher,
    _dir: tempfile::TempDir,
    id: String,
    addr: SocketAddr,
}

impl Phone {
    fn start() -> Phone {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        // The phone's own loopback address plays eth0: the control link's local address, never the
        // source of a dial that is left to the OS.
        let host = format!("127.{}.0.{}", std::process::id() % 200 + 20, n % 250 + 2);
        let dir = tempfile::tempdir().unwrap();
        let d = Dispatcher::new();
        let r = json!({"version": 1, "action": "transport_start", "state_dir": dir.path().to_str().unwrap(),
            "device_name": "Test Phone", "listen": {"host": host, "port": 0}, "allow_loopback": true});
        let v: Value = serde_json::from_str(&d.handle(&r.to_string())).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        let port = v["result"]["listen"]["port"].as_u64().unwrap() as u16;
        Phone {
            id: v["result"]["device_id"].as_str().unwrap().to_owned(),
            addr: SocketAddr::new(host.parse().unwrap(), port),
            d,
            _dir: dir,
        }
    }

    fn act(&self, action: &str, fields: Value) -> Value {
        let mut o = json!({"version": 1, "action": action});
        o.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_str(&self.d.handle(&o.to_string())).unwrap()
    }

    fn status(&self) -> Value {
        self.act("transport_status", json!({}))["result"].clone()
    }

    fn wait(&self, what: &str, limit: Duration, f: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + limit;
        loop {
            let s = self.status();
            if f(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

struct Desktop {
    rt: Runtime,
    ep: Arc<Endpoint>,
}

impl Desktop {
    fn new() -> Desktop {
        let id = DeviceId::parse(&"d".repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        let config = TransportConfig {
            address_policy: AddressPolicy {
                local_networks: true,
                loopback: true,
            },
            payload: PayloadConfig {
                any_port: true,
                accept_timeout: Duration::from_secs(30),
                ..PayloadConfig::default()
            },
            ..TransportConfig::default()
        };
        let caps = vec![
            "kdeconnect.ping".to_owned(),
            "kdeconnect.share.request".to_owned(),
        ];
        let ep = Endpoint::new(
            Arc::new(creds),
            "Desk",
            DeviceType::Desktop,
            caps.clone(),
            caps,
            Arc::new(MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Desktop {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            ep: Arc::new(ep),
        }
    }

    /// Pair through a redirect, as `adb emu redir` does: both ends see 127.0.0.1 as the peer, and the
    /// phone's local address is its own (the emulator's eth0 address).
    fn pair(&self, phone: &Phone) -> (TrustedLink, FaultProxy) {
        let redir = self.rt.block_on(FaultProxy::start(phone.addr));
        let target = DeviceId::parse(&phone.id).unwrap();
        let hs = self
            .rt
            .block_on(self.ep.connect(redir.addr(), &target))
            .unwrap();
        let Handshake::Pending(link) = hs else {
            panic!("expected an unpaired link")
        };
        let join = self
            .rt
            .spawn(async move { link.pair(PairMode::Request, &mut Auto).await });
        let s = phone.wait("a prompt", Duration::from_secs(10), |s| {
            !s["prompts"].as_array().unwrap().is_empty()
        });
        let p = &s["prompts"][0];
        let v = phone.act(
            "transport_pair_decision",
            json!({"request_id": p["request_id"], "connection_id": p["connection_id"], "approve": true,
                "verification_code": p["verification_code"], "peer_cert_sha256": p["peer_cert_sha256"]}),
        );
        assert_eq!(v["ok"], true, "{v}");
        let link = self.rt.block_on(join).unwrap().unwrap();
        phone.wait("trust", Duration::from_secs(10), |s| {
            s["connections"][0]["trusted"] == true
        });
        (link, redir)
    }
}

// ------------------------------------------------------------------------------- the emulator's NAT

#[derive(Clone, Copy, PartialEq)]
enum Kill {
    /// The socket stays up and everything is ACKed, but nothing comes back (the out-of-window RST).
    Silent,
    /// The first data is answered with a reset.
    Reset,
}

/// The default route's NAT: traffic from 127.0.0.1 (the address this network gave the phone) is forwarded to
/// the desktop; any other source is accepted and killed after its first data. Returns the port and how many
/// foreign-source connections were seen.
async fn wlan_nat(target: u16, kill: Kill) -> (u16, Arc<AtomicU64>) {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let foreign = Arc::new(AtomicU64::new(0));
    let seen = foreign.clone();
    tokio::spawn(async move {
        while let Ok((mut c, from)) = l.accept().await {
            if from.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
                seen.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut b = [0u8; 4096];
                    let _ = c.read(&mut b).await;
                    if kill == Kill::Reset {
                        let _ = socket2::SockRef::from(&c).set_linger(Some(Duration::ZERO));
                        return;
                    }
                    while matches!(c.read(&mut b).await, Ok(n) if n > 0) {}
                });
                continue;
            }
            tokio::spawn(async move {
                if let Ok(mut s) = TcpStream::connect((Ipv4Addr::LOCALHOST, target)).await {
                    let _ = tokio::io::copy_bidirectional(&mut c, &mut s).await;
                }
            });
        }
    });
    (port, foreign)
}

/// Announce `size` pattern bytes, accept on the phone, and check the file that arrives.
fn desktop_to_phone(desk: &Desktop, phone: &Phone, link: &mut TrustedLink, size: u64, kill: Kill) {
    let ctx = link.payload_context();
    let listener = desk.rt.block_on(ctx.listen()).unwrap();
    let (nat_port, foreign) = desk.rt.block_on(wlan_nat(listener.port(), kill));
    let packet = share::file_packet("big.bin", size, None, nat_port);
    desk.rt.block_on(link.send_with_payload(&packet)).unwrap();
    let serve = desk.rt.spawn(async move {
        listener
            .send(
                Pattern { pos: 0, len: size },
                size,
                &mut |_| {},
                &CancelToken::new(),
            )
            .await
    });
    let s = phone.wait("the offer", Duration::from_secs(10), |s| {
        s["transfers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["state"] == "offered")
    });
    let id = s["transfers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["state"] == "offered")
        .unwrap()["transfer_id"]
        .clone();
    let mut out = tempfile::tempfile_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let v = phone.act(
        "share_accept",
        json!({"transfer_id": id, "fd": out.as_raw_fd()}),
    );
    assert_eq!(v["ok"], true, "{v}");
    let s = phone.wait("the transfer to end", Duration::from_secs(600), |s| {
        s["transfers"].as_array().unwrap().iter().any(|t| {
            t["transfer_id"] == id && matches!(t["state"].as_str(), Some("done" | "failed"))
        })
    });
    let t = s["transfers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["transfer_id"] == id)
        .unwrap();
    assert_eq!(
        t["state"],
        "done",
        "code {:?} after {} foreign-source dials",
        t["code"],
        foreign.load(Ordering::SeqCst)
    );
    assert_eq!(desk.rt.block_on(serve).unwrap().unwrap(), size);
    let (n, sha) = file_sha(&mut out);
    assert_eq!(n, size);
    assert_eq!(sha, pattern_sha(size), "SHA-256 differs");
}

#[test]
fn a_nat_that_goes_silent_on_the_bound_source_does_not_fail_the_transfer() {
    let (phone, desk) = (Phone::start(), Desktop::new());
    let (mut link, _redir) = desk.pair(&phone);
    desktop_to_phone(&desk, &phone, &mut link, 4 * MIB, Kill::Silent);
}

#[test]
fn a_nat_that_resets_the_bound_source_does_not_fail_the_transfer() {
    let (phone, desk) = (Phone::start(), Desktop::new());
    let (mut link, _redir) = desk.pair(&phone);
    desktop_to_phone(&desk, &phone, &mut link, 4 * MIB, Kill::Reset);
}

#[test]
fn a_big_file_arrives_intact_through_the_nat_both_ways() {
    let (phone, desk) = (Phone::start(), Desktop::new());
    let (mut link, _redir) = desk.pair(&phone);
    let size = big();
    desktop_to_phone(&desk, &phone, &mut link, size, Kill::Silent);

    // Phone -> desktop: the phone listens, the desktop dials it. The phone streams from a descriptor.
    let mut src = tempfile::tempfile_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut w = std::io::BufWriter::new(&mut src);
        let mut chunk = vec![0u8; 256 * 1024];
        let mut pos = 0;
        while pos < size {
            let n = (size - pos).min(chunk.len() as u64) as usize;
            for (k, b) in chunk[..n].iter_mut().enumerate() {
                *b = pattern_at(pos + k as u64);
            }
            w.write_all(&chunk[..n]).unwrap();
            pos += n as u64;
        }
        w.flush().unwrap();
        drop(w);
        src.seek(SeekFrom::Start(0)).unwrap();
    }
    let cid = phone.status()["connections"][0]["connection_id"].clone();
    let v = phone.act(
        "share_send_file",
        json!({"connection_id": cid, "fd": src.as_raw_fd(), "name": "up.bin", "size": size}),
    );
    assert_eq!(v["ok"], true, "{v}");
    let LinkEvent::Packet(p) = desk.rt.block_on(link.recv()).unwrap() else {
        panic!("expected a packet")
    };
    let Ok(share::ShareContent::File(offer)) = share::parse_share(&p, u64::MAX) else {
        panic!("expected a file offer: {p:?}")
    };
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ctx = link.payload_context();
    let path = desk
        .rt
        .block_on(share::receive_file(
            &ctx,
            &offer,
            dir.path(),
            u64::MAX,
            &mut |_| {},
            &CancelToken::new(),
        ))
        .unwrap();
    let (n, sha) = file_sha(&mut std::fs::File::open(path).unwrap());
    assert_eq!(n, size);
    assert_eq!(sha, pattern_sha(size), "SHA-256 differs");
}

/// The handshake budget holds on a saturated machine: 2 x cores busy threads for the whole transfer.
#[test]
fn transfers_complete_on_a_saturated_host() {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let burners: Vec<_> = (0..cores * 2)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(7);
                }
                std::hint::black_box(x);
            })
        })
        .collect();
    let started = Instant::now();
    let (phone, desk) = (Phone::start(), Desktop::new());
    let (mut link, _redir) = desk.pair(&phone);
    for _ in 0..3 {
        desktop_to_phone(&desk, &phone, &mut link, 2 * MIB, Kill::Silent);
    }
    stop.store(true, Ordering::Relaxed);
    for b in burners {
        let _ = b.join();
    }
    eprintln!(
        "three transfers with {} burners: {:?}",
        cores * 2,
        started.elapsed()
    );
}
