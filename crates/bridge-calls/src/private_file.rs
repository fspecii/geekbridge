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

/// Replace `path` with `bytes` so that a crash or power cut leaves either the old file or the new one: a private
/// temporary file next to it (made as in [`write`]), flushed to disk, renamed over `path`, and the directory flushed.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad file name"))?;
    let tmp = path.with_file_name(bridge_core::statefile::unique_temp_name(name));
    if let Err(e) =
        write(&tmp, bytes).and_then(|()| bridge_core::statefile::rename_replace(&tmp, path))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
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

    #[test]
    fn replace_swaps_the_file_leaves_no_temp_and_keeps_the_old_one_on_failure() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("choice.json");
        replace(&f, b"one").unwrap();
        replace(&f, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        // The destination is a directory: the rename fails, the temp file goes, nothing else is touched.
        let dir_target = d.path().join("sub");
        std::fs::create_dir(&dir_target).unwrap();
        assert!(replace(&dir_target, b"x").is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two");
    }

    #[test]
    fn concurrent_replaces_all_succeed_and_leave_one_whole_file() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("choice.json");
        for _ in 0..20 {
            let hs: Vec<_> = (0..8u8)
                .map(|i| {
                    let f = f.clone();
                    std::thread::spawn(move || replace(&f, &vec![b'a' + i; 10_000]))
                })
                .collect();
            for h in hs {
                h.join().unwrap().unwrap();
            }
            let got = std::fs::read(&f).unwrap();
            assert!(got.len() == 10_000 && got.iter().all(|b| *b == got[0]));
            assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        }
    }
}
