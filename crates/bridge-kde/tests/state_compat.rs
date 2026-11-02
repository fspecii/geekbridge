// SPDX-License-Identifier: GPL-3.0-or-later
//! Format-1 golden files, crash-during-write and two-process writer tests for the transport state
//! (identity + trust store) and the generic state-file machinery on owner-only storage.
//! See docs/state-formats.md.
#![cfg(unix)]

use bridge_core::statefile::{Corrupt, Error, Spec, StateFile};
use bridge_kde::fsutil::{PrivateIo, PrivateStateFile};
use bridge_kde::{DeviceCredentials, DeviceId, FileTrustStore, TrustStore as _};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const PHONE_ID: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/state-v1")
}

fn stage(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    for name in files {
        let to = dir.join(name);
        fs::copy(fixtures().join(name), &to).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o600)).unwrap();
    }
    (tmp, dir)
}

#[test]
fn format_1_identity_and_trust_store_load_with_current_code() {
    let (_tmp, dir) = stage(&["identity.pem", "trusted_peers.json"]);
    let (creds, created) = DeviceCredentials::load_or_create(&dir).unwrap();
    assert!(!created, "an existing identity is never regenerated");
    assert_eq!(
        creds.device_id().as_str(),
        "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
    );
    let store = FileTrustStore::open(&dir).unwrap();
    let peer = store
        .get(&DeviceId::parse(PHONE_ID).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(peer.name, "Fixture Phone");
    assert_eq!(peer.protocol_version, 8);
    assert_eq!(peer.paired_at_unix, 1_700_000_000);
    // Plain reads leave the files as they were.
    assert_eq!(
        fs::read(dir.join("trusted_peers.json")).unwrap(),
        fs::read(fixtures().join("trusted_peers.json")).unwrap()
    );
}

// ----------------------------------------------------------- a synthetic format 2

fn v2_add_label(b: &mut serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let n = b.get("n").and_then(|v| v.as_u64()).ok_or("missing n")?;
    b.insert("label".into(), format!("n{n}").into());
    Ok(())
}
static V1: Spec = Spec {
    name: "doc.json",
    current: 1,
    migrations: &[],
    validate: None,
    on_corrupt: Corrupt::Fail,
};
static V2: Spec = Spec {
    name: "doc.json",
    current: 2,
    migrations: &[v2_add_label],
    validate: None,
    on_corrupt: Corrupt::Fail,
};

fn doc(dir: &Path, spec: &'static Spec) -> PrivateStateFile {
    StateFile::new(PrivateIo, dir.join("doc.json"), spec)
}

#[test]
fn a_fake_v2_upgrade_then_a_v1_downgrade_loses_nothing() {
    let (_tmp, dir) = stage(&[]);
    doc(&dir, &V1)
        .update(|b| {
            b.insert("n".into(), 7.into());
            b.insert("keep".into(), "me".into());
            true
        })
        .unwrap();
    // The new build migrates on load and writes format 2 (the v1 file becomes the backup).
    let l = doc(&dir, &V2).update(|_| true).unwrap();
    assert_eq!(l.body["label"], "n7");
    let on_disk: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("doc.json")).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 2);
    assert_eq!(on_disk["keep"], "me");
    let bak: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("doc.json.bak")).unwrap()).unwrap();
    assert_eq!(bak["version"], 1);
    // The old build, run again, sees a newer file: read-only, file untouched.
    let before = fs::read(dir.join("doc.json")).unwrap();
    let old = doc(&dir, &V1);
    assert!(old.load().unwrap().read_only());
    assert!(matches!(
        old.update(|_| true),
        Err(Error::ReadOnly { found: 2, .. })
    ));
    assert_eq!(fs::read(dir.join("doc.json")).unwrap(), before);
}

// ----------------------------------------------------------- crash during a write

const CHILD_DIR: &str = "GB_CHILD_DIR";

/// Not a test: the body that runs in the child processes (does nothing without the env var).
#[test]
fn child_crash_writer() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    match std::env::var("GB_CHILD_KIND").as_deref() {
        Ok("trust") => {
            let store = FileTrustStore::open(&dir).unwrap();
            // GB_FAILPOINT=before_rename aborts the process inside this call.
            store.remove(&DeviceId::parse(PHONE_ID).unwrap()).unwrap();
        }
        _ => {
            doc(&dir, &V1)
                .update(|b| {
                    b.insert("n".into(), 2.into());
                    true
                })
                .unwrap();
        }
    }
}

fn run_child(dir: &Path, kind: &str, failpoint: Option<&str>) -> std::process::ExitStatus {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_crash_writer", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .env("GB_CHILD_KIND", kind)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(f) = failpoint {
        cmd.env("GB_FAILPOINT", f);
    }
    cmd.status().unwrap()
}

#[test]
fn a_kill_between_temp_write_and_rename_leaves_the_old_file_intact() {
    let (_tmp, dir) = stage(&[]);
    doc(&dir, &V1)
        .update(|b| {
            b.insert("n".into(), 1.into());
            true
        })
        .unwrap();
    let before = fs::read(dir.join("doc.json")).unwrap();
    let status = run_child(&dir, "doc", Some("before_rename"));
    assert!(
        !status.success(),
        "the child must have died at the failpoint"
    );
    assert_eq!(
        fs::read(dir.join("doc.json")).unwrap(),
        before,
        "old file intact"
    );
    // A half-written temp file may remain; it must not matter.
    let l = doc(&dir, &V1).load().unwrap();
    assert_eq!(l.body["n"], 1);
    // And the next normal write succeeds.
    assert!(run_child(&dir, "doc", None).success());
    assert_eq!(doc(&dir, &V1).load().unwrap().body["n"], 2);
}

#[test]
fn a_crash_while_unpairing_keeps_the_pairing() {
    let (_tmp, dir) = stage(&["identity.pem", "trusted_peers.json"]);
    // One extra write so a backup exists, as after any normal use.
    let id = DeviceId::parse(PHONE_ID).unwrap();
    let store = FileTrustStore::open(&dir).unwrap();
    let peer = store.get(&id).unwrap().unwrap();
    store.insert(peer).unwrap();
    drop(store);
    let before = fs::read(dir.join("trusted_peers.json")).unwrap();
    assert!(!run_child(&dir, "trust", Some("before_rename")).success());
    assert_eq!(fs::read(dir.join("trusted_peers.json")).unwrap(), before);
    assert!(
        FileTrustStore::open(&dir)
            .unwrap()
            .get(&id)
            .unwrap()
            .is_some()
    );
}

// ----------------------------------------------------------- two writer processes

#[test]
fn child_counter() {
    let Ok(dir) = std::env::var("GB_COUNTER_DIR") else {
        return;
    };
    let f = doc(Path::new(&dir), &V1);
    for _ in 0..40 {
        f.update(|b| {
            let n = b.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
            b.insert("n".into(), (n + 1).into());
            true
        })
        .unwrap();
    }
}

#[test]
fn two_processes_incrementing_one_file_lose_no_update() {
    let (_tmp, dir) = stage(&[]);
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..3)
        .map(|_| {
            Command::new(&exe)
                .args(["--exact", "child_counter", "--test-threads=1"])
                .env("GB_COUNTER_DIR", &dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(doc(&dir, &V1).load().unwrap().body["n"], 120);
}
