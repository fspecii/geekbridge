# Android build verify 4

Recipe: `source scripts/android-env.sh; scripts/build-android.sh --skip-host-tests` (TMPDIR=/home/ambsd/.cache/gb-tmp).

- assembleDebug, testDebugUnitTest (498 test cases, 0 failures) and lintDebug: BUILD SUCCESSFUL in 1m28s. Only existing deprecation warnings.
- APK copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk (dist is git-ignored).
  - sha256 abd62dee06b73b31847b9266575e372adf2aa395bd37f71e17c0fdaf970e3eb6
  - size 10624296 bytes (11 MB), ABIs arm64-v8a + x86_64
- `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`: HTTP 200, Content-Length 10624296 (matches).
- Release isolation: assembleRelease took 1m11s with 126 GB free; scripts/check-release-isolation.sh passed (no debug receivers, cleartext or debug permissions).
- Host cargo test for bridge-android was skipped (--skip-host-tests).
