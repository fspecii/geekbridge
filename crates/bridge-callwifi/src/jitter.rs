// SPDX-License-Identifier: GPL-3.0-or-later
//! Adaptive jitter buffer for 20 ms frames (EXPERIMENTAL).
//!
//! The receiver [`push`](JitterBuffer::push)es frames as they arrive and [`pop`](JitterBuffer::pop)s exactly one
//! frame every 20 ms from its playback clock. The buffer waits for `target` frames before it starts, plays in
//! sequence order, reports a gap as [`Out::Conceal`] (with the next frame's bytes when they are already here, so
//! the decoder can use Opus in-band FEC), drops frames that arrive after their slot was played (`late`), and
//! keeps the delay near the target: the target follows an RFC 3550 interarrival-jitter estimate, a buffer that
//! stays more than two frames above it sheds one frame, and a buffer that runs dry for five pops rebuffers.
//!
//! Sequence numbers are `u32` on the wire and wrap; they are extended to `u64` relative to the newest one seen.
use std::collections::BTreeMap;

use crate::wire::FRAME_MS;

#[derive(Clone, Copy, Debug)]
pub struct JitterConfig {
    /// Never aim below this many frames.
    pub min_frames: usize,
    /// Never aim above, nor hold more than, this many frames (the latency ceiling).
    pub max_frames: usize,
    /// Pops in a row with the buffer above `target + 2` before one frame is shed.
    pub shrink_after: u32,
    /// Empty pops in a row before the buffer stops and rebuffers.
    pub rebuffer_after: u32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            min_frames: 2,
            max_frames: 6,
            shrink_after: 25,
            rebuffer_after: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out<T = Vec<u8>> {
    /// Not started (or rebuffering): play silence, do not run the decoder's concealment.
    Idle,
    /// The next frame, in order.
    Packet(T),
    /// The frame for this slot is missing. `fec` is the following frame, if it has already arrived.
    Conceal { fec: Option<T> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    Queued,
    /// Its slot was already played.
    Late,
    Duplicate,
    /// So far ahead of the playback position that the stream must have restarted: buffer reset to it.
    Restarted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub got: u64,
    pub played: u64,
    pub lost: u64,
    pub late: u64,
    pub duplicate: u64,
    /// Frames shed to bring the delay back down (overflow or sustained excess).
    pub shed: u64,
    pub underruns: u64,
    pub rebuffers: u64,
    pub restarts: u64,
}

pub struct JitterBuffer<T = Vec<u8>> {
    cfg: JitterConfig,
    slots: BTreeMap<u64, T>,
    /// Extended sequence number of the next frame to play, once started.
    next: Option<u64>,
    newest: Option<u64>,
    jitter_ms: f64,
    prev_transit: Option<f64>,
    above: u32,
    dry: u32,
    stats: JitterStats,
}

impl<T: Clone> JitterBuffer<T> {
    pub fn new(cfg: JitterConfig) -> Self {
        assert!(cfg.min_frames >= 1 && cfg.min_frames <= cfg.max_frames);
        Self {
            cfg,
            slots: BTreeMap::new(),
            next: None,
            newest: None,
            jitter_ms: 0.0,
            prev_transit: None,
            above: 0,
            dry: 0,
            stats: JitterStats::default(),
        }
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    pub fn jitter_ms(&self) -> f64 {
        self.jitter_ms
    }

    /// Frames the buffer aims to hold: enough to ride out about 2.5x the measured jitter.
    pub fn target(&self) -> usize {
        let frames = ((2.5 * self.jitter_ms + 10.0) / f64::from(FRAME_MS)).ceil() as usize;
        frames.clamp(self.cfg.min_frames, self.cfg.max_frames)
    }

    /// Frames queued from the playback position on.
    pub fn depth(&self) -> usize {
        match self.next {
            Some(n) => self.slots.range(n..).count(),
            None => self.slots.len(),
        }
    }

    pub fn depth_ms(&self) -> u32 {
        (self.depth() as u32).saturating_mul(FRAME_MS)
    }

    pub fn started(&self) -> bool {
        self.next.is_some()
    }

    fn extend(&self, seq: u32) -> u64 {
        match self.newest {
            None => u64::from(seq) + (1 << 32), // headroom so a small step back never goes below zero
            Some(n) => {
                let d = seq.wrapping_sub(n as u32) as i32;
                n.saturating_add_signed(i64::from(d))
            }
        }
    }

    /// One frame arrived at `arrival_ms` (receiver's monotonic clock).
    pub fn push(&mut self, seq: u32, payload: T, arrival_ms: u64) -> Push {
        let ext = self.extend(seq);
        self.stats.got += 1;
        // Jitter estimate (RFC 3550 6.4.1), in ms, on frames in any order.
        let transit = arrival_ms as f64 - ext as f64 * f64::from(FRAME_MS);
        if let Some(next) = self.next
            && ext >= next + 8 * self.cfg.max_frames as u64
        {
            // A new stream: its sequence jump says nothing about network jitter.
            self.stats.restarts += 1;
            self.slots.clear();
            self.next = None;
            self.newest = Some(ext);
            self.prev_transit = Some(transit);
            self.slots.insert(ext, payload);
            return Push::Restarted;
        }
        if let Some(p) = self.prev_transit {
            let d = (transit - p).abs().min(1000.0);
            self.jitter_ms += (d - self.jitter_ms) / 16.0;
        }
        self.prev_transit = Some(transit);
        if self.newest.is_none_or(|n| ext > n) {
            self.newest = Some(ext);
        }
        if let Some(next) = self.next
            && ext < next
        {
            self.stats.late += 1;
            return Push::Late;
        }
        if self.slots.contains_key(&ext) {
            self.stats.duplicate += 1;
            return Push::Duplicate;
        }
        self.slots.insert(ext, payload);
        // Hard ceiling: never hold more than max_frames; shed the oldest.
        while self.slots.len() > self.cfg.max_frames {
            if let Some((&k, _)) = self.slots.iter().next() {
                self.slots.remove(&k);
                self.stats.shed += 1;
                if let Some(n) = self.next.as_mut() {
                    *n = (*n).max(k + 1);
                }
            }
        }
        Push::Queued
    }

    /// Called once per 20 ms by the playback clock.
    pub fn pop(&mut self) -> Out<T> {
        let next = match self.next {
            Some(n) => n,
            None => {
                if self.slots.len() < self.target() {
                    return Out::Idle;
                }
                let first = *self.slots.keys().next().expect("non-empty");
                self.next = Some(first);
                first
            }
        };
        // Sustained excess: shed one frame so the delay returns towards the target.
        if self.depth() > self.target() + 2 {
            self.above += 1;
            if self.above >= self.cfg.shrink_after {
                self.above = 0;
                if self.slots.remove(&next).is_some() {
                    self.stats.shed += 1;
                    self.next = Some(next + 1);
                    return self.pop();
                }
            }
        } else {
            self.above = 0;
        }
        self.slots.retain(|k, _| *k >= next);
        if let Some(p) = self.slots.remove(&next) {
            self.next = Some(next + 1);
            self.dry = 0;
            self.stats.played += 1;
            return Out::Packet(p);
        }
        if self.slots.is_empty() {
            self.stats.underruns += 1;
            self.dry += 1;
            if self.dry >= self.cfg.rebuffer_after {
                self.dry = 0;
                self.next = None;
                self.stats.rebuffers += 1;
                return Out::Idle;
            }
        } else {
            self.dry = 0;
        }
        self.stats.lost += 1;
        self.next = Some(next + 1);
        Out::Conceal {
            fec: self.slots.get(&(next + 1)).cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jb() -> JitterBuffer {
        JitterBuffer::new(JitterConfig::default())
    }

    fn p(n: u8) -> Vec<u8> {
        vec![n]
    }

    #[test]
    fn waits_for_the_target_then_plays_in_order() {
        let mut b = jb();
        assert_eq!(b.pop(), Out::Idle);
        b.push(10, p(10), 0);
        assert_eq!(b.pop(), Out::Idle, "one frame is below the minimum target");
        b.push(12, p(12), 40);
        b.push(11, p(11), 41);
        assert_eq!(b.pop(), Out::Packet(p(10)));
        assert_eq!(b.pop(), Out::Packet(p(11)));
        assert_eq!(b.pop(), Out::Packet(p(12)));
        assert_eq!(b.stats().played, 3);
    }

    #[test]
    fn a_gap_is_concealed_with_fec_from_the_next_frame_and_a_late_frame_is_dropped() {
        let mut b = jb();
        for s in [0u32, 1, 3] {
            b.push(s, p(s as u8), u64::from(s) * 20);
        }
        assert_eq!(b.pop(), Out::Packet(p(0)));
        assert_eq!(b.pop(), Out::Packet(p(1)));
        assert_eq!(b.pop(), Out::Conceal { fec: Some(p(3)) });
        assert_eq!(b.push(2, p(2), 100), Push::Late);
        assert_eq!(b.pop(), Out::Packet(p(3)));
        let s = b.stats();
        assert_eq!((s.lost, s.late, s.played), (1, 1, 3));
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut b = jb();
        assert_eq!(b.push(5, p(5), 0), Push::Queued);
        assert_eq!(b.push(5, p(9), 1), Push::Duplicate);
        b.push(6, p(6), 20);
        assert_eq!(b.pop(), Out::Packet(p(5)));
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut b = jb();
        let start = u32::MAX - 2;
        for i in 0..6u32 {
            b.push(start.wrapping_add(i), p(i as u8), u64::from(i) * 20);
        }
        for i in 0..6u8 {
            assert_eq!(b.pop(), Out::Packet(p(i)), "frame {i}");
        }
    }

    #[test]
    fn running_dry_conceals_then_rebuffers() {
        let mut b = jb();
        b.push(0, p(0), 0);
        b.push(1, p(1), 20);
        assert_eq!(b.pop(), Out::Packet(p(0)));
        assert_eq!(b.pop(), Out::Packet(p(1)));
        for _ in 0..4 {
            assert_eq!(b.pop(), Out::Conceal { fec: None });
        }
        assert_eq!(b.pop(), Out::Idle);
        assert!(!b.started());
        assert_eq!(b.stats().rebuffers, 1);
        // Frames that come back after the rebuffer start a fresh run.
        b.push(20, p(20), 400);
        b.push(21, p(21), 420);
        assert_eq!(b.pop(), Out::Packet(p(20)));
    }

    #[test]
    fn never_holds_more_than_the_ceiling() {
        let mut b = jb();
        for s in 0..20u32 {
            b.push(s, p(s as u8), 0); // a burst: everything at once
        }
        assert!(b.depth() <= JitterConfig::default().max_frames);
        assert!(b.stats().shed >= 14);
        assert_eq!(b.pop(), Out::Packet(p(14)));
    }

    #[test]
    fn a_stream_far_ahead_is_a_restart() {
        let mut b = jb();
        b.push(0, p(0), 0);
        b.push(1, p(1), 20);
        assert_eq!(b.pop(), Out::Packet(p(0)));
        assert_eq!(b.push(10_000, p(7), 40), Push::Restarted);
        assert!(!b.started());
        b.push(10_001, p(8), 60);
        assert_eq!(b.pop(), Out::Packet(p(7)));
    }

    #[test]
    fn the_target_follows_jitter_and_excess_is_shed() {
        let mut b = jb();
        // Steady 20 ms arrivals: target stays at the minimum.
        for s in 0..50u32 {
            b.push(s, p(0), u64::from(s) * 20);
            b.pop();
        }
        assert_eq!(b.target(), 2);
        // Arrivals alternating 0 / 60 ms late: jitter rises, target rises, capped.
        let mut c = jb();
        for s in 0..200u32 {
            let late = if s % 2 == 0 { 0 } else { 60 };
            c.push(s, p(0), u64::from(s) * 20 + late);
        }
        assert!(c.jitter_ms() > 30.0, "{}", c.jitter_ms());
        assert!(c.target() >= 4 && c.target() <= 6, "{}", c.target());
        // A buffer held at the ceiling with low jitter sheds back towards the target.
        let mut d = jb();
        for s in 0..6u32 {
            d.push(s, p(s as u8), u64::from(s) * 20);
        }
        for s in 6..106u32 {
            d.push(s, p(0), u64::from(s) * 20);
            d.pop();
        }
        assert!(d.stats().shed >= 1);
        assert!(
            d.depth() <= d.target() + 3,
            "depth {} target {}",
            d.depth(),
            d.target()
        );
    }
}
