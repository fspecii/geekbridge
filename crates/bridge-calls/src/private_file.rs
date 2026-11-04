// SPDX-License-Identifier: GPL-3.0-or-later
//! Small state files written without following a planted link and without being readable by others
//! (docs/reports/security-review-7.md S7-12).

use std::io::Write;
use std::path::Path;

/// Write `bytes` to a brand-new file at `path`, mode 0600 on Unix. Whatever was there (a stale temporary file, or a
/// symbolic link someone left in its place) is removed first, never written through.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_way_is_replaced_not_followed_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let victim = d.path().join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        let target = d.path().join("state.json.tmp");
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        write(&target, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}
