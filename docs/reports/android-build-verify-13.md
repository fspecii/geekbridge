Head commit: c106a234c
APK sha256 (geekbridge-debug.apk): 3d2f9db4ffce72a899b162d151d94577a47f5dbbd041c77f27ee9ce575a2669e

# Android build verification 13

Main settled at c106a234c (last push 2026-11-04 01:43 +02:00, no push for 20 minutes). Several pushes during the wait restarted the window.

## CI (check.yml, run 41741462225, head c106a234c): all green

- Performance budget (Linux, no emulator): success
- Android / unit tests, lint, debug APK: success
- Supply chain (cargo-deny, notices): success
- Type-check Windows and macOS targets (from Linux): success
- Coverage report (non-blocking): success
- Rust / ubuntu-latest, macos-latest, windows-latest: success
- Docs links: success

No failures, so no fixes and no flakes to classify.

## Build from a clean worktree

- `scripts/build-android.sh --skip-host-tests`: gradle unit tests and lint pass, debug APK 7.9 MB, arm64-v8a and x86_64.
- Copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk; http://192.168.1.150:8000/geekbridge-debug.apk answers 200 and serves the same sha256.
- `gradle assembleRelease` (--no-daemon) succeeded; `scripts/check-release-isolation.sh` passes (no debug diagnostics, receivers, cleartext or debug permissions).
- Signing key: signer certificate sha256 4d711ab401391ad4041c7dadd070371eb658d303a08b39de29d1df690a9ca3c3 equals the previously served APK's. versionCode 10099 is unchanged, so apk_update_verdict says same-version (adb install -r still works only with -d or a bumped code if the phone has the identical build).
- PC release binaries (geekbridge, -agenda, -calls, -clipboard, -contacts, -ctl, -messages, -service, -share) rebuilt with `cargo build --release --locked` and placed over target/release by temp copy plus mv. The live service was not restarted.
