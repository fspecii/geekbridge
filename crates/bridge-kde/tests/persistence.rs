//! Persistence tests: private-file helpers and the file trust store, including
//! several processes and several store instances writing at once.
//!
//! Platform-neutral tests run on every OS; the permission/symlink tests that
//! need `chmod`/`symlink` are Unix-only. The Windows ACL tests live next to the
//! implementation (`src/fsutil/windows.rs`) because they need its internals.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

#[cfg(unix)]
use bridge_kde::fsutil::ensure_private_dir;
use bridge_kde::fsutil::{
    MAX_STATE_FILE, atomic_write_private, lock_exclusive, lock_exclusive_timeout, read_private_file,
};
use bridge_kde::{DeviceCredentials, DeviceId, FileTrustStore, KdeError, TrustStore, TrustedPeer};

fn peer_with_id(id: &str) -> TrustedPeer {
    let id = DeviceId::parse(id).unwrap();
    let creds = DeviceCredentials::generate_with_id(&id).unwrap();
    TrustedPeer {
        device_id: id,
        name: "Phone".into(),
        device_type: "phone".into(),
        protocol_version: 8,
        cert: creds.cert().clone(),
        paired_at_unix: 1,
    }
}

fn numbered_id(group: u32, n: u32) -> String {
    format!("{group:04}{n:04}{}", "0".repeat(24))
}

fn ids(store: &FileTrustStore) -> Vec<String> {
    let mut v: Vec<String> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|p| p.device_id.to_string())
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- fsutil

#[test]
fn oversize_writes_and_reads_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("state").join("f");
    let big = vec![b'x'; MAX_STATE_FILE as usize + 1];
    assert!(matches!(
        atomic_write_private(&file, &big),
        Err(KdeError::Store(_))
    ));
    // Exactly at the limit is fine and round-trips.
    let max = vec![b'y'; MAX_STATE_FILE as usize];
    atomic_write_private(&file, &max).unwrap();
    assert_eq!(read_private_file(&file).unwrap().unwrap().len(), max.len());
    // A larger file put there behind our back is refused, not truncated.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&file, &big).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(read_private_file(&file), Err(KdeError::Store(_))));
    }
}

#[test]
fn failed_write_leaves_old_content_and_no_temp_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let file = dir.join("f");
    atomic_write_private(&file, b"old").unwrap();
    // A directory squatting on the destination makes the final rename fail.
    let blocked = dir.join("blocked");
    fs::create_dir(&blocked).unwrap();
    assert!(atomic_write_private(&blocked, b"new").is_err());
    assert_eq!(read_private_file(&file).unwrap().unwrap(), b"old");
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.contains(".tmp.")), "{names:?}");
}

#[test]
fn lock_times_out_while_held_and_is_released_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let lock = tmp.path().join("state").join("x.lock");
    let first = lock_exclusive(&lock).unwrap();
    let started = Instant::now();
    let err = lock_exclusive_timeout(&lock, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(err, KdeError::Store(m) if m.contains("timed out")));
    assert!(started.elapsed() >= Duration::from_millis(150));
    drop(first);
    lock_exclusive_timeout(&lock, Duration::from_secs(2)).unwrap();
}

#[test]
fn lock_serialises_read_modify_write_across_threads() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let counter = dir.join("counter");
    let lock = dir.join("counter.lock");
    atomic_write_private(&counter, b"0").unwrap();
    let threads = 8;
    let per = 10;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let (counter, lock, barrier) = (counter.clone(), lock.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..per {
                    // Each iteration opens its own descriptor, like separate
                    // processes would, so this exercises the OS lock itself.
                    let _g = lock_exclusive(&lock).unwrap();
                    let n: u64 = String::from_utf8(read_private_file(&counter).unwrap().unwrap())
                        .unwrap()
                        .parse()
                        .unwrap();
                    atomic_write_private(&counter, (n + 1).to_string().as_bytes()).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        read_private_file(&counter).unwrap().unwrap(),
        (threads * per).to_string().as_bytes()
    );
}

// ----------------------------------------------------- FileTrustStore

#[test]
fn second_instance_sees_changes_and_does_not_lose_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let daemon = FileTrustStore::open(&dir).unwrap();
    let cli = FileTrustStore::open(&dir).unwrap();

    // Both were opened empty; each now writes a different peer.
    let a = peer_with_id(&numbered_id(1, 1));
    let b = peer_with_id(&numbered_id(1, 2));
    cli.insert(a.clone()).unwrap();
    // The long-running instance reads the CLI's pairing without a restart...
    assert_eq!(daemon.get(&a.device_id).unwrap().unwrap(), a);
    daemon.insert(b.clone()).unwrap();
    // ...and its own write did not overwrite the CLI's.
    assert_eq!(ids(&cli), ids(&daemon));
    assert_eq!(ids(&daemon).len(), 2);

    // Revocation by the other instance takes effect immediately.
    assert!(cli.remove(&a.device_id).unwrap());
    assert!(daemon.get(&a.device_id).unwrap().is_none());
    // Removing something another instance already removed is a no-op.
    assert!(!daemon.remove(&a.device_id).unwrap());
    assert_eq!(ids(&daemon), vec![b.device_id.to_string()]);

    // Stale-cache edge: `daemon` removes b, `cli` (still caching b) inserts a.
    assert!(daemon.remove(&b.device_id).unwrap());
    cli.insert(a.clone()).unwrap();
    assert_eq!(ids(&daemon), vec![a.device_id.to_string()]);
}

#[test]
fn deleting_the_file_revokes_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let s = FileTrustStore::open(&dir).unwrap();
    let p = peer_with_id(&numbered_id(2, 1));
    s.insert(p.clone()).unwrap();
    fs::remove_file(s.path()).unwrap();
    assert!(s.get(&p.device_id).unwrap().is_none());
    assert!(s.list().unwrap().is_empty());
    // The store keeps working.
    s.insert(p.clone()).unwrap();
    assert_eq!(s.list().unwrap().len(), 1);
}

#[test]
fn tampering_after_open_fails_closed_and_is_not_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let s = FileTrustStore::open(&dir).unwrap();
    let p = peer_with_id(&numbered_id(3, 1));
    s.insert(p.clone()).unwrap();

    let garbage = b"{ not json".to_vec();
    fs::write(s.path(), &garbage).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(s.path(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    // No stale trust from the cache, and writes refuse to clobber the damage.
    assert!(matches!(
        s.get(&p.device_id),
        Err(KdeError::StoreCorrupt { .. })
    ));
    assert!(matches!(s.list(), Err(KdeError::StoreCorrupt { .. })));
    assert!(matches!(
        s.insert(peer_with_id(&numbered_id(3, 2))),
        Err(KdeError::StoreCorrupt { .. })
    ));
    assert!(matches!(
        s.remove(&p.device_id),
        Err(KdeError::StoreCorrupt { .. })
    ));
    assert_eq!(fs::read(s.path()).unwrap(), garbage);
}

#[test]
fn concurrent_writers_with_separate_instances_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    drop(FileTrustStore::open(&dir).unwrap());
    let writers = 4;
    let per = 5;
    let barrier = Arc::new(Barrier::new(writers));
    let handles: Vec<_> = (0..writers as u32)
        .map(|w| {
            let (dir, barrier) = (dir.clone(), barrier.clone());
            std::thread::spawn(move || {
                let store = FileTrustStore::open(&dir).unwrap();
                let peers: Vec<_> = (0..per as u32)
                    .map(|n| peer_with_id(&numbered_id(10 + w, n)))
                    .collect();
                barrier.wait();
                for p in peers {
                    store.insert(p).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let store = FileTrustStore::open(&dir).unwrap();
    assert_eq!(store.list().unwrap().len(), writers * per);
}

const CHILD_DIR: &str = "GEEKBRIDGE_PERSIST_CHILD_DIR";
const CHILD_GROUP: &str = "GEEKBRIDGE_PERSIST_CHILD_GROUP";

/// Not a real test: the body run inside the child processes spawned by
/// `concurrent_writers_in_separate_processes_lose_nothing`.
#[test]
fn child_process_inserter() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let group: u32 = std::env::var(CHILD_GROUP).unwrap().parse().unwrap();
    let store = FileTrustStore::open(Path::new(&dir)).unwrap();
    for n in 0..5 {
        store.insert(peer_with_id(&numbered_id(group, n))).unwrap();
    }
}

#[test]
fn concurrent_writers_in_separate_processes_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    drop(FileTrustStore::open(&dir).unwrap());
    let exe = std::env::current_exe().unwrap();
    // Spawn all first, wait afterwards, so the processes really overlap.
    let children: Vec<_> = (100..106u32)
        .map(|group| {
            Command::new(&exe)
                .args(["--exact", "child_process_inserter", "--test-threads=1"])
                .env(CHILD_DIR, &dir)
                .env(CHILD_GROUP, group.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "child failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let store = FileTrustStore::open(&dir).unwrap();
    assert_eq!(store.list().unwrap().len(), 6 * 5);
}

// ---------------------------------------------------- Unix-only checks

#[cfg(unix)]
mod unix_only {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn insecure<T: std::fmt::Debug>(r: Result<T, KdeError>) -> bool {
        matches!(r, Err(KdeError::InsecurePermissions { .. }))
    }

    #[test]
    fn symlinked_state_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        atomic_write_private(&real.join("f"), b"secret").unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(insecure(ensure_private_dir(&link)));
        assert!(insecure(FileTrustStore::open(&link).map(|_| ())));
        assert!(insecure(atomic_write_private(&link.join("g"), b"x")));
        assert!(!real.join("g").exists());
    }

    #[test]
    fn dangling_symlink_and_non_directory_state_dir_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dangling = tmp.path().join("dangling");
        symlink(tmp.path().join("nowhere"), &dangling).unwrap();
        assert!(ensure_private_dir(&dangling).is_err());
        assert!(!tmp.path().join("nowhere").exists());
        let file = tmp.path().join("plain");
        fs::write(&file, b"x").unwrap();
        assert!(insecure(ensure_private_dir(&file)));
    }

    #[test]
    fn dangling_symlink_as_file_is_refused_not_treated_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        ensure_private_dir(&dir).unwrap();
        let link = dir.join("f");
        symlink(tmp.path().join("nowhere"), &link).unwrap();
        assert!(insecure(read_private_file(&link)));
    }

    #[test]
    fn write_replaces_a_symlink_instead_of_writing_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        ensure_private_dir(&dir).unwrap();
        let victim = tmp.path().join("victim");
        fs::write(&victim, b"original").unwrap();
        let dest = dir.join("f");
        symlink(&victim, &dest).unwrap();
        atomic_write_private(&dest, b"new").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"original");
        assert!(
            !fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(read_private_file(&dest).unwrap().unwrap(), b"new");
    }

    #[test]
    fn fifo_at_the_path_cannot_hang_or_be_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        ensure_private_dir(&dir).unwrap();
        let fifo = dir.join("f");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path, plain mkfifo(3).
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(insecure(read_private_file(&fifo)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn lock_file_must_be_private_and_not_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        ensure_private_dir(&dir).unwrap();
        let lock = dir.join("x.lock");
        drop(lock_exclusive(&lock).unwrap());
        assert_eq!(
            fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(insecure(lock_exclusive(&lock).map(|_| ())));
        fs::remove_file(&lock).unwrap();
        symlink(tmp.path().join("elsewhere"), &lock).unwrap();
        assert!(insecure(lock_exclusive(&lock).map(|_| ())));
        assert!(!tmp.path().join("elsewhere").exists());
    }

    #[test]
    fn store_refuses_a_loosened_lock_directory_and_file_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer_with_id(&numbered_id(4, 1))).unwrap();
        // Directory loosened after open: writes fail closed. (Reads rely on the
        // file's own owner/mode check; a file planted by someone else fails it.)
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(insecure(s.insert(peer_with_id(&numbered_id(4, 2)))));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        // File loosened after open: the cache is not trusted any more.
        fs::set_permissions(s.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(insecure(s.list()));
        fs::set_permissions(s.path(), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn store_files_are_0600_and_dir_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer_with_id(&numbered_id(5, 1))).unwrap();
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(
                entry.metadata().unwrap().permissions().mode() & 0o777,
                0o600,
                "{:?}",
                entry.file_name()
            );
        }
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
