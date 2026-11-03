# Android build verify 5

Recipe: `source scripts/android-env.sh; scripts/build-android.sh --skip-host-tests` (TMPDIR=/home/ambsd/.cache/gb-tmp). Includes the lld `--icf=all` flag.

- assembleDebug, testDebugUnitTest (533 test cases, 0 failures) and lintDebug: BUILD SUCCESSFUL in 1m01s.
  - The first run failed one test (NotificationMirrorTest.aLongOrPaddedButtonTitleStillPressesTheRealButton) while the working tree held uncommitted edits to NotificationMirror.kt and its test; the rerun after those edits settled passed. The tree was not clean at build time (uncommitted Android and Rust changes from other work).
- APK copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk (dist is git-ignored).
  - sha256 d659cbbd2270e42ccbdbe725f8a68e351f5de275082c738a0631bada580443f4
  - size 10587992 bytes
- `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`: HTTP 200, Content-Length 10587992 (matches). Server not restarted.
- Release isolation: assembleRelease succeeded with 106 GB free; scripts/check-release-isolation.sh passed.
- PC release binaries: `cargo build --release --locked -p bridge-service -p bridge-apps` finished in 2m32s (includes geekbridge-ctl). The live service was not restarted.
- Host cargo test for bridge-android was skipped (--skip-host-tests).
