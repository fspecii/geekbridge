// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard on the phone transport, through the public request interface, against a real
//! `bridge_kde` endpoint playing the desktop. Nothing here runs on Android or talks to a real
//! desktop app; Kotlin's side of the contract (unlock/age decisions) is tested in the JVM tests.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bridge_core::clipboard_wire::{
    self as wire, CLIPBOARD, CLIPBOARD_CONNECT, ITEM, ItemKind, NONE, NoneReason, PULL,
};
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceCredentials, DeviceId, DeviceType, Endpoint,
    Handshake, LinkEvent, MemoryTrustStore, Packet, PairMode, TransportConfig, TrustedLink,
};
use geekbridge_android::Dispatcher;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::time::timeout;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nthe-rest-of-an-image";

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
    dir: TempDir,
    id: String,
    addr: SocketAddr,
}

impl Phone {
    fn start(clipboard: Option<Value>) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Arc::new(Dispatcher::new());
        let mut f = json!({
            "state_dir": dir.path().to_str().unwrap(),
            "device_name": "Test Phone",
            "listen": { "host": loopback_host(), "port": 0 },
            "allow_loopback": true,
        });
        if let Some(c) = clipboard {
            f["clipboard"] = c;
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
            dir,
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
            "transport_clipboard_wait",
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
                packet_line: wire::MAX_ITEM_LINE_BYTES,
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
            &["kdeconnect.ping", CLIPBOARD, CLIPBOARD_CONNECT, ITEM, NONE],
            &["kdeconnect.ping", CLIPBOARD, CLIPBOARD_CONNECT, PULL, ITEM],
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

    /// Send an image item with its payload, as the desktop service does.
    fn send_image(&mut self, bytes: Vec<u8>, request_id: Option<&str>) {
        let link = self.link.as_mut().unwrap();
        let ctx = link.payload_context();
        let size = bytes.len() as u64;
        let mut p = Packet::new(
            ITEM,
            wire::image_item_body("image/png", size, request_id, 0),
        );
        p.payload_size = Some(size as i64);
        let (listener, port) = self.rt.block_on(async {
            let l = ctx.listen().await.unwrap();
            let port = l.port();
            (l, port)
        });
        let mut info = Map::new();
        info.insert("port".into(), port.into());
        p.payload_transfer_info = Some(info);
        self.rt.block_on(link.send_with_payload(&p)).unwrap();
        self.rt.spawn(async move {
            let cancel = bridge_kde::CancelToken::new();
            let mut progress = |_: u64| {};
            let _ = listener
                .send(std::io::Cursor::new(bytes), size, &mut progress, &cancel)
                .await;
        });
    }
}

fn text_event<'a>(evs: &'a [Value], text: &str) -> Option<&'a Value> {
    evs.iter().find(|e| e["text"] == text)
}

// ------------------------------------------------------------------ tests

#[test]
fn clipboard_is_not_advertised_or_accepted_unless_the_app_asks() {
    let phone = Phone::start(None);
    let mut peer = Peer::full();
    peer.pair(&phone);
    let caps = phone.status();
    assert!(!caps.to_string().contains("clipboard"));
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "text", "text": "x"})
        ),
        "clipboard_disabled"
    );
    // A desktop that sends anyway is ignored, and no event is ever produced.
    peer.send(CLIPBOARD, wire::update_body("sneaky"));
    peer.send(PULL, wire::pull_body("req-1", 30_000));
    std::thread::sleep(Duration::from_millis(300));
    let r = phone.wait_events(0, 0);
    assert!(r["events"].as_array().unwrap().is_empty());
    assert_eq!(r["enabled"], false);
}

#[test]
fn desktop_text_urls_and_html_become_events_for_kotlin() {
    let phone = Phone::start(Some(json!({"enabled": true})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    peer.send(CLIPBOARD, wire::update_body("from the pc"));
    peer.send(
        ITEM,
        wire::text_item_body(ItemKind::Html, "plain", Some("<b>plain</b>"), None, 0),
    );
    peer.send(
        ITEM,
        wire::text_item_body(ItemKind::Url, "https://example.org/x", None, None, 0),
    );
    let evs = phone.events(0, 3);
    let t = text_event(&evs, "from the pc").unwrap();
    assert_eq!(
        (t["kind"].as_str(), t["item"].as_str()),
        (Some("item"), Some("text"))
    );
    let h = text_event(&evs, "plain").unwrap();
    assert_eq!(
        (h["item"].as_str(), h["html"].as_str()),
        (Some("html"), Some("<b>plain</b>"))
    );
    assert_eq!(
        text_event(&evs, "https://example.org/x").unwrap()["item"],
        "url"
    );
    // Acknowledging releases them: nothing is kept after Kotlin has taken it.
    let last = phone.wait_events(0, 0)["last_seq"].as_u64().unwrap();
    assert!(
        phone.wait_events(last, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn connect_announcements_older_than_the_link_are_ignored() {
    let phone = Phone::start(Some(json!({})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    peer.send(CLIPBOARD_CONNECT, wire::connect_body("stale", 1_000));
    peer.send(CLIPBOARD_CONNECT, wire::connect_body("unknown age", 0));
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 5_000;
    peer.send(CLIPBOARD_CONNECT, wire::connect_body("fresh", future));
    let evs = phone.events(0, 1);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["text"], "fresh");
}

#[test]
fn a_pull_is_an_event_and_only_kotlins_answer_leaves_the_phone() {
    let phone = Phone::start(Some(json!({})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    peer.send(PULL, wire::pull_body("req-7", 20_000));
    let evs = phone.events(0, 1);
    assert_eq!(evs[0]["kind"], "pull");
    assert_eq!(evs[0]["request_id"], "req-7");
    assert_eq!(evs[0]["max_age_ms"], 20_000);
    let conn = evs[0]["connection_id"].as_u64().unwrap();
    // Nothing was sent by the phone on its own.
    // Kotlin decides: here, "locked".
    phone.ok(
        "transport_clipboard_send",
        json!({"connection_id": conn, "request_id": "req-7", "none": "locked"}),
    );
    let p = peer.recv_packet();
    assert_eq!(p.kind, NONE);
    assert_eq!(
        wire::parse_none(&p.body),
        Ok(("req-7".into(), NoneReason::Locked))
    );
    // Kotlin decides: here, a fresh URL.
    peer.send(PULL, wire::pull_body("req-8", 30_000));
    phone.events(evs[0]["seq"].as_u64().unwrap(), 1);
    phone.ok(
        "transport_clipboard_send",
        json!({
            "connection_id": conn, "request_id": "req-8", "kind": "url",
            "text": "https://example.org/y", "age_ms": 1500
        }),
    );
    let p = peer.recv_packet();
    assert_eq!(p.kind, ITEM);
    let parsed = wire::parse_item(
        &p.body,
        &wire::Limits {
            max_text_bytes: 65536,
            max_image_bytes: 1 << 20,
        },
    )
    .unwrap();
    assert_eq!(parsed.request_id.as_deref(), Some("req-8"));
    assert_eq!(parsed.age_ms, 1500);
}

#[test]
fn pulls_get_an_automatic_disabled_answer_when_the_user_turned_sync_off() {
    let phone = Phone::start(Some(json!({"enabled": false})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    peer.send(PULL, wire::pull_body("req-9", 30_000));
    let p = peer.recv_packet();
    assert_eq!(p.kind, NONE);
    assert_eq!(
        wire::parse_none(&p.body),
        Ok(("req-9".into(), NoneReason::Disabled))
    );
    // Turning it on at runtime takes effect without a restart; turning it off drops queued events.
    phone.ok("transport_clipboard_config", json!({"enabled": true}));
    peer.send(CLIPBOARD, wire::update_body("now on"));
    assert_eq!(phone.events(0, 1)[0]["text"], "now on");
    phone.ok("transport_clipboard_config", json!({"enabled": false}));
    assert!(
        phone.wait_events(0, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn explicit_send_reaches_item_peers_and_stock_peers_differently() {
    let phone = Phone::start(Some(json!({})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    phone.ok(
        "transport_clipboard_send",
        json!({"kind": "text", "text": "tapped send"}),
    );
    let p = peer.recv_packet();
    assert_eq!(p.kind, ITEM);
    assert!(
        p.body.get("request_id").is_none(),
        "an explicit send is not an answer"
    );

    let phone2 = Phone::start(Some(json!({})));
    let mut stock = Peer::new(
        &["kdeconnect.ping", CLIPBOARD, CLIPBOARD_CONNECT],
        &["kdeconnect.ping", CLIPBOARD, CLIPBOARD_CONNECT],
    );
    stock.pair(&phone2);
    phone2.ok(
        "transport_clipboard_send",
        json!({"kind": "text", "text": "to stock"}),
    );
    let p = stock.recv_packet();
    assert_eq!(
        (p.kind.as_str(), p.body["content"].as_str()),
        (CLIPBOARD, Some("to stock"))
    );
    // HTML cannot be sent to a stock peer as such, but its plain text can.
    phone2.ok(
        "transport_clipboard_send",
        json!({"kind": "html", "text": "plain only", "html": "<i>x</i>"}),
    );
    assert_eq!(stock.recv_packet().body["content"], "plain only");
}

#[test]
fn outgoing_items_are_validated_before_they_leave() {
    let phone = Phone::start(Some(json!({})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    let conn = phone.connection_id();
    let big = "x".repeat(70_000);
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "text", "text": big})
        ),
        "too_large"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "text", "text": ""})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "movie", "text": "x"})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "url", "text": "javascript:alert(1)"})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "text", "text": "x", "request_id": "r1"})
        ),
        "invalid_field",
        "an answer needs its connection"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"connection_id": conn, "request_id": "bad id!", "none": "empty"})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"connection_id": conn, "request_id": "r1", "none": "because"})
        ),
        "invalid_field"
    );
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"connection_id": 999, "request_id": "r1", "none": "empty"})
        ),
        "unknown_connection"
    );
    // Nothing invalid reached the desktop.
    phone.ok(
        "transport_clipboard_send",
        json!({"kind": "text", "text": "valid"}),
    );
    assert_eq!(peer.recv_packet().body["text"], "valid");
}

#[cfg(unix)]
#[test]
fn images_stream_both_ways_and_only_genuine_png_or_jpeg_is_used() {
    use std::os::fd::AsRawFd;
    let phone = Phone::start(Some(json!({"max_image_bytes": 1048576})));
    let mut peer = Peer::full();
    peer.pair(&phone);
    let conn = phone.connection_id();

    // Phone -> desktop: a descriptor Kotlin owns; the bytes decide the type.
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("a.png");
    std::fs::write(&img, PNG).unwrap();
    let f = std::fs::File::open(&img).unwrap();
    phone.ok(
        "transport_clipboard_send",
        json!({"connection_id": conn, "request_id": "img-1", "kind": "image", "image_fd": f.as_raw_fd(), "age_ms": 10}),
    );
    let p = peer.recv_packet();
    assert_eq!(
        (p.kind.as_str(), p.body["mime"].as_str()),
        (ITEM, Some("image/png"))
    );
    assert_eq!(p.payload_size, Some(PNG.len() as i64));
    let port = p.payload_transfer_info.as_ref().unwrap()["port"]
        .as_u64()
        .unwrap() as u16;
    let ctx = peer.link.as_ref().unwrap().payload_context();
    let got = peer.rt.block_on(async move {
        let mut out: Vec<u8> = Vec::new();
        let cancel = bridge_kde::CancelToken::new();
        let mut progress = |_: u64| {};
        ctx.receive(
            port,
            Some(PNG.len() as u64),
            1 << 20,
            &mut out,
            &mut progress,
            &cancel,
        )
        .await
        .unwrap();
        out
    });
    assert_eq!(got, PNG);

    // A file that is not an image never leaves, whatever it is called.
    let fake = dir.path().join("fake.png");
    std::fs::write(&fake, b"#!/bin/sh\necho not an image").unwrap();
    let f = std::fs::File::open(&fake).unwrap();
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "image", "image_fd": f.as_raw_fd()})
        ),
        "unsupported_type"
    );
    let big = dir.path().join("big.png");
    let mut bytes = PNG.to_vec();
    bytes.resize(2 << 20, 0);
    std::fs::write(&big, bytes).unwrap();
    let f = std::fs::File::open(&big).unwrap();
    assert_eq!(
        phone.code(
            "transport_clipboard_send",
            json!({"kind": "image", "image_fd": f.as_raw_fd()})
        ),
        "too_large"
    );

    // Desktop -> phone: spooled to the private cache, announced as a path.
    peer.send_image(PNG.to_vec(), None);
    let evs = phone.events(0, 1);
    assert_eq!(evs[0]["item"], "image");
    let path = evs[0]["path"].as_str().unwrap().to_owned();
    assert!(path.starts_with(phone.dir.path().to_str().unwrap()));
    assert_eq!(std::fs::read(&path).unwrap(), PNG);
    let seq = evs[0]["seq"].as_u64().unwrap();

    // Bytes that are not a PNG are discarded without an event, and the spool file is removed.
    peer.send_image(b"GIF89a-not-allowed".to_vec(), None);
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        phone.wait_events(seq, 0)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let leftovers = std::fs::read_dir(phone.dir.path().join("clipboard-cache"))
        .unwrap()
        .count();
    assert_eq!(
        leftovers, 1,
        "only the genuine image remains until Kotlin deletes it"
    );

    // Stopping wipes the spool.
    phone.ok("transport_stop", json!({}));
    assert!(!Path::new(&path).exists());
}

#[test]
fn a_blocked_wait_does_not_block_other_requests_and_ends_on_stop() {
    let phone = Phone::start(Some(json!({})));
    let d = phone.d.clone();
    let waiter = std::thread::spawn(move || {
        let t = Instant::now();
        let v: Value = serde_json::from_str(&d.handle(&request(
            "transport_clipboard_wait",
            json!({"since": 0, "timeout_ms": 30000}),
        )))
        .unwrap();
        (v, t.elapsed())
    });
    std::thread::sleep(Duration::from_millis(200));
    // The dispatcher stays usable while a wait is parked.
    assert_eq!(phone.status()["state"], "running");
    phone.ok("transport_stop", json!({}));
    let (v, took) = waiter.join().unwrap();
    assert!(took < Duration::from_secs(5), "stop must wake the waiter");
    assert_eq!(v["result"]["stopped"], true);
}

#[test]
fn clipboard_options_are_strictly_validated() {
    let d = Dispatcher::new();
    let dir = tempfile::tempdir().unwrap();
    let bad = d.handle(&request(
        "transport_start",
        json!({
            "state_dir": dir.path().to_str().unwrap(), "device_name": "P",
            "listen": {"enabled": false},
            "clipboard": {"enabled": true, "surprise": 1},
        }),
    ));
    assert!(bad.contains("unknown_field"), "{bad}");
}
