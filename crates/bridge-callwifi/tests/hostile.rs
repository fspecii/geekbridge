// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic mutation fuzzing of every parser that takes peer input (the packet parser plus the gate, the
//! Opus decoder, the helper framing), and lab runs over the real link code, clean and under chaos.
use bridge_callwifi::codec::Decoder;
use bridge_callwifi::gate::{Action, Gate, Side};
use bridge_callwifi::helper;
use bridge_callwifi::jitter::{JitterBuffer, JitterConfig, Out};
use bridge_callwifi::wire::{self, FRAME_SAMPLES};
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
    r#"{"ready":true,"listen":true,"speak":false,"test":"works"}"#,
    r#"{"session":1,"on":true}"#,
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

/// The consent gate cannot be talked past: whatever packets arrive, in whatever order, a gate whose consents are
/// not all given never goes live, never accepts a frame, never shows the indicator and never turns speaking on.
#[test]
fn no_packet_sequence_opens_a_gate_without_its_consents() {
    use wire::Msg;
    let mut r = Rng(0xC0FFEE);
    for round in 0..300 {
        // Every combination where at least one consent is missing.
        let (computer_allows, phone_switch, call) = match round % 6 {
            0 => (false, false, false),
            1 => (false, true, true), // phone: needs computer-side nothing, but see below
            2 => (true, false, true),
            3 => (true, true, false),
            4 => (false, false, true),
            _ => (true, false, false),
        };
        for side in [Side::Phone, Side::Computer] {
            // Side-specific: the phone needs its switch and a call; the computer needs its allow.
            let missing = match side {
                Side::Phone => !(phone_switch && call),
                Side::Computer => !computer_allows,
            };
            if !missing {
                continue;
            }
            let mut g = Gate::new(side);
            g.set_consent(computer_allows, phone_switch);
            g.set_call_active(call);
            g.speak_allowed = r.below(2) == 0;
            let mut t = 0u64;
            for _ in 0..200 {
                let seed = SEEDS[r.below(SEEDS.len())].as_bytes();
                let input = mutate(&mut r, seed);
                let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&input) else {
                    continue;
                };
                for kind in wire::ALL {
                    let Ok(m) = wire::parse(kind, &body) else {
                        continue;
                    };
                    t += 3;
                    let acts = g.on_msg(m.clone(), t);
                    let bad = acts.iter().any(|a| {
                        matches!(
                            a,
                            Action::Accept(_) | Action::IndicatorOn | Action::SpeakState(true)
                        ) || matches!(a, Action::Send(Msg::Offer(_)))
                            || matches!(a, Action::Send(Msg::Answer(x)) if x.accept)
                    });
                    // A computer without its allow never accepts an offer; a phone never offers.
                    assert!(!bad, "{side:?} round {round}: {m:?} -> {acts:?}");
                    assert!(g.session().is_none_or(|s| !s.live), "{side:?} went live");
                    assert!(!g.may_send(wire::Dir::Down) && !g.may_send(wire::Dir::Up));
                }
            }
        }
    }
}

/// Inside a live, consented, listen-only session nothing a peer sends turns speaking back on: not on the phone
/// without its own opt-in (even with an uplink route), not on the computer without its own press. No microphone
/// frame is accepted and the computer never sends one.
#[test]
fn no_packet_inside_a_live_listen_only_session_turns_speaking_on() {
    use wire::Msg;
    let mut r = Rng(0xFEED_5EA7);
    for round in 0..200 {
        let mut p = Gate::new(Side::Phone);
        let mut c = Gate::new(Side::Computer);
        p.set_consent(false, true);
        c.set_consent(true, false);
        p.set_call_active(true);
        p.uplink_route = wire::Uplink::TelephonyTx;
        p.speak_allowed = false;
        let offer = match p.on_msg(Msg::Request { session: 7 }, 0).remove(0) {
            Action::Send(m) => m,
            other => panic!("{other:?}"),
        };
        let answer = match c.on_msg(offer, 0).remove(0) {
            Action::Send(m) => m,
            other => panic!("{other:?}"),
        };
        p.on_msg(answer, 0);
        assert!(p.session().is_some_and(|s| s.live) && c.session().is_some_and(|s| s.live));
        let mut t = 0u64;
        for _ in 0..100 {
            let seed = SEEDS[r.below(SEEDS.len())].as_bytes();
            let input = mutate(&mut r, seed);
            let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&input) else {
                continue;
            };
            for kind in wire::ALL {
                let Ok(mut m) = wire::parse(kind, &body) else {
                    continue;
                };
                // Aim at the live session (id 7) so the packets get past the id check.
                match &mut m {
                    Msg::Speak { session, .. } => *session = 7,
                    Msg::Frame(f) => f.session = 7,
                    _ => {}
                }
                t += 3;
                for g in [&mut p, &mut c] {
                    let acts = g.on_msg(m.clone(), t);
                    assert!(
                        !acts.iter().any(|a| matches!(
                            a,
                            Action::SpeakState(true)
                                | Action::Send(Msg::Speak { on: true, .. })
                                | Action::Send(Msg::Frame(_))
                        )),
                        "round {round}: {m:?} -> {acts:?}"
                    );
                }
                // The phone never takes a microphone frame, the computer never has one to send.
                assert!(!p.session().is_some_and(|s| s.up));
                assert!(!c.session().is_some_and(|s| s.up));
            }
        }
    }
}
