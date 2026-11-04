// SPDX-License-Identifier: GPL-3.0-or-later
//! Two seconds after start the service drops its idle code pages from its resident set
//! (`bridge_core::codepages`: 4.4 of 7.7 MB RSS measured on an idle service). The 20 s session-bus
//! probe (`coexist.rs`) repeats the drop after each run, since it touches the D-Bus code again.
//! Linux only; a no-op elsewhere.
/// Drop the idle code pages once, shortly after start. Call from inside the runtime.
pub fn after_startup() {
    #[cfg(target_os = "linux")]
    {
        // Late enough that the first sockets and the tray are up.
        const DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        tokio::spawn(async {
            tokio::time::sleep(DELAY).await;
            bridge_core::codepages::drop_idle();
        });
    }
}

/// Drop the idle code pages again after a LAN announcement (every 15 s). Building the datagram walks
/// the NIC list through libc and std, which maps about 0.5-1 MB of code back in; left alone the idle
/// resident set sat at 4.0-4.6 MB (7.7 MB right after the first announcement) instead of 3.5 MB.
pub fn after_announce() {
    bridge_core::codepages::drop_idle();
}

/// One malloc arena for the whole process (glibc only; set before the first thread starts). glibc gives every thread
/// that allocates its own arena, so each of the two runtime workers, the tray's executor and every short-lived
/// session thread (the Wi-Fi call audio worker, the writer to `pw-cat`) keeps its own free lists: measured on an idle
/// service with a stand-in phone linked, 1.33 MB of anonymous memory against 1.10 MB with `MALLOC_ARENA_MAX=1`, and
/// after two listen sessions 1.85 MB against 1.34 MB (docs/performance.md). The service is I/O bound, so the one lock is never contended.
pub fn single_arena() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: plain glibc call; made at the top of `main` while the process has one thread.
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, 1);
        }
    }
}

/// Give freed heap back to the OS after something that allocated a lot for a short time (a call audio session: the
/// codec's 150 kB of state, the jitter buffer, the pw-cat pipes). Once per session end, never per frame. glibc only.
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: plain glibc call, safe from any thread at any time.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    /// Resident file-backed bytes of this process (`Rss - Anonymous` of `smaps_rollup`), in kB.
    fn file_rss_kb() -> u64 {
        let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name))
                .and_then(|r| r.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        field("Rss:").saturating_sub(field("Anonymous:"))
    }

    #[test]
    fn the_announcement_trim_returns_resident_code_pages() {
        // Other test threads may fault pages back in between the two reads: any one of a few tries is enough.
        let mut seen = Vec::new();
        for _ in 0..5 {
            let before = file_rss_kb();
            super::after_announce();
            let after = file_rss_kb();
            if after < before {
                return;
            }
            seen.push((before, after));
        }
        panic!("the resident file-backed set never shrank: {seen:?}");
    }

    fn anon_kb() -> u64 {
        let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("Anonymous:"))
            .and_then(|r| r.trim().trim_end_matches("kB").trim().parse().ok())
            .unwrap_or(0)
    }

    /// Freed heap inside the arena is resident until it is trimmed: fragment 16 MB, free half of it in 8 kB holes, and
    /// `trim_heap` has to hand the holes back. (Other test threads allocate too, so look for a clear drop, not a number.)
    #[cfg(target_env = "gnu")]
    #[test]
    fn trim_heap_returns_freed_heap_to_the_os() {
        let mut blocks: Vec<Vec<u8>> = (0..2000).map(|_| vec![7u8; 8 * 1024]).collect();
        let full = anon_kb();
        for (i, b) in blocks.iter_mut().enumerate() {
            if i % 2 == 0 {
                *b = Vec::new();
            }
        }
        let freed = anon_kb();
        super::trim_heap();
        let trimmed = anon_kb();
        assert!(
            trimmed + 2 * 1024 < freed.max(full),
            "trim_heap kept {trimmed} kB (full {full}, after freeing {freed})"
        );
        assert_eq!(blocks.iter().filter(|b| !b.is_empty()).count(), 1000);
    }
}
