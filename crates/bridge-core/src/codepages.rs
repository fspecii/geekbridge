// SPDX-License-Identifier: GPL-3.0-or-later
//! Unmaps idle code pages from the process's own page tables to lower its resident set.
//!
//! Start-up leaves most of an executable's (and its shared libraries') code pages mapped: the
//! kernel maps 16 pages around every page fault, and start-up (TLS and D-Bus set-up, key loading,
//! GL driver initialisation) touches code all over the image that an idle process never runs again.
//! They are clean pages of files in the page cache, so they cost no private memory and the kernel
//! can reclaim them at will, but they count in the resident set (RSS): 4.4 of the 7.7 MB of an
//! idle service. `madvise(MADV_DONTNEED)` drops those page-table entries; whatever runs later
//! faults back in just the pages it needs (a minor fault, no I/O while the file is cached).
//!
//! Only private, executable, file-backed mappings (`r-xp` with a path) are touched. Those are never
//! written (no text relocations), so dropping them cannot lose data. Writable data and the
//! read-only-after-relocation mappings are skipped on purpose: `MADV_DONTNEED` there would revert
//! patched pointers to the file's contents. Linux (glibc or musl) only; a no-op elsewhere.

/// The `(start, length)` of every private, executable, file-backed mapping in `/proc/PID/maps` text.
#[cfg(any(target_os = "linux", test))]
fn code_ranges(maps: &str) -> Vec<(usize, usize)> {
    maps.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (range, perms) = (f.next()?, f.next()?);
            let path = f.nth(3)?; // offset, dev, inode, then the path
            if perms != "r-xp" || !path.starts_with('/') {
                return None;
            }
            let (a, b) = range.split_once('-')?;
            let (a, b) = (
                usize::from_str_radix(a, 16).ok()?,
                usize::from_str_radix(b, 16).ok()?,
            );
            (b > a).then_some((a, b - a))
        })
        .collect()
}

/// Drop this process's mappings of idle code pages. Returns the bytes of address space advised
/// (the page cache keeps the data). Cheap: one read of `/proc/self/maps` and a few syscalls.
#[cfg(target_os = "linux")]
pub fn drop_idle() -> usize {
    unsafe extern "C" {
        fn madvise(addr: *mut core::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    const MADV_DONTNEED: i32 = 4;
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return 0;
    };
    let mut advised = 0;
    for (start, len) in code_ranges(&maps) {
        // SAFETY: a clean, never-written private file mapping of this process; MADV_DONTNEED only
        // drops its page-table entries and the next access faults the same bytes back in.
        if unsafe { madvise(start as *mut core::ffi::c_void, len, MADV_DONTNEED) } == 0 {
            advised += len;
        }
    }
    advised
}

#[cfg(not(target_os = "linux"))]
pub fn drop_idle() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_only_private_executable_file_mappings() {
        let maps = "\
55d0c8a00000-55d0c8a01000 r--p 00000000 08:01 100 /usr/bin/geekbridge-service
55d0c8a01000-55d0c9000000 r-xp 00001000 08:01 100 /usr/bin/geekbridge-service
55d0c9000000-55d0c9200000 r--p 00600000 08:01 100 /usr/bin/geekbridge-service
55d0c9200000-55d0c9210000 rw-p 00800000 08:01 100 /usr/bin/geekbridge-service
7f0000000000-7f0000100000 r-xp 00000000 08:01 200 /usr/lib/libc.so.6
7f0000200000-7f0000300000 r-xp 00000000 00:00 0
7ffd00000000-7ffd00001000 r-xp 00000000 00:00 0 [vdso]
7f0000400000-7f0000500000 rwxp 00000000 08:01 300 /tmp/jit
";
        assert_eq!(
            code_ranges(maps),
            vec![
                (0x55d0c8a01000, 0x55d0c9000000 - 0x55d0c8a01000),
                (0x7f0000000000, 0x100000)
            ]
        );
        assert!(code_ranges("junk\n\n").is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_idle_code_keeps_the_process_working() {
        assert!(drop_idle() > 0);
        // Code and data used after the drop are paged back in transparently.
        let v: Vec<u32> = (0..1000).collect();
        assert_eq!(v.iter().sum::<u32>(), 499_500);
        assert!(drop_idle() > 0);
        assert_eq!(format!("{:?}", Some(3)), "Some(3)");
    }
}
