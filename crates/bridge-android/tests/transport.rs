// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests of the transport actor through the public request interface.
//!
//! The "desktop" is a `bridge_kde::Endpoint` with generated credentials and its own runtime, so
//! these tests exercise real TCP + TLS + pairing over loopback; loopback is allowed only because
//! each test opts in explicitly (it is off by default). Nothing here talks to a real KDE Connect
//! device or an Android runtime: interoperability is not established by these tests.
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceCredentials, DeviceId, DeviceType, Endpoint,
    Handshake, KdeError, LinkEvent, MemoryTrustStore, Packet, PairMode, PendingLink,
    TransportConfig, TrustStore, TrustedLink,
};
use geekbridge_android::Dispatcher;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio::time::timeout;

// ------------------------------------------------------------------------------- phone side

fn parse(out: &str) -> Value {
    let v: Value = serde_json::from_str(out).expect("response is JSON");
    assert_eq!(v["version"], 1, "{out}");
    v
}

fn ok(v: &Value) -> &Value {
    assert_eq!(v["ok"], true, "{v}");
    &v["result"]
}

fn code(v: &Value) -> &str {
    assert_eq!(v["ok"], false, "{v}");
    v["error"]["code"].as_str().expect("error code")
}

fn request(action: &str, fields: Value) -> Value {
    let mut o = Map::new();
    o.insert("version".into(), json!(1));
    o.insert("action".into(), json!(action));
    if let Value::Object(f) = fields {
        o.extend(f);
    }
    Value::Object(o)
}

/// A loopback address of its own for each phone. Linux hands a just-freed ephemeral port straight
/// back to the next `bind(0)`, so "the port is closed after stop" probes would otherwise race with
/// the other tests' listeners on 127.0.0.1. Other platforms only route 127.0.0.1 by default.
fn loopback_host() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    if cfg!(target_os = "linux") {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let b = std::process::id() % 250 + 2;
        format!("127.{b}.{}.{}", (n >> 8) % 250 + 1, n % 250 + 1)
    } else {
        "127.0.0.1".to_owned()
    }
}

fn start_request(dir: &Path, extra: Value) -> Value {
    let mut r = request(
        "transport_start",
        json!({
            "state_dir": dir.to_str().unwrap(),
            "device_name": "Test Phone",
            "listen": { "host": loopback_host(), "port": 0 },
            "allow_loopback": true,
        }),
    );
    if let Value::Object(e) = extra {
        r.as_object_mut().unwrap().extend(e);
    }
    r
}

struct Phone {
    d: Dispatcher,
    dir: TempDir,
    id: String,
    cert: String,
    addr: SocketAddr,
}

impl Phone {
    fn start() -> Phone {
        Self::start_with(json!({}))
    }

    fn start_with(extra: Value) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let d = Dispatcher::new();
        let v = parse(&d.handle(&start_request(dir.path(), extra).to_string()));
        let r = ok(&v);
        let addr = format!(
            "{}:{}",
            r["listen"]["host"].as_str().unwrap(),
            r["listen"]["port"]
        )
        .parse()
        .unwrap();
        Phone {
            id: r["device_id"].as_str().unwrap().to_owned(),
            cert: r["cert_sha256"].as_str().unwrap().to_owned(),
            d,
            dir,
            addr,
        }
    }

    fn raw(&self, v: &Value) -> Value {
        parse(&self.d.handle(&v.to_string()))
    }

    fn act(&self, action: &str, fields: Value) -> Value {
        self.raw(&request(action, fields))
    }

    fn status(&self) -> Value {
        let v = self.act("transport_status", json!({}));
        ok(&v).clone()
    }

    fn wait(&self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let s = self.status();
            if f(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn array<'a>(s: &'a Value, key: &str) -> &'a Vec<Value> {
        s[key].as_array().unwrap()
    }

    fn decision(&self, prompt: &Value, approve: bool) -> Value {
        self.act(
            "transport_pair_decision",
            json!({
                "request_id": prompt["request_id"],
                "connection_id": prompt["connection_id"],
                "approve": approve,
                "verification_code": prompt["verification_code"],
                "peer_cert_sha256": prompt["peer_cert_sha256"],
            }),
        )
    }

    fn stop(&self) -> Value {
        let v = self.act("transport_stop", json!({}));
        ok(&v).clone()
    }

    fn first_trusted(s: &Value) -> bool {
        Self::array(s, "connections")
            .first()
            .is_some_and(|c| c["trusted"] == true)
    }

    fn events<'a>(s: &'a Value, kind: &str) -> Vec<&'a Value> {
        Self::array(s, "events")
            .iter()
            .filter(|e| e["kind"] == kind)
            .collect()
    }
}

// ------------------------------------------------------------------------------- desktop peer

type Seen = Arc<Mutex<Option<ApprovalRequest>>>;

struct Auto {
    answer: bool,
    seen: Seen,
}

impl Approver for Auto {
    async fn approve(&mut self, request: ApprovalRequest) -> bool {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
        self.answer
    }
}

struct Peer {
    rt: Runtime,
    ep: Arc<Endpoint>,
    id: DeviceId,
    cert: String,
}

fn peer_with(c: char, creds: DeviceCredentials, store: Arc<dyn TrustStore>) -> Peer {
    peer_with_caps(c, creds, store, &[])
}

/// A desktop that also sends the GeekBridge-only `outgoing_extra` packet types.
fn peer_with_caps(
    c: char,
    creds: DeviceCredentials,
    store: Arc<dyn TrustStore>,
    outgoing_extra: &[&str],
) -> Peer {
    let id = creds.device_id().clone();
    let cert = creds.cert().fingerprint_hex();
    let config = TransportConfig {
        address_policy: AddressPolicy {
            local_networks: true,
            loopback: true,
        },
        payload: bridge_kde::PayloadConfig {
            any_port: true,
            accept_timeout: Duration::from_secs(10),
            ..bridge_kde::PayloadConfig::default()
        },
        ..TransportConfig::default()
    };
    let caps = vec![
        "kdeconnect.ping".to_owned(),
        "kdeconnect.share.request".to_owned(),
    ];
    let mut outgoing = caps.clone();
    outgoing.extend(outgoing_extra.iter().map(|s| (*s).to_owned()));
    let ep = Endpoint::new(
        Arc::new(creds),
        &format!("Desk {c}"),
        DeviceType::Desktop,
        caps.clone(),
        outgoing,
        store,
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
        id,
        cert,
    }
}

impl Peer {
    fn new(c: char) -> Peer {
        let id = DeviceId::parse(&c.to_string().repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        peer_with(c, creds, Arc::new(MemoryTrustStore::new()))
    }

    /// A computer that answers shares (declares `geekbridge.share.result`).
    #[cfg(unix)]
    fn answering(c: char) -> Peer {
        let id = DeviceId::parse(&c.to_string().repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        peer_with_caps(
            c,
            creds,
            Arc::new(MemoryTrustStore::new()),
            &["geekbridge.share.result"],
        )
    }

    fn dial(&self, addr: SocketAddr, target: &str) -> Result<Handshake, KdeError> {
        let target = DeviceId::parse(target).unwrap();
        self.rt.block_on(async {
            timeout(Duration::from_secs(15), self.ep.connect(addr, &target))
                .await
                .expect("dial timed out")
        })
    }

    fn listen(&self) -> (TcpListener, SocketAddr) {
        let l = self.rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let a = l.local_addr().unwrap();
        (l, a)
    }

    /// Accept one connection and finish the handshake in the background.
    fn accept_one(&self, listener: TcpListener) -> JoinHandle<Result<Handshake, KdeError>> {
        let ep = self.ep.clone();
        self.rt.spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            ep.accept(stream).await
        })
    }

    fn spawn_pair(
        &self,
        link: PendingLink,
        mode: PairMode,
        answer: bool,
    ) -> (JoinHandle<Result<TrustedLink, KdeError>>, Seen) {
        let seen: Seen = Arc::default();
        let mut approver = Auto {
            answer,
            seen: seen.clone(),
        };
        let join = self
            .rt
            .spawn(async move { link.pair(mode, &mut approver).await });
        (join, seen)
    }

    fn finish<T: Send + 'static>(&self, join: JoinHandle<T>) -> T {
        self.rt
            .block_on(async { timeout(Duration::from_secs(15), join).await })
            .expect("peer task timed out")
            .expect("peer task panicked")
    }

    fn recv(&self, link: &mut TrustedLink) -> Result<LinkEvent, KdeError> {
        self.rt.block_on(async {
            timeout(Duration::from_secs(10), link.recv())
                .await
                .expect("peer recv timed out")
        })
    }

    fn send(&self, link: &mut TrustedLink, kind: &str, body: Map<String, Value>) {
        self.try_send(link, kind, body).unwrap();
    }

    fn try_send(
        &self,
        link: &mut TrustedLink,
        kind: &str,
        body: Map<String, Value>,
    ) -> Result<(), KdeError> {
        self.rt.block_on(link.send(&Packet::new(kind, body)))
    }
}

/// The peer's approver runs on its own runtime, possibly just after the phone shows its prompt.
fn wait_seen(seen: &Seen) -> ApprovalRequest {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(r) = seen.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return r;
        }
        assert!(Instant::now() < end, "the peer was never asked");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pending(hs: Handshake) -> PendingLink {
    match hs {
        Handshake::Pending(l) => l,
        Handshake::Trusted(_) => panic!("expected an untrusted peer"),
    }
}

fn trusted(hs: Handshake) -> TrustedLink {
    match hs {
        Handshake::Trusted(l) => l,
        Handshake::Pending(_) => panic!("expected a trusted peer"),
    }
}

/// The peer asks the phone to pair; the phone's user approves with the displayed values.
fn pair_by_peer(phone: &Phone, peer: &Peer) -> TrustedLink {
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _seen) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("a pairing prompt", |s| {
        !Phone::array(s, "prompts").is_empty()
    });
    let prompt = &Phone::array(&s, "prompts")[0];
    ok(&phone.decision(prompt, true));
    let link = peer.finish(join).expect("pairing completes");
    phone.wait("trusted connection", Phone::first_trusted);
    link
}

// ------------------------------------------------------------------------------- lifecycle

#[test]
fn the_screen_state_needs_no_running_transport_and_rejects_bad_input() {
    let d = Dispatcher::new();
    for on in [false, true] {
        let v = parse(&d.handle(&request("transport_screen", json!({ "on": on })).to_string()));
        assert_eq!(ok(&v)["screen_on"], on);
    }
    let v = parse(&d.handle(&request("transport_screen", json!({ "on": "no" })).to_string()));
    assert_eq!(v["ok"], false);
    let v = parse(&d.handle(&request("transport_screen", json!({})).to_string()));
    assert_eq!(v["ok"], false);
}

#[test]
fn heartbeat_now_needs_no_running_transport_and_takes_no_arguments() {
    let d = Dispatcher::new();
    let v = parse(&d.handle(&request("transport_heartbeat_now", json!({})).to_string()));
    assert_eq!(ok(&v)["nudged"], true);
    let v = parse(&d.handle(&request("transport_heartbeat_now", json!({ "x": 1 })).to_string()));
    assert_eq!(v["ok"], false);
}

#[test]
fn wifi_bind_takes_a_unicast_ipv4_address_or_nothing() {
    let d = Dispatcher::new();
    let v = parse(
        &d.handle(&request("transport_wifi_bind", json!({ "ip": "192.168.1.20" })).to_string()),
    );
    assert_eq!(ok(&v)["bound"], true);
    for bad in [
        json!("127.0.0.1"),
        json!("0.0.0.0"),
        json!("fe80::1"),
        json!("x"),
        json!(5),
        json!(null),
    ] {
        let v = parse(&d.handle(&request("transport_wifi_bind", json!({ "ip": bad })).to_string()));
        assert_eq!(v["ok"], false, "{bad}");
    }
    let v = parse(&d.handle(&request("transport_wifi_bind", json!({})).to_string()));
    assert_eq!(ok(&v)["bound"], false, "no address clears it");
    let v = parse(&d.handle(&request("transport_wifi_bind", json!({ "x": 1 })).to_string()));
    assert_eq!(v["ok"], false);
    // The kill switch: a bool, nothing else.
    let v = parse(
        &d.handle(&request("transport_wifi_bind_enabled", json!({"enabled": true})).to_string()),
    );
    assert_eq!(ok(&v)["enabled"], true);
    for bad in [
        json!({}),
        json!({"enabled": "no"}),
        json!({"enabled": true, "x": 1}),
    ] {
        let v = parse(&d.handle(&request("transport_wifi_bind_enabled", bad).to_string()));
        assert_eq!(v["ok"], false);
    }
}

#[test]
fn nothing_starts_before_transport_start() {
    let d = Dispatcher::new();
    let dir = tempfile::tempdir().unwrap();
    let v = parse(&d.handle(r#"{"version":1,"action":"version"}"#));
    assert_eq!(ok(&v)["validation_only"], true);
    assert!(ok(&v)["transport_actions"].as_array().unwrap().len() >= 8);
    ok(&parse(&d.handle(
        r#"{"version":1,"action":"validate_sms","recipient":"+1","text":"hi"}"#,
    )));
    ok(&parse(&d.handle(
        r#"{"version":1,"action":"validate_clipboard","text":"x"}"#,
    )));
    let s = parse(&d.handle(r#"{"version":1,"action":"transport_status"}"#));
    assert_eq!(ok(&s), &json!({ "state": "stopped" }));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    // Commands that need a running transport say so rather than starting one.
    for (action, fields) in [
        (
            "transport_connect",
            json!({"host": "10.0.0.2", "port": 1716, "device_id": "a".repeat(32)}),
        ),
        ("transport_disconnect", json!({"connection_id": 1})),
        ("transport_pair_request", json!({"connection_id": 1})),
        (
            "transport_pair_decision",
            json!({"request_id": 1, "connection_id": 1, "approve": false}),
        ),
        ("transport_unpair", json!({"device_id": "a".repeat(32)})),
        ("transport_ping", json!({"connection_id": 1})),
    ] {
        let v = parse(&d.handle(&request(action, fields).to_string()));
        assert_eq!(code(&v), "not_running", "{action}");
    }
    let stopped = parse(&d.handle(r#"{"version":1,"action":"transport_stop"}"#));
    assert_eq!(ok(&stopped)["was_running"], false);
}

#[test]
fn start_reports_identity_listener_and_only_ping() {
    let phone = Phone::start();
    let s = phone.status();
    assert_eq!(s["state"], "running");
    assert_eq!(s["local"]["device_id"], phone.id.as_str());
    assert_eq!(s["local"]["device_name"], "Test Phone");
    assert_eq!(s["listen"]["port"], phone.addr.port());
    assert!(Phone::array(&s, "connections").is_empty());
    assert!(Phone::array(&s, "prompts").is_empty());
    // Discovery is off unless asked for.
    assert_eq!(s["discovery"]["listening"], false);
    assert_eq!(s["discovery"]["announcing"], false);

    let v = phone.raw(&start_request(phone.dir.path(), json!({})));
    assert_eq!(code(&v), "already_running");

    // Advertised capabilities are exactly what is implemented.
    let peer = Peer::new('p');
    let (listener, addr) = peer.listen();
    let accepted = peer.accept_one(listener);
    let phone2 = Phone::start();
    let v = phone2.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": peer.id.as_str()}),
    );
    ok(&v);
    let hs = peer.finish(accepted).unwrap();
    // The peer saw the phone's identity during the handshake.
    assert_eq!(hs.info().peer.device_type, DeviceType::Phone);
    let caps = [
        "kdeconnect.ping",
        "kdeconnect.share.request",
        "geekbridge.heartbeat",
    ];
    // The phone also accepts the computer's `geekbridge.goodbye`; it never sends one.
    let mut incoming = caps.to_vec();
    incoming.push("geekbridge.goodbye");
    incoming.push("geekbridge.share.result");
    assert_eq!(hs.info().peer.incoming_capabilities, incoming);
    assert_eq!(hs.info().peer.outgoing_capabilities, caps);
}

#[cfg(unix)]
#[test]
fn state_directory_is_private_and_responses_do_not_leak_it() {
    use std::os::unix::fs::PermissionsExt;
    let phone = Phone::start();
    let kde = phone.dir.path().join("kde");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&kde), 0o700);
    assert_eq!(mode(&kde.join("identity.pem")), 0o600);
    let dir = phone.dir.path().to_str().unwrap().to_owned();
    for out in [
        phone.status().to_string(),
        phone
            .raw(&start_request(phone.dir.path(), json!({})))
            .to_string(),
    ] {
        assert!(!out.contains(&dir), "{out}");
        assert!(!out.contains("PRIVATE KEY"), "{out}");
    }
}

#[cfg(unix)]
#[test]
fn a_loose_state_directory_is_refused_and_the_host_stays_usable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let kde = dir.path().join("kde");
    std::fs::create_dir(&kde).unwrap();
    std::fs::set_permissions(&kde, std::fs::Permissions::from_mode(0o755)).unwrap();
    let d = Dispatcher::new();
    let v = parse(&d.handle(&start_request(dir.path(), json!({})).to_string()));
    assert_eq!(code(&v), "insecure_state");
    assert_eq!(
        ok(&parse(
            &d.handle(r#"{"version":1,"action":"transport_status"}"#)
        ))["state"],
        "stopped"
    );
    std::fs::set_permissions(&kde, std::fs::Permissions::from_mode(0o700)).unwrap();
    ok(&parse(
        &d.handle(&start_request(dir.path(), json!({})).to_string()),
    ));
}

#[test]
fn identity_is_stable_across_restart_and_the_port_is_released() {
    let phone = Phone::start();
    let port = phone.addr.port();
    let stopped = phone.stop();
    assert_eq!(stopped["was_running"], true);
    // The listener is gone as soon as stop returns.
    assert!(std::net::TcpStream::connect(phone.addr).is_err());
    let v = phone.raw(&start_request(
        phone.dir.path(),
        json!({"listen": {"host": phone.addr.ip().to_string(), "port": port}}),
    ));
    let r = ok(&v);
    assert_eq!(r["device_id"], phone.id.as_str());
    assert_eq!(r["cert_sha256"], phone.cert.as_str());
    assert_eq!(r["identity_created"], false);
    assert_eq!(r["listen"]["port"], port);
    // A second stop is a no-op.
    phone.stop();
    assert_eq!(phone.stop()["was_running"], false);
}

#[test]
fn dropping_the_dispatcher_tears_the_transport_down() {
    let phone = Phone::start();
    let addr = phone.addr;
    let Phone { d, dir, .. } = phone;
    drop(d);
    assert!(std::net::TcpStream::connect(addr).is_err());
    drop(dir);
}

#[test]
fn start_and_stop_race_with_status_without_wedging() {
    let dir = tempfile::tempdir().unwrap();
    let d = Arc::new(Dispatcher::new());
    let reader = {
        let d = d.clone();
        std::thread::spawn(move || {
            for _ in 0..300 {
                let v = parse(&d.handle(r#"{"version":1,"action":"transport_status"}"#));
                let state = ok(&v)["state"].as_str().unwrap().to_owned();
                assert!(["stopped", "starting", "running", "stopping"].contains(&state.as_str()));
            }
        })
    };
    for _ in 0..6 {
        let v = parse(&d.handle(&start_request(dir.path(), json!({})).to_string()));
        ok(&v);
        ok(&parse(
            &d.handle(r#"{"version":1,"action":"transport_stop"}"#),
        ));
    }
    reader.join().unwrap();
}

// ------------------------------------------------------------------------------- request strictness

#[test]
fn transport_requests_are_strict() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dispatcher::new();
    let call = |v: Value| parse(&d.handle(&v.to_string()));
    let raw = |s: &str| parse(&d.handle(s));
    let base = |extra: Value| start_request(dir.path(), extra);

    // Unknown, duplicate and wrongly typed fields, at the top level and nested.
    assert_eq!(code(&call(base(json!({"surprise": 1})))), "unknown_field");
    assert_eq!(
        code(&call(base(json!({"listen": {"surprise": 1}})))),
        "unknown_field"
    );
    assert_eq!(
        code(&call(base(json!({"discovery": {"surprise": 1}})))),
        "unknown_field"
    );
    assert_eq!(code(&call(base(json!({"listen": 5})))), "invalid_field");
    assert_eq!(code(&call(base(json!({"listen": null})))), "invalid_field");
    assert_eq!(
        code(&call(base(json!({"allow_loopback": "yes"})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(json!({"listen": {"port": 70000}})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(json!({"listen": {"port": -1}})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(json!({"listen": {"port": 1.5}})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(json!({"listen": {"host": "localhost"}})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(
            json!({"listen": {"enabled": false, "port": 1716}})
        ))),
        "invalid_field"
    );
    let nested_dup = format!(
        r#"{{"version":1,"action":"transport_start","state_dir":{},"device_name":"x","listen":{{"port":1,"port":2}}}}"#,
        json!(dir.path().to_str().unwrap())
    );
    assert_eq!(code(&raw(&nested_dup)), "invalid_request");
    assert_eq!(
        code(&raw(
            r#"{"version":1,"action":"transport_stop","action":"transport_stop"}"#
        )),
        "invalid_request"
    );
    assert_eq!(
        code(&call(request(
            "transport_start",
            json!({"device_name": "x"})
        ))),
        "missing_field"
    );
    // Version and action strictness still apply.
    assert_eq!(
        code(&raw(r#"{"version":2,"action":"transport_status"}"#)),
        "unsupported_version"
    );
    assert_eq!(
        code(&call(request("transport_status", json!({"x": 1})))),
        "unknown_field"
    );
    assert_eq!(
        code(&call(request("transport_status", json!({"since": -1})))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(request("transport_stop", json!({"x": 1})))),
        "unknown_field"
    );
    assert_eq!(
        code(&call(request("transport_start ", json!({})))),
        "unknown_action"
    );

    // State directory and name checks.
    for bad in [
        "",
        "relative/dir",
        "/definitely/not/there",
        "/tmp/../tmp",
        "/tmp\0",
    ] {
        let mut r = base(json!({}));
        r["state_dir"] = json!(bad);
        assert_eq!(code(&call(r)), "invalid_state_dir", "{bad:?}");
    }
    let mut r = base(json!({}));
    r["state_dir"] = json!(format!("/{}", "a".repeat(1100)));
    assert_eq!(code(&call(r)), "invalid_state_dir");
    for bad in [json!(""), json!("..."), json!("x".repeat(129))] {
        let mut r = base(json!({}));
        r["device_name"] = bad;
        assert_eq!(code(&call(r)), "invalid_device_name");
    }

    // Policy checks.
    let mut r = base(json!({}));
    r["allow_loopback"] = json!(false);
    assert_eq!(code(&call(r)), "loopback_not_allowed");
    assert_eq!(
        code(&call(base(
            json!({"listen": {"enabled": false}, "discovery": {"announce": true}})
        ))),
        "discovery_requires_listener"
    );
    assert_eq!(
        code(&call(base(
            json!({"discovery": {"announce_to": "127.0.0.1:1716"}})
        ))),
        "invalid_field"
    );
    assert_eq!(
        code(&call(base(
            json!({"discovery": {"announce": true, "announce_to": "8.8.8.8:1716"}})
        ))),
        "address_rejected"
    );
    // Every failure above left the host stopped and the directory untouched.
    assert_eq!(
        ok(&call(request("transport_status", json!({}))))["state"],
        "stopped"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    // Size: a transport request over the transport bound is refused unparsed-by-action.
    let big = base(json!({"device_name": "x".repeat(20_000)}));
    assert_eq!(code(&call(big)), "request_too_large");
}

#[test]
fn connect_arguments_and_address_policy() {
    // Loopback is off by default: starting on a LAN-style wildcard bind, dialling loopback fails.
    let dir = tempfile::tempdir().unwrap();
    let d = Dispatcher::new();
    let mut r = start_request(dir.path(), json!({}));
    r["listen"] = json!({"host": "0.0.0.0", "port": 0});
    r["allow_loopback"] = json!(false);
    ok(&parse(&d.handle(&r.to_string())));
    let call = |v: Value| parse(&d.handle(&v.to_string()));
    let id = "b".repeat(32);
    let connect = |host: &str, port: u64, device: &str| {
        call(request(
            "transport_connect",
            json!({"host": host, "port": port, "device_id": device}),
        ))
    };
    assert_eq!(code(&connect("127.0.0.1", 1716, &id)), "address_rejected");
    assert_eq!(code(&connect("8.8.8.8", 1716, &id)), "address_rejected");
    assert_eq!(code(&connect("0.0.0.0", 1716, &id)), "address_rejected");
    assert_eq!(code(&connect("localhost", 1716, &id)), "invalid_endpoint");
    assert_eq!(code(&connect("example.com", 1716, &id)), "invalid_endpoint");
    assert_eq!(code(&connect("192.168.1.2", 0, &id)), "invalid_endpoint");
    assert_eq!(
        code(&connect("192.168.1.2", 70000, &id)),
        "invalid_endpoint"
    );
    assert_eq!(
        code(&connect("192.168.1.2", 1716, "short")),
        "invalid_device_id"
    );
    // Nothing was attempted.
    let s = ok(&call(request("transport_status", json!({})))).clone();
    assert!(Phone::array(&s, "attempts").is_empty());
}

// ------------------------------------------------------------------------------- inbound pairing

#[test]
fn peer_requests_pairing_and_the_local_user_approves() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, seen) = peer.spawn_pair(link, PairMode::Request, true);

    let s = phone.wait("a pairing prompt", |s| {
        !Phone::array(s, "prompts").is_empty()
    });
    let prompt = &Phone::array(&s, "prompts")[0];
    let conn = &Phone::array(&s, "connections")[0];
    // The prompt names exactly this connection, certificate and verification code.
    let shown = wait_seen(&seen);
    assert_eq!(prompt["direction"], "incoming");
    assert_eq!(prompt["verification_code"], shown.verification_key.as_str());
    assert_eq!(prompt["peer_device_id"], peer.id.as_str());
    assert_eq!(prompt["peer_cert_sha256"], peer.cert.as_str());
    assert_eq!(prompt["own_cert_sha256"], phone.cert.as_str());
    assert_eq!(prompt["connection_id"], conn["connection_id"]);
    assert!(prompt["expires_in_ms"].as_u64().unwrap() > 0);
    assert_eq!(conn["phase"], "pairing_await");
    assert_eq!(conn["trusted"], false);
    assert_eq!(conn["role"], "acceptor");
    assert!(Phone::array(&s, "paired").is_empty());

    // Nothing is possible before trust, and nothing pairs without a decision.
    let id = conn["connection_id"].clone();
    assert_eq!(
        code(&phone.act("transport_ping", json!({"connection_id": id}))),
        "not_trusted"
    );
    assert_eq!(
        code(&phone.act("transport_pair_request", json!({"connection_id": id}))),
        "wrong_state"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !join.is_finished(),
        "pairing must wait for the local decision"
    );
    assert!(Phone::array(&phone.status(), "paired").is_empty());

    let r = phone.decision(prompt, true);
    assert_eq!(ok(&r)["decision"], "approved");
    let mut link = peer.finish(join).expect("pairing completes");
    let s = phone.wait("trusted connection", Phone::first_trusted);
    assert_eq!(Phone::array(&s, "paired")[0]["device_id"], peer.id.as_str());
    assert!(Phone::array(&s, "prompts").is_empty());
    assert_eq!(Phone::events(&s, "paired").len(), 1);
    // A decision cannot be replayed.
    assert_eq!(code(&phone.decision(prompt, true)), "unknown_request");

    // Trusted channel works both ways, with ping only.
    let id = Phone::array(&s, "connections")[0]["connection_id"].clone();
    ok(&phone.act("transport_ping", json!({"connection_id": id})));
    match peer.recv(&mut link).unwrap() {
        LinkEvent::Packet(p) => assert_eq!(p.kind, "kdeconnect.ping"),
        other => panic!("{other:?}"),
    }
    let mut body = Map::new();
    body.insert("message".into(), json!("x".repeat(10_000)));
    peer.send(&mut link, "kdeconnect.ping", body);
    peer.send(&mut link, "kdeconnect.battery", Map::new());
    let s = phone.wait("ping counted", |s| {
        Phone::array(s, "connections")[0]["pings_received"] == 1
            && Phone::array(s, "connections")[0]["ignored_packets"] == 1
    });
    assert_eq!(Phone::array(&s, "connections").len(), 1);
}

#[test]
fn local_refusal_pairs_nothing() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("prompt", |s| !Phone::array(s, "prompts").is_empty());
    // Rejecting needs only the request, and never the code.
    let p = &Phone::array(&s, "prompts")[0];
    let r = phone.act(
        "transport_pair_decision",
        json!({"request_id": p["request_id"], "connection_id": p["connection_id"], "approve": false}),
    );
    assert_eq!(ok(&r)["decision"], "rejected");
    assert!(peer.finish(join).is_err());
    let s = phone.wait("connection removed", |s| {
        Phone::array(s, "connections").is_empty()
    });
    assert!(Phone::array(&s, "paired").is_empty());
    assert!(Phone::array(&s, "prompts").is_empty());
    let failed = Phone::events(&s, "pairing_failed");
    assert_eq!(failed[0]["code"], "pairing_rejected_locally");
    // No trust record was written: a fresh dial is untrusted again.
    assert!(matches!(
        peer.dial(phone.addr, &phone.id).unwrap(),
        Handshake::Pending(_)
    ));
}

#[test]
fn approval_needs_the_exact_code_and_certificate() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("prompt", |s| !Phone::array(s, "prompts").is_empty());
    let p = Phone::array(&s, "prompts")[0].clone();
    let with = |code: Value, cert: Value| {
        phone.act(
            "transport_pair_decision",
            json!({"request_id": p["request_id"], "connection_id": p["connection_id"], "approve": true,
                   "verification_code": code, "peer_cert_sha256": cert}),
        )
    };
    // Approving without naming what was shown is a schema error and changes nothing.
    let bare = phone.act(
        "transport_pair_decision",
        json!({"request_id": p["request_id"], "connection_id": p["connection_id"], "approve": true}),
    );
    assert_eq!(code(&bare), "missing_field");
    // Wrong request or connection id.
    let other = |rid: u64, cid: u64| {
        phone.act(
            "transport_pair_decision",
            json!({"request_id": rid, "connection_id": cid, "approve": true,
                   "verification_code": p["verification_code"], "peer_cert_sha256": p["peer_cert_sha256"]}),
        )
    };
    let (rid, cid) = (
        p["request_id"].as_u64().unwrap(),
        p["connection_id"].as_u64().unwrap(),
    );
    assert_eq!(code(&other(rid + 1, cid)), "unknown_request");
    assert_eq!(code(&other(rid, cid + 1)), "unknown_request");
    // Wrong code, then wrong certificate, then a third wrong try rejects the request.
    assert_eq!(
        code(&with(json!("00000000"), p["peer_cert_sha256"].clone())),
        "verification_mismatch"
    );
    assert_eq!(
        code(&with(
            p["verification_code"].clone(),
            json!("ab".repeat(32))
        )),
        "certificate_mismatch"
    );
    assert_eq!(
        Phone::array(&phone.status(), "prompts").len(),
        1,
        "still waiting"
    );
    assert_eq!(
        code(&with(json!("00000000"), json!("ab".repeat(32)))),
        "too_many_attempts"
    );
    assert!(Phone::array(&phone.status(), "prompts").is_empty());
    assert!(peer.finish(join).is_err());
    let s = phone.wait("connection removed", |s| {
        Phone::array(s, "connections").is_empty()
    });
    assert!(Phone::array(&s, "paired").is_empty());
}

#[test]
fn code_is_accepted_case_insensitively_and_trimmed() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("prompt", |s| !Phone::array(s, "prompts").is_empty());
    let p = &Phone::array(&s, "prompts")[0];
    let typed = format!(
        " {} ",
        p["verification_code"].as_str().unwrap().to_lowercase()
    );
    let r = phone.act(
        "transport_pair_decision",
        json!({"request_id": p["request_id"], "connection_id": p["connection_id"], "approve": true,
               "verification_code": typed,
               "peer_cert_sha256": p["peer_cert_sha256"].as_str().unwrap().to_uppercase()}),
    );
    ok(&r);
    peer.finish(join).unwrap();
}

#[test]
fn the_peer_refusing_ends_the_pairing_cleanly() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, false);
    // The peer's own user declined, so it cancels; the phone's prompt must disappear.
    assert!(peer.finish(join).is_err());
    let s = phone.wait("pairing failure", |s| {
        !Phone::events(s, "pairing_failed").is_empty()
    });
    assert_eq!(
        Phone::events(&s, "pairing_failed")[0]["code"],
        "pairing_rejected_by_peer"
    );
    assert!(Phone::array(&s, "prompts").is_empty());
    assert!(Phone::array(&s, "connections").is_empty());
    assert!(Phone::array(&s, "paired").is_empty());
}

// ------------------------------------------------------------------------------- outbound pairing

#[test]
fn phone_dials_parks_then_requests_pairing() {
    let phone = Phone::start();
    let peer = Peer::new('q');
    let (listener, addr) = peer.listen();
    let accepted = peer.accept_one(listener);
    let v = phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": peer.id.as_str()}),
    );
    assert_eq!(ok(&v)["accepted"], true);
    // A second dial to the same device while the first exists is refused.
    let again = phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": peer.id.as_str()}),
    );
    assert_eq!(code(&again), "already_connected");

    let s = phone.wait("a parked connection", |s| {
        !Phone::array(s, "connections").is_empty()
    });
    let conn = &Phone::array(&s, "connections")[0];
    assert_eq!(conn["phase"], "parked");
    assert_eq!(conn["role"], "initiator");
    assert_eq!(conn["trusted"], false);
    assert_eq!(conn["peer"]["device_id"], peer.id.as_str());
    assert_eq!(conn["peer"]["cert_sha256"], peer.cert.as_str());
    assert!(Phone::array(&s, "attempts").is_empty());
    assert!(Phone::array(&s, "prompts").is_empty());
    let cid = conn["connection_id"].clone();
    assert_eq!(
        code(&phone.act("transport_ping", json!({"connection_id": cid}))),
        "not_trusted"
    );
    assert_eq!(
        code(&phone.act(
            "transport_pair_request",
            json!({"connection_id": cid, "mode": "later"})
        )),
        "invalid_field"
    );
    assert_eq!(
        code(&phone.act("transport_pair_request", json!({"connection_id": 999}))),
        "unknown_connection"
    );

    let link = pending(peer.finish(accepted).unwrap());
    let (join, seen) = peer.spawn_pair(link, PairMode::AwaitPeerRequest, true);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        seen.lock().unwrap().is_none(),
        "a parked phone sends nothing"
    );

    let r = phone.act("transport_pair_request", json!({"connection_id": cid}));
    assert_eq!(ok(&r)["phase"], "pairing_request");
    assert_eq!(
        code(&phone.act("transport_pair_request", json!({"connection_id": cid}))),
        "wrong_state"
    );
    let s = phone.wait("our own prompt", |s| !Phone::array(s, "prompts").is_empty());
    let prompt = &Phone::array(&s, "prompts")[0];
    assert_eq!(prompt["direction"], "outgoing");
    let shown = wait_seen(&seen);
    assert_eq!(prompt["verification_code"], shown.verification_key.as_str());
    ok(&phone.decision(prompt, true));
    let _link = peer.finish(join).expect("pairing completes");
    let s = phone.wait("trusted", Phone::first_trusted);
    assert_eq!(Phone::array(&s, "paired")[0]["device_id"], peer.id.as_str());
}

#[test]
fn a_parked_connection_can_be_closed_and_unpair_forgets_the_peer() {
    let phone = Phone::start();
    let peer = Peer::new('q');
    let (listener, addr) = peer.listen();
    let accepted = peer.accept_one(listener);
    ok(&phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": peer.id.as_str()}),
    ));
    let s = phone.wait("parked", |s| !Phone::array(s, "connections").is_empty());
    let cid = Phone::array(&s, "connections")[0]["connection_id"].clone();
    let _link = peer.finish(accepted).unwrap();
    ok(&phone.act("transport_disconnect", json!({"connection_id": cid})));
    assert!(Phone::array(&phone.status(), "connections").is_empty());
    assert_eq!(
        code(&phone.act("transport_disconnect", json!({"connection_id": cid}))),
        "unknown_connection"
    );
    // Unpairing a device that was never paired is not an error and reports it.
    let r = phone.act("transport_unpair", json!({"device_id": peer.id.as_str()}));
    assert_eq!(ok(&r)["was_paired"], false);
    assert_eq!(
        code(&phone.act("transport_unpair", json!({"device_id": "nope"}))),
        "invalid_device_id"
    );
}

// ------------------------------------------------------------------------------- trust lifecycle

#[test]
fn paired_peer_reconnects_without_a_prompt_and_unpair_works_both_ways() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let mut link = pair_by_peer(&phone, &peer);
    let first = Phone::array(&phone.status(), "connections")[0]["connection_id"].clone();

    // The same side dialling again replaces the old connection rather than adding a second.
    let mut link2 = trusted(peer.dial(phone.addr, &phone.id).unwrap());
    let s = phone.wait("replacement", |s| {
        let c = Phone::array(s, "connections");
        c.len() == 1 && c[0]["connection_id"] != first
    });
    assert!(
        Phone::array(&s, "prompts").is_empty(),
        "a pinned peer needs no pairing"
    );
    assert_eq!(Phone::array(&s, "connections")[0]["trusted"], true);
    assert!(
        peer.recv(&mut link).is_err(),
        "the superseded link is closed"
    );

    // Local unpair: record removed, the peer is told, the connection ends.
    let r = phone.act("transport_unpair", json!({"device_id": peer.id.as_str()}));
    assert_eq!(ok(&r)["was_paired"], true);
    assert_eq!(ok(&r)["disconnected"], true);
    assert!(matches!(
        peer.recv(&mut link2).unwrap(),
        LinkEvent::PeerUnpaired
    ));
    let s = phone.status();
    assert!(Phone::array(&s, "paired").is_empty());
    assert!(Phone::array(&s, "connections").is_empty());
    assert_eq!(Phone::events(&s, "unpaired").len(), 1);
}

/// The computer forgot the phone without telling it (it unpaired while no link was up), so the phone
/// still trusts it. The computer's next pairing request is a normal prompt on the phone and pairs on
/// the first try; it used to sit unanswered until the computer gave up.
#[test]
fn a_peer_that_forgot_the_phone_can_re_pair_first_try() {
    let phone = Phone::start();
    let creds =
        DeviceCredentials::generate_with_id(&DeviceId::parse(&"q".repeat(32)).unwrap()).unwrap();
    let first = peer_with('q', creds.clone(), Arc::new(MemoryTrustStore::new()));
    let link = pair_by_peer(&phone, &first);
    drop(link);
    phone.wait("the link to end", |s| {
        Phone::array(s, "connections").is_empty()
    });
    assert_eq!(Phone::array(&phone.status(), "paired").len(), 1);

    // Same identity, empty trust store: the computer after its local "forget".
    let again = peer_with('q', creds, Arc::new(MemoryTrustStore::new()));
    let link = pending(again.dial(phone.addr, &phone.id).unwrap());
    let (join, _seen) = again.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("a pairing prompt", |s| {
        !Phone::array(s, "prompts").is_empty()
    });
    ok(&phone.decision(&Phone::array(&s, "prompts")[0], true));
    let _held = again.finish(join).expect("the first re-pair attempt works");
    let s = phone.wait("trusted connection", Phone::first_trusted);
    assert_eq!(Phone::array(&s, "paired").len(), 1);
}

#[test]
fn renaming_the_phone_is_validated_and_the_next_connection_announces_it() {
    let phone = Phone::start();
    for bad in ["", "  ", "a\u{1b}b", &"n".repeat(33)] {
        let v = phone.act("transport_set_name", json!({"device_name": bad}));
        assert_eq!(code(&v), "invalid_device_name", "{bad:?}");
    }
    assert_eq!(phone.status()["local"]["device_name"], "Test Phone");

    let v = phone.act(
        "transport_set_name",
        json!({"device_name": " Ana's Pixel "}),
    );
    assert_eq!(ok(&v)["device_name"], "Anas Pixel");
    assert_eq!(phone.status()["local"]["device_name"], "Anas Pixel");

    let peer = Peer::new('p');
    let hs = peer.dial(phone.addr, &phone.id).unwrap();
    assert_eq!(hs.info().peer.device_name, "Anas Pixel");
}

#[test]
fn peer_unpairing_removes_trust_on_the_phone() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let link = pair_by_peer(&phone, &peer);
    peer.rt.block_on(link.unpair()).unwrap();
    let s = phone.wait("peer_unpaired", |s| {
        !Phone::events(s, "peer_unpaired").is_empty()
    });
    assert!(Phone::array(&s, "connections").is_empty());
    assert!(Phone::array(&s, "paired").is_empty());
}

/// With the app closed nothing polls the status, so a `{pair:false}` must wake `transport_wait`: that is
/// what lets the app drop the computer and stop its background service.
#[test]
fn a_peer_unpairing_wakes_the_event_wait_and_ends_its_transfers() {
    let phone = Arc::new(Phone::start());
    let peer = Peer::new('p');
    let mut link = pair_by_peer(&phone, &peer);
    let since = phone.status()["last_event_seq"].as_u64().unwrap();

    // Nothing new: it waits out the timeout instead of returning at once.
    let t0 = Instant::now();
    let r = phone.act("transport_wait", json!({"since": since, "timeout_ms": 150}));
    assert_eq!(ok(&r)["last_event_seq"], since);
    assert_eq!(ok(&r)["stopped"], false);
    assert!(t0.elapsed() >= Duration::from_millis(140));
    // A newer event than `since` is reported without blocking.
    let t0 = Instant::now();
    let r = phone.act("transport_wait", json!({"since": 0, "timeout_ms": 30_000}));
    assert_eq!(ok(&r)["last_event_seq"], since);
    assert!(t0.elapsed() < Duration::from_secs(5));

    // An offer in flight when the computer unpairs must not stay answerable.
    let ctx = link.payload_context();
    let listener = peer.rt.block_on(ctx.listen()).unwrap();
    let packet = bridge_kde::share::file_packet("a.bin", 10, None, listener.port());
    peer.rt.block_on(link.send_with_payload(&packet)).unwrap();
    phone.wait("the offer", |s| !Phone::array(s, "transfers").is_empty());

    let waiter = {
        let phone = phone.clone();
        std::thread::spawn(move || {
            phone.act(
                "transport_wait",
                json!({"since": since, "timeout_ms": 60_000}),
            )
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    peer.rt.block_on(link.unpair()).unwrap();
    let woken = waiter.join().unwrap();
    assert!(ok(&woken)["last_event_seq"].as_u64().unwrap() > since);
    let s = phone.status();
    assert!(!Phone::events(&s, "peer_unpaired").is_empty());
    assert!(Phone::array(&s, "paired").is_empty());
    assert_eq!(Phone::array(&s, "transfers")[0]["state"], "cancelled");
}

#[test]
fn stopping_the_transport_ends_the_event_wait() {
    let phone = Arc::new(Phone::start());
    let since = phone.status()["last_event_seq"].as_u64().unwrap();
    let waiter = {
        let phone = phone.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let r = phone.act(
                "transport_wait",
                json!({"since": since, "timeout_ms": 60_000}),
            );
            (r, t0.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    phone.stop();
    let (r, took) = waiter.join().unwrap();
    assert_eq!(ok(&r)["stopped"], true);
    assert!(took < Duration::from_secs(10), "{took:?}");
    // Stopped: the app's wait loop ends here.
    assert_eq!(code(&phone.act("transport_wait", json!({}))), "not_running");
}

#[test]
fn a_different_certificate_under_a_known_device_id_is_refused() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let _link = pair_by_peer(&phone, &peer);
    // An impostor with the same device id but a new key.
    let impostor = Peer::new('p');
    assert_ne!(impostor.cert, peer.cert);
    let attempt = impostor.dial(phone.addr, &phone.id);
    if let Ok(hs) = attempt {
        // Even if the handshake reached the phone's side, the phone must not trust it.
        assert!(matches!(hs, Handshake::Pending(_)));
    }
    let s = phone.wait("rejection counted", |s| {
        s["counters"]["inbound_failed"] == 1
    });
    assert_eq!(
        Phone::array(&s, "connections").len(),
        1,
        "only the genuine peer"
    );
    assert_eq!(
        Phone::array(&s, "connections")[0]["peer"]["cert_sha256"],
        peer.cert.as_str()
    );
}

#[test]
fn oversized_packets_close_the_connection() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let mut link = pair_by_peer(&phone, &peer);
    let mut body = Map::new();
    body.insert("message".into(), json!("x".repeat(200_000)));
    // The phone hangs up once the line limit is crossed, so the tail of the write may fail.
    let _ = peer.try_send(&mut link, "kdeconnect.ping", body);
    let s = phone.wait("link closed", |s| Phone::array(s, "connections").is_empty());
    let closed = Phone::events(&s, "link_closed");
    assert_eq!(closed[0]["code"], "packet_too_large");
    // Trust is untouched by a protocol violation.
    assert_eq!(Phone::array(&s, "paired").len(), 1);
}

#[test]
fn a_second_unpaired_connection_for_a_device_is_refused() {
    let phone = Phone::start();
    let peer = Peer::new('p');
    let first = pending(peer.dial(phone.addr, &phone.id).unwrap());
    phone.wait("first connection", |s| {
        Phone::array(s, "connections").len() == 1
    });
    let _second = peer.dial(phone.addr, &phone.id);
    let s = phone.wait("duplicate counted", |s| {
        s["counters"]["rejected_duplicates"] == 1
    });
    assert_eq!(Phone::array(&s, "connections").len(), 1);
    drop(first);
}

#[test]
fn loopback_peers_are_refused_unless_opted_in() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dispatcher::new();
    let mut r = start_request(dir.path(), json!({}));
    r["listen"] = json!({"host": "0.0.0.0", "port": 0});
    r["allow_loopback"] = json!(false);
    let v = parse(&d.handle(&r.to_string()));
    let port = ok(&v)["listen"]["port"].as_u64().unwrap();
    let id = ok(&v)["device_id"].as_str().unwrap().to_owned();
    let peer = Peer::new('p');
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    assert!(
        peer.dial(addr, &id).is_err(),
        "the phone closes loopback peers before reading"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = parse(&d.handle(r#"{"version":1,"action":"transport_status"}"#));
        let s = ok(&s);
        if s["counters"]["inbound_failed"] == 1 {
            assert!(Phone::array(s, "connections").is_empty());
            break;
        }
        assert!(Instant::now() < deadline, "{s}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ------------------------------------------------------------------------------- stop/restart

#[test]
fn stop_rejects_pending_requests_and_nothing_leaks_into_a_restart() {
    let phone = Phone::start();
    let port = phone.addr.port();
    let peer = Peer::new('p');
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("prompt", |s| !Phone::array(s, "prompts").is_empty());
    let stale = Phone::array(&s, "prompts")[0].clone();

    phone.stop();
    // The in-flight pairing ends on the peer's side, trust is not recorded, and the old
    // request id means nothing.
    assert!(peer.finish(join).is_err());
    assert_eq!(phone.status(), json!({"state": "stopped"}));
    assert_eq!(code(&phone.decision(&stale, true)), "not_running");
    assert!(std::net::TcpStream::connect(phone.addr).is_err());

    ok(&phone.raw(&start_request(
        phone.dir.path(),
        json!({"listen": {"host": phone.addr.ip().to_string(), "port": port}}),
    )));
    let s = phone.status();
    assert!(Phone::array(&s, "prompts").is_empty());
    assert!(Phone::array(&s, "connections").is_empty());
    assert!(Phone::array(&s, "paired").is_empty());
    assert!(Phone::array(&s, "events").is_empty());
    assert_eq!(code(&phone.decision(&stale, true)), "unknown_request");

    // A new prompt gets a new id even when the connection numbering starts over.
    let link = pending(peer.dial(phone.addr, &phone.id).unwrap());
    let (join, _) = peer.spawn_pair(link, PairMode::Request, true);
    let s = phone.wait("second prompt", |s| !Phone::array(s, "prompts").is_empty());
    let fresh = &Phone::array(&s, "prompts")[0];
    assert_ne!(fresh["request_id"], stale["request_id"]);
    assert_eq!(code(&phone.decision(&stale, true)), "unknown_request");
    ok(&phone.decision(fresh, false));
    assert!(peer.finish(join).is_err());
}

#[test]
fn stop_cancels_dials_in_flight() {
    let phone = Phone::start();
    // A listener that accepts and then says nothing keeps the handshake pending.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();
    ok(&phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": "c".repeat(32)}),
    ));
    let (mut stream, _) = silent.accept().unwrap();
    let s = phone.status();
    assert_eq!(Phone::array(&s, "attempts").len(), 1);
    assert_eq!(
        code(&phone.act(
            "transport_connect",
            json!({"host": "127.0.0.1", "port": addr.port(), "device_id": "c".repeat(32)})
        )),
        "already_connected"
    );
    phone.stop();
    // The socket is closed by stop: the silent side reads the phone's identity line, then EOF.
    use std::io::Read;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
    assert!(sink.len() < 8192);
    ok(&phone.raw(&start_request(phone.dir.path(), json!({}))));
    assert!(Phone::array(&phone.status(), "attempts").is_empty());
}

#[test]
fn connect_failures_are_reported_as_static_codes() {
    let phone = Phone::start();
    // Nothing listens here.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    ok(&phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": port, "device_id": "d".repeat(32)}),
    ));
    let s = phone.wait("failure event", |s| {
        !Phone::events(s, "connect_failed").is_empty()
    });
    let e = Phone::events(&s, "connect_failed")[0].clone();
    // Named, so a redial log tells "the computer is not listening" from "no route to it".
    assert_eq!(e["code"], "conn_refused");
    assert_eq!(e["device_id"], "d".repeat(32).as_str());
    assert!(Phone::array(&s, "attempts").is_empty());
    let text = s.to_string();
    // The OS message never leaks, only the static code.
    assert!(!text.contains("Connection refused") && !text.contains("os error"));
    // `since` filters events.
    let seq = e["seq"].as_u64().unwrap();
    let v = phone.act("transport_status", json!({"since": seq}));
    assert!(Phone::array(ok(&v), "events").is_empty());
}

// ------------------------------------------------------------------------------- discovery

fn datagram(id: char, tcp_port: u16) -> Vec<u8> {
    let mut body = Map::new();
    body.insert("deviceId".into(), json!(id.to_string().repeat(32)));
    body.insert("deviceName".into(), json!("Desk"));
    body.insert("protocolVersion".into(), json!(8));
    body.insert("deviceType".into(), json!("desktop"));
    body.insert("incomingCapabilities".into(), json!(["kdeconnect.ping"]));
    body.insert("outgoingCapabilities".into(), json!(["kdeconnect.ping"]));
    body.insert("tcpPort".into(), json!(tcp_port));
    Packet::new("kdeconnect.identity", body).to_line()
}

#[test]
fn discovery_lists_peers_but_never_connects_or_trusts() {
    let phone = Phone::start_with(json!({"discovery": {"listen": true, "port": 0}}));
    let s = phone.status();
    assert_eq!(s["discovery"]["listening"], true);
    let port = s["discovery"]["port"].as_u64().unwrap();
    // A peer's TCP port that would show if the phone dialled out on hearing it.
    let bait = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    bait.set_nonblocking(true).unwrap();
    let bait_port = bait.local_addr().unwrap().port();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let to = format!("127.0.0.1:{port}");
    udp.send_to(b"garbage", &to).unwrap();
    udp.send_to(&datagram('z', 80), &to).unwrap(); // privileged port
    udp.send_to(&vec![b' '; 9000], &to).unwrap();
    udp.send_to(&datagram('e', bait_port), &to).unwrap();
    let s = phone.wait("a discovered peer", |s| {
        !Phone::array(s, "discovered").is_empty()
    });
    let found = Phone::array(&s, "discovered");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["device_id"], "e".repeat(32).as_str());
    assert_eq!(found[0]["tcp_port"], bait_port);
    assert_eq!(found[0]["paired"], false);
    assert_eq!(found[0]["ip"], "127.0.0.1");
    std::thread::sleep(Duration::from_millis(400));
    // Other local software probes listening loopback ports (HTTP scanners were seen doing so), so
    // a stray connection only counts if what arrives is our protocol greeting.
    while let Ok((mut stream, _)) = bait.accept() {
        use std::io::Read;
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut greeting = [0u8; 256];
        let n = stream.read(&mut greeting).unwrap_or(0);
        assert!(
            !String::from_utf8_lossy(greeting.get(..n).unwrap_or_default()).contains("kdeconnect"),
            "discovery must not dial"
        );
    }
    let s = phone.status();
    assert!(Phone::array(&s, "connections").is_empty());
    assert!(Phone::array(&s, "attempts").is_empty());
    assert!(Phone::array(&s, "paired").is_empty());
}

#[test]
fn discovery_ignores_datagrams_from_loopback_unless_opted_in() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dispatcher::new();
    let mut r = start_request(
        dir.path(),
        json!({"discovery": {"listen": true, "port": 0}}),
    );
    r["listen"] = json!({"host": "0.0.0.0", "port": 0});
    r["allow_loopback"] = json!(false);
    let v = parse(&d.handle(&r.to_string()));
    let port = ok(&v)["discovery"]["port"].as_u64().unwrap();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.send_to(&datagram('e', 40000), format!("127.0.0.1:{port}"))
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let s = parse(&d.handle(r#"{"version":1,"action":"transport_status"}"#));
    assert!(Phone::array(ok(&s), "discovered").is_empty());
}

#[test]
fn announce_follows_a_ready_listener_and_is_kde_shaped() {
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let target = rx.local_addr().unwrap();
    let phone = Phone::start_with(json!({
        "discovery": {"announce": true, "announce_to": target.to_string()}
    }));
    assert_eq!(phone.status()["discovery"]["announcing"], true);
    let mut buf = vec![0u8; 16384];
    let (n, _) = rx.recv_from(&mut buf).unwrap();
    // The advertised TCP port already accepts connections.
    let line = &buf[..n];
    let packet = Packet::from_line(line.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(packet.kind, "kdeconnect.identity");
    let port = packet.body["tcpPort"].as_u64().unwrap();
    assert_eq!(port, u64::from(phone.addr.port()));
    assert!(std::net::TcpStream::connect(phone.addr).is_ok());
    assert_eq!(packet.body["deviceId"], phone.id.as_str());
    assert_eq!(packet.body["protocolVersion"], 8);
    assert_eq!(packet.body["deviceType"], "phone");
    // Plaintext broadcast: stock capabilities only; the geekbridge.* names travel inside TLS.
    assert_eq!(
        packet.body["incomingCapabilities"],
        json!(["kdeconnect.ping", "kdeconnect.share.request"])
    );
    assert_eq!(
        packet.body["outgoingCapabilities"],
        json!(["kdeconnect.ping", "kdeconnect.share.request"])
    );
    assert!(!String::from_utf8_lossy(line).contains("geekbridge"));
    // The listening port field is the computer's; the phone's announcement carries tcpPort only.
    assert!(packet.body.get("geekbridgePort").is_none());
    assert!(packet.body.get("targetDeviceId").is_none());
    // After stop nothing more is announced.
    phone.stop();
    rx.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    assert!(rx.recv_from(&mut buf).is_err());
}

#[test]
fn discovery_bind_failure_does_not_fail_the_start() {
    let taken = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let phone = Phone::start_with(json!({"discovery": {"listen": true, "port": port}}));
    let s = phone.status();
    assert_eq!(s["discovery"]["listening"], false);
    assert_eq!(s["discovery"]["error"], "discovery_bind_failed");
    assert_eq!(s["state"], "running");
}

// ------------------------------------------------------------------------------- sharing

// The sharing tests hand raw file descriptors across the JSON request interface (Android only).
#[cfg(unix)]
mod sharing {
    use super::*;
    use bridge_kde::share;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;

    fn payload(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 253) as u8).collect()
    }

    fn transfers(s: &Value) -> &Vec<Value> {
        Phone::array(s, "transfers")
    }

    fn paired() -> (Phone, Peer, TrustedLink) {
        let phone = Phone::start();
        let peer = Peer::new('d');
        let link = pair_by_peer(&phone, &peer);
        (phone, peer, link)
    }

    fn conn_id(s: &Value) -> u64 {
        Phone::array(s, "connections")[0]["connection_id"]
            .as_u64()
            .unwrap()
    }

    /// The desktop announces a file and serves it from `bytes`; returns the serving task.
    fn desktop_offers(
        peer: &Peer,
        link: &mut TrustedLink,
        name: &str,
        bytes: Vec<u8>,
    ) -> JoinHandle<Result<u64, bridge_kde::PayloadError>> {
        let ctx = link.payload_context();
        let listener = peer.rt.block_on(ctx.listen()).unwrap();
        let packet = share::file_packet(
            name,
            bytes.len() as u64,
            Some(1_700_000_000_000),
            listener.port(),
        );
        peer.rt.block_on(link.send_with_payload(&packet)).unwrap();
        let size = bytes.len() as u64;
        peer.rt.spawn(async move {
            listener
                .send(
                    std::io::Cursor::new(bytes),
                    size,
                    &mut |_| {},
                    &bridge_kde::CancelToken::new(),
                )
                .await
        })
    }

    #[test]
    fn an_offered_file_is_written_only_after_accept() {
        let (phone, peer, mut link) = paired();
        let data = payload(200_000);
        let serve = desktop_offers(&peer, &mut link, "../../evil/photo.jpg", data.clone());
        let s = phone.wait("an offer", |s| !transfers(s).is_empty());
        let t = &transfers(&s)[0];
        assert_eq!(t["state"], "offered");
        assert_eq!(t["direction"], "incoming");
        assert_eq!(t["name"], "photo.jpg"); // path components stripped
        assert_eq!(t["size"], 200_000);
        assert_eq!(t["bytes"], 0);
        // Nothing connects to the payload socket until the user answers.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!serve.is_finished());

        let mut out = tempfile::tempfile().unwrap();
        let v = phone.act(
            "share_accept",
            json!({"transfer_id": t["transfer_id"], "fd": out.as_raw_fd()}),
        );
        ok(&v);
        let s = phone.wait("completion", |s| transfers(s)[0]["state"] == "done");
        assert_eq!(transfers(&s)[0]["bytes"], 200_000);
        assert_eq!(peer.finish(serve).unwrap(), 200_000);
        let mut got = Vec::new();
        out.seek(SeekFrom::Start(0)).unwrap();
        out.read_to_end(&mut got).unwrap();
        assert_eq!(got, data);
        // A second answer is refused and a finished entry can be dismissed.
        let again = phone.act(
            "share_accept",
            json!({"transfer_id": t["transfer_id"], "fd": out.as_raw_fd()}),
        );
        assert_eq!(code(&again), "wrong_state");
        ok(&phone.act("share_dismiss", json!({"transfer_id": t["transfer_id"]})));
        assert!(transfers(&phone.status()).is_empty());
    }

    #[test]
    fn declining_never_opens_the_payload_socket() {
        let (phone, peer, mut link) = paired();
        let serve = desktop_offers(&peer, &mut link, "a.bin", payload(1000));
        let s = phone.wait("an offer", |s| !transfers(s).is_empty());
        let id = transfers(&s)[0]["transfer_id"].clone();
        ok(&phone.act("share_decline", json!({"transfer_id": id})));
        let s = phone.status();
        assert_eq!(transfers(&s)[0]["state"], "declined");
        let out = tempfile::tempfile().unwrap();
        let v = phone.act(
            "share_accept",
            json!({"transfer_id": id, "fd": out.as_raw_fd()}),
        );
        assert_eq!(code(&v), "wrong_state");
        assert!(!serve.is_finished());
    }

    #[test]
    fn the_phone_sends_a_file_from_a_descriptor() {
        let (phone, peer, mut link) = paired();
        let data = payload(150_000);
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&data).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        let cid = conn_id(&phone.status());
        let v = phone.act(
            "share_send_file",
            json!({"connection_id": cid, "fd": f.as_raw_fd(), "name": "dir/../x.bin", "size": data.len(), "last_modified_ms": 1_700_000_000_000u64}),
        );
        let id = ok(&v)["transfer_id"].as_u64().unwrap();
        drop(f); // Rust duplicated the descriptor; closing ours must not break the transfer
        let LinkEvent::Packet(p) = peer.recv(&mut link).unwrap() else {
            panic!("expected a packet")
        };
        let Ok(share::ShareContent::File(offer)) = share::parse_share(&p, 1 << 30) else {
            panic!("expected a file offer: {p:?}")
        };
        assert_eq!(offer.name, "x.bin");
        assert_eq!(offer.size, Some(150_000));
        let dir = tempfile::tempdir().unwrap();
        let ctx = link.payload_context();
        let path = peer
            .rt
            .block_on(share::receive_file(
                &ctx,
                &offer,
                dir.path(),
                1 << 30,
                &mut |_| {},
                &bridge_kde::CancelToken::new(),
            ))
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), data);
        let s = phone.wait("done", |s| {
            transfers(s)
                .iter()
                .any(|t| t["transfer_id"] == id && t["state"] == "done")
        });
        assert_eq!(transfers(&s)[0]["direction"], "outgoing");
    }

    #[test]
    fn cancel_stops_an_outgoing_transfer_waiting_for_the_computer() {
        let (phone, peer, mut link) = paired();
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&payload(5000)).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        let cid = conn_id(&phone.status());
        let v = phone.act(
            "share_send_file",
            json!({"connection_id": cid, "fd": f.as_raw_fd(), "name": "w.bin", "size": 5000}),
        );
        let id = ok(&v)["transfer_id"].clone();
        let _ = peer.recv(&mut link); // the announcement
        phone.wait("waiting", |s| transfers(s)[0]["state"] == "waiting");
        ok(&phone.act("share_cancel", json!({"transfer_id": id})));
        assert_eq!(transfers(&phone.status())[0]["state"], "cancelled");
        let again = phone.act("share_cancel", json!({"transfer_id": id}));
        assert_eq!(code(&again), "wrong_state");
    }

    #[test]
    fn files_queued_behind_a_closing_link_are_failed_not_left_waiting() {
        let (phone, peer, mut link) = paired();
        let cid = conn_id(&phone.status());
        // Queue a burst and close at once. Whatever the loop got to before the close was announced to
        // the peer; everything it never ran has to be failed by the drain in `run_trusted`.
        let mut ids = Vec::new();
        let mut files = Vec::new();
        for i in 0..3 {
            let mut f = tempfile::tempfile().unwrap();
            f.write_all(&payload(1000)).unwrap();
            f.seek(SeekFrom::Start(0)).unwrap();
            let v = phone.act(
                "share_send_file",
                json!({"connection_id": cid, "fd": f.as_raw_fd(), "name": format!("q{i}.bin"), "size": 1000}),
            );
            ids.push(ok(&v)["transfer_id"].as_u64().unwrap());
            files.push(f);
        }
        ok(&phone.act("transport_disconnect", json!({"connection_id": cid})));
        let mut announced = 0;
        while let Ok(LinkEvent::Packet(p)) = peer.recv(&mut link) {
            if matches!(
                share::parse_share(&p, 1 << 30),
                Ok(share::ShareContent::File(_))
            ) {
                announced += 1;
            }
        }
        let s = phone.wait("the unsent files to fail", |s| {
            transfers(s)
                .iter()
                .filter(|t| t["state"] == "failed" && t["code"] == "link_lost")
                .count()
                == ids.len() - announced
        });
        assert!(Phone::array(&s, "connections").is_empty());
        // An announced file may still wait for the computer to collect it; nothing else may be waiting.
        let waiting = transfers(&s)
            .iter()
            .filter(|t| t["state"] == "waiting")
            .count();
        assert_eq!(waiting, announced, "{s}");
    }

    #[test]
    fn text_and_links_both_ways() {
        let (phone, peer, mut link) = paired();
        peer.send(&mut link, "kdeconnect.share.request", {
            let mut b = Map::new();
            b.insert("url".into(), json!("https://example.com/a"));
            b
        });
        peer.send(&mut link, "kdeconnect.share.request", {
            let mut b = Map::new();
            b.insert("text".into(), json!("hello\u{0}world"));
            b
        });
        let s = phone.wait("two items", |s| transfers(s).len() == 2);
        let t = transfers(&s);
        assert_eq!(
            (t[0]["kind"].as_str(), t[0]["text"].as_str()),
            (Some("url"), Some("https://example.com/a"))
        );
        assert_eq!(
            (t[1]["kind"].as_str(), t[1]["text"].as_str()),
            (Some("text"), Some("helloworld"))
        );
        assert!(
            t.iter()
                .all(|x| x["state"] == "done" && x["direction"] == "incoming")
        );
        ok(&phone.act("share_dismiss", json!({"transfer_id": t[0]["transfer_id"]})));

        let cid = conn_id(&s);
        ok(&phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "url", "text": "https://example.org"}),
        ));
        let LinkEvent::Packet(p) = peer.recv(&mut link).unwrap() else {
            panic!()
        };
        assert_eq!(
            share::parse_share(&p, 1),
            Ok(share::ShareContent::Url("https://example.org".into()))
        );
        let bad = phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "other", "text": "x"}),
        );
        assert_eq!(code(&bad), "invalid_field");
        let empty = phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "text", "text": ""}),
        );
        assert_eq!(code(&empty), "invalid_field");
    }

    #[test]
    fn requests_are_validated() {
        let phone = Phone::start();
        let f = tempfile::tempfile().unwrap();
        let fd = f.as_raw_fd();
        // Not running / unknown things never touch the descriptor.
        let off = Dispatcher::new();
        let v =
            parse(&off.handle(&request("share_decline", json!({"transfer_id": 1})).to_string()));
        assert_eq!(code(&v), "not_running");
        assert_eq!(
            code(&phone.act("share_decline", json!({"transfer_id": 99}))),
            "unknown_transfer"
        );
        assert_eq!(
            code(&phone.act("share_cancel", json!({"transfer_id": 99}))),
            "unknown_transfer"
        );
        assert_eq!(
            code(&phone.act("share_dismiss", json!({"transfer_id": 99}))),
            "unknown_transfer"
        );
        assert_eq!(
            code(&phone.act("share_accept", json!({"transfer_id": 99, "fd": fd}))),
            "unknown_transfer"
        );
        assert_eq!(
            code(&phone.act(
                "share_send_file",
                json!({"connection_id": 99, "fd": fd, "name": "a", "size": 1})
            )),
            "unknown_connection"
        );
        assert_eq!(
            code(&phone.act(
                "share_send_file",
                json!({"connection_id": 1, "fd": 999_999, "name": "a", "size": 1})
            )),
            "invalid_fd"
        );
        assert_eq!(
            code(&phone.act(
                "share_send_file",
                json!({"connection_id": 1, "fd": fd, "name": "", "size": 1})
            )),
            "invalid_field"
        );
        // Unknown fields and a negative descriptor are rejected by the envelope.
        assert_eq!(
            code(&phone.act("share_decline", json!({"transfer_id": 1, "x": 1}))),
            "unknown_field"
        );
        assert_eq!(
            code(&phone.act("share_accept", json!({"transfer_id": 1, "fd": -1}))),
            "invalid_field"
        );
        // An untrusted connection cannot share.
        let peer = Peer::new('u');
        let hs = peer.dial(phone.addr, &phone.id).unwrap();
        let _pending = pending(hs);
        let s = phone.wait("a parked connection", |s| {
            !Phone::array(s, "connections").is_empty()
        });
        let cid = conn_id(&s);
        assert_eq!(
            code(&phone.act(
                "share_send_file",
                json!({"connection_id": cid, "fd": fd, "name": "a", "size": 1})
            )),
            "not_trusted"
        );
        assert_eq!(
            code(&phone.act(
                "share_send_text",
                json!({"connection_id": cid, "kind": "text", "text": "x"})
            )),
            "not_trusted"
        );
    }

    #[test]
    fn too_many_offers_and_oversized_files_are_refused() {
        let (phone, peer, mut link) = paired();
        // An announced size above the cap is ignored outright.
        let mut huge = share::file_packet("big.bin", 1, None, 1740);
        huge.payload_size = Some(9 * 1024 * 1024 * 1024);
        peer.rt.block_on(link.send_with_payload(&huge)).unwrap();
        // Offers beyond the pending limit are dropped, not queued without bound.
        for i in 0..12 {
            let p = share::file_packet(&format!("f{i}.bin"), 10, None, 1740);
            peer.rt.block_on(link.send_with_payload(&p)).unwrap();
        }
        phone.wait("offers", |s| transfers(s).len() >= 8);
        std::thread::sleep(Duration::from_millis(200));
        let s = phone.status();
        assert_eq!(transfers(&s).len(), 8);
        assert!(transfers(&s).iter().all(|t| t["name"] != "big.bin"));
        assert!(
            Phone::array(&s, "connections")[0]["ignored_packets"]
                .as_u64()
                .unwrap()
                >= 1
        );
    }

    #[test]
    fn a_stranger_cannot_collect_the_payload() {
        let (phone, peer, mut link) = paired();
        let f = tempfile::tempfile().unwrap();
        let mut w = &f;
        w.write_all(&payload(2000)).unwrap();
        let cid = conn_id(&phone.status());
        let mut rf = f.try_clone().unwrap();
        rf.seek(SeekFrom::Start(0)).unwrap();
        let v = phone.act(
            "share_send_file",
            json!({"connection_id": cid, "fd": rf.as_raw_fd(), "name": "s.bin", "size": 2000}),
        );
        ok(&v);
        let LinkEvent::Packet(p) = peer.recv(&mut link).unwrap() else {
            panic!()
        };
        let Ok(share::ShareContent::File(offer)) = share::parse_share(&p, 1 << 30) else {
            panic!()
        };
        // A raw TCP client without a pinned certificate gets nothing: TLS needs the paired cert.
        let stranger = std::net::TcpStream::connect(("127.0.0.1", offer.port))
            .or_else(|_| std::net::TcpStream::connect((phone.addr.ip(), offer.port)));
        if let Ok(mut s) = stranger {
            s.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut buf = [0u8; 64];
            // The server speaks TLS: it never sends our file bytes in the clear.
            let n = s.read(&mut buf).unwrap_or(0);
            assert!(!buf[..n].windows(8).any(|w| w == &payload(2000)[..8]));
        }
        // The real peer still gets it afterwards.
        let dir = tempfile::tempdir().unwrap();
        let ctx = link.payload_context();
        let path = peer
            .rt
            .block_on(share::receive_file(
                &ctx,
                &offer,
                dir.path(),
                1 << 30,
                &mut |_| {},
                &bridge_kde::CancelToken::new(),
            ))
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), payload(2000));
    }

    fn answer(peer: &Peer, link: &mut TrustedLink, body: Value) {
        peer.send(
            link,
            "geekbridge.share.result",
            body.as_object().unwrap().clone(),
        );
    }

    fn results(s: &Value) -> &Vec<Value> {
        Phone::array(s, "share_results")
    }

    fn send_small(phone: &Phone, cid: u64, name: &str) -> u64 {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&payload(3000)).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        let v = phone.act(
            "share_send_file",
            json!({"connection_id": cid, "fd": f.as_raw_fd(), "name": name, "size": 3000}),
        );
        ok(&v)["transfer_id"].as_u64().unwrap()
    }

    fn next_share(peer: &Peer, link: &mut TrustedLink) -> Packet {
        loop {
            let LinkEvent::Packet(p) = peer.recv(link).unwrap() else {
                continue;
            };
            if p.kind == "kdeconnect.share.request" {
                return p;
            }
        }
    }

    #[test]
    fn the_phone_numbers_its_shares_only_for_a_computer_that_answers() {
        // A plain computer (stock-like) sees no extra key and gets no id for the text either.
        let (phone, peer, mut link) = paired();
        let cid = conn_id(&phone.status());
        send_small(&phone, cid, "plain.bin");
        let p = next_share(&peer, &mut link);
        assert!(p.body.get("geekbridgeShareId").is_none(), "{p:?}");
        let v = phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "text", "text": "hi"}),
        );
        assert_eq!(ok(&v)["result_expected"], false);
        let p = next_share(&peer, &mut link);
        assert!(p.body.get("geekbridgeShareId").is_none(), "{p:?}");
        // An answer from a computer that never declared the type is not believed.
        answer(&peer, &mut link, json!({"textId": 1, "outcome": "saved"}));
        std::thread::sleep(Duration::from_millis(200));
        assert!(results(&phone.status()).is_empty());

        // A computer that declared it gets the phone's transfer number and the text's number.
        let phone = Phone::start();
        let peer = Peer::answering('e');
        let mut link = pair_by_peer(&phone, &peer);
        let cid = conn_id(&phone.status());
        let id = send_small(&phone, cid, "numbered.bin");
        let p = next_share(&peer, &mut link);
        assert_eq!(p.body["geekbridgeShareId"], id);
        let v = phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "url", "text": "https://example.com/"}),
        );
        let text_id = ok(&v)["text_id"].as_u64().unwrap();
        assert_eq!(ok(&v)["result_expected"], true);
        let p = next_share(&peer, &mut link);
        assert_eq!(p.body["geekbridgeShareId"], text_id);
    }

    #[test]
    fn a_refusal_ends_the_wait_at_once_and_is_reported_in_words_codes() {
        let phone = Phone::start();
        let peer = Peer::answering('e');
        let mut link = pair_by_peer(&phone, &peer);
        let cid = conn_id(&phone.status());
        let id = send_small(&phone, cid, "refused.bin");
        let _ = next_share(&peer, &mut link);
        phone.wait("waiting", |s| transfers(s)[0]["state"] == "waiting");
        answer(
            &peer,
            &mut link,
            json!({"transferId": id, "outcome": "receive_off", "reason": "not_allowed"}),
        );
        let s = phone.wait("the refusal", |s| !results(s).is_empty());
        let t = &transfers(&s)[0];
        // Not "waiting" for the two minutes the payload socket would have waited.
        assert_eq!(t["state"], "declined");
        assert_eq!(t["code"], "receive_off");
        let r = &results(&s)[0];
        assert_eq!(
            (
                r["kind"].clone(),
                r["id"].clone(),
                r["outcome"].clone(),
                r["reason"].clone()
            ),
            (
                json!("file"),
                json!(id),
                json!("receive_off"),
                json!("not_allowed")
            )
        );
        // The first answer stands; a later contradictory one changes nothing.
        answer(
            &peer,
            &mut link,
            json!({"transferId": id, "outcome": "accepted"}),
        );
        std::thread::sleep(Duration::from_millis(200));
        let s = phone.status();
        assert_eq!(results(&s).len(), 1);
        assert_eq!(transfers(&s)[0]["state"], "declined");
    }

    #[test]
    fn accepted_keeps_the_transfer_going_and_text_answers_are_recorded() {
        let phone = Phone::start();
        let peer = Peer::answering('e');
        let mut link = pair_by_peer(&phone, &peer);
        let cid = conn_id(&phone.status());
        let id = send_small(&phone, cid, "ok.bin");
        let p = next_share(&peer, &mut link);
        answer(
            &peer,
            &mut link,
            json!({"transferId": id, "outcome": "accepted", "reason": "user"}),
        );
        let s = phone.wait("accepted", |s| !results(s).is_empty());
        assert_eq!(results(&s)[0]["outcome"], "accepted");
        assert_eq!(transfers(&s)[0]["state"], "waiting");
        // It still completes when the computer fetches it.
        let Ok(share::ShareContent::File(offer)) = share::parse_share(&p, 1 << 30) else {
            panic!()
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = link.payload_context();
        peer.rt
            .block_on(share::receive_file(
                &ctx,
                &offer,
                dir.path(),
                1 << 30,
                &mut |_| {},
                &bridge_kde::CancelToken::new(),
            ))
            .unwrap();
        phone.wait("done", |s| transfers(s)[0]["state"] == "done");

        let v = phone.act(
            "share_send_text",
            json!({"connection_id": cid, "kind": "text", "text": "note"}),
        );
        let tid = ok(&v)["text_id"].as_u64().unwrap();
        let _ = next_share(&peer, &mut link);
        answer(
            &peer,
            &mut link,
            json!({"textId": tid, "outcome": "saved", "reason": "kept"}),
        );
        let s = phone.wait("text answer", |s| results(s).len() == 2);
        assert_eq!(
            (
                results(&s)[1]["kind"].clone(),
                results(&s)[1]["outcome"].clone()
            ),
            (json!("text"), json!("saved"))
        );
    }

    #[test]
    fn answers_about_nothing_the_phone_shared_are_ignored() {
        let phone = Phone::start();
        let peer = Peer::answering('e');
        let mut link = pair_by_peer(&phone, &peer);
        for body in [
            json!({"transferId": 4242, "outcome": "declined"}),
            json!({"textId": 4242, "outcome": "saved"}),
            json!({"transferId": 0, "outcome": "declined"}),
            json!({"transferId": "x", "outcome": "declined"}),
            json!({"transferId": 1, "outcome": "exploded"}),
            json!({"outcome": "declined"}),
        ] {
            answer(&peer, &mut link, body);
        }
        // An incoming offer of the computer's own is not the phone's share either.
        let _serve = desktop_offers(&peer, &mut link, "theirs.bin", payload(10));
        let s = phone.wait("their offer", |s| !transfers(s).is_empty());
        let their = transfers(&s)[0]["transfer_id"].clone();
        answer(
            &peer,
            &mut link,
            json!({"transferId": their, "outcome": "declined"}),
        );
        std::thread::sleep(Duration::from_millis(300));
        let s = phone.status();
        assert!(results(&s).is_empty(), "{s}");
        assert_eq!(transfers(&s)[0]["state"], "offered");
        assert!(
            Phone::array(&s, "connections")[0]["ignored_packets"]
                .as_u64()
                .unwrap()
                >= 6
        );
    }
}

// ------------------------------------------------------------------------------- QR pairing

const QR_TOKEN: &str = "00112233445566778899aabbccddeeff";

/// Dial `peer`, wait until the connection is parked, and return its id.
fn dial_parked(phone: &Phone, peer: &Peer) -> (Value, JoinHandle<Result<Handshake, KdeError>>) {
    let (listener, addr) = peer.listen();
    let accepted = peer.accept_one(listener);
    let v = phone.act(
        "transport_connect",
        json!({"host": "127.0.0.1", "port": addr.port(), "device_id": peer.id.as_str()}),
    );
    assert_eq!(ok(&v)["accepted"], true);
    let s = phone.wait("a parked connection", |s| {
        !Phone::array(s, "connections").is_empty()
    });
    (
        Phone::array(&s, "connections")[0]["connection_id"].clone(),
        accepted,
    )
}

#[test]
fn a_scanned_qr_sends_its_token_only_to_the_pinned_certificate_and_flags_the_prompt() {
    let phone = Phone::start();
    let peer = Peer::new('w');
    let (cid, accepted) = dial_parked(&phone, &peer);
    let link = pending(peer.finish(accepted).unwrap());
    let (join, seen) = peer.spawn_pair(link, PairMode::AwaitPeerRequest, true);

    let r = phone.act(
        "transport_pair_request",
        json!({"connection_id": cid, "qr_token": QR_TOKEN, "qr_cert_sha256": peer.cert.to_uppercase()}),
    );
    assert_eq!(ok(&r)["phase"], "pairing_request");
    let s = phone.wait("our prompt", |s| !Phone::array(s, "prompts").is_empty());
    let prompt = &Phone::array(&s, "prompts")[0];
    assert_eq!(
        prompt["qr_pinned"], true,
        "the phone UI can skip the code comparison"
    );
    ok(&phone.decision(prompt, true));
    let _link = peer.finish(join).expect("pairing completes");
    let shown = wait_seen(&seen);
    assert_eq!(shown.presented_token.as_deref(), Some(QR_TOKEN));
    phone.wait("trusted", Phone::first_trusted);
}

#[test]
fn a_different_certificate_than_the_qr_pinned_never_gets_the_token() {
    let phone = Phone::start();
    let peer = Peer::new('x');
    let (cid, accepted) = dial_parked(&phone, &peer);
    let link = pending(peer.finish(accepted).unwrap());
    let (_join, seen) = peer.spawn_pair(link, PairMode::AwaitPeerRequest, true);

    let r = phone.act(
        "transport_pair_request",
        json!({"connection_id": cid, "qr_token": QR_TOKEN, "qr_cert_sha256": "ab".repeat(32)}),
    );
    assert_eq!(code(&r), "qr_certificate_mismatch");
    // The connection is gone and the peer never heard a thing.
    phone.wait("the connection to be dropped", |s| {
        Phone::array(s, "connections").is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        seen.lock().unwrap().is_none(),
        "no pair request, so no token, reached the peer"
    );
    assert!(Phone::array(&phone.status(), "paired").is_empty());
}

#[test]
fn qr_fields_come_as_a_pair_and_only_for_a_request() {
    let phone = Phone::start();
    let peer = Peer::new('y');
    let (cid, _accepted) = dial_parked(&phone, &peer);
    for fields in [
        json!({"connection_id": cid, "qr_token": QR_TOKEN}),
        json!({"connection_id": cid, "qr_cert_sha256": peer.cert}),
    ] {
        assert_eq!(
            code(&phone.act("transport_pair_request", fields)),
            "missing_field"
        );
    }
    assert_eq!(
        code(&phone.act(
            "transport_pair_request",
            json!({"connection_id": cid, "mode": "await", "qr_token": QR_TOKEN, "qr_cert_sha256": peer.cert})
        )),
        "invalid_field"
    );
    // None of those consumed the parked connection.
    ok(&phone.act("transport_pair_request", json!({"connection_id": cid})));
}

#[test]
fn qr_text_is_parsed_by_the_core() {
    let phone = Phone::start();
    let text = format!(
        "geekbridge://pair?v=1&id={}&fp={}&t={QR_TOKEN}&a=192.168.1.20%3A1716,%5Bfe80%3A%3A1%5D%3A1717&n=Work%20laptop",
        "a".repeat(32),
        "cd".repeat(32)
    );
    let v = phone.act("pairing_parse_qr", json!({"text": text}));
    let r = ok(&v);
    assert_eq!(r["device_id"], "a".repeat(32));
    assert_eq!(r["name"], "Work laptop");
    assert_eq!(r["cert_sha256"], "cd".repeat(32));
    assert_eq!(r["token"], QR_TOKEN);
    assert_eq!(
        r["addresses"][0],
        json!({"host": "192.168.1.20", "port": 1716})
    );
    assert_eq!(r["addresses"][1], json!({"host": "fe80::1", "port": 1717}));
    assert_eq!(
        code(&phone.act("pairing_parse_qr", json!({"text": "https://example.com"}))),
        "qr_not_geekbridge"
    );
    assert_eq!(
        code(&phone.act(
            "pairing_parse_qr",
            json!({"text": text.replace("v=1", "v=9")})
        )),
        "qr_version"
    );
    assert_eq!(
        code(&phone.act(
            "pairing_parse_qr",
            json!({"text": text.replace(QR_TOKEN, "short")})
        )),
        "qr_malformed"
    );
}

// ------------------------------------------------------------------------------- announce pause (test-gaps-4)

#[test]
fn announcements_stay_paused_while_linked_a_newcomer_wakes_them_and_a_storm_is_rate_limited() {
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = rx.local_addr().unwrap();
    let phone = Phone::start_with(json!({
        "discovery": {"listen": true, "port": 0, "announce": true, "announce_to": target.to_string()}
    }));
    let discovery_port = phone.status()["discovery"]["port"].as_u64().unwrap();
    let to = format!("127.0.0.1:{discovery_port}");
    let mut buf = vec![0u8; 16384];
    // Nobody is paired, so the first round announces at once.
    rx.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    rx.recv_from(&mut buf).expect("the first announcement");

    // Pair one computer and keep it linked: every paired computer is now linked.
    let peer = Peer::new('p');
    let _link = pair_by_peer(&phone, &peer);
    phone.wait("a trusted link", Phone::first_trusted);
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_millis(1200)))
        .unwrap();
    assert!(
        rx.recv_from(&mut buf).is_err(),
        "nothing is sent while every paired computer is linked"
    );

    // A paired computer announcing itself is not a newcomer: no wake.
    udp.send_to(&datagram('p', 40001), &to).unwrap();
    assert!(
        rx.recv_from(&mut buf).is_err(),
        "the linked computer's own announcement is not answered"
    );

    // An unpaired computer is answered at once, with a KDE-shaped identity of this phone.
    rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let t0 = Instant::now();
    udp.send_to(&datagram('n', 40002), &to).unwrap();
    let (n, _) = rx.recv_from(&mut buf).expect("a newcomer is answered");
    assert!(t0.elapsed() < Duration::from_secs(2));
    let packet = Packet::from_line(buf[..n].strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(packet.kind, "kdeconnect.identity");
    assert_eq!(packet.body["deviceId"], phone.id.as_str());

    // A burst of further newcomers is answered at most once per WAKE_GAP (5 s): quiet for the next second,
    // then exactly one more announcement, not one per datagram.
    for i in 0..10u16 {
        udp.send_to(&datagram('m', 40010 + i), &to).unwrap();
    }
    rx.set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    assert!(
        rx.recv_from(&mut buf).is_err(),
        "the burst did not cause an announcement storm"
    );
    rx.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
    rx.recv_from(&mut buf)
        .expect("one deferred announcement for the burst");
    rx.set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    assert!(rx.recv_from(&mut buf).is_err(), "and only one");
    phone.stop();
}
