// SPDX-License-Identifier: GPL-3.0-or-later
//! Short deterministic fuzz pass for CI: a fixed seed, 2000 inputs per target (override with
//! `GEEKBRIDGE_FUZZ_ITERS`), the same targets, oracles and resource limits as the long runs.
//! A failure prints the crash file and the `gb-fuzz replay-iter` line that reproduces it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bridge_fuzz::{
    CountingAlloc,
    harness::{Campaign, Limits, run_campaign},
    targets::TARGETS,
};
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const SEED: u64 = 0x6B65_656B;

#[test]
fn every_target_survives_2000_deterministic_inputs() {
    let iters: u64 = std::env::var("GEEKBRIDGE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let dir = std::env::temp_dir().join("geekbridge-fuzz-crashes");
    let mut failures = Vec::new();
    // One test, targets in sequence: the allocation peak is process-wide, so nothing may run beside it.
    for t in TARGETS {
        let started = Instant::now();
        let c = Campaign {
            seed: SEED,
            iterations: Some(iters),
            deadline: None,
            start_iter: 0,
            limits: Limits {
                hang: Duration::from_secs(60),
                ..Limits::default()
            },
            crash_dir: dir.clone(),
            max_findings: 3,
        };
        let r = run_campaign(t, &c);
        eprintln!(
            "{:<18} {:>6} runs {:>6.1}s {} findings",
            t.name,
            r.runs,
            started.elapsed().as_secs_f32(),
            r.findings.len()
        );
        for (i, f, path) in r.findings {
            failures.push(format!(
                "{} iter {i}: {f}\n  saved {}\n  replay: gb-fuzz replay-iter {} {SEED} {i}",
                t.name,
                path.display(),
                t.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "fuzz findings:\n{}",
        failures.join("\n")
    );
}
