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
        // Delivered events are released by the phone, so keep what each poll returns.
        let mut evs: Vec<Value> = Vec::new();
        loop {
            let r = self.wait_events(since, 1);
            evs.extend(r["events"].as_array().unwrap().iter().cloned());
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
fn events_say_which_computer_asked_and_a_body_cannot_forge_it() {
    // The phone limits texts per computer (not per connection): it must learn who asked from the core,
    // never from the packet body the computer controls.
    let phone = Phone::start(Some(json!({"sms": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    peer.send(
        "kdeconnect.sms.request",
        body(json!({
            "requestId": "r1", "messageBody": "hi", "addresses": [{"address": "+4915112345678"}],
            "device": "f".repeat(32), "computer": "Someone else",
        })),
    );
    let ev = &phone.events(0, 1)[0];
    assert_eq!(ev["kind"], "kdeconnect.sms.request");
    assert_eq!(ev["device"], "d".repeat(32));
    assert_eq!(ev["computer"], "Desk");
    // The forged fields stay where the computer put them, inside the body.
    assert_eq!(ev["body"]["device"], "f".repeat(32));
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

fn debug_peer() -> Peer {
    Peer::new(
        &[
            "kdeconnect.ping",
            "geekbridge.debug.log",
            "geekbridge.debug.state",
        ],
        &["kdeconnect.ping", "geekbridge.debug.state_request"],
    )
}

#[test]
fn debug_packets_flow_only_with_the_flag_a_declaring_computer_and_within_limits() {
    // Declared only when the app turned it on.
    let off = Phone::start(Some(json!({"sms": true})));
    let caps = &off.started["capabilities"];
    assert!(!caps.to_string().contains("geekbridge.debug"), "{caps}");
    assert_eq!(
        off.code(
            "transport_data_send",
            json!({"kind": "geekbridge.debug.log", "body": {"seq": 1, "lines": []}})
        ),
        "data_disabled"
    );

    let phone = Phone::start(Some(json!({"debug": true})));
    assert!(
        phone.started["capabilities"]["incoming"]
            .to_string()
            .contains("geekbridge.debug.state_request")
    );

    // A stock-like computer that does not declare the debug packets never gets them.
    let mut stock = Peer::new(&["kdeconnect.ping"], &["kdeconnect.ping"]);
    stock.pair(&phone);
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.debug.log", "body": {"seq": 1, "lines": ["x"]}})
        ),
        "not_connected"
    );
    stock.send(
        "geekbridge.debug.state_request",
        body(json!({"requestId": "r"})),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(stock);

    let phone = Phone::start(Some(json!({"debug": true})));
    let mut peer = debug_peer();
    peer.pair(&phone);
    phone.ok(
        "transport_data_send",
        json!({"kind": "geekbridge.debug.log", "body": {"seq": 1, "lines": ["hello"]}}),
    );
    let p = peer.recv_kind("geekbridge.debug.log");
    assert_eq!(p.body["lines"][0], "hello");
    // The state request becomes an event for Kotlin, and the answer goes back.
    peer.send(
        "geekbridge.debug.state_request",
        body(json!({"requestId": "r9"})),
    );
    let ev = phone.events(0, 1);
    assert_eq!(ev[0]["kind"], "geekbridge.debug.state_request");
    assert_eq!(ev[0]["body"]["requestId"], "r9");
    phone.ok(
        "transport_data_send",
        json!({"kind": "geekbridge.debug.state", "body": {"requestId": "r9", "text": "sms: not offered: x"}}),
    );
    assert_eq!(
        peer.recv_kind("geekbridge.debug.state").body["text"],
        "sms: not offered: x"
    );
    // Size cap (128 KiB) and the own rate limit.
    let huge = "z".repeat(130 * 1024);
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.debug.log", "body": {"seq": 2, "lines": [huge]}})
        ),
        "too_large"
    );
    let mut limited = 0;
    for seq in 3..40 {
        let v = phone.call(
            "transport_data_send",
            json!({"kind": "geekbridge.debug.log", "body": {"seq": seq, "lines": ["f"]}}),
        );
        if v["ok"] == false {
            assert_eq!(v["error"]["code"], "rate_limited");
            limited += 1;
        }
    }
    assert!(limited >= 20, "{limited}");
    // Debug traffic did not use the SMS budget: switching it on later still works at once.
    phone.ok("transport_data_config", json!({"debug": false}));
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.debug.log", "body": {"seq": 99, "lines": []}})
        ),
        "data_disabled"
    );
}

fn battery_peer() -> Peer {
    // A desktop (or stock KDE Connect) that accepts `kdeconnect.battery` and can ask for it.
    Peer::new(
        &["kdeconnect.ping", "kdeconnect.battery"],
        &["kdeconnect.ping", "kdeconnect.battery.request"],
    )
}

#[test]
fn battery_is_declared_only_when_enabled_in_the_stock_names() {
    let off = Phone::start(Some(ALL()));
    assert!(!off.started.to_string().contains("battery"));
    let on = Phone::start(Some(json!({"battery": true})));
    let caps = &on.started["capabilities"];
    assert!(
        caps["outgoing"]
            .to_string()
            .contains("\"kdeconnect.battery\""),
        "{caps}"
    );
    assert!(
        caps["incoming"]
            .to_string()
            .contains("\"kdeconnect.battery.request\""),
        "{caps}"
    );
    // No other private data comes with it.
    assert!(!caps.to_string().contains("sms") && !caps.to_string().contains("contacts"));
}

#[test]
fn battery_goes_only_to_allowed_computers_and_a_request_surfaces_as_an_event() {
    let phone = Phone::start(Some(json!({"battery": true})));
    let mut peer = battery_peer();
    peer.pair(&phone);
    // Until the app sends the permit list nothing is restricted; once it has, this computer needs to be on it.
    phone.ok(
        "transport_permits",
        json!({"permits": {"battery": ["e".repeat(32)]}}),
    );
    let level = json!({"currentCharge": 55, "isCharging": true, "thresholdEvent": 0});
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "kdeconnect.battery", "body": level})
        ),
        "not_connected"
    );
    peer.send("kdeconnect.battery.request", body(json!({"request": true})));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    phone.ok(
        "transport_permits",
        json!({"permits": {"battery": ["d".repeat(32)]}}),
    );
    peer.send("kdeconnect.battery.request", body(json!({"request": true})));
    let evs = phone.events(0, 1);
    assert_eq!(evs[0]["kind"], "kdeconnect.battery.request");
    // The reply goes to that connection only, with the stock field names.
    phone.ok(
        "transport_data_send",
        json!({"kind": "kdeconnect.battery", "body": level, "connection_id": evs[0]["connection_id"]}),
    );
    let p = peer.recv_kind("kdeconnect.battery");
    assert_eq!(p.body["currentCharge"], 55);
    assert_eq!(p.body["isCharging"], true);

    // Switching the feature off in the app refuses further sends and drops requests.
    phone.ok("transport_data_config", json!({"battery": false}));
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "kdeconnect.battery", "body": level})
        ),
        "data_disabled"
    );
}

fn notification_peer() -> Peer {
    // A desktop (or stock KDE Connect) that accepts `kdeconnect.notification` and can ask, dismiss and press buttons.
    Peer::new(
        &["kdeconnect.ping", "kdeconnect.notification"],
        &[
            "kdeconnect.ping",
            "kdeconnect.notification.request",
            "kdeconnect.notification.action",
        ],
    )
}

#[test]
fn notifications_are_declared_only_when_enabled_in_the_stock_names_without_reply() {
    let off = Phone::start(Some(ALL()));
    assert!(!off.started.to_string().contains("notification"));
    let on = Phone::start(Some(json!({"notifications": true})));
    let caps = on.started["capabilities"].to_string();
    assert!(caps.contains("\"kdeconnect.notification\""), "{caps}");
    assert!(
        caps.contains("\"kdeconnect.notification.request\""),
        "{caps}"
    );
    assert!(
        caps.contains("\"kdeconnect.notification.action\""),
        "{caps}"
    );
    assert!(!caps.contains("notification.reply"), "{caps}");
    assert!(!caps.contains("sms") && !caps.contains("contacts"));
}

#[test]
fn notifications_go_only_to_allowed_computers_and_requests_surface_as_events() {
    let phone = Phone::start(Some(json!({"notifications": true})));
    let mut peer = notification_peer();
    peer.pair(&phone);
    phone.ok(
        "transport_permits",
        json!({"permits": {"notifications": ["e".repeat(32)]}}),
    );
    let post =
        json!({"id": "n1", "appName": "Chat", "title": "Ada", "text": "hi", "isClearable": true});
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "kdeconnect.notification", "body": post})
        ),
        "not_connected"
    );
    peer.send(
        "kdeconnect.notification.request",
        body(json!({"cancel": "n1"})),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a computer that was not allowed is ignored"
    );

    phone.ok(
        "transport_permits",
        json!({"permits": {"notifications": ["d".repeat(32)]}}),
    );
    peer.send(
        "kdeconnect.notification.request",
        body(json!({"cancel": "n1"})),
    );
    peer.send(
        "kdeconnect.notification.action",
        body(json!({"key": "n1", "action": "Archive"})),
    );
    // One wait may hand back both events (they are released once delivered), so ask for both.
    let got = phone.events(0, 2);
    assert_eq!(got[0]["kind"], "kdeconnect.notification.request");
    assert_eq!(got[0]["body"]["cancel"], "n1");
    assert_eq!(got[1]["kind"], "kdeconnect.notification.action");
    phone.ok(
        "transport_data_send",
        json!({"kind": "kdeconnect.notification", "body": post}),
    );
    let p = peer.recv_kind("kdeconnect.notification");
    assert_eq!(p.body["id"], "n1");
    assert_eq!(p.body["appName"], "Chat");

    phone.ok("transport_data_config", json!({"notifications": false}));
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "kdeconnect.notification", "body": post})
        ),
        "data_disabled"
    );
}

fn photos_peer() -> Peer {
    // A GeekBridge desktop: accepts the list and thumbnail packets and can ask.
    Peer::new(
        &[
            "kdeconnect.ping",
            "geekbridge.photos.list",
            "geekbridge.photos.thumb",
        ],
        &["kdeconnect.ping", "geekbridge.photos.request"],
    )
}

#[test]
fn photos_are_declared_only_when_enabled_and_never_with_other_private_data() {
    let off = Phone::start(Some(ALL()));
    assert!(!off.started.to_string().contains("photos"));
    let on = Phone::start(Some(json!({"photos": true})));
    let caps = &on.started["capabilities"];
    assert!(
        caps["incoming"]
            .to_string()
            .contains("\"geekbridge.photos.request\""),
        "{caps}"
    );
    let out = caps["outgoing"].to_string();
    assert!(
        out.contains("\"geekbridge.photos.list\"") && out.contains("\"geekbridge.photos.thumb\""),
        "{out}"
    );
    assert!(!caps.to_string().contains("sms") && !caps.to_string().contains("contacts"));
}

#[test]
fn photos_go_only_to_allowed_computers_and_requests_surface_as_events() {
    let phone = Phone::start(Some(json!({"photos": true})));
    let mut peer = photos_peer();
    peer.pair(&phone);
    phone.ok(
        "transport_permits",
        json!({"permits": {"photos": ["e".repeat(32)]}}),
    );
    let list = json!({"items": [], "partial": false});
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.photos.list", "body": list})
        ),
        "not_connected"
    );
    peer.send(
        "geekbridge.photos.request",
        body(json!({"op": "list", "limit": 24})),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a computer that was not allowed is ignored"
    );

    phone.ok(
        "transport_permits",
        json!({"permits": {"photos": ["d".repeat(32)]}}),
    );
    peer.send(
        "geekbridge.photos.request",
        body(json!({"op": "list", "limit": 24})),
    );
    let evs = phone.events(0, 1);
    assert_eq!(evs[0]["kind"], "geekbridge.photos.request");
    assert_eq!(evs[0]["body"]["op"], "list");
    // The answer goes to that connection with the GeekBridge names.
    phone.ok(
        "transport_data_send",
        json!({"kind": "geekbridge.photos.list", "body": list, "connection_id": evs[0]["connection_id"]}),
    );
    let p = peer.recv_kind("geekbridge.photos.list");
    assert_eq!(p.body["items"].as_array().unwrap().len(), 0);

    phone.ok("transport_data_config", json!({"photos": false}));
    assert_eq!(
        phone.code(
            "transport_data_send",
            json!({"kind": "geekbridge.photos.list", "body": list})
        ),
        "data_disabled"
    );
}

/// Against the REAL stock KDE Connect daemon's capabilities (captured fixture): the phone sends it only
/// the stock packets it declared (SMS answers, battery) and never a GeekBridge extension, even with the
/// feature on and the computer allowed.
#[test]
fn a_stock_desktop_gets_stock_packets_only() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bridge-kde/tests/fixtures/kdeconnect-real/daemon-identity-secure.json"
    );
    let line = std::fs::read(path).unwrap();
    let id =
        bridge_kde::Identity::from_packet(&Packet::from_line(&line[..line.len() - 1]).unwrap())
            .unwrap();
    let incoming: Vec<&str> = id
        .incoming_capabilities
        .iter()
        .map(String::as_str)
        .collect();
    let outgoing: Vec<&str> = id
        .outgoing_capabilities
        .iter()
        .map(String::as_str)
        .collect();
    let phone = Phone::start(Some(
        json!({"contacts": true, "calendar": true, "sms": true, "debug": true, "battery": true, "photos": true}),
    ));
    let mut peer = Peer::new(&incoming, &outgoing);
    peer.pair(&phone);
    for kind in [
        "geekbridge.photos.list",
        "geekbridge.photos.thumb",
        "geekbridge.calendar.sync",
        "geekbridge.sms.status",
        "geekbridge.debug.log",
        "geekbridge.debug.state",
    ] {
        assert_eq!(
            phone.code("transport_data_send", json!({"kind": kind, "body": {}})),
            "not_connected",
            "{kind}"
        );
    }
    // The stock names it does declare still flow.
    let r = phone.ok(
        "transport_data_send",
        json!({"kind": "kdeconnect.battery", "body": {"currentCharge": 50, "isCharging": false, "thresholdEvent": 0}}),
    );
    assert_eq!(r["queued"], 1);
    assert_eq!(
        peer.recv_kind("kdeconnect.battery").body["currentCharge"],
        50
    );
}
