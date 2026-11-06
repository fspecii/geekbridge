// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix backend: mode 0600/0700, owner checks and `flock`, all on open file
//! descriptors (`O_NOFOLLOW` + `fstat`) so that a path swapped after a check
//! cannot redirect what we read or write.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{KdeError, Result};

use super::MAX_STATE_FILE;

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn insecure(what: String, detail: &str) -> KdeError {
    KdeError::InsecurePermissions {
        what,
        detail: detail.into(),
    }
}

/// `O_NOFOLLOW` reports a symlink as ELOOP (EMLINK on FreeBSD).
fn is_symlink_refusal(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(c) if c == libc::ELOOP || c == libc::EMLINK)
}

/// Create `dir` (mode 0700) if needed, then require that it is a real
/// directory (not a symlink), owned by us, and not accessible to others.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    open_private_dir(dir).map(|_| ())
}

/// Like [`ensure_private_dir`] but hands back the verified descriptor.
fn open_private_dir(dir: &Path) -> Result<File> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => {}
        // Already there (or a non-directory squatting on the name); the
        // checks on the opened descriptor below decide.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let what = format!("state directory {}", dir.display());
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| {
            if is_symlink_refusal(&e) || e.raw_os_error() == Some(libc::ENOTDIR) {
                insecure(what.clone(), "not a directory (or is a symlink)")
            } else {
                e.into()
            }
        })?;
    // fstat on the descriptor we will keep using: no check/use gap.
    check_owner_and_mode(&f.metadata()?, what, 0o077)?;
    Ok(f)
}

fn check_owner_and_mode(md: &fs::Metadata, what: String, forbidden: u32) -> Result<()> {
    if md.uid() != euid() {
        return Err(insecure(what, "owned by another user"));
    }
    if md.mode() & forbidden != 0 {
        return Err(KdeError::InsecurePermissions {
            what,
            detail: format!("mode {:o} grants access to group/other", md.mode() & 0o777),
        });
    }
    Ok(())
}

/// Read a private file, verifying type, owner and mode on the open descriptor.
/// `None` if absent.
pub fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let what = format!("file {}", path.display());
    let f = match fs::OpenOptions::new()
        .read(true)
        // NONBLOCK: a FIFO planted at the path must not hang us in open().
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if is_symlink_refusal(&e) => {
            return Err(insecure(what, "not a regular file (or is a symlink)"));
        }
        Err(e) => return Err(e.into()),
    };
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(insecure(what, "not a regular file (or is a symlink)"));
    }
    check_owner_and_mode(&md, what.clone(), 0o077)?;
    if md.len() > MAX_STATE_FILE {
        return Err(KdeError::Store(format!(
            "{what} is larger than {MAX_STATE_FILE} bytes"
        )));
    }
    let mut buf = Vec::with_capacity(md.len() as usize);
    (&f).take(MAX_STATE_FILE + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_STATE_FILE {
        // Grew while we were reading.
        return Err(KdeError::Store(format!(
            "{what} is larger than {MAX_STATE_FILE} bytes"
        )));
    }
    Ok(Some(buf))
}

/// Write `data` to `path` atomically with mode 0600: temp file in the same
/// directory (O_EXCL), fsync, rename, fsync of the directory.
pub fn atomic_write_private(path: &Path, data: &[u8]) -> Result<()> {
    if data.len() as u64 > MAX_STATE_FILE {
        return Err(KdeError::Store(format!(
            "refusing to write {} bytes: more than the {MAX_STATE_FILE} byte limit that can be read back",
            data.len()
        )));
    }
    let dir = path
        .parent()
        .ok_or_else(|| KdeError::Store("path has no parent".into()))?;
    let dir_fd = open_private_dir(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| KdeError::Store("bad file name".into()))?;
    let tmp: PathBuf = dir.join(bridge_core::statefile::unique_temp_name(name));
    let result = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        bridge_core::statefile::slow_disk_hook();
        bridge_core::statefile::failpoint("before_rename");
        fs::rename(&tmp, path)?;
        dir_fd.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    } else {
        sweep_stale_temps(dir, name);
    }
    result
}

/// Remove leftovers of writers that died between creating the temp file and renaming it (a crash
/// or kill). Only files an hour old go, so a concurrent writer's live temp file is never touched.
fn sweep_stale_temps(dir: &Path, name: &str) {
    let prefix = format!(".{name}.tmp.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Exclusive `flock(2)` on a private lock file. Released on drop or when the
/// process dies, so there are no stale locks.
#[derive(Debug)]
pub struct FileLock {
    // Closing the descriptor releases the lock.
    _file: File,
}

pub fn lock_private_file(path: &Path, timeout: Duration) -> Result<FileLock> {
    let dir = path
        .parent()
        .ok_or_else(|| KdeError::Store("lock path has no parent".into()))?;
    ensure_private_dir(dir)?;
    let what = format!("lock file {}", path.display());
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            if is_symlink_refusal(&e) {
                insecure(what.clone(), "is a symlink")
            } else {
                e.into()
            }
        })?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(insecure(what, "not a regular file"));
    }
    check_owner_and_mode(&md, what, 0o077)?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: the descriptor is open and owned by `file` for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(FileLock { _file: file });
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(c) if c == libc::EINTR => continue,
            Some(c) if c == libc::EWOULDBLOCK || c == libc::EAGAIN => {
                if Instant::now() >= deadline {
                    return Err(KdeError::Store(format!(
                        "timed out after {timeout:?} waiting for another process to release {}",
                        path.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return Err(err.into()),
        }
    }
}
