# Android build verification 12

Head commit: b9b67bdaeabc4b8cad48c961a0a6746a85676dd2
Debug APK sha256: 0416a635e0faf4079db17e141251f1b1d1e9cc181388f9e2f8bac994f36109cd

## Settled CI

No push to main for more than 15 minutes after b9b67bda; check.yml run 41696258868 completed with success. Per job:

- Android / unit tests, lint, debug APK: success
- Type-check Windows and macOS targets (from Linux): success
- Performance budget (Linux, no emulator): success
- Supply chain (cargo-deny, notices): success
- Rust / macos-latest: success
- Rust / windows-latest: success
- Rust / ubuntu-latest: success
- Coverage report (non-blocking): success
- Docs links: success

No failures, so nothing to fix or classify.

## Local build (clean worktree of the head)

- scripts/build-android.sh --skip-host-tests: gradle unit tests and lintDebug passed, APK 7.7 MB, abis arm64-v8a and x86_64.
- Copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk; the server on :8000 was down and was restarted, curl -sI returns 200.
- assembleRelease: the daemon build failed in R8 (minifyReleaseWithR8); `gradle --no-daemon assembleRelease` succeeded. scripts/check-release-isolation.sh on the release APK: ok.
- apk-signer check against the previously served APK (sha256 fb0aab3a...0438): same signer certificate 4d711ab4...a3c3; verdict same-version (same versionCode, same key, so it installs over the existing app).
- PC release binaries rebuilt (cargo build --release --locked) and placed over target/release of the main checkout via temp copy and mv (16 executables); the live service was not restarted.
