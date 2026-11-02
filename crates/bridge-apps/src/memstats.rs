//! Live-heap accounting for memory work (feature `memstats`, off in releases). A counting global
//! allocator tracks live and peak bytes of everything Rust allocates; GL driver allocations
//! (Mesa, NVIDIA) bypass it, so `rss - live` is roughly "what the driver and libraries cost".
//! Set `GEEKBRIDGE_MEMSTATS=1` to get a line on stderr every 3 s.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: forwards every call to `System` unchanged and only adds atomic counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
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
            LIVE.fetch_sub(l.size(), Relaxed);
            grew(new);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// (live bytes, peak bytes) of Rust allocations.
pub fn snapshot() -> (usize, usize) {
    (LIVE.load(Relaxed), PEAK.load(Relaxed))
}

/// Starts the reporter thread when `GEEKBRIDGE_MEMSTATS` is set.
pub fn start() {
    if std::env::var_os("GEEKBRIDGE_MEMSTATS").is_none() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("gb-memstats".into())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3));
                let (live, peak) = snapshot();
                let rss = std::fs::read_to_string("/proc/self/statm")
                    .ok()
                    .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
                    .map_or(0, |pages| pages * 4);
                eprintln!(
                    "memstats live={} kB peak={} kB rss={rss} kB",
                    live / 1024,
                    peak / 1024
                );
            }
        });
}

/// Prints the current live/peak heap with a label (when the reporter is enabled).
pub fn mark(label: &str) {
    if std::env::var_os("GEEKBRIDGE_MEMSTATS").is_some() {
        let (live, peak) = snapshot();
        eprintln!(
            "memstats mark {label}: live={} kB peak={} kB",
            live / 1024,
            peak / 1024
        );
    }
}
