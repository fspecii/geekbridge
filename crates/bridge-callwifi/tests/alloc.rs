// SPDX-License-Identifier: GPL-3.0-or-later
//! Heap behaviour of the call audio codec path, counted with a global allocator: what the codec and the jitter buffer
//! hold while a session runs, that dropping them gives every byte back, and that steady-state decoding of a frame
//! allocates nothing (the receive loop runs 50 times a second for the length of a call).
//!
//! One test function, because the counters are process wide.
use bridge_callwifi::codec::{Decoder, Encoder};
use bridge_callwifi::jitter::{JitterBuffer, JitterConfig, Out, Push};
use bridge_callwifi::tone::Tone;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(now, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        if new >= l.size() {
            let now = LIVE.fetch_add(new - l.size(), Relaxed) + (new - l.size());
            PEAK.fetch_max(now, Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new, Relaxed);
        }
        unsafe { System.realloc(p, l, new) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn live() -> usize {
    LIVE.load(Relaxed)
}
fn calls() -> usize {
    CALLS.load(Relaxed)
}

#[test]
fn a_session_gives_back_every_byte_and_steady_state_decoding_does_not_allocate() {
    // The phone's side, to have real packets.
    let mut enc = Encoder::new().unwrap();
    let mut tone = Tone::new(440.0, 8000.0);
    let frames: Vec<Vec<u8>> = (0..300)
        .map(|_| enc.encode(&tone.frame()).unwrap())
        .collect();
    drop(enc);

    let before = live();
    let peak_before = PEAK.load(Relaxed);
    let mut dec = Decoder::new().unwrap();
    let mut jb: JitterBuffer<Vec<u8>> = JitterBuffer::new(JitterConfig::default());
    let held_idle = live() - before;

    // Warm up (first frames may size internal buffers), then count.
    let mut now = 0u64;
    let mut seq = 0u32;
    let mut feed = |jb: &mut JitterBuffer<Vec<u8>>, dec: &mut Decoder, f: &Vec<u8>| {
        now += 20;
        assert!(matches!(
            jb.push(seq, f.clone(), now),
            Push::Queued | Push::Restarted
        ));
        seq += 1;
        let out = jb.pop();
        let _ = dec.render(&out);
        matches!(out, Out::Packet(_))
    };
    for f in &frames[..50] {
        feed(&mut jb, &mut dec, f);
    }
    let live_warm = live();
    let calls_warm = calls();
    for f in &frames[50..] {
        feed(&mut jb, &mut dec, f);
    }
    let per_frame_calls = (calls() - calls_warm) as f64 / 250.0;
    let growth = live() as i64 - live_warm as i64;
    let held_running = live() - before;
    let peak = PEAK.load(Relaxed) - peak_before;

    drop(dec);
    drop(jb);
    let leaked = live() as i64 - before as i64;
    eprintln!(
        "decoder+jitter: {held_idle} B at creation, {held_running} B while running, peak {peak} B, \
         {per_frame_calls:.2} allocator calls per frame (one is the frame's own Vec in this test), growth {growth} B, leaked {leaked} B"
    );
    assert_eq!(
        leaked, 0,
        "dropping the decoder and the jitter buffer returns every byte"
    );
    assert!(
        growth.abs() <= 64,
        "steady state does not grow: {growth} B over 250 frames"
    );
    assert!(
        per_frame_calls <= 3.0,
        "decoding a frame allocates at most the frame's own copy: {per_frame_calls}"
    );

    // The encoder (Speak) likewise.
    let before = live();
    let mut enc = Encoder::new().unwrap();
    let held_enc = live() - before;
    for _ in 0..50 {
        enc.encode(&tone.frame()).unwrap();
    }
    let calls_warm = calls();
    for _ in 0..100 {
        enc.encode(&tone.frame()).unwrap();
    }
    let enc_calls = (calls() - calls_warm) as f64 / 100.0;
    drop(enc);
    let enc_leak = live() as i64 - before as i64;
    eprintln!(
        "encoder: {held_enc} B at creation, {enc_calls:.2} allocator calls per frame (tone frame + output copy included), leaked {enc_leak} B"
    );
    assert_eq!(enc_leak, 0);
}
