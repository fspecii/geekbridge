// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke test against the real api.github.com and the real asset CDN, read-only, nothing is
//! installed. Ignored by default and gated by `GEEKBRIDGE_REALNET=1` (about 36 MB of downloads and
//! ~8 unauthenticated API requests):
//! `GEEKBRIDGE_REALNET=1 cargo test -p bridge-update realnet -- --ignored --nocapture`.

use super::*;

const TAG_URL: &str = "https://api.github.com/repos/fspecii/geekbridge/releases/tags/v0.1.0-rc.1";
const JSON: &str = "application/vnd.github+json";

fn log(msg: impl AsRef<str>) {
    eprintln!("REALNET {}", msg.as_ref());
}

fn updater(dir: &Path, channel: UpdateChannel, download_timeout: Duration) -> Arc<Updater> {
    let mut cfg = Config::github("0.0.1");
    cfg.repo = "fspecii/geekbridge".into();
    cfg.state_file = Some(dir.join(format!("state-{channel:?}.json")));
    cfg.download_root = Some(dir.join("dl"));
    cfg.channel = Some(channel);
    cfg.manual_cooldown = Duration::ZERO;
    cfg.download_timeout = download_timeout;
    Updater::new(cfg).unwrap()
}

fn remaining(u: &Updater) -> i64 {
    // `/rate_limit` itself is free.
    let (f, body) = u
        .get_bytes(
            "https://api.github.com/rate_limit",
            JSON,
            MAX_API_BYTES,
            None,
        )
        .unwrap();
    assert_eq!(f.status, 200);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    v["resources"]["core"]["remaining"].as_i64().unwrap()
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.count())
}

#[test]
#[ignore = "real network"]
fn real_github() {
    if std::env::var("GEEKBRIDGE_REALNET").is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let u = updater(tmp.path(), UpdateChannel::Stable, Duration::from_secs(120));
    let start = remaining(&u);
    log(format!("rate limit remaining at start: {start}"));

    // Stable channel: the only release is a pre-release, so there is nothing to offer.
    let r = u.check(true);
    let s = u.status();
    log(format!(
        "stable check: {r:?} state={:?} latest={:?} err={:?}",
        s.state,
        s.latest.as_ref().map(|l| &l.version),
        s.error
    ));
    assert!(r.is_ok() && s.latest.is_none() && !s.newer);
    log(format!("rate limit after stable check: {}", remaining(&u)));

    // Dev channel: the rolling `dev` prerelease does not exist upstream (yet).
    let d = updater(tmp.path(), UpdateChannel::Dev, Duration::from_secs(120));
    let r = d.check(true);
    let s = d.status();
    log(format!(
        "dev check: {r:?} state={:?} latest={:?} err={:?}",
        s.state,
        s.latest.as_ref().map(|l| &l.version),
        s.error
    ));
    assert!(r.is_ok());

    // ETag: a 200 with an ETag, then a conditional request that answers 304 for free.
    let before = remaining(&u);
    let (f1, body) = u.get_bytes(TAG_URL, JSON, MAX_API_BYTES, None).unwrap();
    let mid = remaining(&u);
    let etag = f1.etag.clone().expect("GitHub sends an ETag");
    let (f2, b2) = u
        .get_bytes(TAG_URL, JSON, MAX_API_BYTES, Some(&etag))
        .unwrap();
    let after = remaining(&u);
    log(format!(
        "ETag {etag}: first {} (cost {}), conditional {} body {} bytes (cost {})",
        f1.status,
        before - mid,
        f2.status,
        b2.len(),
        mid - after
    ));
    assert_eq!((f1.status, f2.status), (200, 304));

    // Real API shape.
    let release = Release::parse(&body).unwrap();
    log(format!(
        "tag={} prerelease={} draft={} assets={} version={:?}",
        release.tag_name,
        release.prerelease,
        release.draft,
        release.assets.len(),
        release.version().map(|v| v.to_string())
    ));
    assert!(
        release.prerelease
            && !release.draft
            && release.version().is_some_and(|v| v.is_prerelease())
    );
    let mut cached = Cached {
        version: "0.1.0-rc.1".into(),
        tag: release.tag_name.clone(),
        notes: String::new(),
        release_url: String::new(),
        asset: None,
        sha256: None,
        dev: false,
        dev_newer: false,
    };
    u.attach_installer(&mut cached, &release).unwrap();
    let asset = cached.asset.clone().expect("a linux asset");
    let sha = cached.sha256.clone().expect("SHA256SUMS lists it");
    log(format!(
        "asset {} {} bytes sha256 {sha} api digest {:?}",
        asset.name,
        asset.size,
        asset.api_digest()
    ));
    assert_eq!(asset.api_digest().as_deref(), Some(sha.as_str()));

    // Redirect to the CDN, hash check, private directory, read-only file.
    let apk = release
        .asset("geekbridge-android-arm64.apk")
        .unwrap()
        .clone();
    let apk_sha = apk.api_digest().unwrap();
    let (dir, file) = u.download_verified(&apk, &apk_sha).unwrap();
    log(format!(
        "small download ok: {} ({} bytes)",
        file.display(),
        std::fs::metadata(&file).unwrap().len()
    ));
    assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
    let _ = std::fs::remove_dir_all(dir);
    let dl = tmp.path().join("dl");
    assert_eq!(entries(&dl), 0);

    // Failures leave nothing behind.
    let bad = "0".repeat(64);
    let e = u.download_verified(&apk, &bad).unwrap_err();
    log(format!("wrong digest: {e:?}; leftovers {}", entries(&dl)));
    assert_eq!(e, Error::HashMismatch);
    assert_eq!(entries(&dl), 0);
    let mut short = apk.clone();
    short.size -= 1;
    let e = u.download_verified(&short, &apk_sha).unwrap_err();
    log(format!(
        "size cap (declared > cap): {e:?}; leftovers {}",
        entries(&dl)
    ));
    assert_eq!(e, Error::TooLarge);
    assert_eq!(entries(&dl), 0);
    let mut long = apk.clone();
    long.size += 1;
    let e = u.download_verified(&long, &apk_sha).unwrap_err();
    log(format!(
        "size mismatch (cap above real size): {e:?}; leftovers {}",
        entries(&dl)
    ));
    assert_eq!(e, Error::SizeMismatch);
    assert_eq!(entries(&dl), 0);

    // A deadline in the middle of the big download.
    let t = updater(
        tmp.path(),
        UpdateChannel::Stable,
        Duration::from_millis(400),
    );
    let e = t.download_verified(&asset, &sha).unwrap_err();
    log(format!(
        "deadline mid-download: {e:?}; leftovers {}",
        entries(&dl)
    ));
    assert_eq!(entries(&dl), 0);

    // The real installer end to end.
    let (dir, file) = u.download_verified(&asset, &sha).unwrap();
    log(format!(
        "full download ok: {} ({} bytes)",
        file.display(),
        std::fs::metadata(&file).unwrap().len()
    ));
    let _ = std::fs::remove_dir_all(dir);
    log(format!("rate limit remaining at end: {}", remaining(&u)));
}

/// One stable check with whatever network and clock the process was started with; prints the
/// outcome. Used under `unshare -rn` (offline), a dead `HTTPS_PROXY` and a skewed clock.
#[test]
#[ignore = "real network"]
fn real_check_outcome() {
    if std::env::var("GEEKBRIDGE_REALNET").is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let u = updater(tmp.path(), UpdateChannel::Stable, Duration::from_secs(20));
    let t = std::time::Instant::now();
    let r = u.check(true);
    let s = u.status();
    log(format!(
        "check: {r:?} state={:?} error={:?} after {:?}",
        s.state,
        s.error,
        t.elapsed()
    ));
}
