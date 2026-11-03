# Android build verification 9

Date: 2026-11-03. Built from a clean worktree of origin/main at commit a20091eae (CI size budget for the photos request
types), because the shared tree had an unrelated uncommitted edit (scripts/e2e/wayland.sh).

## CI wait

The Cross-platform Rust checks run for the newest main commit stayed queued for the whole 30 minute bounded wait (the
runner queue was backed up; main moved from a5e44bccf to a20091eae meanwhile, with the phone-photos work landing in
android/). No android/ change was in flight at build time, so the build went ahead on the newest origin/main. CI for
this commit was not confirmed.

## Android

- `scripts/build-android.sh --skip-host-tests`: exit 0; Gradle `testDebugUnitTest` (566 tests, 0 failures) and
  `lintDebug` passed.
- Debug APK `dist/android/geekbridge-debug.apk` (also `dist/android/serve/geekbridge-debug.apk`), arm64-v8a and x86_64:
  - sha256 `34c636a74d766c12cacb98f62c37f65bf81b8d0f4dacc2edbcf60df5bab7ea75`
  - size 8074550 bytes (7.8 MB)
- LAN server on :8000 was already running; `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk` returned 200 with
  `Content-Length: 8074550`, equal to the file size.
- `assembleRelease` + `scripts/check-release-isolation.sh` (66 GB free): ok. The first release attempt failed in R8 with
  every `android.*` class reported missing; that came from a stale Gradle daemon, and `./gradlew --no-daemon
  assembleRelease` succeeded.

## Desktop

`cargo build --release --locked -p bridge-service -p bridge-apps`: exit 0. The binaries in
`scripts/release-binaries.txt` were copied to temp names and moved over `target/release` in the main checkout (running
processes keep their old inode); the live service was not restarted.
