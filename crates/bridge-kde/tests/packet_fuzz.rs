//! Fuzz-ish property tests for packet parsing and line framing.
//!
//! No external fuzzing crate: a fixed-seed xorshift generator drives the cases, so every failure
//! reproduces from the printed seed. `GEEKBRIDGE_FUZZ_ITERS` raises the iteration count (the soak
//! script sets it). The properties: the parser never panics or overflows the stack, whatever it
//! accepts re-serialises to the same packet, and the line reader reassembles any chunking.

use bridge_kde::{KdeError, Packet, packet::LineReader};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, duplex};

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
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        (0..self.below(max)).map(|_| self.next() as u8).collect()
    }
}

fn iters(default: usize) -> usize {
    std::env::var("GEEKBRIDGE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn text(rng: &mut Rng) -> String {
    const POOL: &[&str] = &[
        "a", "Z", "0", " ", "\"", "\\", "\n", "\r", "\0", "é", "日", "😀", "\u{2028}", "}", "{",
        "/",
    ];
    (0..rng.below(24))
        .map(|_| POOL[rng.below(POOL.len())])
        .collect()
}

fn value(rng: &mut Rng, depth: usize) -> Value {
    match rng.below(if depth > 4 { 5 } else { 7 }) {
        0 => Value::Null,
        1 => Value::Bool(rng.next() & 1 == 1),
        2 => json!(rng.next() as i64),
        3 => json!((rng.next() % 1000) as f64 / 8.0),
        4 => Value::String(text(rng)),
        5 => Value::Array((0..rng.below(4)).map(|_| value(rng, depth + 1)).collect()),
        _ => Value::Object(object(rng, depth + 1)),
    }
}

fn object(rng: &mut Rng, depth: usize) -> Map<String, Value> {
    (0..rng.below(5))
        .map(|_| (text(rng), value(rng, depth)))
        .collect()
}

fn packet(rng: &mut Rng) -> Packet {
    let mut kind = text(rng);
    if kind.is_empty() {
        kind.push('k');
    }
    let with_payload = rng.below(3) == 0;
    Packet {
        id: rng.next() as i64,
        kind,
        body: object(rng, 0),
        payload_size: with_payload.then(|| (rng.next() % 1_000_000) as i64 - 1),
        payload_transfer_info: with_payload.then(|| object(rng, 0)),
    }
}

fn parse(line: &[u8]) -> Result<Packet, KdeError> {
    Packet::from_line(line)
}

#[test]
fn serialised_packets_round_trip_and_stay_on_one_line() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..iters(3000) {
        let p = packet(&mut rng);
        let line = p.to_line();
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1, "case {i}");
        assert_eq!(*line.last().unwrap(), b'\n');
        let back = parse(&line[..line.len() - 1]).unwrap_or_else(|e| panic!("case {i}: {e}"));
        assert_eq!(back, p, "case {i}");
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    for _ in 0..iters(20_000) {
        let _ = parse(&rng.bytes(300));
    }
}

#[test]
fn mutated_valid_packets_never_panic_and_accepted_ones_round_trip() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let mut accepted = 0usize;
    for _ in 0..iters(10_000) {
        let line = packet(&mut rng).to_line();
        let mut line = line[..line.len() - 1].to_vec();
        for _ in 0..1 + rng.below(4) {
            if line.is_empty() {
                break;
            }
            let at = rng.below(line.len());
            match rng.below(4) {
                0 => line[at] = rng.next() as u8,
                1 => line.truncate(at),
                2 => line.insert(at, rng.next() as u8),
                _ => {
                    line.remove(at);
                }
            }
        }
        if let Ok(p) = parse(&line) {
            accepted += 1;
            let again = p.to_line();
            // Not full equality: serde_json (without `float_roundtrip`) may move a huge float by
            // one ulp when re-parsing a line it printed. Structure and identity must survive.
            let q = parse(&again[..again.len() - 1]).unwrap();
            assert_eq!((q.id, &q.kind), (p.id, &p.kind));
            assert_eq!(q.payload_size, p.payload_size);
        }
    }
    assert!(accepted > 0, "the mutator should leave some packets valid");
}

#[test]
fn structural_hostile_packets_are_rejected_not_crashed_on() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not an object", b"[1,2,3]".to_vec()),
        ("empty", Vec::new()),
        ("null", b"null".to_vec()),
        ("missing type", br#"{"id":1,"body":{}}"#.to_vec()),
        ("empty type", br#"{"id":1,"type":"","body":{}}"#.to_vec()),
        (
            "type not string",
            br#"{"id":1,"type":7,"body":{}}"#.to_vec(),
        ),
        (
            "body not object",
            br#"{"id":1,"type":"t","body":[]}"#.to_vec(),
        ),
        ("id float", br#"{"id":1.5,"type":"t","body":{}}"#.to_vec()),
        ("id huge", br#"{"id":1e999,"type":"t","body":{}}"#.to_vec()),
        (
            "id too wide",
            br#"{"id":18446744073709551615,"type":"t","body":{}}"#.to_vec(),
        ),
        (
            "id string overlong",
            br#"{"id":"123456789012345678901","type":"t","body":{}}"#.to_vec(),
        ),
        (
            "negative payload",
            br#"{"id":1,"type":"t","body":{},"payloadSize":-2,"payloadTransferInfo":{}}"#.to_vec(),
        ),
        (
            "payload without info",
            br#"{"id":1,"type":"t","body":{},"payloadSize":5}"#.to_vec(),
        ),
        (
            "info not object",
            br#"{"id":1,"type":"t","body":{},"payloadSize":5,"payloadTransferInfo":3}"#.to_vec(),
        ),
        ("invalid utf8", vec![b'{', 0xff, 0xfe, b'}']),
        (
            "type too long",
            format!(r#"{{"id":1,"type":"{}","body":{{}}}}"#, "k".repeat(257)).into_bytes(),
        ),
        (
            "depth 65",
            format!(r#"{{"id":1,"type":"t","body":{{"a":{}}}}}"#, deep(64)).into_bytes(),
        ),
        ("depth 1e6", deep(1_000_000).into_bytes()),
        ("unterminated string", br#"{"id":1,"type":"t"#.to_vec()),
    ];
    for (name, line) in cases {
        assert!(parse(&line).is_err(), "{name} must be rejected");
    }
    // Duplicate keys: whichever copy wins, the result is a well-formed packet or an error.
    if let Ok(p) = parse(br#"{"id":1,"id":2,"type":"t","body":{},"body":{"x":1}}"#) {
        assert_eq!(p.kind, "t");
    }
}

#[test]
fn depth_just_inside_the_limit_is_accepted() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let inside = format!(r#"{{"id":1,"type":"t","body":{{"a":{}}}}}"#, deep(60));
    assert!(parse(inside.as_bytes()).is_ok());
}

#[tokio::test]
async fn line_reader_reassembles_any_chunking_and_enforces_the_limit() {
    let mut rng = Rng(0x1357_9bdf_2468_ace0);
    for case in 0..iters(300) / 10 + 1 {
        let packets: Vec<Packet> = (0..1 + rng.below(6)).map(|_| packet(&mut rng)).collect();
        let wire: Vec<u8> = packets.iter().flat_map(Packet::to_line).collect();
        let (mut tx, rx) = duplex(64);
        let mut chunks = Vec::new();
        let mut rest = &wire[..];
        while !rest.is_empty() {
            let n = (1 + rng.below(40)).min(rest.len());
            chunks.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        let writer = tokio::spawn(async move {
            for c in chunks {
                tx.write_all(&c).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut reader = LineReader::new(rx);
        for (i, expected) in packets.iter().enumerate() {
            let line = tokio::time::timeout(Duration::from_secs(10), reader.read_line(1 << 20))
                .await
                .expect("timeout")
                .unwrap_or_else(|e| panic!("case {case} packet {i}: {e}"));
            assert_eq!(&parse(&line).unwrap(), expected, "case {case} packet {i}");
        }
        writer.await.unwrap();
        assert!(matches!(
            reader.read_line(1 << 20).await,
            Err(KdeError::UnexpectedEof)
        ));
    }

    // A line over the limit is refused without buffering it all, and the reader stays poisoned.
    let (mut tx, rx) = duplex(4096);
    let flood = tokio::spawn(async move {
        let chunk = vec![b'x'; 1024];
        for _ in 0..64 {
            if tx.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });
    let mut reader = LineReader::new(rx);
    assert!(matches!(
        reader.read_line(2048).await,
        Err(KdeError::LineTooLong { .. })
    ));
    assert!(
        reader.read_line(2048).await.is_err(),
        "poisoned after an error"
    );
    flood.abort();
}
