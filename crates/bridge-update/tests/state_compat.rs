// SPDX-License-Identifier: GPL-3.0-or-later
//! `update-state.json` compatibility (docs/state-formats.md): the format-1 golden file keeps
//! loading, unknown fields survive a save, a newer-format file is never rewritten, damage falls
//! back instead of failing.

use bridge_core::ipc::UpdateState;
use bridge_update::{Config, Updater};
use std::{fs, path::Path, sync::Arc};

fn updater(state: &Path) -> Arc<Updater> {
    let mut cfg = Config::github("0.1.0");
    cfg.state_file = Some(state.to_owned());
    cfg.download_root = Some(state.parent().unwrap().join("dl"));
    Updater::new(cfg).unwrap()
}

fn staged() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("update-state.json");
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/state-v1/update-state.json");
    fs::copy(golden, &path).unwrap();
    (tmp, path)
}

#[test]
fn format_1_file_loads_and_is_upgraded_in_place_on_the_next_save() {
    let (_tmp, path) = staged();
    let u = updater(&path);
    let st = u.status();
    assert!(!st.enabled, "the user's switch survives the update");
    assert_eq!(st.state, UpdateState::UpToDate);
    // Unknown fields from a newer minor build survive; the version is added.
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    v["added_later"] = serde_json::json!({"x": 1});
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    u.set_enabled(true);
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["added_later"]["x"], 1);
    assert!(path.with_extension("json.bak").exists());
    assert!(updater(&path).status().enabled);
}

#[test]
fn a_newer_format_file_is_read_but_left_alone() {
    let (_tmp, path) = staged();
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    v["version"] = 5.into();
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let u = updater(&path);
    assert!(!u.status().enabled, "still honours what it understands");
    u.set_enabled(true);
    assert_eq!(fs::read(&path).unwrap(), before, "not overwritten");
}

#[test]
fn damaged_files_fall_back_to_the_backup_or_the_defaults() {
    let (_tmp, path) = staged();
    let u = updater(&path);
    u.set_enabled(true);
    u.set_enabled(false); // second save: the backup now holds `enabled: true`
    for bad in [&b""[..], b"{", b"\0\0", b"[1,2]"] {
        fs::write(&path, bad).unwrap();
        let st = updater(&path).status();
        assert!(st.enabled, "restored from the backup");
    }
    fs::remove_file(path.with_extension("json.bak")).unwrap();
    fs::write(&path, b"{").unwrap();
    let st = updater(&path).status();
    assert!(st.enabled, "defaults: automatic checks on");
    assert_eq!(st.state, UpdateState::Idle);
    assert_eq!(fs::read(&path).unwrap(), b"{", "reading does not rewrite");
}
