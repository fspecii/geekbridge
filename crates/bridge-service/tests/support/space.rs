//! Scratch space for tests that move big payloads. Payloads are generated, not stored, wherever
//! possible; where a real file is unavoidable it lives under `GEEKBRIDGE_TEST_TMP` (default: the
//! system temp dir), and a test that cannot get the room because the *environment* is full (a
//! shared tmpfs quota, a small disk) skips with a message instead of failing. A failure with room
//! to spare is still a failure.
#![allow(dead_code)]

use std::{io, path::Path, path::PathBuf};

/// Where big test files go: `GEEKBRIDGE_TEST_TMP`, else `std::env::temp_dir()`.
pub fn root() -> PathBuf {
    std::env::var_os("GEEKBRIDGE_TEST_TMP")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// A scratch directory under [`root`], removed on drop.
pub fn tempdir() -> tempfile::TempDir {
    let root = root();
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("gb-test-")
        .tempdir_in(root)
        .unwrap()
}

/// ENOSPC, EDQUOT or `StorageFull` / `QuotaExceeded`: the disk or quota is full, not the product.
pub fn is_env_full(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::QuotaExceeded | io::ErrorKind::StorageFull
    ) || matches!(e.raw_os_error(), Some(28 | 122))
}

/// Tries to reserve `bytes` under `dir` (then releases them). `Some(why)` only when the
/// environment cannot hold them.
pub fn shortfall(dir: &Path, bytes: u64) -> Option<String> {
    let probe = dir.join(".gb-space-probe");
    let r = (|| {
        let f = std::fs::File::create(&probe)?;
        reserve(&f, bytes)
    })();
    let _ = std::fs::remove_file(&probe);
    match r {
        Err(e) if is_env_full(&e) => Some(format!("{e} while reserving {} MiB", bytes >> 20)),
        _ => None,
    }
}

#[cfg(unix)]
fn reserve(f: &std::fs::File, bytes: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // posix_fallocate returns the errno itself (EOPNOTSUPP on some filesystems: not a shortage).
    match unsafe { libc::posix_fallocate(f.as_raw_fd(), 0, bytes as libc::off_t) } {
        0 => Ok(()),
        n => Err(io::Error::from_raw_os_error(n)),
    }
}
#[cfg(not(unix))]
fn reserve(f: &std::fs::File, bytes: u64) -> io::Result<()> {
    f.set_len(bytes)
}

/// `true` (after printing why) when `dir` cannot hold `bytes`; the caller returns early.
pub fn skip_without_room(dir: &Path, bytes: u64) -> bool {
    match shortfall(dir, bytes) {
        Some(why) => {
            eprintln!(
                "SKIPPED: not enough scratch space in {} ({why}); set GEEKBRIDGE_TEST_TMP to a \
                 roomier directory",
                dir.display()
            );
            true
        }
        None => false,
    }
}
