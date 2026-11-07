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
//! Only private, read-only or executable, file-backed mappings (`r-xp` and `r--p` with a path: code, `.rodata`, `.eh_frame`, the
//! dynamic symbol tables) with no privately written page are touched. Those are never
//! written (no text relocations), so dropping them cannot lose data. Writable data and the
//! read-only-after-relocation mappings are skipped on purpose: `MADV_DONTNEED` there would revert
//! patched pointers to the file's contents. The advice is given from inside `dl_iterate_phdr` only to the read-only
//! segments of loaded objects, so a library unloaded between reading `smaps` and `madvise` can never have its address
//! range (by then someone's heap or stack) zeroed. Linux (glibc or musl), 64-bit, only; a no-op elsewhere.

/// The `(start, length)` of every private, executable, file-backed mapping in `/proc/PID/maps` or
/// `/proc/PID/smaps` text that is safe to drop. A mapping is left alone when it
/// - is not a plain file path (`[vdso]`, anonymous JIT memory, `/memfd:` or `/dev/` device objects, SysV
///   shared memory): the bytes behind those can be the only copy, and
/// - has any anonymous page (`Anonymous:` above zero in smaps): a private page that was written
///   once (a debugger's breakpoint, a text relocation, a hot patch) is a copy-on-write page that
///   `MADV_DONTNEED` would silently turn back into the file's bytes.
///
/// `Private_Dirty` / `Shared_Dirty` are deliberately not looked at: for a file mapping they also count page-cache pages that
/// are dirty because the file was just written (a binary linked seconds ago, which is what CI runs), not a private copy.
/// Skipping those left the whole text resident on the CI runner (8 MB RSS against 2-4 MB for the same binary a minute later).
///
/// Given plain `maps` text there are no `Anonymous:` lines to check; the caller reads `smaps`.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), test))]
fn code_ranges(maps: &str) -> Vec<(usize, usize)> {
    fn plain_file(path: &str) -> bool {
        path.starts_with('/')
            && !path.starts_with("/memfd:")
            // A file on tmpfs (`/dev/shm/app`) is an ordinary file; the other `/dev/` paths are devices.
            && (!path.starts_with("/dev/") || path.starts_with("/dev/shm/"))
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
            if key == "Anonymous" {
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
        if !(perms == "r-xp" || perms == "r--p") || !plain_file(path) {
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

/// The part of `[start, end)` (a loaded object's read-only segment, any alignment) that lies inside one of the page-aligned
/// `candidates` `(start, len)`, rounded outwards to `page` (a power of two), as `(start, len)` pairs. Pure; the
/// rounding stays inside the candidate, so a neighbouring mapping is never touched.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), test))]
fn within_candidates(
    start: usize,
    end: usize,
    candidates: &[(usize, usize)],
    page: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for &(c, len) in candidates {
        let (lo, hi) = (start.max(c), end.min(c.saturating_add(len)));
        if lo >= hi {
            continue;
        }
        let lo = (lo & !(page - 1)).max(c);
        let hi = (hi.saturating_add(page - 1) & !(page - 1)).min(c.saturating_add(len));
        if hi > lo {
            out.push((lo, hi - lo));
        }
    }
    out
}

/// ELF program-header and `dl_iterate_phdr` layouts of 64-bit Linux (glibc and musl share them); only the leading fields
/// of `dl_phdr_info` are declared, the loader passes its real size in the callback.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod elf {
    #[repr(C)]
    pub struct Phdr {
        pub p_type: u32,
        pub p_flags: u32,
        pub p_offset: u64,
        pub p_vaddr: u64,
        pub p_paddr: u64,
        pub p_filesz: u64,
        pub p_memsz: u64,
        pub p_align: u64,
    }
    #[repr(C)]
    pub struct PhdrInfo {
        pub addr: usize,
        pub name: *const core::ffi::c_char,
        pub phdr: *const Phdr,
        pub phnum: u16,
    }
    pub const PT_LOAD: u32 = 1;
    pub const PF_W: u32 = 2;
}

/// Drop this process's mappings of idle code pages. Returns the bytes of address space advised
/// (the page cache keeps the data). Cheap: one read of `/proc/self/smaps` and a few syscalls.
///
/// `smaps` is a snapshot: by the time `madvise` runs, a library could have been unloaded and its address range handed to
/// an anonymous mapping (a thread stack, the heap), where `MADV_DONTNEED` would zero live data. So the advice is
/// only given inside the read-only `PT_LOAD` segments of the objects the dynamic loader reports, from within
/// `dl_iterate_phdr`, which holds the loader's lock: no `dlopen`/`dlclose` can map or unmap anything meanwhile, so
/// the range is still the same file's mapping. File mappings that are not loaded objects (a data file, a locale archive) are left alone.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn drop_idle() -> usize {
    use core::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
        fn sysconf(name: c_int) -> core::ffi::c_long;
        fn dl_iterate_phdr(
            callback: unsafe extern "C" fn(*mut elf::PhdrInfo, usize, *mut c_void) -> c_int,
            data: *mut c_void,
        ) -> c_int;
    }
    const MADV_DONTNEED: i32 = 4;
    const SC_PAGESIZE: c_int = 30;
    struct Job {
        candidates: Vec<(usize, usize)>,
        page: usize,
        advised: usize,
        pending: Vec<(usize, usize)>,
    }
    unsafe extern "C" fn visit(info: *mut elf::PhdrInfo, _size: usize, data: *mut c_void) -> c_int {
        // SAFETY: `data` is the `&mut Job` passed below and `info` is the loader's record for one object, valid for this
        // call, with `phnum` program headers at `phdr`.
        let (job, info) = unsafe { (&mut *data.cast::<Job>(), &*info) };
        if info.phdr.is_null() {
            return 0;
        }
        // Read every program header first and advise afterwards: the headers sit in the object's first read-only
        // mapping, and dropping that before reading the next header would fault it (and 15 neighbours) straight back in.
        job.pending.clear();
        for i in 0..usize::from(info.phnum) {
            let ph = unsafe { &*info.phdr.add(i) };
            if ph.p_type != elf::PT_LOAD || ph.p_flags & elf::PF_W != 0 {
                continue;
            }
            let start = info.addr.wrapping_add(ph.p_vaddr as usize);
            let end = start.wrapping_add(ph.p_memsz as usize);
            if end > start {
                job.pending
                    .extend(within_candidates(start, end, &job.candidates, job.page));
            }
        }
        for &(a, len) in &job.pending {
            // SAFETY: inside a mapping that smaps showed as private, file-backed, read-only or executable and never
            // written, of an object the loader still holds (it cannot be unloaded while this callback runs, so the
            // range cannot have been reused); MADV_DONTNEED only drops page-table entries, the next access faults
            // the same bytes back in.
            if unsafe { madvise(a as *mut c_void, len, MADV_DONTNEED) } == 0 {
                job.advised += len;
            }
        }
        0
    }
    let Ok(maps) = std::fs::read_to_string("/proc/self/smaps") else {
        return 0;
    };
    let page = match unsafe { sysconf(SC_PAGESIZE) } {
        p if p >= 4096 && (p & (p - 1)) == 0 => p as usize,
        _ => return 0,
    };
    let mut job = Job {
        candidates: code_ranges(&maps),
        page,
        advised: 0,
        pending: Vec::with_capacity(32),
    };
    // SAFETY: the callback only reads the loader's records and the `Job`; it never panics or unwinds.
    unsafe { dl_iterate_phdr(visit, (&raw mut job).cast()) };
    job.advised
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn drop_idle() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_only_private_read_only_or_executable_file_mappings() {
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
                (0x55d0c8a00000, 0x1000),
                (0x55d0c8a01000, 0x55d0c9000000 - 0x55d0c8a01000),
                (0x55d0c9000000, 0x200000),
                (0x7f0000000000, 0x100000)
            ]
        );
        assert!(code_ranges("junk\n\n").is_empty());
    }

    #[test]
    fn leaves_memfd_device_and_privately_modified_mappings_alone_but_takes_dirty_page_cache() {
        let smaps = "\
7f0000000000-7f0000100000 r-xp 00000000 08:01 200 /usr/lib/libc.so.6
Size:               1024 kB
Anonymous:             0 kB
7f0000200000-7f0000300000 r-xp 00000000 00:01 5 /memfd:jit (deleted)
Anonymous:             0 kB
7f0000400000-7f0000500000 r-xp 00000000 08:01 300 /usr/lib/libpatched.so
Anonymous:             4 kB
7f0000600000-7f0000700000 r-xp 00000000 00:05 6 /dev/dri/card0
Anonymous:             0 kB
7f0000800000-7f0000900000 r-xp 00000000 08:01 400 /usr/lib/libjustbuilt.so
Anonymous:             0 kB
Private_Dirty:         8 kB
Shared_Dirty:        512 kB
7f0000a00000-7f0000b00000 r--p 00000000 08:01 500 /usr/lib/librelro.so
Anonymous:            16 kB
7f0000c00000-7f0000d00000 r--p 00000000 08:01 501 /usr/lib/librodata.so
Anonymous:             0 kB
7f0000e00000-7f0000f00000 r-xp 00000000 00:05 7 /dev/shm/gb/svc
Anonymous:             0 kB
7f0001000000-7f0001100000 r-xp 00000000 00:01 8 /dev/shm/gb/patched
Anonymous:             4 kB
";
        assert_eq!(
            code_ranges(smaps),
            vec![
                (0x7f0000000000, 0x100000),
                (0x7f0000800000, 0x100000),
                (0x7f0000c00000, 0x100000),
                (0x7f0000e00000, 0x100000)
            ]
        );
    }

    #[test]
    fn advice_stays_inside_the_candidate_and_is_rounded_to_pages() {
        let c = [(0x10000, 0x4000), (0x20000, 0x1000)];
        // A segment that starts and ends inside the first candidate, off the page grid: rounded outwards.
        assert_eq!(
            within_candidates(0x10800, 0x11200, &c, 0x1000),
            vec![(0x10000, 0x2000)]
        );
        // A segment that spills past both ends of a candidate is clipped to it.
        assert_eq!(
            within_candidates(0xf000, 0x15000, &c, 0x1000),
            vec![(0x10000, 0x4000)]
        );
        // Rounding never leaves the candidate even with a large page.
        assert_eq!(
            within_candidates(0x10100, 0x10200, &c, 0x4000),
            vec![(0x10000, 0x4000)]
        );
        // Spanning two candidates gives two pieces; touching or missing ones give nothing.
        assert_eq!(
            within_candidates(0x13000, 0x20800, &c, 0x1000),
            vec![(0x13000, 0x1000), (0x20000, 0x1000)]
        );
        assert!(within_candidates(0x14000, 0x20000, &c, 0x1000).is_empty());
        assert!(within_candidates(0x30000, 0x31000, &c, 0x1000).is_empty());
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
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
