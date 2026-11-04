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
}
