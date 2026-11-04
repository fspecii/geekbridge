// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic mutation fuzzing of every parser that takes peer input (the packet parser plus the gate, the
//! Opus decoder, the helper framing), and lab runs over the real link code, clean and under chaos.
use bridge_callaudio::codec::Decoder;
use bridge_callaudio::gate::{Action, Gate, Side};
use bridge_callaudio::helper;
use bridge_callaudio::jitter::{JitterBuffer, JitterConfig, Out};
use bridge_callaudio::lab::{self, LabConfig};
use bridge_callaudio::wire::{self, FRAME_SAMPLES};
use serde_json::{Map, Value};

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

fn mutate(r: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + r.below(4) {
        match r.below(4) {
            0 if !v.is_empty() => {
                let i = r.below(v.len());
                v[i] = r.next() as u8;
            }
            1 if !v.is_empty() => {
                let i = r.below(v.len());
                v.remove(i);
            }
            2 => {
                let i = r.below(v.len() + 1);
                v.insert(i, b"0123456789\",:{}[]-e.\\u"[r.below(22)]);
            }
            _ => {
                let i = r.below(v.len() + 1);
                v.splice(i..i, seed[..r.below(seed.len())].iter().copied());
            }
        }
    }
    v
}

const SEEDS: &[&str] = &[
    r#"{"session":1,"dirs":["down","up"],"codec":"opus","rate":16000,"frameMs":20,"uplink":"telephony_tx"}"#,
    r#"{"session":1,"dir":"down","seq":4294967295,"ts":0,"cap":0,"opus":"+P/+AAEC"}"#,
    r#"{"session":1,"accept":false,"reason":"no_consent"}"#,
    r#"{"session":1,"accept":true,"dirs":["down"]}"#,
    r#"{"session":1,"dir":"up","got":1,"lost":0,"late":0,"depthMs":40}"#,
    r#"{"session":9007199254740991,"reason":"call_ended"}"#,
];

#[test]
fn hostile_packets_never_panic_round_trip_and_never_pass_a_gate_without_consent() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut dec = Decoder::new().unwrap();
    let mut parsed = 0;
    for i in 0..30_000 {
        let seed = SEEDS[i % SEEDS.len()].as_bytes();
        let input = mutate(&mut r, seed);
        let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&input) else {
            continue;
        };
        let body: Map<String, Value> = body;
        for kind in wire::ALL {
            let Ok(m) = wire::parse(kind, &body) else {
                continue;
            };
            parsed += 1;
            assert_eq!(wire::parse(m.kind(), &wire::body(&m)).as_ref(), Ok(&m));
            if let wire::Msg::Frame(f) = &m {
                assert_eq!(
                    dec.render(&Out::Packet(f.opus.clone())).len(),
                    FRAME_SAMPLES
                );
            }
            for side in [Side::Phone, Side::Computer] {
                let acts = Gate::new(side).on_msg(m.clone(), 0);
                assert!(
                    !acts
                        .iter()
                        .any(|a| matches!(a, Action::Accept(_) | Action::IndicatorOn)),
                    "{m:?}"
                );
            }
        }
    }
    assert!(
        parsed > 1000,
        "the mutator should keep many inputs valid ({parsed})"
    );
}

#[test]
fn random_bytes_into_the_opus_decoder_always_give_one_frame() {
    let mut r = Rng(7);
    let mut dec = Decoder::new().unwrap();
    for _ in 0..5_000 {
        let n = 1 + r.below(wire::MAX_OPUS_BYTES);
        let p: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
        assert_eq!(dec.render(&Out::Packet(p.clone())).len(), FRAME_SAMPLES);
        assert_eq!(
            dec.render(&Out::Conceal { fec: Some(p) }).len(),
            FRAME_SAMPLES
        );
    }
}

#[test]
fn hostile_helper_streams_never_panic_and_round_trip() {
    let mut r = Rng(11);
    let mut seed = helper::pcm_vector();
    seed.extend_from_slice(helper::HELLO_VECTOR);
    for _ in 0..20_000 {
        let input = mutate(&mut r, &seed);
        if let Ok((frames, used)) = helper::decode_all(&input) {
            let again: Vec<u8> = frames.iter().flat_map(helper::encode).collect();
            assert_eq!(again, input[..used]);
        }
    }
}

#[test]
fn jitter_buffer_survives_random_arrivals_within_its_ceiling() {
    let mut r = Rng(3);
    let cfg = JitterConfig::default();
    let mut b: JitterBuffer<u32> = JitterBuffer::new(cfg);
    let mut now = 0u64;
    for _ in 0..50_000 {
        if r.below(3) == 0 {
            b.pop();
            now += 20;
        } else {
            b.push(r.next() as u32 % 2000, 0, now + r.below(300) as u64);
        }
        assert!(b.depth() <= cfg.max_frames);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lab_carries_both_tones_over_the_real_link_with_low_delay() {
    let r = lab::run(LabConfig {
        seconds: 2.0,
        ..LabConfig::default()
    })
    .await
    .unwrap();
    assert!(r.tls.starts_with("TLSv1_3"), "{}", r.tls);
    for (d, hz) in [(&r.down, 440.0), (&r.up, 1000.0)] {
        assert!((d.dominant_hz - hz).abs() < 10.0, "{d:?}");
        assert!(d.played > 80, "{d:?}");
        let mut l = d.latency_ms.clone();
        l.sort_unstable();
        // Two frames of buffer plus scheduling; 150 ms is the budget (about 40 ms idle, more on a loaded CI box).
        assert!(l[l.len() / 2] <= 150, "median {}", l[l.len() / 2]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lab_survives_delay_and_loss_and_keeps_the_tone() {
    let r = lab::run(LabConfig {
        seconds: 3.0,
        faults: Some(bridge_chaos::Faults {
            latency: std::time::Duration::from_millis(30),
            loss: 0.03,
            loss_penalty: std::time::Duration::from_millis(80),
            ..Default::default()
        }),
        seed: 5,
        ..LabConfig::default()
    })
    .await
    .unwrap();
    assert!((r.down.dominant_hz - 440.0).abs() < 10.0, "{:?}", r.down);
    assert!(r.down.played > 60, "{:?}", r.down);
    assert!(r.down.jitter.got > 100);
}
