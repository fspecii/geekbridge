// SPDX-License-Identifier: GPL-3.0-or-later
//! Private-file helpers. Secrets and trust records are only written where the
//! OS lets us restrict them to the current user; elsewhere we refuse.
//!
//! * Unix: mode 0600 files / 0700 directories owned by the effective user,
//!   checked on the open descriptor (`O_NOFOLLOW` + `fstat`); `flock` locks.
//! * Windows: owner = current user and a protected DACL granting only that
//!   user and LocalSystem, checked on the open handle; reparse points refused;
//!   `LockFileEx` locks.
//! * Anything else: refuses with [`KdeError::UnsupportedPlatform`].

use std::path::Path;
use std::time::Duration;

use crate::error::Result;

/// Largest file this crate will read back (identity or trust store). Writes
/// above this are refused so that nothing is stored that cannot be loaded.
pub const MAX_STATE_FILE: u64 = 1024 * 1024;

/// How long [`lock_exclusive`] waits for another process before failing.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;
    use crate::error::KdeError;
    use std::time::Duration;

    const MSG: &str = "owner-only file permissions are only enforced on Unix and Windows; refusing to store keys or trust records";

    #[derive(Debug)]
    pub struct FileLock(());

    pub fn ensure_private_dir(_dir: &Path) -> Result<()> {
        Err(KdeError::UnsupportedPlatform(MSG))
    }
    pub fn read_private_file(_path: &Path) -> Result<Option<Vec<u8>>> {
        Err(KdeError::UnsupportedPlatform(MSG))
    }
    pub fn atomic_write_private(_path: &Path, _data: &[u8]) -> Result<()> {
        Err(KdeError::UnsupportedPlatform(MSG))
    }
    pub fn lock_private_file(_path: &Path, _timeout: Duration) -> Result<FileLock> {
        Err(KdeError::UnsupportedPlatform(MSG))
    }
}

pub use imp::{FileLock, atomic_write_private, ensure_private_dir, read_private_file};

/// Take an exclusive, cross-process lock on the private lock file `path`
/// (created if absent; its directory must satisfy [`ensure_private_dir`]),
/// waiting up to [`LOCK_TIMEOUT`]. The lock is released when the guard drops
/// or the process exits, whichever comes first, so a crashed holder never
/// leaves it stuck. The lock file itself is never deleted.
///
/// It is held on a separate file, never on the data file: the data file is
/// replaced by rename, which would silently swap the object being locked.
pub fn lock_exclusive(path: &Path) -> Result<FileLock> {
    lock_exclusive_timeout(path, LOCK_TIMEOUT)
}

/// [`lock_exclusive`] with an explicit timeout.
pub fn lock_exclusive_timeout(path: &Path, timeout: Duration) -> Result<FileLock> {
    imp::lock_private_file(path, timeout)
}

/// Owner-only storage backend for [`bridge_core::statefile`]: every read, write and lock goes
/// through this module's permission checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrivateIo;

impl bridge_core::statefile::StateIo for PrivateIo {
    type Err = crate::error::KdeError;

    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        read_private_file(path)
    }
    fn write_atomic(&self, path: &Path, data: &[u8]) -> Result<()> {
        atomic_write_private(path, data)
    }
    fn lock(&self, lock_path: &Path) -> Result<Box<dyn Send>> {
        Ok(Box::new(lock_exclusive(lock_path)?))
    }
    fn is_recoverable(&self, err: &crate::error::KdeError) -> bool {
        use crate::error::KdeError;
        // Oversized counts as damaged; loose permissions, links and an unreadable file
        // (`PermissionDenied`: intact but not ours to read) are refusals, so nothing replaces it.
        matches!(err, KdeError::Store(m) if m.contains("larger than"))
    }
}

/// A [`bridge_core::statefile::StateFile`] on owner-only storage.
pub type PrivateStateFile = bridge_core::statefile::StateFile<PrivateIo>;

impl From<bridge_core::statefile::Error<crate::error::KdeError>> for crate::error::KdeError {
    fn from(e: bridge_core::statefile::Error<crate::error::KdeError>) -> Self {
        use bridge_core::statefile::Error;
        match e {
            Error::Io(e) => e,
            Error::Corrupt { path, reason } => {
                crate::error::KdeError::StoreCorrupt { path, reason }
            }
            Error::ReadOnly { path, found, .. } => {
                crate::error::KdeError::StoreNewer { path, found }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::error::KdeError;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_write_is_private_and_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let file = dir.join("f");
        atomic_write_private(&file, b"one").unwrap();
        atomic_write_private(&file, b"two").unwrap();
        assert_eq!(read_private_file(&file).unwrap().unwrap(), b"two");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // No temp files left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn refuses_loose_permissions_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let file = dir.join("f");
        atomic_write_private(&file, b"x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            read_private_file(&file),
            Err(KdeError::InsecurePermissions { .. })
        ));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            ensure_private_dir(&dir),
            Err(KdeError::InsecurePermissions { .. })
        ));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(matches!(
            read_private_file(&link),
            Err(KdeError::InsecurePermissions { .. })
        ));
    }

    #[test]
    fn missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            read_private_file(&tmp.path().join("nope"))
                .unwrap()
                .is_none()
        );
    }
}
