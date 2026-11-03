// SPDX-License-Identifier: GPL-3.0-or-later
//! The Wi-Fi bind safety net against a listener that accepts the bound dial and then never answers (the E2E-14
//! failure: a bound socket that connects and then dies). Linux only: 127.0.0.2 stands in for the Wi-Fi address and
//! the listener tells the two source addresses apart.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bridge_kde::{
    AddressPolicy, DeviceCredentials, DeviceId, DeviceType, Endpoint, MemoryTrustStore,
    TransportConfig,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::bindhealth;
use crate::Dispatcher;

const WIFI: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

fn req(action: &str, fields: Value) -> String {
    let mut o = serde_json::Map::new();
    o.insert("version".into(), json!(1));
    o.insert("action".into(), json!(action));
    if let Value::Object(f) = fields {
        o.extend(f);
    }
    Value::Object(o).to_string()
}

fn call(d: &Dispatcher, action: &str, fields: Value) -> Value {
    let v: Value = serde_json::from_str(&d.handle(&req(action, fields))).unwrap();
    assert_eq!(v["ok"], true, "{action}: {v}");
    v["result"].clone()
}

/// A computer whose listener blackholes every connection that arrives from the Wi-Fi address and serves the rest.
struct Desk {
    _rt: tokio::runtime::Runtime,
    id: DeviceId,
    addr: SocketAddr,
    sources: Arc<Mutex<Vec<IpAddr>>>,
}

impl Desk {
    fn start() -> Desk {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let id = DeviceId::parse(&"d".repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        let ep = Arc::new(
            Endpoint::new(
                Arc::new(creds),
                "Bind Desk",
                DeviceType::Desktop,
                vec!["kdeconnect.ping".to_owned()],
                vec!["kdeconnect.ping".to_owned()],
                Arc::new(MemoryTrustStore::new()),
                TransportConfig {
                    address_policy: AddressPolicy {
                        local_networks: true,
                        loopback: true,
                    },
                    ..TransportConfig::default()
                },
            )
            .unwrap(),
        );
        let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();
        let sources = Arc::new(Mutex::new(Vec::new()));
        let seen = sources.clone();
        rt.spawn(async move {
            let mut held = Vec::new(); // keeps every accepted socket and link open
            loop {
                let Ok((stream, peer)) = listener.accept().await else {
                    return;
                };
                seen.lock().unwrap().push(peer.ip());
                if peer.ip() == IpAddr::V4(WIFI) {
                    held.push(Held::Silent(stream)); // accepted, never answered
                } else {
                    let ep = ep.clone();
                    let hs = tokio::spawn(async move { ep.accept(stream).await });
                    held.push(Held::Serving(hs));
                }
            }
        });
        Desk {
            _rt: rt,
            id,
            addr,
            sources,
        }
    }

    fn bound_attempts(&self) -> usize {
        self.sources
            .lock()
            .unwrap()
            .iter()
            .filter(|ip| **ip == IpAddr::V4(WIFI))
            .count()
    }
}

#[allow(dead_code)]
enum Held {
    Silent(tokio::net::TcpStream),
    Serving(tokio::task::JoinHandle<bridge_kde::Result<bridge_kde::Handshake>>),
}

fn start_phone(dir: &std::path::Path) -> Dispatcher {
    let d = Dispatcher::new();
    call(
        &d,
        "transport_start",
        json!({
            "state_dir": dir.to_str().unwrap(),
            "device_name": "Bind Phone",
            "listen": { "host": "127.9.9.9", "port": 0 },
            "allow_loopback": true,
        }),
    );
    d
}

fn wait_connected(d: &Dispatcher, within: Duration) -> Value {
    let end = Instant::now() + within;
    loop {
        let s = call(d, "transport_status", json!({}));
        if !s["connections"].as_array().unwrap().is_empty() {
            return s;
        }
        assert!(Instant::now() < end, "no connection within {within:?}: {s}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connect(d: &Dispatcher, desk: &Desk) {
    call(
        d,
        "transport_connect",
        json!({"host": "127.0.0.1", "port": desk.addr.port(), "device_id": desk.id.to_string()}),
    );
}

#[test]
fn a_blackholed_bound_dial_is_retried_unbound_within_the_budget_and_not_tried_again() {
    let _g = bindhealth::test_lock();
    bindhealth::clear();
    bindhealth::set_enabled(true);
    super::set_wifi_bind(Some(WIFI));
    let desk = Desk::start();
    let dir = tempfile::tempdir().unwrap();
    let phone = start_phone(dir.path());

    let t0 = Instant::now();
    connect(&phone, &desk);
    let s = wait_connected(&phone, bindhealth::BOUND_BUDGET + Duration::from_secs(4));
    let took = t0.elapsed();
    assert!(
        took >= bindhealth::BOUND_BUDGET - Duration::from_secs(1),
        "the bound dial was tried first and given its budget: {took:?}"
    );
    assert_eq!(
        desk.bound_attempts(),
        1,
        "one bound attempt reached the listener"
    );
    assert!(
        bindhealth::is_unhealthy(WIFI),
        "the bound path is marked unhealthy"
    );
    let event = s["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "wifi_bind_fallback")
        .expect("the fallback is reported");
    assert_eq!(event["code"], bindhealth::REASON_STALLED);

    // Flapping: while the mark stands, every further dial (the redials of a flapping link) leaves unbound at once.
    for _ in 0..3 {
        let id = call(&phone, "transport_status", json!({}))["connections"][0]["connection_id"]
            .as_u64()
            .unwrap();
        call(
            &phone,
            "transport_disconnect",
            json!({ "connection_id": id }),
        );
        let end = Instant::now() + Duration::from_secs(5);
        while !call(&phone, "transport_status", json!({}))["connections"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            assert!(Instant::now() < end, "the link did not close");
            std::thread::sleep(Duration::from_millis(20));
        }
        let t = Instant::now();
        connect(&phone, &desk);
        wait_connected(&phone, Duration::from_secs(3));
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "no bound trial this time"
        );
    }
    assert_eq!(
        desk.bound_attempts(),
        1,
        "the bound path was not tried again"
    );

    call(&phone, "transport_stop", json!({}));
    super::set_wifi_bind(None);
    bindhealth::clear();
}

#[test]
fn switched_off_nothing_is_ever_bound() {
    let _g = bindhealth::test_lock();
    bindhealth::clear();
    super::set_wifi_bind(Some(WIFI));
    let desk = Desk::start();
    let dir = tempfile::tempdir().unwrap();
    let phone = start_phone(dir.path());
    let off = call(
        &phone,
        "transport_wifi_bind_enabled",
        json!({"enabled": false}),
    );
    assert_eq!(off["enabled"], false);
    connect(&phone, &desk);
    wait_connected(&phone, Duration::from_secs(3));
    assert_eq!(
        desk.bound_attempts(),
        0,
        "the kill switch keeps dials unbound"
    );
    call(
        &phone,
        "transport_wifi_bind_enabled",
        json!({"enabled": true}),
    );
    call(&phone, "transport_stop", json!({}));
    super::set_wifi_bind(None);
}
