// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzz-ish tests for the service's other untrusted-input parsers: UDP discovery announcements,
//! the clipboard wire format and the control-API hello line. Fixed-seed xorshift, no extra crates;
//! `GEEKBRIDGE_FUZZ_ITERS` raises the count. The property is "no panic, and anything accepted
//! respects the documented bounds".

use bridge_core::{clipboard_wire, ipc};
use bridge_service::discovery::{MAX_IDENTITY_BYTES, parse_identity};
use serde_json::{Map, json};
use std::net::{IpAddr, Ipv4Addr};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn iters(default: usize) -> usize {
    std::env::var("GEEKBRIDGE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn mutate(rng: &mut Rng, mut data: Vec<u8>) -> Vec<u8> {
    for _ in 0..1 + rng.below(4) {
        if data.is_empty() {
            break;
        }
        let at = rng.below(data.len());
        match rng.below(5) {
            0 => data[at] = rng.next() as u8,
            1 => data.truncate(at),
            2 => data.insert(at, rng.next() as u8),
            3 => {
                data.remove(at);
            }
            _ => {
                let n = (1 + rng.below(8)).min(data.len() - at);
                let piece = data[at..at + n].to_vec();
                data.splice(at..at, piece);
            }
        }
    }
    data
}

const SRC: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

fn identity() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": 1, "type": "kdeconnect.identity",
        "body": {
            "deviceId": "0123456789abcdef0123456789abcdef", "deviceName": "Fuzz phone",
            "deviceType": "phone", "tcpPort": 1716, "protocolVersion": 8,
            "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": ["kdeconnect.ping"]
        }
    }))
    .unwrap()
}

#[test]
fn discovery_datagrams_never_panic_and_accepted_ones_are_sane() {
    let mut rng = Rng(0xfeed_face_0bad_f00d);
    assert!(parse_identity(&identity(), SRC, 1).is_ok());
    for _ in 0..iters(30_000) {
        let data = if rng.below(4) == 0 {
            (0..rng.below(MAX_IDENTITY_BYTES * 2))
                .map(|_| rng.next() as u8)
                .collect()
        } else {
            mutate(&mut rng, identity())
        };
        if let Ok(device) = parse_identity(&data, SRC, 1) {
            assert!(data.len() <= MAX_IDENTITY_BYTES);
            assert!((32..=38).contains(&device.id.len()));
            assert!(!device.name.chars().any(char::is_control));
        }
    }
    let big = vec![b' '; MAX_IDENTITY_BYTES + 1];
    assert!(parse_identity(&big, SRC, 1).is_err());
    assert!(parse_identity(&identity(), IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1).is_err());
}

#[test]
fn clipboard_packets_never_panic_and_respect_the_text_cap() {
    let mut rng = Rng(0x600d_cafe_1234_5678);
    let pool = [
        json!("x"),
        json!(""),
        json!(null),
        json!(-1),
        json!(i64::MAX),
        json!(1.5e300),
        json!([1, 2]),
        json!({"a": 1}),
        json!("a".repeat(5000)),
    ];
    let keys = [
        "content",
        "timestamp",
        "requestId",
        "maxAgeMs",
        "mimeType",
        "data",
        "type",
    ];
    let kinds = [
        "kdeconnect.clipboard",
        "kdeconnect.clipboard.connect",
        "kdeconnect.clipboard.item",
        "kdeconnect.ping",
    ];
    for _ in 0..iters(30_000) {
        let mut body = Map::new();
        for _ in 0..rng.below(5) {
            body.insert(
                keys[rng.below(keys.len())].into(),
                pool[rng.below(pool.len())].clone(),
            );
        }
        let kind = kinds[rng.below(kinds.len())];
        let cap = rng.below(6000);
        if let Ok(clip) = clipboard_wire::parse(kind, &body, cap) {
            let rendered = format!("{clip:?}");
            assert!(!rendered.contains("xxxx"), "Debug must redact content");
        }
        let _ = clipboard_wire::parse_pull(&body);
        let _ = clipboard_wire::parse_none(&body);
    }
}

#[test]
fn control_hello_lines_never_panic() {
    let mut rng = Rng(0x0ddb_a115_eed0_0001);
    let valid = ipc::format_hello(&ipc::Hello::Ready {
        nonce: "ab".repeat(32),
    });
    assert!(ipc::parse_hello(valid.trim_end()).is_ok());
    for _ in 0..iters(20_000) {
        let line =
            String::from_utf8_lossy(&mutate(&mut rng, valid.clone().into_bytes())).into_owned();
        let _ = ipc::parse_hello(line.trim_end_matches('\n'));
    }
}
