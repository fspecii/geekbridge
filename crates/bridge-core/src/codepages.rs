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
//! Only private, executable, file-backed mappings (`r-xp` with a path) with no privately written page are touched. Those are never
//! written (no text relocations), so dropping them cannot lose data. Writable data and the
//! read-only-after-relocation mappings are skipped on purpose: `MADV_DONTNEED` there would revert
//! patched pointers to the file's contents. Linux (glibc or musl) only; a no-op elsewhere.

/// The `(start, length)` of every private, executable, file-backed mapping in `/proc/PID/maps` or
/// `/proc/PID/smaps` text that is safe to drop. A mapping is left alone when it
/// - is not a plain file path (`[vdso]`, anonymous JIT memory, `/memfd:` or `/dev/` objects, SysV
///   shared memory): the bytes behind those can be the only copy, and
/// - has any anonymous page (`Anonymous:` above zero in smaps): a private page that was written
///   once (a debugger's breakpoint, a text relocation, a hot patch) is a copy-on-write page that
///   `MADV_DONTNEED` would silently turn back into the file's bytes.
///
/// Given plain `maps` text there are no `Anonymous:` lines to check; the caller reads `smaps`.
#[cfg(any(target_os = "linux", test))]
fn code_ranges(maps: &str) -> Vec<(usize, usize)> {
    fn plain_file(path: &str) -> bool {
        path.starts_with('/')
            && !path.starts_with("/memfd:")
            && !path.starts_with("/dev/")
            && !path.starts_with("/SYSV")
            && !path.starts_with("/proc/")
            && !path.starts_with("/run/user/")
    }
    let mut out: Vec<(usize, usize)> = Vec::new();
    // Where the current mapping's range sits in `out`, if it qualified.
    let mut current: Option<usize> = None;
    for line in maps.lines() {
        let mut f = line.split_whitespace();
        let Some(first) = f.next() else { continue };
        if let Some(key) = first.strip_suffix(':') {
            // A smaps attribute line of the current mapping.
            if key == "Anonymous" || key == "Private_Dirty" || key == "Shared_Dirty" {
                let kb: u64 = f.next().and_then(|v| v.parse().ok()).unwrap_or(1);
                if kb > 0
                    && let Some(i) = current.take()
                {
                    out.remove(i);
                }
            }
            continue;
        }
        current = None;
        let (Some(perms), Some(path)) = (f.next(), f.nth(3)) else {
            continue;
        };
        if perms != "r-xp" || !plain_file(path) {
            continue;
        }
        let Some((a, b)) = first.split_once('-') else {
            continue;
        };
        let (Ok(a), Ok(b)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16)) else {
            continue;
        };
        if b > a {
            out.push((a, b - a));
            current = Some(out.len() - 1);
        }
    }
    out
}

/// Drop this process's mappings of idle code pages. Returns the bytes of address space advised
/// (the page cache keeps the data). Cheap: one read of `/proc/self/smaps` and a few syscalls.
#[cfg(target_os = "linux")]
pub fn drop_idle() -> usize {
    unsafe extern "C" {
        fn madvise(addr: *mut core::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    const MADV_DONTNEED: i32 = 4;
    let Ok(maps) = std::fs::read_to_string("/proc/self/smaps") else {
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

    #[test]
    fn leaves_memfd_device_and_privately_modified_mappings_alone() {
        let smaps = "\
7f0000000000-7f0000100000 r-xp 00000000 08:01 200 /usr/lib/libc.so.6
Size:               1024 kB
Anonymous:             0 kB
7f0000200000-7f0000300000 r-xp 00000000 00:01 5 /memfd:jit (deleted)
Anonymous:             0 kB
7f0000400000-7f0000500000 r-xp 00000000 08:01 300 /usr/lib/libpatched.so
Anonymous:             4 kB
7f0000600000-7f0000700000 r-xp 00000000 00:05 6 /dev/shm/code
Anonymous:             0 kB
7f0000800000-7f0000900000 r-xp 00000000 08:01 400 /usr/lib/libdirty.so
Anonymous:             0 kB
Private_Dirty:         8 kB
";
        assert_eq!(code_ranges(smaps), vec![(0x7f0000000000, 0x100000)]);
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
