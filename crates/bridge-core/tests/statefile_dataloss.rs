// SPDX-License-Identifier: GPL-3.0-or-later
//! Data-loss guards of the shared state-file layer, driven through the public API with the plain-std backend:
//! a newer build's file is never rewritten, a damaged file is kept as evidence without displacing the good
//! backup, an oversize or failing-to-migrate file is damaged rather than partly trusted, and unknown fields
//! survive a read-modify-write.

use bridge_core::statefile::{
    self, Corrupt, Error, MAX_STATE_BYTES, PlainIo, Spec, StateFile, Status,
};
use serde_json::{Map, Value, json};
use std::path::Path;

fn add_flag(body: &mut Map<String, Value>) -> Result<(), String> {
    body.insert("flag".into(), Value::Bool(true));
    Ok(())
}

fn refuse(_: &mut Map<String, Value>) -> Result<(), String> {
    Err("cannot upgrade".into())
}

static FAIL: Spec = Spec {
    name: "t",
    current: 2,
    migrations: &[add_flag],
    validate: None,
    on_corrupt: Corrupt::Fail,
};
static RESET: Spec = Spec {
    name: "t",
    current: 2,
    migrations: &[add_flag],
    validate: None,
    on_corrupt: Corrupt::Reset,
};
static BAD_MIGRATION: Spec = Spec {
    name: "t",
    current: 2,
    migrations: &[refuse],
    validate: None,
    on_corrupt: Corrupt::Fail,
};

fn file(dir: &Path, spec: &'static Spec) -> StateFile<PlainIo> {
    StateFile::new(PlainIo, dir.join("t.json"), spec)
}

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_file_from_a_newer_build_is_never_rewritten_or_backed_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    let original = br#"{"version":9,"future":{"keep":"me"}}"#;
    std::fs::write(&path, original).unwrap();
    let f = file(dir.path(), &FAIL);

    let loaded = f.load().unwrap();
    assert!(loaded.read_only());
    assert_eq!(loaded.status, Status::Newer { found: 9 });

    let mut called = false;
    let err = f
        .update(|_| {
            called = true;
            true
        })
        .unwrap_err();
    assert!(!called, "the edit must not run against a read-only file");
    assert!(
        matches!(
            err,
            Error::ReadOnly {
                found: 9,
                known: 2,
                ..
            }
        ),
        "{err}"
    );
    assert!(err.to_string().contains("newer version"));

    let locked = f.lock().unwrap();
    assert!(matches!(
        locked.save(&body(json!({"x": 1}))),
        Err(Error::ReadOnly { .. })
    ));
    assert!(matches!(
        locked.save_and_refresh_backup(&body(json!({"x": 1}))),
        Err(Error::ReadOnly { .. })
    ));
    drop(locked);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!statefile::backup_path(&path).exists());
    assert!(!statefile::corrupt_path(&path).exists());
}

#[test]
fn a_damaged_file_is_kept_as_evidence_and_never_replaces_the_good_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    let f = file(dir.path(), &RESET);
    f.update(|b| {
        b.insert("n".into(), json!(1));
        true
    })
    .unwrap();
    f.update(|b| {
        b.insert("n".into(), json!(2));
        true
    })
    .unwrap();
    let good_backup = std::fs::read(statefile::backup_path(&path)).unwrap();

    std::fs::write(&path, b"{ this is not json").unwrap();
    let loaded = f.load().unwrap();
    assert!(
        matches!(loaded.status, Status::Recovered { .. }),
        "{:?}",
        loaded.status
    );
    assert_eq!(loaded.body["n"], 1, "falls back to the previous generation");

    f.update(|b| {
        b.insert("n".into(), json!(3));
        true
    })
    .unwrap();
    assert_eq!(
        std::fs::read(statefile::corrupt_path(&path)).unwrap(),
        b"{ this is not json"
    );
    assert_eq!(
        std::fs::read(statefile::backup_path(&path)).unwrap(),
        good_backup,
        "a damaged file must not push out the good backup"
    );
    assert_eq!(f.load().unwrap().body["n"], 3);
}

#[test]
fn damage_with_no_backup_follows_the_policy_and_keeps_the_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    for raw in [
        b"".as_slice(),
        b"[1,2,3]",
        br#"{"version":"two"}"#,
        br#"{"version":0}"#,
        b"\xff\xfe\x00garbage",
    ] {
        std::fs::write(&path, raw).unwrap();
        let err = file(dir.path(), &FAIL).load().unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{raw:?}: {err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            raw,
            "Fail leaves the file alone"
        );
        let reset = file(dir.path(), &RESET).load().unwrap();
        assert!(matches!(reset.status, Status::Reset { .. }), "{raw:?}");
        assert!(reset.body.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), raw, "loading never writes");
    }
}

#[test]
fn an_oversize_file_is_damaged_not_partially_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    let pad = "x".repeat(MAX_STATE_BYTES as usize);
    std::fs::write(&path, format!(r#"{{"version":2,"pad":"{pad}"}}"#)).unwrap();
    let err = file(dir.path(), &FAIL).load().unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. } | Error::Io(_)), "{err}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        pad.len() as u64 + r#"{"version":2,"pad":""}"#.len() as u64
    );
}

#[test]
fn a_failing_migration_marks_the_file_damaged_and_leaves_it_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    let original = br#"{"version":1,"precious":true}"#;
    std::fs::write(&path, original).unwrap();
    let err = file(dir.path(), &BAD_MIGRATION).load().unwrap_err();
    assert!(
        matches!(&err, Error::Corrupt { reason, .. } if reason.contains("cannot upgrade")),
        "{err}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn unknown_fields_survive_an_update_and_the_old_file_becomes_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    std::fs::write(
        &path,
        br#"{"version":1,"added_by_a_newer_minor":[1,2],"a":1}"#,
    )
    .unwrap();
    let f = file(dir.path(), &FAIL);
    let before = f
        .update(|b| {
            b.insert("a".into(), json!(2));
            true
        })
        .unwrap();
    assert_eq!(before.status, Status::Migrated { from: 1 });
    let after = f.load().unwrap();
    assert_eq!(after.status, Status::Current);
    assert_eq!(after.body["added_by_a_newer_minor"], json!([1, 2]));
    assert_eq!(after.body["a"], 2);
    assert_eq!(after.body["flag"], true, "the migration ran");
    // The pre-migration file is the one-generation backup.
    let bak: Value =
        serde_json::from_slice(&std::fs::read(statefile::backup_path(&path)).unwrap()).unwrap();
    assert_eq!(bak["version"], 1);
    assert_eq!(bak["a"], 1);

    // An update that declines to save writes nothing.
    let stamp = std::fs::read(&path).unwrap();
    f.update(|_| false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), stamp);
}

#[test]
fn save_and_refresh_backup_makes_a_revocation_survive_a_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.json");
    let f = file(dir.path(), &RESET);
    let locked = f.lock().unwrap();
    locked.save(&body(json!({"trusted": ["a", "b"]}))).unwrap();
    locked
        .save_and_refresh_backup(&body(json!({"trusted": ["a"]})))
        .unwrap();
    drop(locked);
    // Damage the primary: the fallback must not resurrect the revoked entry.
    std::fs::write(&path, b"garbage").unwrap();
    let loaded = f.load().unwrap();
    assert!(matches!(loaded.status, Status::Recovered { .. }));
    assert_eq!(loaded.body["trusted"], json!(["a"]));
}
