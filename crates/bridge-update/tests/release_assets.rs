// SPDX-License-Identifier: GPL-3.0-or-later
//! The assets a real release build produced (`scripts/release-local.sh`, `$GB_RELEASE_DIR/<tag>/out`) are published
//! to a local mock "GitHub" and consumed by the real desktop updater: asset selection, the `SHA256SUMS` listing, the
//! API `digest` cross-check, the download and the re-hash. It is skipped (passes) unless `GB_RELEASE_OUT` names the
//! directory, so it costs nothing in CI: `GB_RELEASE_OUT=<dir> GB_RELEASE_TAG=v0.1.0 cargo test -p bridge-update
//! --test release_assets -- --nocapture`. Nothing leaves the machine.

use bridge_core::ipc::UpdateState;
use bridge_update::{Config, Platform, Updater};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn sha_hex(data: &[u8]) -> String {
    bridge_core::ipc::encode_hex(&Sha256::digest(data))
}

/// Serves `/repos/fspecii/geekbridge/releases/latest` and `/dl/<file>` from the directory.
fn serve(mut conn: TcpStream, dir: &Path, release: &[u8]) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match conn.read(&mut byte) {
            Ok(1) => buf.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let path = text
        .split("\r\n")
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_owned();
    let body: Option<Vec<u8>> = if path == "/repos/fspecii/geekbridge/releases/latest" {
        Some(release.to_vec())
    } else if let Some(name) = path.strip_prefix("/dl/") {
        let safe = !name.contains('/') && !name.contains("..");
        safe.then(|| std::fs::read(dir.join(name)).ok()).flatten()
    } else {
        None
    };
    match body {
        Some(b) => {
            let _ = conn.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", b.len()).as_bytes(),
            );
            let _ = conn.write_all(&b);
        }
        None => {
            let _ = conn.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

#[test]
fn produced_release_assets_are_consumed_by_the_updater() {
    let Ok(dir) = std::env::var("GB_RELEASE_OUT") else {
        eprintln!("GB_RELEASE_OUT not set: skipped");
        return;
    };
    let dir = PathBuf::from(dir);
    let tag = std::env::var("GB_RELEASE_TAG").expect("GB_RELEASE_TAG, e.g. v0.1.0");
    let version = tag.trim_start_matches('v');

    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).expect("SHA256SUMS in the output");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());

    // The release as GitHub would describe it: every produced file, with the API digest.
    let mut assets = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in &names {
        let data = std::fs::read(dir.join(name)).unwrap();
        assets.push(serde_json::json!({
            "name": name, "size": data.len(),
            "digest": format!("sha256:{}", sha_hex(&data)),
            "browser_download_url": format!("{base}/dl/{name}"),
        }));
        // Every line of SHA256SUMS must be the real digest of the file it names (the updater trusts the listing).
        if name != "SHA256SUMS" {
            assert_eq!(
                bridge_update::fuzzing::find_checksum(&sums, name),
                Some(sha_hex(&data)),
                "SHA256SUMS line for {name}"
            );
        }
    }
    for must in [
        "geekbridge-linux-x86_64.tar.gz",
        "geekbridge-android.apk",
        "geekbridge-android-arm64.apk",
        "geekbridge-sbom.cdx.json",
        "SHA256SUMS",
    ] {
        assert!(
            names.iter().any(|n| n == must),
            "asset {must} missing from {names:?}"
        );
    }
    let release = serde_json::to_vec(&serde_json::json!({
        "tag_name": tag, "html_url": format!("{base}/release/{tag}"),
        "body": "rehearsal", "assets": assets,
    }))
    .unwrap();

    let dir2 = dir.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (d, r) = (dir2.clone(), release.clone());
            std::thread::spawn(move || serve(conn, &d, &r));
        }
    });

    let state = tempfile::tempdir().unwrap();
    let cfg = |current: &str| {
        let mut cfg = Config::github(current);
        cfg.base_url = base.clone();
        cfg.platform = Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        cfg.state_file = Some(state.path().join(format!("update-{current}.json")));
        let root = state.path().join(format!("dl-{current}"));
        std::fs::create_dir_all(&root).unwrap();
        cfg.download_root = Some(root);
        cfg.manual_cooldown = Duration::ZERO;
        cfg
    };

    // From the public release candidate: offered, downloaded, verified against SHA256SUMS.
    let up: Arc<Updater> = Updater::new(cfg("0.1.0-rc.1")).unwrap();
    up.check(true).unwrap();
    let st = up.status();
    assert_eq!(st.state, UpdateState::Available, "{st:?}");
    assert!(st.newer && st.downloadable, "{st:?}");
    let latest = st.latest.unwrap();
    assert_eq!(latest.version, version);
    assert_eq!(
        latest.asset_name.as_deref(),
        Some("geekbridge-linux-x86_64.tar.gz"),
        "the canonical archive, not the installer tarball, the .deb, the Arch package, the APKs or the SBOM"
    );
    let want = sha_hex(&std::fs::read(dir.join("geekbridge-linux-x86_64.tar.gz")).unwrap());
    assert_eq!(latest.sha256, Some(want.clone()));
    let got = up.download().unwrap();
    assert_eq!(sha_hex(&std::fs::read(&got).unwrap()), want);
    assert_eq!(up.status().state, UpdateState::Downloaded);

    // From the same version: nothing newer.
    let same = Updater::new(cfg(version)).unwrap();
    same.check(true).unwrap();
    assert!(!same.status().newer, "{:?}", same.status());
}
