// SPDX-License-Identifier: GPL-3.0-or-later
//! Persisted-state compatibility (docs/state-formats.md): the golden files in
//! `tests/fixtures/state-v1/` are what this code base wrote at format 1. Every build must still
//! load them, keep the pairing and settings they hold, refuse to clobber files from a newer
//! build, and cope with damaged files. If a change makes one of these tests fail, the change is a
//! format break: add a migration and bump the version instead of editing the fixtures.
//!
//! `GB_REGEN_FIXTURES=1 cargo test -p bridge-service --test state_compat regenerate -- --ignored`
//! rewrites the fixtures (throwaway keys; commit the result only for a deliberate new baseline).
#![cfg(unix)]

mod support;

use bridge_core::ipc::{ErrorCode, Feature, Request};
use bridge_kde::{self as kde, DeviceId, TrustStore as _};
use bridge_service::{clipboard, daemon, private::PrivateState};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use support::*;

const DESKTOP_ID: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const PHONE_ID: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
/// State files of format 1 (the phone's own identity is only used by the test phone).
const STATE_FILES: &[&str] = &[
    "identity.pem",
    "trusted_peers.json",
    "consent.json",
    "share.json",
    "clipboard-settings.json",
    "device-name",
    "ipc-token",
    "tray-paused",
    "update-state.json",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/state-v1")
}

/// Copy the golden files into `<tmp>/state` with the owner-only modes the service requires.
fn stage() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    for name in STATE_FILES {
        let to = dir.join(name);
        fs::copy(fixtures().join(name), &to).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o600)).unwrap();
    }
    (tmp, dir)
}

fn phone() -> Arc<Phone> {
    let pem = fs::read(fixtures().join("phone-identity.pem")).unwrap();
    let creds = kde::DeviceCredentials::from_pem(&pem, kde::cert::unix_now()).unwrap();
    assert_eq!(creds.device_id().as_str(), PHONE_ID);
    // The phone paired with the fixture desktop earlier, so it trusts that certificate.
    let desktop = fs::read(fixtures().join("identity.pem")).unwrap();
    let desktop = kde::DeviceCredentials::from_pem(&desktop, kde::cert::unix_now()).unwrap();
    let trusted = kde::TrustedPeer {
        device_id: desktop.device_id().clone(),
        name: "Fixture Desktop".into(),
        device_type: "desktop".into(),
        protocol_version: 8,
        cert: desktop.cert().clone(),
        paired_at_unix: 1_700_000_000,
    };
    Phone::with_credentials(Arc::new(creds), vec![trusted])
}

fn options(dir: &Path) -> impl FnOnce(&mut daemon::DaemonOptions) {
    let dir = dir.to_owned();
    move |o| {
        o.device_name_file = Some(dir.join(daemon::DEVICE_NAME_FILE));
        o.clipboard_settings = Some(dir.join(clipboard::SETTINGS_FILE));
        o.share.settings_path = Some(dir.join("share.json"));
    }
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_file() && !e.file_name().to_string_lossy().ends_with(".lock"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn format_1_files_load_unchanged() {
    let (_tmp, dir) = stage();
    let state = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(state.credentials.device_id().as_str(), DESKTOP_ID);
    let id = DeviceId::parse(PHONE_ID).unwrap();
    let peer = state.store.get(&id).unwrap().expect("pairing survives");
    assert_eq!(peer.name, "Fixture Phone");
    assert!(state.consents.is_enabled(PHONE_ID, Feature::Clipboard));
    assert!(state.consents.is_enabled(PHONE_ID, Feature::Sms));
    assert!(!state.consents.is_enabled(PHONE_ID, Feature::Contacts));
    // The token is kept byte for byte, so running clients and scripts keep working.
    assert_eq!(
        state.token.to_file_contents().as_bytes(),
        fs::read(fixtures().join("ipc-token")).unwrap().as_slice()
    );
    // Reading never rewrites the primary files.
    for name in [
        "trusted_peers.json",
        "consent.json",
        "share.json",
        "clipboard-settings.json",
    ] {
        assert_eq!(
            fs::read(dir.join(name)).unwrap(),
            fs::read(fixtures().join(name)).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn clipboard_settings_without_a_version_still_load() {
    let (_tmp, dir) = stage();
    let svc = clipboard::ClipboardService::with_settings_file(
        Arc::new(clipboard::MemoryClipboard::default()),
        "d",
        dir.join(clipboard::SETTINGS_FILE),
    );
    let s = svc.settings();
    assert_eq!(s.hotkey, "Ctrl+Alt+K");
    assert_eq!(s.window_ms, 45_000);
    // Saving adds the version and keeps what a newer build might have written.
    let path = dir.join(clipboard::SETTINGS_FILE);
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    v["from_the_future"] = true.into();
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    svc.update_settings(Some(50_000), None, None).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["window_ms"], 50_000);
    assert_eq!(v["from_the_future"], true);
}

#[tokio::test]
async fn pairing_and_settings_survive_an_upgrade_with_a_live_phone() {
    let (tmp, dir) = stage();
    let rig = Rig::start_in(tmp, options(&dir)).await;
    let status = rig.status().await;
    assert_eq!(status.device_id, DESKTOP_ID);
    assert_eq!(
        status.device_name, "Fixture Desktop",
        "the saved name is restored"
    );
    let paired = status
        .paired
        .iter()
        .find(|d| d.device_id == PHONE_ID)
        .unwrap();
    assert!(
        paired
            .consents
            .iter()
            .any(|c| c.feature == Feature::Clipboard && c.enabled)
    );
    // The phone (same identity as before the update) reconnects with no new pairing.
    let phone = phone();
    let _link = reconnect(&rig, &phone).await;
    let status = rig.status().await;
    assert!(status.challenges.is_empty(), "no re-pairing prompt");
    assert_eq!(rig.trust_len(), 1);
    // And the next change is written without disturbing the rest.
    rig.ok(Request::SetConsent {
        device_id: PHONE_ID.into(),
        feature: Feature::Contacts,
        enabled: true,
    })
    .await;
    let consent: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("consent.json")).unwrap()).unwrap();
    assert_eq!(consent["version"], 1);
    assert_eq!(consent["devices"][PHONE_ID].as_array().unwrap().len(), 3);
    assert!(
        dir.join("consent.json.bak").exists(),
        "one rollback generation"
    );
}

#[tokio::test]
async fn files_from_a_newer_version_are_read_but_never_overwritten() {
    let (tmp, dir) = stage();
    // A "version 9" build wrote every JSON file, adding fields we do not know.
    for name in [
        "trusted_peers.json",
        "consent.json",
        "share.json",
        "clipboard-settings.json",
    ] {
        let path = dir.join(name);
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        v["version"] = 9.into();
        v["added_in_v9"] = "x".into();
        fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }
    let before = snapshot(&dir);
    let rig = Rig::start_in(tmp, options(&dir)).await;
    // Still paired and still allowed: the downgraded build reads what it understands.
    let status = rig.status().await;
    assert!(status.paired.iter().any(|d| d.device_id == PHONE_ID));
    let phone = phone();
    let _link = reconnect(&rig, &phone).await;
    // Changes are refused with a clear error rather than written.
    assert_eq!(
        rig.refused(Request::SetConsent {
            device_id: PHONE_ID.into(),
            feature: Feature::Contacts,
            enabled: true,
        })
        .await,
        ErrorCode::Failed
    );
    rig.ok(Request::SetDeviceName {
        name: "Renamed".into(),
    })
    .await;
    let after = snapshot(&dir);
    let strip = |v: Vec<(String, Vec<u8>)>| {
        v.into_iter()
            .filter(|(n, _)| n != "device-name" && !n.ends_with(".bak"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        strip(before),
        strip(after),
        "newer files are byte-identical"
    );
}

#[tokio::test]
async fn damaged_state_files_do_not_brick_the_service() {
    for damage in [&b""[..], b"{", b"\0\0\0\0", b"[]"] {
        let (tmp, dir) = stage();
        for name in [
            "consent.json",
            "share.json",
            "clipboard-settings.json",
            "ipc-token",
        ] {
            fs::write(dir.join(name), damage).unwrap();
        }
        fs::write(dir.join("device-name"), damage).unwrap();
        fs::write(dir.join("tray-paused"), damage).unwrap();
        fs::write(dir.join("update-state.json"), damage).unwrap();
        let identity = fs::read(dir.join("identity.pem")).unwrap();
        let rig = Rig::start_in(tmp, options(&dir)).await;
        let status = rig.status().await;
        assert_eq!(status.device_id, DESKTOP_ID, "identity untouched");
        assert!(
            status.paired.iter().any(|d| d.device_id == PHONE_ID),
            "trust untouched"
        );
        assert!(
            status
                .paired
                .iter()
                .all(|d| d.consents.iter().all(|c| !c.enabled)),
            "consent is opt-in: nothing enabled after losing the file"
        );
        assert_eq!(fs::read(dir.join("identity.pem")).unwrap(), identity);
        // The service can still save settings afterwards.
        rig.ok(Request::SetConsent {
            device_id: PHONE_ID.into(),
            feature: Feature::Sms,
            enabled: true,
        })
        .await;
    }
}

#[tokio::test]
async fn a_damaged_identity_or_trust_store_is_restored_from_its_backup() {
    let (tmp, dir) = stage();
    // First start creates the rollback copies.
    drop(PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1)));
    let st = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
    st.store
        .insert(
            st.store
                .get(&DeviceId::parse(PHONE_ID).unwrap())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    drop(st);
    fs::write(dir.join("identity.pem"), b"").unwrap();
    fs::write(dir.join("trusted_peers.json"), b"{ truncated").unwrap();
    let rig = Rig::start_in(tmp, options(&dir)).await;
    let status = rig.status().await;
    assert_eq!(
        status.device_id, DESKTOP_ID,
        "same identity: pairings stay valid"
    );
    assert!(status.paired.iter().any(|d| d.device_id == PHONE_ID));
    let phone = phone();
    let _link = reconnect(&rig, &phone).await;
}

// ------------------------------------------------------------------ fixture generation

#[test]
#[ignore = "rewrites tests/fixtures/state-v1; run deliberately"]
fn regenerate_fixtures() {
    if std::env::var_os("GB_REGEN_FIXTURES").is_none() {
        return;
    }
    let out = fixtures();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    fs::create_dir_all(&out).unwrap();
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let desktop_pem =
        kde::DeviceCredentials::fixture_pem(&DeviceId::parse(DESKTOP_ID).unwrap(), 100).unwrap();
    let phone_pem =
        kde::DeviceCredentials::fixture_pem(&DeviceId::parse(PHONE_ID).unwrap(), 100).unwrap();
    kde::fsutil::atomic_write_private(&dir.join("identity.pem"), desktop_pem.as_bytes()).unwrap();
    let phone_creds =
        kde::DeviceCredentials::from_pem(phone_pem.as_bytes(), kde::cert::unix_now()).unwrap();
    let store = kde::FileTrustStore::open(&dir).unwrap();
    store
        .insert(kde::TrustedPeer {
            device_id: DeviceId::parse(PHONE_ID).unwrap(),
            name: "Fixture Phone".into(),
            device_type: "phone".into(),
            protocol_version: 8,
            cert: phone_creds.cert().clone(),
            paired_at_unix: 1_700_000_000,
        })
        .unwrap();
    let mut consents = bridge_service::private::ConsentStore::open(&dir).unwrap();
    consents.set(PHONE_ID, Feature::Clipboard, true).unwrap();
    consents.set(PHONE_ID, Feature::Sms, true).unwrap();
    for (name, body) in [
        ("share.json", format!("{{\n  \"version\": 1,\n  \"devices\": {{\n    \"{PHONE_ID}\": {{ \"receive\": true, \"auto_accept\": false }}\n  }}\n}}\n")),
        // Written before the version field existed: version 1 by definition.
        ("clipboard-settings.json", "{\"window_ms\":45000,\"hotkey\":\"Ctrl+Alt+K\",\"max_image_bytes\":4194304}".into()),
        ("device-name", "Fixture Desktop".into()),
        ("tray-paused", format!("[\"{PHONE_ID}\"]")),
        ("update-state.json", "{\"enabled\":false,\"skipped_version\":\"9.9.9\",\"etag\":null,\"last_checked_unix\":1700000000,\"release\":null,\"no_public_release\":false}".into()),
    ] {
        kde::fsutil::atomic_write_private(&dir.join(name), body.as_bytes()).unwrap();
    }
    // A fixed, obviously fake token.
    kde::fsutil::atomic_write_private(
        &dir.join("ipc-token"),
        format!("{}\n", "ab".repeat(32)).as_bytes(),
    )
    .unwrap();
    for name in STATE_FILES {
        fs::copy(dir.join(name), out.join(name)).unwrap();
    }
    fs::write(out.join("phone-identity.pem"), phone_pem).unwrap();
    fs::write(
        out.join("README.md"),
        "Golden state files at format 1 (docs/state-formats.md). Throwaway keys, no real data.\nDo not edit by hand; see regenerate_fixtures in tests/state_compat.rs.\n",
    )
    .unwrap();
}
