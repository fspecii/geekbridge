// SPDX-License-Identifier: GPL-3.0-or-later
//! Running fuzz inputs with resource checks: counting allocator (peak extra bytes per input), a
//! per-input time limit and a watchdog that aborts on a true hang, plus crash-file reporting.

use crate::{mutate, rng::Rng, targets::Target};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    any::Any,
    fmt,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        Mutex, Once,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

/// `#[global_allocator]` in a fuzz binary or test: tracks live bytes and the peak since the last reset.
pub struct CountingAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn note(grow: usize) {
    let now = LIVE.fetch_add(grow, Relaxed) + grow;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: forwards every call to `System` unchanged; the counters are only bookkeeping.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            note(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            note(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                note(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

/// Peak live bytes above the level at `reset_peak`.
fn reset_peak() -> usize {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    base
}

#[derive(Clone, Copy)]
pub struct Limits {
    /// Slower than this is reported as a timeout finding.
    pub max_time: Duration,
    /// A single input still running after this aborts the process (after saving the input).
    pub hang: Duration,
    /// Allocation allowance: `base + per_input_byte * input.len()` extra live bytes at peak.
    pub alloc_base: usize,
    pub alloc_per_byte: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_time: Duration::from_millis(1500),
            hang: Duration::from_secs(20),
            alloc_base: 24 << 20,
            alloc_per_byte: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Finding {
    Panic(String),
    Slow(Duration),
    Alloc { peak: usize, allowed: usize },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finding::Panic(m) => write!(f, "panic: {m}"),
            Finding::Slow(d) => write!(f, "slow input: {d:?}"),
            Finding::Alloc { peak, allowed } => {
                write!(f, "allocation {peak} bytes over allowance {allowed}")
            }
        }
    }
}

static PANIC_MSG: Mutex<Option<String>> = Mutex::new(None);
static PANICKED: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

/// Record panics (on any thread) instead of printing them, so a campaign's log stays readable.
fn install_hook() {
    HOOK.call_once(|| {
        panic::set_hook(Box::new(|info| {
            let msg = format!("{info}");
            if let Ok(mut g) = PANIC_MSG.lock() {
                g.get_or_insert(msg);
            }
            PANICKED.store(true, Relaxed);
        }));
    });
}

fn payload_text(p: Box<dyn Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "non-string panic".into())
}

// ------------------------------------------------------------------------------------ watchdog

static CURRENT: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
static STARTED_MS: AtomicU64 = AtomicU64::new(0);
static ITER: AtomicU64 = AtomicU64::new(0);
static WATCHDOG: Once = Once::new();

fn epoch() -> Instant {
    static E: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *E.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

fn start_watchdog(limits: Limits, crash_dir: PathBuf) {
    WATCHDOG.call_once(|| {
        let _ = epoch();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let started = STARTED_MS.load(Relaxed);
                if started != 0 && now_ms().saturating_sub(started) > limits.hang.as_millis() as u64
                {
                    // Not holding the lock the hung thread may own: try_lock only.
                    if let Ok(g) = CURRENT.try_lock()
                        && let Some((name, input)) = g.as_ref()
                    {
                        let path = save_crash(&crash_dir, name, "hang", input);
                        eprintln!("HANG in target {name}: input saved to {}", path.display());
                    } else {
                        eprintln!("HANG (input not recoverable)");
                    }
                    std::process::abort();
                }
            }
        });
    });
}

pub fn save_crash(dir: &Path, target: &str, kind: &str, input: &[u8]) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
    }
    let path = dir.join(format!("{kind}-{target}-{h:016x}"));
    let _ = std::fs::write(&path, input);
    path
}

// --------------------------------------------------------------------------------------- run

/// Run one input; `None` means it passed every check.
pub fn run_one(t: &Target, input: &[u8], limits: Limits) -> Option<Finding> {
    install_hook();
    PANICKED.store(false, Relaxed);
    if let Ok(mut g) = PANIC_MSG.lock() {
        *g = None;
    }
    if let Ok(mut g) = CURRENT.lock() {
        *g = Some((t.name.to_owned(), input.to_vec()));
    }
    let base = reset_peak();
    let t0 = Instant::now();
    STARTED_MS.store(now_ms(), Relaxed);
    let result = panic::catch_unwind(AssertUnwindSafe(|| (t.run)(input)));
    STARTED_MS.store(0, Relaxed);
    let took = t0.elapsed();
    let peak = PEAK.load(Relaxed).saturating_sub(base);

    if let Err(p) = result {
        return Some(Finding::Panic(payload_text(p)));
    }
    if PANICKED.load(Relaxed) {
        let msg = PANIC_MSG
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
        return Some(Finding::Panic(format!("(on another thread) {msg}")));
    }
    let allowed = limits.alloc_base + limits.alloc_per_byte * input.len();
    if peak > allowed {
        return Some(Finding::Alloc { peak, allowed });
    }
    if took > limits.max_time {
        return Some(Finding::Slow(took));
    }
    None
}

/// Read every regular file under `fuzz/corpus/<target>` (small files only).
pub fn load_corpus(t: &Target) -> Vec<Vec<u8>> {
    let dir = workspace_root().join("fuzz/corpus").join(t.name);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if let Ok(b) = std::fs::read(&p)
                && b.len() <= 64 * 1024
            {
                out.push(b);
            }
        }
    }
    if out.is_empty() {
        out.extend((t.builtin_seeds)());
    }
    out
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub struct Report {
    pub target: &'static str,
    pub runs: u64,
    pub findings: Vec<(u64, Finding, PathBuf)>,
}

pub struct Campaign {
    pub seed: u64,
    pub iterations: Option<u64>,
    pub deadline: Option<Instant>,
    pub start_iter: u64,
    pub limits: Limits,
    pub crash_dir: PathBuf,
    /// Stop collecting after this many distinct findings for one target.
    pub max_findings: usize,
}

/// The input for iteration `i` of a campaign: pure function of `(seed, i)`.
pub fn input_for(t: &Target, seeds: &[Vec<u8>], dict: &mutate::Dict, seed: u64, i: u64) -> Vec<u8> {
    let mut rng = Rng::for_iteration(seed, i);
    // The first pass over the seeds replays them untouched so the happy path is always exercised.
    if (i as usize) < seeds.len() {
        return seeds[i as usize].clone();
    }
    mutate::next_input(&mut rng, seeds, dict, t.kinds)
}

pub fn run_campaign(t: &Target, c: &Campaign) -> Report {
    start_watchdog(c.limits, c.crash_dir.clone());
    let seeds = load_corpus(t);
    let dict = mutate::harvest(&seeds);
    let mut report = Report {
        target: t.name,
        runs: 0,
        findings: Vec::new(),
    };
    let mut i = c.start_iter;
    loop {
        if c.iterations.is_some_and(|n| report.runs >= n)
            || c.deadline.is_some_and(|d| Instant::now() >= d)
        {
            break;
        }
        let input = input_for(t, &seeds, &dict, c.seed, i);
        ITER.store(i, Relaxed);
        if let Some(f) = run_one(t, &input, c.limits) {
            let kind = match f {
                Finding::Panic(_) => "crash",
                Finding::Slow(_) => "timeout",
                Finding::Alloc { .. } => "oom",
            };
            let path = save_crash(&c.crash_dir, t.name, kind, &input);
            report.findings.push((i, f, path));
            if report.findings.len() >= c.max_findings {
                report.runs += 1;
                break;
            }
        }
        report.runs += 1;
        i += 1;
    }
    report
}

/// Iteration the campaign is on (for the runner's progress line).
pub fn current_iteration() -> u64 {
    ITER.load(Relaxed)
}
