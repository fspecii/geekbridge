// SPDX-License-Identifier: GPL-3.0-or-later
//! Test signals and their analysis: a sine generator and a dominant-frequency finder (Goertzel scan). Used by the
//! tests and by the lab to prove a tone crossed the whole chain. Analysis only ever returns numbers; no audio is
//! kept.
use crate::wire::{FRAME_SAMPLES, RATE};

pub struct Tone {
    hz: f64,
    amp: f64,
    n: u64,
}

impl Tone {
    pub fn new(hz: f64, amp: f64) -> Self {
        Self { hz, amp, n: 0 }
    }

    pub fn frame(&mut self) -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|_| {
                let t = self.n as f64 / f64::from(RATE);
                self.n += 1;
                ((t * self.hz * std::f64::consts::TAU).sin() * self.amp * 32767.0) as i16
            })
            .collect()
    }
}

/// Power of one frequency in `x` (Goertzel).
pub fn power_at(x: &[i16], hz: f64, rate: u32) -> f64 {
    let w = std::f64::consts::TAU * hz / f64::from(rate);
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - c * s1 * s2
}

/// The strongest frequency between 100 Hz and 4 kHz, to within 1 Hz (coarse 10 Hz scan, then refine).
pub fn dominant_hz(x: &[i16], rate: u32) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let best = |lo: f64, hi: f64, step: f64| {
        let mut f = lo;
        let (mut bf, mut bp) = (lo, -1.0);
        while f <= hi {
            let p = power_at(x, f, rate);
            if p > bp {
                (bf, bp) = (f, p);
            }
            f += step;
        }
        bf
    };
    let coarse = best(100.0, 4000.0, 10.0);
    best(coarse - 10.0, coarse + 10.0, 1.0)
}

pub fn rms(x: &[i16]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_tone() {
        let mut t = Tone::new(1234.0, 0.5);
        let x: Vec<i16> = (0..20).flat_map(|_| t.frame()).collect();
        assert!((dominant_hz(&x, RATE) - 1234.0).abs() <= 1.0);
        assert!(rms(&x) > 10_000.0);
        assert_eq!(rms(&[]), 0.0);
    }
}
