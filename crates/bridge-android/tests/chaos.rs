// SPDX-License-Identifier: GPL-3.0-or-later
//! Failure injection against the Android transport actor: pairing through a lossy, fragmenting,
//! slow link, a reconnect storm with descriptor / thread / memory checks, a stalled handshake,
//! and the kernel keepalive on accepted sockets. A TCP fault proxy (bridge-chaos) sits between a
//! desktop-side `bridge_kde` endpoint and the phone's listener.
//!
//! `GEEKBRIDGE_CHAOS=soak` runs 1000 storm cycles instead of 100.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bridge_chaos::{FaultProxy, Faults};
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceCredentials, DeviceId, DeviceType, Endpoint,
    Handshake, MemoryTrustStore, PairMode, TransportConfig, TrustedLink,
};
use geekbridge_android::Dispatcher;
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio::time::timeout;

fn soak() -> bool {
    std::env::var("GEEKBRIDGE_CHAOS").is_ok_and(|v| v == "soak")
        || std::env::var("GEEKBRIDGE_SOAK").is_ok_and(|v| v == "1")
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, |d| d.count())
}
fn rss_kib() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(0, |p| p * 4)
}

fn req(action: &str, fields: Value) -> String {
    let mut o = serde_json::Map::new();
    o.insert("version".into(), json!(1));
    o.insert("action".into(), json!(action));
    if let Value::Object(f) = fields {
        o.extend(f);
    }
    Value::Object(o).to_string()
}

struct Phone {
    d: Dispatcher,
    _dir: tempfile::TempDir,
    id: String,
    addr: SocketAddr,
}

impl Phone {
    fn start(n: u32) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Dispatcher::new();
        let out = d.handle(&req(
            "transport_start",
            json!({
                "state_dir": dir.path().to_str().unwrap(),
                "device_name": "Chaos Phone",
                "listen": { "host": format!("127.{}.{}.1", std::process::id() % 250 + 2, n), "port": 0 },
                "allow_loopback": true,
            }),
        ));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        let r = &v["result"];
        let addr = format!(
            "{}:{}",
            r["listen"]["host"].as_str().unwrap(),
            r["listen"]["port"]
        )
        .parse()
        .unwrap();
        Phone {
            d,
            _dir: dir,
            id: r["device_id"].as_str().unwrap().to_owned(),
            addr,
        }
    }

    fn status(&self) -> Value {
        let v: Value =
            serde_json::from_str(&self.d.handle(&req("transport_status", json!({})))).unwrap();
        v["result"].clone()
    }

    fn wait(&self, what: &str, within: Duration, f: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + within;
        loop {
            let s = self.status();
            if f(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Yes;
impl Approver for Yes {
    async fn approve(&mut self, _: ApprovalRequest) -> bool {
        true
    }
}

struct Desk {
    rt: Runtime,
    ep: Arc<Endpoint>,
}

impl Desk {
    fn new() -> Desk {
        let id = DeviceId::parse(&"d".repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        let config = TransportConfig {
            address_policy: AddressPolicy {
                local_networks: true,
                loopback: true,
            },
            ..TransportConfig::default()
        };
        let caps = vec!["kdeconnect.ping".to_owned()];
        let ep = Endpoint::new(
            Arc::new(creds),
            "Chaos Desk",
            DeviceType::Desktop,
            caps.clone(),
            caps,
            Arc::new(MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Desk {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            ep: Arc::new(ep),
        }
    }

    fn dial(&self, addr: SocketAddr, target: &str, within: Duration) -> Option<Handshake> {
        let target = DeviceId::parse(target).unwrap();
        self.rt.block_on(async {
            timeout(within, self.ep.connect(addr, &target))
                .await
                .ok()?
                .ok()
        })
    }

    fn proxy(&self, to: SocketAddr, faults: Faults, seed: u64) -> FaultProxy {
        self.rt.block_on(FaultProxy::start_with(to, faults, seed))
    }

    /// Dial through `via`, ask the phone to pair and let its user approve.
    fn pair(&self, phone: &Phone, via: SocketAddr) -> TrustedLink {
        let Some(Handshake::Pending(link)) = self.dial(via, &phone.id, Duration::from_secs(30))
        else {
            panic!("expected an untrusted handshake")
        };
        let join = self
            .rt
            .spawn(async move { link.pair(PairMode::Request, &mut Yes).await });
        let s = phone.wait("a pairing prompt", Duration::from_secs(30), |s| {
            !s["prompts"].as_array().unwrap().is_empty()
        });
        let p = &s["prompts"][0];
        let v: Value = serde_json::from_str(&phone.d.handle(&req(
            "transport_pair_decision",
            json!({
                "request_id": p["request_id"],
                "connection_id": p["connection_id"],
                "approve": true,
                "verification_code": p["verification_code"],
                "peer_cert_sha256": p["peer_cert_sha256"],
            }),
        )))
        .unwrap();
        assert_eq!(v["ok"], true, "{v}");
        let link = self
            .rt
            .block_on(async { timeout(Duration::from_secs(30), join).await })
            .expect("pairing timed out")
            .unwrap()
            .expect("pairing completes");
        phone.wait("trusted connection", Duration::from_secs(30), |s| {
            s["connections"]
                .as_array()
                .unwrap()
                .first()
                .is_some_and(|c| c["trusted"] == true)
        });
        link
    }
}

#[test]
fn pairing_through_loss_fragmentation_and_a_slow_handshake() {
    let _g = serial();
    let variants = [
        Faults {
            latency: Duration::from_millis(15),
            loss: 0.3,
            loss_penalty: Duration::from_millis(60),
            ..Faults::default()
        },
        Faults {
            fragment: true,
            ..Faults::default()
        },
        Faults {
            handshake_delay: Duration::from_secs(2),
            bandwidth: Some(16 * 1024),
            ..Faults::default()
        },
    ];
    for (i, faults) in variants.into_iter().enumerate() {
        let desk = Desk::new();
        let phone = Phone::start(10 + i as u32);
        let proxy = desk.proxy(phone.addr, faults, 3 + i as u64);
        let link = desk.pair(&phone, proxy.addr());
        drop(link);
        phone.wait("connection gone", Duration::from_secs(15), |s| {
            s["connections"].as_array().unwrap().is_empty()
        });
        phone.d.handle(&req("transport_stop", json!({})));
    }
}

#[test]
fn a_stalled_handshake_leaves_nothing_on_the_phone() {
    let _g = serial();
    let desk = Desk::new();
    let phone = Phone::start(30);
    // The proxy never forwards the first bytes in time: the peer gives up, the phone must drop the
    // half-open connection by itself (its handshake deadline), not keep it forever.
    let proxy = desk.proxy(
        phone.addr,
        Faults {
            handshake_delay: Duration::from_secs(60),
            ..Faults::default()
        },
        1,
    );
    let r = desk.dial(proxy.addr(), &phone.id, Duration::from_secs(2));
    assert!(r.is_none(), "dial through a stalled proxy cannot succeed");
    drop(proxy);
    phone.wait("no connections", Duration::from_secs(20), |s| {
        s["connections"].as_array().unwrap().is_empty()
    });
    // The phone is still serving.
    let direct = desk.pair(&phone, phone.addr);
    drop(direct);
    phone.d.handle(&req("transport_stop", json!({})));
}

#[test]
fn reconnect_storm_leaks_no_fds_threads_or_memory() {
    let _g = serial();
    let desk = Desk::new();
    let phone = Phone::start(40);
    let proxy = desk.proxy(phone.addr, Faults::default(), 17);
    drop(desk.pair(&phone, proxy.addr()));
    phone.wait("connection gone", Duration::from_secs(15), |s| {
        s["connections"].as_array().unwrap().is_empty()
    });

    let total = if soak() { 1000 } else { 100 };
    let warmup = (total / 10).max(10);
    let mut base = None;
    for i in 0..total {
        let mode = i % 4;
        proxy.set(Faults {
            reset_after_bytes: (mode == 3).then_some(900 + (i as u64 * 37) % 1500),
            fragment: mode == 2,
            ..Faults::default()
        });
        match desk.dial(proxy.addr(), &phone.id, Duration::from_secs(10)) {
            Some(Handshake::Trusted(link)) => {
                if mode == 1 {
                    proxy.reset_all();
                }
                drop(link);
            }
            Some(Handshake::Pending(_)) => {
                panic!("cycle {i}: a paired desktop must reconnect trusted")
            }
            None => assert_eq!(
                mode, 3,
                "cycle {i}: only the reset mode may fail to connect"
            ),
        }
        phone.wait("connection gone", Duration::from_secs(15), |s| {
            s["connections"].as_array().unwrap().is_empty()
        });
        if i + 1 == warmup {
            std::thread::sleep(Duration::from_millis(300));
            base = Some((fds(), threads(), rss_kib()));
        }
    }
    let (f0, t0, r0) = base.unwrap();
    proxy.set(Faults::default());
    let end = Instant::now() + Duration::from_secs(8);
    while fds() > f0 + 6 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let (f1, t1, r1) = (fds(), threads(), rss_kib());
    eprintln!(
        "android storm cycles={total}: fds {f0}->{f1}, threads {t0}->{t1}, rss {r0}->{r1} KiB"
    );
    assert!(f1 <= f0 + 6, "fd leak {f0} -> {f1}");
    assert!(t1 <= t0 + 12, "thread growth {t0} -> {t1}");
    assert!(r1 <= r0 + 8 * 1024, "memory growth {r0} -> {r1} KiB");
    let s = phone.status();
    assert!(
        s["events"].as_array().unwrap().len() <= 512,
        "event log must stay bounded"
    );
    phone.d.handle(&req("transport_stop", json!({})));
}

/// A computer that vanishes without a FIN is noticed by the kernel, not by a timer in this process
/// (docs/android-power.md): every accepted socket must have the keepalive timer armed.
#[cfg(target_os = "linux")]
#[test]
fn accepted_sockets_have_the_keepalive_timer_armed() {
    let _g = serial();
    let desk = Desk::new();
    let phone = Phone::start(50);
    let link = desk.pair(&phone, phone.addr);
    let port = phone.addr.port();
    // Right after pairing the retransmit timer may still be running; wait for the link to idle.
    let end = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    loop {
        let text = std::fs::read_to_string("/proc/net/tcp").unwrap();
        let mut found = false;
        let mut armed = true;
        for l in text.lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            let lp = u16::from_str_radix(f[1].rsplit(':').next().unwrap(), 16).unwrap();
            if lp == port && f[3] == "01" {
                let (tr, when) = f[5].split_once(':').unwrap();
                let cs = u64::from_str_radix(when, 16).unwrap();
                found = true;
                // 2 = keepalive timer; at most the 2 min idle time (12 000 centiseconds) away.
                armed &= tr == "02" && cs <= 12_100;
                last = l.to_owned();
            }
        }
        if found && armed {
            break;
        }
        assert!(
            Instant::now() < end,
            "keepalive timer not armed on the accepted socket: {last}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(link);
    phone.d.handle(&req("transport_stop", json!({})));
}

/// The phone dials the computer again by itself after a reset in the middle of a live link, at the
/// endpoint it dialed last, and ends up with exactly one trusted connection. The reset comes from the
/// fault proxy (a TCP RST, as a Wi-Fi power-save or NAT drop produces), so the loss reason is
/// named in the event log (`conn_reset` or `peer_closed`).
#[test]
fn the_phone_redials_after_a_reset_mid_link_and_keeps_a_single_connection() {
    let _g = serial();
    let desk = Desk::new();
    let phone = Phone::start(41);
    let pairing_proxy = desk.proxy(phone.addr, Faults::default(), 3);
    drop(desk.pair(&phone, pairing_proxy.addr()));
    phone.wait("connection gone", Duration::from_secs(15), |s| {
        s["connections"].as_array().unwrap().is_empty()
    });

    // The computer's own listener, behind a fault proxy; it keeps every trusted link it accepts.
    let (links_tx, links_rx) = std::sync::mpsc::channel::<TrustedLink>();
    let listener = desk
        .rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let desk_addr = listener.local_addr().unwrap();
    let ep = desk.ep.clone();
    desk.rt.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (ep, tx) = (ep.clone(), links_tx.clone());
            tokio::spawn(async move {
                if let Ok(Handshake::Trusted(link)) = ep.accept(stream).await {
                    let _ = tx.send(link);
                    std::future::pending::<()>().await; // hold the link open
                }
            });
        }
    });
    let proxy = desk.proxy(desk_addr, Faults::default(), 5);

    let v: Value = serde_json::from_str(&phone.d.handle(&req(
        "transport_connect",
        json!({
            "host": "127.0.0.1",
            "port": proxy.port(),
            "device_id": "d".repeat(32),
        }),
    )))
    .unwrap();
    assert_eq!(v["ok"], true, "{v}");
    phone.wait("first link", Duration::from_secs(15), |s| {
        s["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["trusted"] == true)
    });
    links_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    proxy.reset_all();
    let s = phone.wait("redialed link", Duration::from_secs(30), |s| {
        let ev = s["events"].as_array().unwrap();
        ev.iter().any(|e| e["kind"] == "redial")
            && s["connections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["trusted"] == true)
    });
    let lost = s["events"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|e| e["kind"] == "link_closed")
        .expect("a link_closed event");
    // The proxy's teardown reaches the phone as an RST or a bare close, depending on timing.
    assert!(
        matches!(lost["code"].as_str(), Some("conn_reset" | "peer_closed")),
        "{lost}"
    );
    assert_eq!(s["connections"].as_array().unwrap().len(), 1, "{s}");
    links_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    phone.d.handle(&req("transport_stop", json!({})));
}
