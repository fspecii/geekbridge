# Android build verification 8

Date: 2026-11-03. Built from a clean worktree of origin/main at commit 9ffcc90e5 (battery optimisation onboarding),
because the shared tree had an unrelated uncommitted edit (scripts/e2e/wayland.sh).

## Android

- `scripts/build-android.sh --skip-host-tests`: exit 0; Gradle `testDebugUnitTest` and `lintDebug` passed.
- Debug APK `dist/android/geekbridge-debug.apk` (also `dist/android/serve/geekbridge-debug.apk`), arm64-v8a and x86_64:
  - sha256 `fcbdb18d88c881d69f354038d796ae5d607410c3295d2fdd08a4a929b7ead2c1`
  - size 8049051 bytes (7.7 MB)
- LAN server on :8000 was already running and not restarted; `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`
  returned 200 with `Content-Length: 8049051`, equal to the file size.
- `assembleRelease` + `scripts/check-release-isolation.sh` (81 GB free): ok, no debug diagnostics, receivers, cleartext
  or debug permissions.

## Desktop

`cargo build --release --locked -p bridge-service -p bridge-apps`: exit 0 (with the Android env sourced the target
directory is `target/android`). The nine binaries in `scripts/release-binaries.txt` were copied to temp names and moved
over `target/release` in the main checkout (running processes keep their old inode); the live service was not
restarted and picks up the new binaries on its next start. geekbridge-service sha256 matches the worktree build.
