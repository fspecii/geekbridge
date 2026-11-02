// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone data relay (contacts, calendar, SMS) through the public request interface, against a real
//! `bridge_kde` endpoint playing the desktop. The Android providers are Kotlin's.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    started: Value,
}

impl Phone {
    fn start(data: Option<Value>) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Arc::new(Dispatcher::new());
        let mut f = json!({
            "state_dir": dir.path().to_str().unwrap(),
            "device_name": "Test Phone",
            "listen": { "host": loopback_host(), "port": 0 },
            "allow_loopback": true,
        });
        if let Some(c) = data {
            f["data"] = c;
        }
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
            started: r.clone(),
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
    fn code(&self, action: &str, fields: Value) -> String {
        let v = self.call(action, fields);
        assert_eq!(v["ok"], false, "{action}: {v}");
        v["error"]["code"].as_str().unwrap().to_owned()
    }
    fn status(&self) -> Value {
        self.ok("transport_status", json!({}))
    }
    fn connection_id(&self) -> u64 {
        self.status()["connections"][0]["connection_id"]
            .as_u64()
            .unwrap()
    }
    fn wait_events(&self, since: u64, secs: u64) -> Value {
        self.ok(
            "transport_data_wait",
            json!({"since": since, "timeout_ms": secs * 1000}),
        )
    }
    /// Poll `wait` (it returns promptly once an event exists) until `n` events are seen.
    fn events(&self, since: u64, n: usize) -> Vec<Value> {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let r = self.wait_events(since, 1);
            let evs = r["events"].as_array().unwrap().clone();
            if evs.len() >= n {
                return evs;
            }
            assert!(
                Instant::now() < end,
                "timed out waiting for {n} events: {r}"
            );
        }
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
                packet_line: 4 * 1024 * 1024,
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

    fn full() -> Peer {
        Self::new(
            &[
                "kdeconnect.ping",
                "kdeconnect.contacts.response_uids_timestamps",
                "kdeconnect.contacts.response_vcards",
                "geekbridge.calendar.sync",
                "kdeconnect.sms.messages",
                "geekbridge.sms.status",
            ],
            &[
                "kdeconnect.ping",
                "kdeconnect.contacts.request_all_uids_timestamps",
                "geekbridge.calendar.request",
                "kdeconnect.sms.request_conversations",
                "kdeconnect.sms.request",
            ],
        )
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

    fn recv_kind(&mut self, kind: &str) -> Packet {
        loop {
            let p = self.recv_packet();
            if p.kind == kind {
                return p;
            }
        }
    }
}

const ALL: fn() -> Value = || json!({"contacts": true, "calendar": true, "sms": true});

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn capabilities_are_declared_per_enabled_feature_only() {
    let off = Phone::start(None);
    let s = off.started.to_string();
    assert!(!s.contains("calendar") && !s.contains("kdeconnect.sms") && !s.contains("contacts"));
    let cal = Phone::start(Some(json!({"calendar": true})));
    let caps = cal.started["capabilities"].to_string();
    assert!(caps.contains("geekbridge.calendar.request"), "{caps}");
    assert!(caps.contains("geekbridge.calendar.sync"));
    assert!(!caps.contains("kdeconnect.sms") && !caps.contains("contacts"));
    let all = Phone::start(Some(ALL()));
    let caps = all.started["capabilities"].to_string();
    for k in [
        "kdeconnect.contacts.request_vcards_by_uid",
        "kdeconnect.contacts.response_vcards",
        "kdeconnect.sms.request",
        "kdeconnect.sms.messages",
        "geekbridge.sms.status",
    ] {
        assert!(caps.contains(k), "{k}");
    }
}

#[test]
fn desktop_requests_become_events_and_a_disabled_feature_drops() {
    let phone = Phone::start(Some(json!({"calendar": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    let cid = phone.connection_id();
    peer.send(
        "geekbridge.calendar.request",
        body(json!({"requestId": "r1", "since": "t:1"})),
    );
    peer.send("kdeconnect.sms.request_conversations", Map::new());
    let evs = phone.events(0, 1);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["kind"], "geekbridge.calendar.request");
    assert_eq!(evs[0]["connection_id"], cid);
    assert_eq!(evs[0]["body"]["since"], "t:1");
    // The SMS request of the disabled feature never surfaces, and is not replayed.
    std::thread::sleep(Duration::from_millis(300));
    let r = phone.wait_events(evs[0]["seq"].as_u64().unwrap(), 0);
    assert!(r["events"].as_array().unwrap().is_empty());
    // Switching it on later does not resurrect it either.
    phone.ok("transport_data_config", json!({"sms": true}));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    peer.send("kdeconnect.sms.request_conversations", Map::new());
    assert_eq!(
        phone.events(0, 1)[0]["kind"],
        "kdeconnect.sms.request_conversations"
    );
}

#[test]
fn sends_reach_the_peer_and_are_checked() {
    let phone = Phone::start(Some(json!({"contacts": true, "sms": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    phone.ok(
        "transport_data_send",
        json!({"kind": "geekbridge.sms.status", "body": {"requestId": "r1", "state": "sent"}}),
    );
    let p = peer.recv_kind("geekbridge.sms.status");
    assert_eq!(p.body["state"], "sent");
    // Large (but in-bounds) vCard batches go through.
    let big = "x".repeat(900 * 1024);
    phone.ok(
        "transport_data_send",
        json!({"kind": "kdeconnect.contacts.response_vcards", "body": {"uids": ["a"], "a": big}}),
    );
    assert_eq!(
        peer.recv_kind("kdeconnect.contacts.response_vcards").body["uids"][0],
        "a"
    );
    // Not allow-listed, wrong direction, disabled feature, over 1 MiB.
    for (kind, code) in [
        ("kdeconnect.sms.request", "kind_not_allowed"),
        ("kdeconnect.ping", "kind_not_allowed"),
        ("geekbridge.calendar.sync", "data_disabled"),
    ] {
        assert_eq!(
            phone.code("transport_data_send", json!({"kind": kind, "body": {}})),
            code
        );
    }
    let huge = "y".repeat(1024 * 1024 + 10);
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "kdeconnect.contacts.response_vcards", "body": {"a": huge}})
        ),
        "too_large"
    );
    // Other actions keep the small request cap.
    assert_eq!(
        phone.code(
            "transport_ping",
            json!({"connection_id": 1, "pad": "z".repeat(20_000)})
        ),
        "request_too_large"
    );
}

#[test]
fn sending_is_rate_limited_and_needs_a_trusted_link() {
    let phone = Phone::start(Some(json!({"sms": true})));
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.sms.status", "body": {}})
        ),
        "not_connected"
    );
    let mut peer = Peer::full();
    peer.pair(&phone);
    let mut limited = 0;
    for _ in 0..40 {
        let v = phone.call(
            "transport_data_send",
            json!({"kind": "geekbridge.sms.status", "body": {"requestId": "r", "state": "sent"}}),
        );
        if v["ok"] == false {
            let code = v["error"]["code"].as_str().unwrap();
            assert!(["rate_limited", "busy"].contains(&code), "{code}");
            limited += 1;
        }
    }
    assert!(limited >= 10, "{limited}");
}

#[test]
fn a_computer_outside_the_permit_list_gets_nothing_and_is_ignored() {
    let phone = Phone::start(Some(json!({"sms": true, "calendar": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    // The list names a different computer for SMS and nobody for the calendar.
    phone.ok(
        "transport_permits",
        json!({"permits": {"sms": ["e".repeat(32)]}}),
    );
    peer.send("kdeconnect.sms.request_conversations", Map::new());
    peer.send(
        "geekbridge.calendar.request",
        body(json!({"requestId": "r1"})),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for kind in ["geekbridge.sms.status", "geekbridge.calendar.sync"] {
        assert_eq!(
            phone.code("transport_data_send", json!({"kind": kind, "body": {}})),
            "not_connected"
        );
    }
    // Allowing this computer for SMS only opens SMS.
    phone.ok(
        "transport_permits",
        json!({"permits": {"sms": ["d".repeat(32)]}}),
    );
    peer.send("kdeconnect.sms.request_conversations", Map::new());
    assert_eq!(
        phone.events(0, 1)[0]["kind"],
        "kdeconnect.sms.request_conversations"
    );
    phone.ok(
        "transport_data_send",
        json!({"kind": "geekbridge.sms.status", "body": {"requestId": "r1", "state": "sent"}}),
    );
    assert_eq!(
        peer.recv_kind("geekbridge.sms.status").body["state"],
        "sent"
    );
    // Bad lists are refused.
    for bad in [
        json!({"permits": {"bogus": []}}),
        json!({"permits": {"sms": ["not an id"]}}),
        json!({"permits": {"sms": [1]}}),
    ] {
        assert!(phone.call("transport_permits", bad)["ok"] == false);
    }
}
