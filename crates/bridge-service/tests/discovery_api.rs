//! Black-box tests of the passive discovery pipeline: UDP identity datagram -> bounded cache ->
//! loopback HTTP/1.0 snapshot API -> `bridge_core::fetch_snapshot`.
//!
//! Everything binds to 127.0.0.1 on ephemeral ports. Nothing here needs a phone, a LAN or a
//! paired device, and nothing asserts that pairing or any phone feature exists.
//!
//! Tests named `*_known_gap_*` are `#[ignore]`d: they state the behaviour we *want* and currently
//! fail. They are tracked in `docs/architecture.md`; un-ignore them when fixed.
//! Run them with `cargo test -p bridge-service --test discovery_api -- --ignored`.
use bridge_core::{ConnectionState, Device, MAX_API_BYTES, MAX_DEVICES, Snapshot, fetch_snapshot};
use bridge_service::{
    DEVICE_TTL_MS, SharedState, discovery, expire_devices, listen_discovery, now_ms, serve_api,
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::RwLock,
    task::JoinHandle,
    time,
};

const GOOD_ID: &str = "0123456789abcdef0123456789abcdef";

/// A running listener + API pair on loopback. Tasks are aborted on drop.
struct Service {
    udp: SocketAddr,
    api_port: u16,
    state: SharedState,
    tasks: Vec<JoinHandle<io::Result<()>>>,
}
impl Service {
    async fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp = socket.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_port = listener.local_addr().unwrap().port();
        let state: SharedState = Arc::new(RwLock::new(Snapshot::empty(udp.port())));
        let tasks = vec![
            tokio::spawn(listen_discovery(socket, state.clone())),
            tokio::spawn(serve_api(listener, state.clone())),
        ];
        Self {
            udp,
            api_port,
            state,
            tasks,
        }
    }
    /// API only, no UDP listener: lets a test control cache contents directly.
    async fn api_only(devices: Vec<Device>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_port = listener.local_addr().unwrap().port();
        let mut snapshot = Snapshot::empty(1716);
        snapshot.devices = devices;
        let state: SharedState = Arc::new(RwLock::new(snapshot));
        let tasks = vec![tokio::spawn(serve_api(listener, state.clone()))];
        Self {
            udp: "127.0.0.1:9".parse().unwrap(),
            api_port,
            state,
            tasks,
        }
    }
    async fn send_from(&self, sender: &UdpSocket, datagram: &[u8]) {
        sender.send_to(datagram, self.udp).await.unwrap();
    }
    async fn fetch(&self) -> io::Result<Snapshot> {
        let port = self.api_port;
        tokio::task::spawn_blocking(move || fetch_snapshot(port))
            .await
            .unwrap()
    }
    async fn raw(&self, request: &[u8]) -> Vec<u8> {
        raw_exchange(self.api_port, request)
            .await
            .unwrap_or_default()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// What a KDE Connect Android v8 phone broadcasts (compact JSON, `id` is a ms timestamp, trailing LF).
fn identity_json(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({"id": 1730000000000u64, "type": "kdeconnect.identity", "body": {
        "deviceId": id, "deviceName": name, "deviceType": "phone", "protocolVersion": 8, "tcpPort": 1716,
        "incomingCapabilities": ["kdeconnect.ping", "kdeconnect.battery.request"],
        "outgoingCapabilities": ["kdeconnect.ping", "kdeconnect.battery"],
    }})
}
fn datagram(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn padded(mut bytes: Vec<u8>, total: usize) -> Vec<u8> {
    assert!(bytes.len() <= total);
    bytes.resize(total, b' ');
    bytes
}
fn id_n(n: usize) -> String {
    format!("{n:032x}")
}

/// Put an identity datagram into the cache the way the listener does, minus the per-source limiter.
async fn seed(service: &Service, datagram: &[u8]) {
    let device = discovery::parse_identity(datagram, Ipv4Addr::LOCALHOST.into(), now_ms()).unwrap();
    bridge_service::record_device(&mut *service.state.write().await, device);
}

async fn wait_for(what: &str, mut cond: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        time::sleep(Duration::from_millis(20)).await;
    }
}

/// Connect, send `request`, read until the server closes or 5 s pass. Reset after the write counts as closed.
async fn raw_exchange(port: u16, request: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    let _ = stream.write_all(request).await;
    let mut bytes = Vec::new();
    let read = time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes)).await;
    assert!(read.is_ok(), "server kept the connection open for >5s");
    Ok(bytes) // a read error (RST) leaves whatever arrived before it
}
fn split_response(bytes: &[u8]) -> (String, Vec<u8>) {
    let at = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no header terminator");
    (
        String::from_utf8_lossy(&bytes[..at]).into_owned(),
        bytes[at + 4..].to_vec(),
    )
}

fn device(id: &str, name: &str, last_seen: u64) -> Device {
    Device {
        id: id.into(),
        name: name.into(),
        device_type: "phone".into(),
        address: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 8)),
        tcp_port: 1716,
        protocol_version: 8,
        incoming_capabilities: vec![],
        outgoing_capabilities: vec![],
        state: ConnectionState::Discovered,
        last_seen_unix_ms: last_seen,
    }
}

// ---------------------------------------------------------------------------------------------
// UDP -> cache -> API
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn udp_identity_is_served_through_the_local_api_as_discovered_only() {
    let service = Service::start().await;
    assert!(
        service.fetch().await.unwrap().devices.is_empty(),
        "fresh service must list nothing"
    );

    let before = now_ms();
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    service
        .send_from(&sender, &datagram(&identity_json(GOOD_ID, "Pixel 8")))
        .await;
    wait_for("device visible through the API", async || {
        !service.fetch().await.unwrap().devices.is_empty()
    })
    .await;
    let after = now_ms();

    let snapshot = service.fetch().await.unwrap();
    assert_eq!(snapshot.api_version, bridge_core::API_VERSION);
    assert_eq!(snapshot.discovery_port, service.udp.port());
    assert_eq!(snapshot.discovery_error, None);
    let [d] = snapshot.devices.as_slice() else {
        panic!("expected exactly one device: {:?}", snapshot.devices)
    };
    assert_eq!(
        (d.id.as_str(), d.name.as_str(), d.device_type.as_str()),
        (GOOD_ID, "Pixel 8", "phone")
    );
    assert_eq!(
        d.address,
        sender.local_addr().unwrap().ip(),
        "address must come from the datagram source, never the body"
    );
    assert_eq!((d.tcp_port, d.protocol_version), (1716, 8));
    assert_eq!(
        d.incoming_capabilities,
        ["kdeconnect.ping", "kdeconnect.battery.request"]
    );
    assert_eq!(
        d.outgoing_capabilities,
        ["kdeconnect.ping", "kdeconnect.battery"]
    );
    assert_eq!(
        d.state,
        ConnectionState::Discovered,
        "discovery must never imply trust"
    );
    assert!(
        (before..=after).contains(&d.last_seen_unix_ms),
        "last_seen must be stamped on receipt"
    );
}

#[tokio::test]
async fn body_cannot_choose_the_recorded_address() {
    // A hostile body that tries to smuggle its own address/state is ignored; only the UDP source counts.
    let service = Service::start().await;
    let mut value = identity_json(GOOD_ID, "Pixel 8");
    value["body"]["address"] = "10.9.9.9".into();
    value["body"]["state"] = "paired".into();
    value["body"]["lastSeenUnixMs"] = 1.into();
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    service.send_from(&sender, &datagram(&value)).await;
    wait_for("device cached", async || {
        !service.state.read().await.devices.is_empty()
    })
    .await;
    let d = service.fetch().await.unwrap().devices.remove(0);
    assert_eq!(d.address, sender.local_addr().unwrap().ip());
    assert_eq!(d.state, ConnectionState::Discovered);
    assert!(d.last_seen_unix_ms > 1_000_000_000_000);
}

#[tokio::test]
async fn malformed_and_oversized_datagrams_are_dropped_without_killing_the_listener() {
    let service = Service::start().await;
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let valid = |id: &str| serde_json::to_vec(&identity_json(id, "Phone")).unwrap();
    let max = discovery::MAX_IDENTITY_BYTES;

    let mut bad: Vec<(&str, Vec<u8>)> = vec![
        ("empty datagram", vec![]),
        ("binary garbage", (0..=255u8).cycle().take(900).collect()),
        ("truncated json", valid(&id_n(1))[..40].to_vec()),
        ("json array", b"[]".to_vec()),
        ("json null", b"null".to_vec()),
        (
            "deep nesting",
            [vec![b'['; 5000], vec![b']'; 5000]].concat(),
        ),
        (
            "wrong packet type",
            serde_json::to_vec(&serde_json::json!({"type":"kdeconnect.pair","body":{"pair":true}}))
                .unwrap(),
        ),
        (
            "missing body",
            br#"{"type":"kdeconnect.identity"}"#.to_vec(),
        ),
        ("one byte over limit", padded(valid(&id_n(2)), max + 1)),
        (
            "far over limit (truncated by recv buffer)",
            padded(valid(&id_n(3)), 60_000),
        ),
        (
            "valid json then garbage",
            [valid(&id_n(4)), b"\x00\xff garbage".to_vec()].concat(),
        ),
    ];
    for (key, value) in [
        ("tcpPort", serde_json::json!(70000)),
        ("tcpPort", serde_json::json!(-1)),
        ("deviceId", serde_json::json!(12345)),
        ("deviceName", serde_json::json!(null)),
        ("incomingCapabilities", serde_json::json!("kdeconnect.ping")),
    ] {
        let mut packet = identity_json(&id_n(5), "Phone");
        packet["body"][key] = value;
        bad.push((
            "wrongly typed/ranged field",
            serde_json::to_vec(&packet).unwrap(),
        ));
    }
    for (_, bytes) in &bad {
        service.send_from(&sender, bytes).await;
    }

    // Boundary: exactly MAX_IDENTITY_BYTES is accepted. It is also the sentinel proving that all the
    // earlier datagrams from this socket were processed (loopback UDP from one socket is ordered).
    service
        .send_from(&sender, &padded(valid(&id_n(6)), max))
        .await;
    wait_for("boundary-size identity cached", async || {
        !service.state.read().await.devices.is_empty()
    })
    .await;

    let ids: Vec<String> = service
        .fetch()
        .await
        .unwrap()
        .devices
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(
        ids,
        [id_n(6)],
        "only the identity exactly at the size limit may be accepted"
    );
    assert!(
        service.tasks.iter().all(|t| !t.is_finished()),
        "a hostile datagram ended a service task"
    );
}

#[tokio::test]
async fn unspecified_and_multicast_sources_are_not_cached() {
    // recv_from never yields these from a real socket, so this guards the parser contract directly.
    let bytes = datagram(&identity_json(GOOD_ID, "Phone"));
    for source in ["0.0.0.0", "224.0.0.251", "ff02::fb", "::"] {
        let ip: IpAddr = source.parse().unwrap();
        assert!(
            discovery::parse_identity(&bytes, ip, 1).is_err(),
            "accepted source {source}"
        );
    }
}

#[tokio::test]
async fn repeated_identity_dedups_by_device_id_and_refreshes_the_record() {
    let service = Service::start().await;
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for _ in 0..25 {
        service
            .send_from(&sender, &datagram(&identity_json(GOOD_ID, "Old name")))
            .await;
    }
    wait_for("first record", async || {
        !service.state.read().await.devices.is_empty()
    })
    .await;
    let first_seen = service.state.read().await.devices[0].last_seen_unix_ms;

    time::sleep(Duration::from_millis(15)).await;
    service
        .send_from(&sender, &datagram(&identity_json(GOOD_ID, "New name")))
        .await;
    wait_for("record renamed", async || {
        service.state.read().await.devices[0].name == "New name"
    })
    .await;

    let devices = service.fetch().await.unwrap().devices;
    assert_eq!(
        devices.len(),
        1,
        "26 datagrams for one deviceId must be one record"
    );
    assert!(
        devices[0].last_seen_unix_ms > first_seen,
        "re-announcement must refresh last_seen"
    );
}

#[tokio::test]
async fn cache_is_bounded_and_first_come_first_kept() {
    let service = Service::start().await;
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let flood = MAX_DEVICES + 40;
    // Seeded through the parser and `record_device` directly: the listener's per-source limiter (see
    // `one_source_cannot_flood_the_discovery_list`) would otherwise stop a single loopback sender at a few ids.
    for n in 0..flood {
        seed(
            &service,
            &datagram(&identity_json(&id_n(n), &format!("Phone {n:03}"))),
        )
        .await;
    }
    wait_for("cache filled to the cap", async || {
        service.state.read().await.devices.len() == MAX_DEVICES
    })
    .await;
    // Give stragglers a chance to (incorrectly) push past the cap.
    time::sleep(Duration::from_millis(150)).await;

    let snapshot = service
        .fetch()
        .await
        .expect("a full-but-small cache must still be servable");
    assert_eq!(snapshot.devices.len(), MAX_DEVICES);
    let mut ids: Vec<_> = snapshot.devices.iter().map(|d| d.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), MAX_DEVICES, "duplicate ids in snapshot");
    assert!(
        !ids.contains(&id_n(flood - 1)),
        "late newcomers are dropped, not evicting earlier entries"
    );

    // A device already in the cache can still be refreshed when the cache is full.
    service
        .send_from(
            &sender,
            &datagram(&identity_json(&id_n(0), "Renamed while full")),
        )
        .await;
    wait_for("existing entry updated while full", async || {
        service
            .state
            .read()
            .await
            .devices
            .iter()
            .any(|d| d.id == id_n(0) && d.name == "Renamed while full")
    })
    .await;
    assert_eq!(service.state.read().await.devices.len(), MAX_DEVICES);
}

#[tokio::test]
async fn one_udp_source_can_list_only_a_few_device_ids() {
    let service = Service::start().await;
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for n in 0..40 {
        service
            .send_from(&sender, &datagram(&identity_json(&id_n(n), "Invented")))
            .await;
        tokio::task::yield_now().await;
    }
    time::sleep(Duration::from_millis(300)).await;
    let listed = service.state.read().await.devices.len();
    assert!(
        (1..=4).contains(&listed),
        "one source listed {listed} invented phones"
    );
}

#[tokio::test]
async fn same_device_id_from_a_second_source_address_overwrites_the_record() {
    // Characterisation of the trust boundary (finding F2): the cache keys on the *claimed* deviceId
    // only, so any host that can reach UDP 1716 re-points an existing id at its own address.
    // If a source-binding policy is added, this test is the one to flip.
    let service = Service::start().await;
    let Ok(second) = UdpSocket::bind("127.0.0.2:0").await else {
        eprintln!("skipping: cannot bind 127.0.0.2 on this platform");
        return;
    };
    let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    service
        .send_from(&first, &datagram(&identity_json(GOOD_ID, "Real phone")))
        .await;
    wait_for("first record", async || {
        !service.state.read().await.devices.is_empty()
    })
    .await;
    service
        .send_from(&second, &datagram(&identity_json(GOOD_ID, "Impostor")))
        .await;
    wait_for("overwritten", async || {
        service.state.read().await.devices[0].name == "Impostor"
    })
    .await;

    let devices = service.fetch().await.unwrap().devices;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].address, second.local_addr().unwrap().ip());
}

// ---------------------------------------------------------------------------------------------
// Expiry
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn expiry_drops_only_records_older_than_the_ttl() {
    let now = now_ms();
    let service = Service::api_only(vec![
        device(&id_n(1), "Stale", now - DEVICE_TTL_MS - 1_000),
        device(&id_n(2), "Fresh", now),
        device(&id_n(3), "Nearly stale", now - DEVICE_TTL_MS + 60_000),
    ])
    .await;
    let expiry = tokio::spawn(expire_devices(service.state.clone())); // first tick fires immediately
    wait_for("stale entry expired", async || {
        service.state.read().await.devices.len() == 2
    })
    .await;
    expiry.abort();

    let names: Vec<String> = service
        .fetch()
        .await
        .unwrap()
        .devices
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["Fresh", "Nearly stale"]); // sorted by name
}

#[tokio::test]
async fn api_never_serves_a_record_older_than_the_ttl_even_without_the_expiry_task() {
    let service = Service::api_only(vec![device(
        &id_n(1),
        "Stale",
        now_ms() - DEVICE_TTL_MS - 1_000,
    )])
    .await;
    // No expiry task running at all: the API itself must not present an expired record as live.
    assert!(service.fetch().await.unwrap().devices.is_empty());
}

#[tokio::test]
async fn expiry_survives_a_backwards_clock_step() {
    // Simulates: record stamped at T, then the system clock is set 1 h back so now < last_seen.
    let service = Service::api_only(vec![device(
        &id_n(1),
        "Time traveller",
        now_ms() + 3_600_000,
    )])
    .await;
    let expiry = tokio::spawn(expire_devices(service.state.clone()));
    time::sleep(Duration::from_millis(300)).await;
    expiry.abort();
    assert!(
        service.state.read().await.devices.is_empty(),
        "record from the 'future' was never expired"
    );
}

// ---------------------------------------------------------------------------------------------
// API exposure
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn api_answers_only_the_exact_get_and_never_leaks_state_on_rejection() {
    let service = Service::api_only(vec![device(GOOD_ID, "Secretly named phone", now_ms())]).await;
    let rejected: &[(&str, &[u8])] = &[
        ("POST", b"POST /v1/discovery HTTP/1.0\r\n\r\n"),
        ("PUT", b"PUT /v1/discovery HTTP/1.0\r\n\r\n"),
        ("DELETE", b"DELETE /v1/discovery HTTP/1.0\r\n\r\n"),
        ("HEAD", b"HEAD /v1/discovery HTTP/1.0\r\n\r\n"),
        (
            "OPTIONS (CORS preflight)",
            b"OPTIONS /v1/discovery HTTP/1.0\r\n\r\n",
        ),
        ("lowercase method", b"get /v1/discovery HTTP/1.0\r\n\r\n"),
        ("command path", b"POST /v1/send HTTP/1.0\r\n\r\n"),
        ("root", b"GET / HTTP/1.0\r\n\r\n"),
        ("trailing slash", b"GET /v1/discovery/ HTTP/1.0\r\n\r\n"),
        (
            "query string",
            b"GET /v1/discovery?callback=x HTTP/1.0\r\n\r\n",
        ),
        (
            "fragment-ish suffix",
            b"GET /v1/discovery%00 HTTP/1.0\r\n\r\n",
        ),
        ("other version", b"GET /v2/discovery HTTP/1.0\r\n\r\n"),
        ("traversal", b"GET /v1/../v1/discovery HTTP/1.0\r\n\r\n"),
        (
            "absolute-form URI",
            b"GET http://127.0.0.1/v1/discovery HTTP/1.0\r\n\r\n",
        ),
        (
            "browser-style HTTP/1.1 with Host",
            b"GET /v1/discovery HTTP/1.1\r\nHost: evil.example:17380\r\n\r\n",
        ),
        (
            "request with body",
            b"GET /v1/discovery HTTP/1.0\r\nContent-Length: 2\r\n\r\n{}",
        ),
        (
            "request smuggling pipeline",
            b"GET /v1/nothing HTTP/1.0\r\n\r\nGET /v1/discovery HTTP/1.0\r\n\r\n",
        ),
        (
            "junk line",
            b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03\r\n\r\n",
        ), // TLS ClientHello prefix
    ];
    for (label, request) in rejected {
        let bytes = service.raw(request).await;
        let (head, body) = split_response(&bytes);
        assert!(
            head.starts_with("HTTP/1.0 404"),
            "{label}: unexpected response {head:?}"
        );
        assert_eq!(body, b"{}", "{label}: rejection must not carry state");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(GOOD_ID),
            "{label}: leaked device data"
        );
    }
}

#[tokio::test]
async fn successful_response_is_a_non_cacheable_same_origin_only_json_document() {
    let service = Service::api_only(vec![device(GOOD_ID, "Phone", now_ms())]).await;
    let bytes = service.raw(b"GET /v1/discovery HTTP/1.0\r\n\r\n").await;
    let (head, body) = split_response(&bytes);
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("HTTP/1.0 200 OK"));
    assert!(lower.contains("content-type: application/json"));
    assert!(lower.contains("cache-control: no-store"));
    assert!(
        lower.contains(&format!("content-length: {}", body.len())),
        "Content-Length must match: {head}"
    );
    assert!(
        !lower.contains("access-control-allow"),
        "CORS must stay closed so web pages cannot read the API"
    );
    assert!(body.len() <= MAX_API_BYTES);
    let snapshot: Snapshot = serde_json::from_slice(&body).unwrap();
    assert_eq!(snapshot.devices.len(), 1);
    // Discovery data only: nothing but the documented fields is exposed.
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let mut keys: Vec<_> = value["devices"][0]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "address",
            "device_type",
            "id",
            "incoming_capabilities",
            "last_seen_unix_ms",
            "name",
            "outgoing_capabilities",
            "protocol_version",
            "state",
            "tcp_port"
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Resource exhaustion: requests
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn oversized_request_is_cut_off_without_a_response_and_service_stays_up() {
    let service = Service::api_only(vec![device(GOOD_ID, "Phone", now_ms())]).await;
    let started = Instant::now();
    let no_terminator = vec![b'A'; 64 * 1024];
    let padded_header = [
        b"GET /v1/discovery HTTP/1.0\r\nX-Pad: ".as_slice(),
        &vec![b'a'; 4096],
        b"\r\n\r\n",
    ]
    .concat();
    for (label, request) in [
        ("no terminator", no_terminator),
        ("valid request line, huge header", padded_header),
    ] {
        let reply = service.raw(&request).await;
        assert!(
            reply.is_empty(),
            "{label}: server answered an oversized request: {:?}",
            String::from_utf8_lossy(&reply[..reply.len().min(60)])
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "oversized requests must be rejected on size, not on timeout"
    );
    assert_eq!(
        service.fetch().await.unwrap().devices.len(),
        1,
        "service must keep working"
    );
}

#[tokio::test]
async fn silent_and_trickling_clients_are_dropped_within_the_request_deadline() {
    let service = Service::api_only(vec![device(GOOD_ID, "Phone", now_ms())]).await;

    // 1. Connects and says nothing.
    let started = Instant::now();
    let mut idle = TcpStream::connect((Ipv4Addr::LOCALHOST, service.api_port))
        .await
        .unwrap();
    let mut buf = Vec::new();
    let closed = time::timeout(Duration::from_secs(5), idle.read_to_end(&mut buf)).await;
    assert!(closed.is_ok(), "idle connection was never closed");
    assert!(buf.is_empty());
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "idle client held a slot for {:?}",
        started.elapsed()
    );

    // 2. A valid request delivered one byte per 150 ms (31 bytes ~ 4.6 s) must not be honoured.
    let request = b"GET /v1/discovery HTTP/1.0\r\n\r\n";
    let started = Instant::now();
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, service.api_port))
        .await
        .unwrap();
    let (mut rd, mut wr) = stream.into_split();
    let reader = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = rd.read_to_end(&mut b).await;
        (b, Instant::now())
    });
    for byte in request {
        if wr.write_all(&[*byte]).await.is_err() {
            break;
        }
        time::sleep(Duration::from_millis(150)).await;
    }
    let (reply, closed_at) = time::timeout(Duration::from_secs(5), reader)
        .await
        .expect("trickle never closed")
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&reply).contains("200 OK"),
        "slow-drip request was served"
    );
    assert!(
        closed_at.duration_since(started) < Duration::from_millis(3500),
        "total request deadline not enforced"
    );
}

#[tokio::test]
async fn idle_connections_can_stall_clients_but_only_for_a_bounded_time() {
    // The API caps concurrency at 16. 16 idle sockets (any local process can open them) therefore
    // starve real clients -- but only until the per-request deadline expires. Finding F3 is about the
    // starvation; this test pins the *bounded* part so a regression to "forever" is caught.
    let service = Service::api_only(vec![device(GOOD_ID, "Phone", now_ms())]).await;
    let mut hogs = Vec::new();
    for _ in 0..16 {
        hogs.push(
            TcpStream::connect((Ipv4Addr::LOCALHOST, service.api_port))
                .await
                .unwrap(),
        );
    }
    time::sleep(Duration::from_millis(100)).await; // let the server accept them all

    let blocked = Instant::now();
    let during = service.fetch().await; // client read timeout is 1 s; permits are all held
    assert!(
        during.is_err(),
        "expected the saturated API to time the client out"
    );
    assert!(blocked.elapsed() < Duration::from_secs(3));

    for hog in &mut hogs {
        let mut sink = Vec::new();
        time::timeout(Duration::from_secs(5), hog.read_to_end(&mut sink))
            .await
            .expect("hog never evicted")
            .ok();
    }
    assert_eq!(
        service
            .fetch()
            .await
            .expect("API must recover once idle sockets are evicted")
            .devices
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------------------------
// Resource exhaustion: response size
// ---------------------------------------------------------------------------------------------

/// ~13 KiB identity that is legal under every per-field rule (100 capabilities of 128 bytes).
fn heavy_identity(n: usize) -> Vec<u8> {
    let mut value = identity_json(&id_n(n), "Heavy");
    let caps: Vec<String> = (0..100)
        .map(|i| format!("{i:03}{}", "c".repeat(125)))
        .collect();
    value["body"]["incomingCapabilities"] = caps.into();
    let bytes = datagram(&value);
    assert!(
        bytes.len() < discovery::MAX_IDENTITY_BYTES,
        "fixture must be a legal identity"
    );
    bytes
}

async fn flood_with_heavy_devices(service: &Service, count: usize) {
    for n in 0..count {
        seed(service, &heavy_identity(n)).await;
    }
    wait_for("heavy devices cached", async || {
        service.state.read().await.devices.len() == count
    })
    .await;
}

#[tokio::test]
async fn api_never_emits_more_than_max_api_bytes_even_when_the_cache_is_heavy() {
    let service = Service::start().await;
    flood_with_heavy_devices(&service, 30).await; // 30 x ~13 KiB > 256 KiB
    let cached: usize = serde_json::to_vec(&*service.state.read().await)
        .unwrap()
        .len();
    assert!(
        cached > MAX_API_BYTES,
        "fixture too small to exercise the limit ({cached} bytes)"
    );

    let bytes = service.raw(b"GET /v1/discovery HTTP/1.0\r\n\r\n").await;
    assert!(
        bytes.len() <= MAX_API_BYTES,
        "server wrote {} bytes",
        bytes.len()
    );
    let snapshot = service
        .fetch()
        .await
        .expect("oversized cache must degrade to a smaller snapshot, not an error");
    assert!(snapshot.devices.len() <= MAX_DEVICES);
}

#[tokio::test]
async fn api_stays_available_under_a_heavy_identity_flood() {
    let service = Service::start().await;
    flood_with_heavy_devices(&service, 30).await;
    let snapshot = service
        .fetch()
        .await
        .expect("one LAN host must not be able to blank the local API");
    assert!(!snapshot.devices.is_empty());
    assert_eq!(
        snapshot.devices.len(),
        30,
        "device identities must survive capability truncation"
    );
    assert!(snapshot.capabilities_truncated);
}

#[tokio::test]
async fn heavy_flood_does_not_hide_an_established_device() {
    let service = Service::start().await;
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    service
        .send_from(&sender, &datagram(&identity_json(GOOD_ID, "Zed's phone")))
        .await;
    wait_for("real phone cached", async || {
        !service.state.read().await.devices.is_empty()
    })
    .await;
    flood_with_heavy_devices_after(&service, &sender, 30).await;

    let snapshot = service.fetch().await.unwrap();
    assert!(
        snapshot.devices.iter().any(|d| d.id == GOOD_ID),
        "established phone was truncated out of the API"
    );
}
async fn flood_with_heavy_devices_after(service: &Service, sender: &UdpSocket, count: usize) {
    for n in 0..count {
        service.send_from(sender, &heavy_identity(n + 1000)).await;
        tokio::task::yield_now().await;
    }
    time::sleep(Duration::from_millis(300)).await;
}

// ---------------------------------------------------------------------------------------------
// Parser interoperability with the pinned KDE Connect references
// ---------------------------------------------------------------------------------------------

#[test]
fn parser_accepts_identity_variants_the_kde_reference_accepts() {
    // the KDE Connect sources core/deviceinfo.h: toUdpDiscoveryPacket() sends ONLY
    // deviceId/deviceName/protocolVersion/tcpPort; DeviceType::FromString maps "smartphone" to phone
    // and anything unknown to Unknown rather than rejecting the packet; filterName() truncates.
    let src: IpAddr = "192.168.1.8".parse().unwrap();
    let kde_desktop_udp = serde_json::json!({"id": 1, "type": "kdeconnect.identity", "body": {
        "deviceId": GOOD_ID, "deviceName": "Desktop", "protocolVersion": 8, "tcpPort": 1716}});
    let mut smartphone = identity_json(GOOD_ID, "Phone");
    smartphone["body"]["deviceType"] = "smartphone".into();
    let mut long_name = identity_json(GOOD_ID, "Phone");
    long_name["body"]["deviceName"] = "n".repeat(100).into();
    let mut no_port = identity_json(GOOD_ID, "Phone");
    no_port["body"].as_object_mut().unwrap().remove("tcpPort");
    let failures: Vec<_> = [
        ("kde desktop v8 udp", kde_desktop_udp),
        ("smartphone alias", smartphone),
        ("name longer than 64 (KDE truncates to 32)", long_name),
        ("missing tcpPort (Android defaults to 1716)", no_port),
    ]
    .into_iter()
    .filter(|(_, v)| discovery::parse_identity(&datagram(v), src, 1).is_err())
    .map(|(label, _)| label)
    .collect();
    assert!(
        failures.is_empty(),
        "rejected but KDE accepts: {failures:?}"
    );
}

#[test]
fn parser_accepts_what_current_android_actually_broadcasts() {
    // Shape taken from the KDE Connect sources DeviceInfo.toIdentityPacket() + LanLinkProvider
    // (tcpPort added, LF terminated, numeric ms `id`, unknown extra body keys must be tolerated).
    let mut value = identity_json("_a1b2c3d4_e5f6_4789_abcd_ef0123456789", "Pixel 8 Pro");
    value["body"]["someFutureField"] = serde_json::json!({"nested": [1, 2, 3]});
    value["payloadSize"] = 0.into();
    let device =
        discovery::parse_identity(&datagram(&value), "192.168.1.8".parse().unwrap(), 7).unwrap();
    assert_eq!(device.id.len(), 37);
    assert_eq!(device.name, "Pixel 8 Pro");
}
