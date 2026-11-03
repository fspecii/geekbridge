# Android build verification 7

Date: 2026-11-03. Built from a clean checkout of commit 43a037dc5 (the working tree had unrelated uncommitted
edits in progress, so a separate worktree was used).

Preceding change (43a037dc5): `geekbridge-ctl --help` now lists all ten consent features, with a test that the help
names every `Feature::ALL` entry; the `parse_permits` comment no longer states a count; `scripts/build-desktop.sh`,
`scripts/package-linux.sh` and `release.yml` read one list, `scripts/release-binaries.txt` (nine binaries), checked by
`scripts/check-binary-list.sh`. fmt, `clippy --workspace --all-targets --locked -D warnings` and
`cargo test --workspace --locked --no-fail-fast` all exited 0.

## Android

- `scripts/build-android.sh --skip-host-tests`: exit 0; Gradle `testDebugUnitTest` and `lintDebug` passed.
- Debug APK `dist/android/geekbridge-debug.apk` (also `dist/android/serve/geekbridge-debug.apk`), arm64-v8a and x86_64:
  - sha256 `b9413a43d65e84fa113a5d63dd745cef325978a5451a49639643f6aac1922659`
  - size 8005951 bytes (7.7 MB)
- LAN server on :8000 was already running and not restarted; `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`
  returned 200 with `Content-Length: 8005951`, equal to the file size.
- `assembleRelease` + `scripts/check-release-isolation.sh`: ok, no debug diagnostics, receivers, cleartext or debug
  permissions.

## Desktop

`cargo build --release --locked -p bridge-service -p bridge-apps`: exit 0. The live service was not restarted.
