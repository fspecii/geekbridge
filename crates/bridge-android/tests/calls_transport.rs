// SPDX-License-Identifier: GPL-3.0-or-later
//! Call control on the phone transport, through the public request interface, against a real
//! `bridge_kde` endpoint playing the desktop. Telecom itself (Android's `TelecomManager`) is Kotlin's
//! and is covered by the JVM tests.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bridge_core::calls_link::{self as wire, LinkAction, LinkFailure, REQUEST, RESULT, STATE};
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
    fn start(calls: Option<Value>) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Arc::new(Dispatcher::new());
        let mut f = json!({
            "state_dir": dir.path().to_str().unwrap(),
            "device_name": "Test Phone",
            "listen": { "host": loopback_host(), "port": 0 },
            "allow_loopback": true,
        });
        if let Some(c) = calls {
            f["calls"] = c;
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
            "transport_calls_wait",
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

    fn full() -> Peer {
        Self::new(
            &["kdeconnect.ping", STATE, RESULT],
            &["kdeconnect.ping", REQUEST],
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

#[test]
fn calls_are_not_advertised_or_acted_on_unless_the_app_asks() {
    let phone = Phone::start(None);
    let mut peer = Peer::full();
    peer.pair(&phone);
    assert!(!phone.status().to_string().contains("geekbridge.calls"));
    assert_eq!(
        phone.code("transport_calls_report", json!({"state": "ringing"})),
        "calls_disabled"
    );
    peer.send(REQUEST, wire::request_body("r1", LinkAction::Answer));
    std::thread::sleep(Duration::from_millis(300));
    let r = phone.wait_events(0, 0);
    assert!(r["events"].as_array().unwrap().is_empty());
    assert_eq!(r["enabled"], false);
}

#[test]
fn state_reports_reach_the_computer_and_are_validated() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    phone.ok(
        "transport_calls_report",
        json!({"state": "ringing", "number": "+15550100", "name": "Ada", "can_answer": true, "can_end": true}),
    );
    let p = peer.recv_kind(STATE);
    let report = wire::LinkCallReport::parse(&p.body).unwrap();
    assert_eq!(report.state, wire::PhoneState::Ringing);
    assert_eq!(report.number.unwrap().expose(), "+15550100");
    assert!(report.can_answer && report.can_end);

    // An idle report carries no party even if Kotlin passes one.
    phone.ok(
        "transport_calls_report",
        json!({"state": "idle", "number": "+15550100"}),
    );
    let p = peer.recv_kind(STATE);
    assert!(!p.body.contains_key("number"));

    for bad in [
        json!({"state": "dialing"}),
        json!({"state": "ringing", "number": "a".repeat(500)}),
        json!({"state": "ringing", "extra": 1}),
        json!({"state": 4}),
    ] {
        let code = phone.code("transport_calls_report", bad);
        assert!(
            ["invalid_field", "wrong_type", "unknown_field"].contains(&code.as_str()),
            "{code}"
        );
    }
}

#[test]
fn a_computer_that_connects_mid_ring_is_told_and_requests_become_events() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    phone.ok(
        "transport_calls_report",
        json!({"state": "ringing", "can_answer": true, "can_end": true}),
    );
    let mut peer = Peer::full();
    peer.pair(&phone);
    let p = peer.recv_kind(STATE);
    assert_eq!(
        wire::LinkCallReport::parse(&p.body).unwrap().state,
        wire::PhoneState::Ringing
    );

    peer.send(REQUEST, wire::request_body("req-9", LinkAction::Answer));
    let evs = phone.events(0, 1);
    assert_eq!(evs[0]["kind"], "request");
    assert_eq!(evs[0]["action"], "answer");
    assert_eq!(evs[0]["request_id"], "req-9");
    let conn = evs[0]["connection_id"].as_u64().unwrap();
    assert_eq!(conn, phone.connection_id());

    // Kotlin answers; only that answer leaves the phone.
    phone.ok(
        "transport_calls_result",
        json!({"connection_id": conn, "request_id": "req-9"}),
    );
    let r = peer.recv_kind(RESULT);
    assert_eq!(
        wire::parse_result(&r.body),
        Ok(("req-9".to_owned(), Ok(())))
    );

    phone.ok(
        "transport_calls_result",
        json!({"connection_id": conn, "request_id": "req-10", "failure": "permission_missing"}),
    );
    let r = peer.recv_kind(RESULT);
    assert_eq!(
        wire::parse_result(&r.body),
        Ok(("req-10".to_owned(), Err(LinkFailure::PermissionMissing)))
    );
    assert_eq!(
        phone.code(
            "transport_calls_result",
            json!({"connection_id": conn, "request_id": "bad id", "failure": "failed"})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_calls_result",
            json!({"connection_id": 999, "request_id": "ok1"})
        ),
        "unknown_connection"
    );
}

#[test]
fn malformed_requests_are_ignored_and_a_disabled_app_refuses_without_telling_kotlin() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    let mut bad = Map::new();
    bad.insert("requestId".into(), json!("r1"));
    bad.insert("action".into(), json!("dial"));
    peer.send(REQUEST, bad);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    phone.ok("transport_calls_config", json!({"enabled": false}));
    peer.send(REQUEST, wire::request_body("r2", LinkAction::Hangup));
    let r = peer.recv_kind(RESULT);
    assert_eq!(
        wire::parse_result(&r.body),
        Ok(("r2".to_owned(), Err(LinkFailure::PermissionMissing)))
    );
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_blocked_wait_ends_on_stop() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    let d = phone.d.clone();
    let waiter = std::thread::spawn(move || {
        serde_json::from_str::<Value>(&d.handle(&request(
            "transport_calls_wait",
            json!({"since": 0, "timeout_ms": 30000}),
        )))
        .unwrap()
    });
    std::thread::sleep(Duration::from_millis(100));
    phone.ok("transport_stop", json!({}));
    let v = waiter.join().unwrap();
    assert_eq!(v["result"]["stopped"], true);
}

#[test]
fn a_computer_outside_the_permit_list_is_refused_and_hears_no_call_state() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    phone.ok(
        "transport_permits",
        json!({"permits": {"calls": ["e".repeat(32)]}}),
    );
    // A report is queued for nobody.
    let r = phone.ok(
        "transport_calls_report",
        json!({"state": "ringing", "number": "+4915112345", "can_answer": true, "can_end": true}),
    );
    assert_eq!(r["queued"], 0);
    // Its request is refused at once and never reaches Kotlin.
    peer.send(REQUEST, wire::request_body("r1", LinkAction::Answer));
    let res = peer.recv_kind(RESULT);
    assert_eq!(
        wire::parse_result(&res.body),
        Ok(("r1".to_owned(), Err(LinkFailure::PermissionMissing)))
    );
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Allowed again: reports and requests flow.
    phone.ok(
        "transport_permits",
        json!({"permits": {"calls": ["d".repeat(32)]}}),
    );
    let r = phone.ok(
        "transport_calls_report",
        json!({"state": "ringing", "can_answer": true, "can_end": true}),
    );
    assert_eq!(r["queued"], 1);
    peer.send(REQUEST, wire::request_body("r2", LinkAction::Answer));
    assert_eq!(phone.events(0, 1)[0]["request_id"], "r2");
}
