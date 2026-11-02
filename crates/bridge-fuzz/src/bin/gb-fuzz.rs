// SPDX-License-Identifier: GPL-3.0-or-later
//! Runner for the deterministic mutation fuzzer.
//!
//!   gb-fuzz run <target|all> [--seconds N] [--iters N] [--seed S] [--out DIR]
//!   gb-fuzz replay <target> <file>          run one saved crash file
//!   gb-fuzz replay-iter <target> <seed> <i> regenerate and run iteration i of a seed
//!   gb-fuzz seed                            (re)write fuzz/corpus from the committed fixtures
//!   gb-fuzz list
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bridge_fuzz::{
    CountingAlloc,
    harness::{self, Campaign, Limits, run_campaign, run_one, workspace_root},
    mutate,
    targets::{TARGETS, Target, by_name},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn pick(name: &str) -> Vec<&'static Target> {
    if name == "all" {
        TARGETS.iter().collect()
    } else {
        vec![by_name(name).unwrap_or_else(|| panic!("unknown target {name}; try `gb-fuzz list`"))]
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => TARGETS.iter().for_each(|t| println!("{}", t.name)),
        Some("seed") => seed(),
        Some("replay") => {
            let t = by_name(&args[1]).expect("target");
            let input = std::fs::read(&args[2]).expect("read file");
            match run_one(t, &input, Limits::default()) {
                Some(f) => {
                    println!("FAIL {}: {f}", t.name);
                    std::process::exit(1);
                }
                None => println!("ok"),
            }
        }
        Some("replay-iter") => {
            let t = by_name(&args[1]).expect("target");
            let seed: u64 = args[2].parse().expect("seed");
            let i: u64 = args[3].parse().expect("iteration");
            let seeds = harness::load_corpus(t);
            let dict = mutate::harvest(&seeds);
            let input = harness::input_for(t, &seeds, &dict, seed, i);
            let out = std::env::temp_dir().join(format!("gb-fuzz-{}-{seed}-{i}.bin", t.name));
            std::fs::write(&out, &input).unwrap();
            println!("input written to {} ({} bytes)", out.display(), input.len());
            if let Some(f) = run_one(t, &input, Limits::default()) {
                println!("FAIL {}: {f}", t.name);
                std::process::exit(1);
            }
            println!("ok");
        }
        Some("run") => run(&args),
        _ => {
            eprintln!(
                "usage: gb-fuzz run <target|all> [--seconds N] [--iters N] [--seed S] [--out DIR] | replay | replay-iter | seed | list"
            );
            std::process::exit(2);
        }
    }
}

fn run(args: &[String]) {
    let targets = pick(args.get(1).map_or("all", String::as_str));
    let seconds: u64 = arg(args, "--seconds").map_or(60, |s| s.parse().expect("--seconds"));
    let iterations: Option<u64> = arg(args, "--iters").map(|s| s.parse().expect("--iters"));
    let seed: u64 = arg(args, "--seed").map_or(0x6B65_656B, |s| s.parse().expect("--seed"));
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| "dist/fuzz/run".into()));
    let per = Duration::from_secs(seconds);
    let mut failed = false;
    for t in targets {
        let started = Instant::now();
        let c = Campaign {
            seed,
            iterations,
            deadline: iterations.is_none().then(|| started + per),
            start_iter: arg(args, "--start").map_or(0, |s| s.parse().expect("--start")),
            limits: Limits::default(),
            crash_dir: out.join("crashes"),
            max_findings: 20,
        };
        println!(
            "[{}] start seed={seed} budget={}",
            t.name,
            iterations.map_or(format!("{seconds}s"), |n| format!("{n} iters"))
        );
        let r = run_campaign(t, &c);
        println!(
            "[{}] {} runs in {:.1}s, {} findings",
            t.name,
            r.runs,
            started.elapsed().as_secs_f32(),
            r.findings.len()
        );
        for (i, f, path) in &r.findings {
            failed = true;
            println!(
                "  FINDING iter={i} {f}\n    saved {}  (replay: gb-fuzz replay-iter {} {seed} {i})",
                path.display(),
                t.name
            );
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn seed() {
    let root = workspace_root();
    let fixtures: Vec<(String, Vec<u8>, String)> = [
        "crates/bridge-kde/tests/fixtures/kdeconnect-real",
        "crates/bridge-kde/tests/fixtures/kdeconnect-android-real",
    ]
    .iter()
    .flat_map(|d| {
        std::fs::read_dir(root.join(d))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
    })
    .filter(|e| {
        e.path().extension().is_some_and(|x| x == "json") && e.file_name() != "manifest.json"
    })
    .filter_map(|e| {
        let b = std::fs::read(e.path()).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
        let kind = v.get("type")?.as_str()?.to_owned();
        let dir = e
            .path()
            .parent()?
            .file_name()?
            .to_string_lossy()
            .into_owned();
        Some((
            format!("{dir}--{}", e.file_name().to_string_lossy()),
            b,
            kind,
        ))
    })
    .collect();
    for t in TARGETS {
        let dir = root.join("fuzz/corpus").join(t.name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut n = 0;
        for (name, bytes, kind) in &fixtures {
            if bytes.len() <= 16 * 1024
                && t.fixture_prefixes
                    .iter()
                    .any(|p| *p == "*" || kind.starts_with(p))
            {
                std::fs::write(dir.join(name), bytes).unwrap();
                n += 1;
            }
        }
        for (i, b) in (t.builtin_seeds)().iter().enumerate() {
            std::fs::write(dir.join(format!("builtin-{i:02}")), b).unwrap();
            n += 1;
        }
        println!("{}: {n} seeds", t.name);
    }
}
