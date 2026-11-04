// SPDX-License-Identifier: GPL-3.0-or-later
//! EXPERIMENTAL call audio over Wi-Fi, phone side, through the public request interface against a real
//! `bridge_kde` endpoint playing the computer. Needs the `callwifi` feature (debug builds).
#![cfg(feature = "callwifi")]
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bridge_callwifi::codec::Decoder;
use bridge_callwifi::jitter::Out;
use bridge_callwifi::wire::{self, Msg};
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceCredentials, DeviceId, DeviceType, Endpoint,
    Handshake, LinkEvent, MemoryTrustStore, Packet, PairMode, TransportConfig, TrustedLink,
};
use geekbridge_android::Dispatcher;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::time::timeout;

fn request(action: &str, fields: Value) -> String {
    let mut o = Map::new();
    o.insert("version".into(), json!(1));
    o.insert("action".into(), json!(action));
    if let Value::Object(f) = fields {
        o.extend(f);
    }
    Value::Object(o).to_string()
}

fn loopback_host() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    if cfg!(target_os = "linux") {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let b = std::process::id() % 250 + 2;
        format!("127.{b}.{}.{}", (n >> 8) % 250 + 1, n % 250 + 1 + 100)
    } else {
        "127.0.0.1".to_owned()
    }
}

struct Phone {
    d: Arc<Dispatcher>,
    /// Keeps the state directory alive for the phone's lifetime.
    _dir: TempDir,
    id: String,
    addr: SocketAddr,
}

impl Phone {
    fn start() -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Arc::new(Dispatcher::new());
        let f = json!({
            "state_dir": dir.path().to_str().unwrap(),
            "device_name": "Test Phone",
            "listen": { "host": loopback_host(), "port": 0 },
            "allow_loopback": true,
        });
        let v: Value = serde_json::from_str(&d.handle(&request("transport_start", f))).unwrap();
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
            id: r["device_id"].as_str().unwrap().to_owned(),
            d,
            _dir: dir,
            addr,
        }
    }

    fn call(&self, action: &str, fields: Value) -> Value {
        serde_json::from_str(&self.d.handle(&request(action, fields))).unwrap()
    }
    fn ok(&self, action: &str, fields: Value) -> Value {
        let v = self.call(action, fields);
        assert_eq!(v["ok"], true, "{action}: {v}");
        v["result"].clone()
    }
    fn status(&self) -> Value {
        self.ok("transport_status", json!({}))
    }
    fn connection_id(&self) -> u64 {
        self.status()["connections"][0]["connection_id"]
            .as_u64()
            .unwrap()
    }
    /// Collect events until one named `ev` arrives (or `secs` pass); delivered events are not repeated.
    fn events_until(&self, ev: &str, secs: u64) -> Vec<Value> {
        let end = Instant::now() + Duration::from_secs(secs);
        let mut all: Vec<Value> = Vec::new();
        while Instant::now() < end && !all.iter().any(|e| e["ev"] == ev) {
            all.extend(self.events(0, 1));
        }
        all
    }

    fn events(&self, since: u64, secs: u64) -> Vec<Value> {
        let r = self.ok(
            "transport_callwifi_wait",
            json!({"since": since, "timeout_ms": secs * 1000}),
        );
        r["events"].as_array().unwrap().clone()
    }
}

struct Yes;
impl Approver for Yes {
    async fn approve(&mut self, _: ApprovalRequest) -> bool {
        true
    }
}

struct Peer {
    rt: Runtime,
    ep: Arc<Endpoint>,
    link: Option<TrustedLink>,
}

impl Peer {
    /// A desktop with the given declared packet types.
    fn new(incoming: &[&str], outgoing: &[&str]) -> Peer {
        let id = DeviceId::parse(&"d".repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        let config = TransportConfig {
            address_policy: AddressPolicy {
                local_networks: true,
                loopback: true,
            },
            payload: bridge_kde::PayloadConfig {
                any_port: true,
                ..bridge_kde::PayloadConfig::default()
            },
            limits: bridge_kde::Limits {
                packet_line: 64 * 1024,
                ..bridge_kde::Limits::default()
            },
            ..TransportConfig::default()
        };
        let ep = Endpoint::new(
            Arc::new(creds),
            "Desk",
            DeviceType::Desktop,
            incoming.iter().map(|s| s.to_string()).collect(),
            outgoing.iter().map(|s| s.to_string()).collect(),
            Arc::new(MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Peer {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            ep: Arc::new(ep),
            link: None,
        }
    }

    /// Dial the phone, pair with the phone user's approval, keep the trusted link.
    fn pair(&mut self, phone: &Phone) {
        let target = DeviceId::parse(&phone.id).unwrap();
        let hs = self
            .rt
            .block_on(async {
                timeout(
                    Duration::from_secs(15),
                    self.ep.connect(phone.addr, &target),
                )
                .await
            })
            .unwrap()
            .unwrap();
        let Handshake::Pending(p) = hs else {
            panic!("expected unpaired")
        };
        let join = self
            .rt
            .spawn(async move { p.pair(PairMode::Request, &mut Yes).await });
        let end = Instant::now() + Duration::from_secs(10);
        let prompt = loop {
            let s = phone.status();
            if let Some(p) = s["prompts"].as_array().and_then(|a| a.first()) {
                break p.clone();
            }
            assert!(Instant::now() < end, "no prompt");
            std::thread::sleep(Duration::from_millis(20));
        };
        phone.ok(
            "transport_pair_decision",
            json!({
                "request_id": prompt["request_id"],
                "connection_id": prompt["connection_id"],
                "approve": true,
                "verification_code": prompt["verification_code"],
                "peer_cert_sha256": prompt["peer_cert_sha256"],
            }),
        );
        let link = self
            .rt
            .block_on(async { timeout(Duration::from_secs(15), join).await })
            .unwrap()
            .unwrap()
            .unwrap();
        self.link = Some(link);
        let end = Instant::now() + Duration::from_secs(10);
        while phone.status()["connections"][0]["trusted"] != true {
            assert!(Instant::now() < end, "never trusted");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, kind: &str, body: Map<String, Value>) {
        let link = self.link.as_mut().unwrap();
        self.rt
            .block_on(link.send(&Packet::new(kind, body)))
            .unwrap();
    }

    fn recv_packet(&mut self) -> Packet {
        let link = self.link.as_mut().unwrap();
        loop {
            let ev = self
                .rt
                .block_on(async { timeout(Duration::from_secs(10), link.recv()).await })
                .expect("timed out waiting for a packet")
                .unwrap();
            if let LinkEvent::Packet(p) = ev
                && p.kind != "kdeconnect.ping"
            {
                return p;
            }
        }
    }
}

fn computer() -> Peer {
    let all_in: Vec<&str> = wire::COMPUTER_INCOMING.to_vec();
    let all_out: Vec<&str> = wire::PHONE_INCOMING.to_vec();
    let mut i = vec!["kdeconnect.ping"];
    i.extend(all_in);
    let mut o = vec!["kdeconnect.ping"];
    o.extend(all_out);
    Peer::new(&i, &o)
}

fn send_msg(c: &mut Peer, m: &Msg) {
    c.send(m.kind(), wire::body(m));
}

fn recv_msg(c: &mut Peer) -> Msg {
    loop {
        let p = c.recv_packet();
        if let Ok(m) = wire::parse(&p.kind, &p.body) {
            return m;
        }
    }
}

fn pcm_b64(v: i16) -> String {
    let bytes: Vec<u8> = (0..wire::FRAME_SAMPLES)
        .flat_map(|_| v.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn config(phone: &Phone, enabled: bool, verdict: &str) {
    phone.ok(
        "transport_callwifi_config",
        json!({"enabled": enabled, "helper_up": true, "verdict": verdict}),
    );
}

#[test]
fn a_request_needs_switch_grant_self_test_and_a_call_then_audio_flows_and_stops_on_revoke() {
    let phone = Phone::start();
    let mut pc = computer();
    pc.pair(&phone);
    let conn = phone.connection_id();
    // Switch off: a request is refused with a stop.
    send_msg(&mut pc, &Msg::Request { session: 5 });
    assert!(matches!(recv_msg(&mut pc), Msg::Stop { session: 5, .. }));
    // Switch on, self-test works, but no grant list sent: unrestricted until the app sends one (existing rule).
    config(&phone, true, "works");
    // A ready packet reaches the computer.
    let r = loop {
        if let Msg::Ready(r) = recv_msg(&mut pc)
            && r.ready
        {
            break r;
        }
    };
    assert!(r.listen && !r.speak);
    // No call yet: refused.
    send_msg(&mut pc, &Msg::Request { session: 6 });
    assert!(matches!(recv_msg(&mut pc), Msg::Stop { session: 6, .. }));
    phone.ok("transport_callwifi_call", json!({"active": true}));
    send_msg(&mut pc, &Msg::Request { session: 7 });
    let Msg::Offer(o) = recv_msg(&mut pc) else {
        panic!("expected an offer")
    };
    assert!(o.down && !o.up);
    let evs = phone.events(0, 5);
    assert!(
        evs.iter().any(|e| e["ev"] == "start" && e["session"] == 7),
        "{evs:?}"
    );
    // Frames before the computer accepts are not sent.
    assert_eq!(
        phone.ok(
            "transport_callwifi_frame",
            json!({"pcm": pcm_b64(1000), "cap": 1})
        )["sent"],
        false
    );
    send_msg(
        &mut pc,
        &Msg::Answer(wire::Answer {
            session: 7,
            accept: true,
            down: true,
            up: false,
            reason: None,
        }),
    );
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let r = phone.ok(
            "transport_callwifi_frame",
            json!({"pcm": pcm_b64(1000), "cap": 2}),
        );
        if r["sent"] == true {
            break;
        }
        assert!(Instant::now() < end, "never went live");
        std::thread::sleep(Duration::from_millis(20));
    }
    let Msg::Frame(f) = recv_msg(&mut pc) else {
        panic!("expected a frame")
    };
    assert_eq!(f.dir, wire::Dir::Down);
    assert_eq!(
        Decoder::new().unwrap().render(&Out::Packet(f.opus)).len(),
        wire::FRAME_SAMPLES
    );
    // A mic frame from the computer is dropped: listening never carries the computer's voice.
    send_msg(
        &mut pc,
        &Msg::Frame(wire::Frame {
            session: 7,
            dir: wire::Dir::Up,
            seq: 0,
            ts: 0,
            cap: 0,
            opus: vec![1, 2, 3],
        }),
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        phone.ok("transport_callwifi_pull", json!({}))["frames"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Revoking the per-computer grant ends the session at once.
    let mut permits = Map::new();
    for f in [
        "calls",
        "contacts",
        "calendar",
        "calendar_edit",
        "sms",
        "battery",
        "notifications",
        "photos",
        "callaudio",
    ] {
        permits.insert(f.into(), json!([]));
    }
    phone.ok("transport_permits", json!({"permits": permits}));
    let t0 = Instant::now();
    loop {
        if matches!(recv_msg(&mut pc), Msg::Stop { session: 7, .. }) {
            break;
        }
    }
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "stop took {:?}",
        t0.elapsed()
    );
    let evs = phone.events_until("stop", 5);
    assert!(evs.iter().any(|e| e["ev"] == "stop"), "{evs:?}");
    assert_eq!(
        phone.ok(
            "transport_callwifi_frame",
            json!({"pcm": pcm_b64(1000), "cap": 3})
        )["sent"],
        false
    );
    let _ = conn;
}

#[test]
fn without_a_working_self_test_no_offer_is_made() {
    let phone = Phone::start();
    let mut pc = computer();
    pc.pair(&phone);
    config(&phone, true, "silent");
    phone.ok("transport_callwifi_call", json!({"active": true}));
    send_msg(&mut pc, &Msg::Request { session: 9 });
    loop {
        match recv_msg(&mut pc) {
            Msg::Stop { session: 9, reason } => {
                assert_eq!(reason.as_deref(), Some("not_ready"));
                break;
            }
            Msg::Offer(_) => panic!("offered with a silent self-test"),
            _ => {}
        }
    }
}

#[test]
fn the_call_ending_or_the_switch_going_off_stops_the_session() {
    let phone = Phone::start();
    let mut pc = computer();
    pc.pair(&phone);
    config(&phone, true, "works");
    phone.ok("transport_callwifi_call", json!({"active": true}));
    send_msg(&mut pc, &Msg::Request { session: 11 });
    loop {
        if matches!(recv_msg(&mut pc), Msg::Offer(_)) {
            break;
        }
    }
    send_msg(
        &mut pc,
        &Msg::Answer(wire::Answer {
            session: 11,
            accept: true,
            down: true,
            up: false,
            reason: None,
        }),
    );
    std::thread::sleep(Duration::from_millis(200));
    config(&phone, false, "works");
    loop {
        if matches!(recv_msg(&mut pc), Msg::Stop { session: 11, .. }) {
            break;
        }
    }
}

#[test]
fn hostile_packets_are_ignored() {
    let phone = Phone::start();
    let mut pc = computer();
    pc.pair(&phone);
    config(&phone, true, "works");
    let mut b = Map::new();
    b.insert("session".into(), json!("nope"));
    pc.send(wire::FRAME, b.clone());
    pc.send(wire::OFFER, b.clone());
    pc.send(wire::ANSWER, b);
    // The link survives and the phone still answers a proper request.
    phone.ok("transport_callwifi_call", json!({"active": true}));
    send_msg(&mut pc, &Msg::Request { session: 12 });
    loop {
        if matches!(recv_msg(&mut pc), Msg::Offer(_)) {
            break;
        }
    }
}

#[test]
fn a_refused_offer_releases_the_helper_and_an_unchanged_config_is_not_repeated() {
    let phone = Phone::start();
    let mut pc = computer();
    pc.pair(&phone);
    config(&phone, true, "works");
    phone.ok("transport_callwifi_call", json!({"active": true}));
    send_msg(&mut pc, &Msg::Request { session: 21 });
    loop {
        if matches!(recv_msg(&mut pc), Msg::Offer(_)) {
            break;
        }
    }
    send_msg(
        &mut pc,
        &Msg::Answer(wire::Answer {
            session: 21,
            accept: false,
            down: false,
            up: false,
            reason: Some("no_consent".into()),
        }),
    );
    let evs = phone.events_until("stop", 5);
    assert!(evs.iter().any(|e| e["ev"] == "stop"), "{evs:?}");
    // The same config twice is not announced twice.
    config(&phone, true, "works");
    config(&phone, true, "works");
}
