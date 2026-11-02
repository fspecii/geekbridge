# Android build verify 3

Recipe: `source scripts/android-env.sh; scripts/build-android.sh --skip-host-tests` (TMPDIR=/home/ambsd/.cache/gb-tmp).

- assembleDebug, testDebugUnitTest (488 test cases, 0 failures, includes CallControlTest dialNeedsPermissionAnIdlePhoneAndAPlainNumber) and lintDebug: BUILD SUCCESSFUL. No compile, manifest or test fixes were needed. Only existing deprecation warnings.
- scripts/e2e/call-ui.sh --stop: all 12 stages PASS (dist/android/logs/e2e-callui-results.tsv). Its own emulator was stopped; an unrelated `gbreg` emulator (port 5664) and Xvfb :153 belong to another run and were left alone.
- APK copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk (dist is git-ignored).
  - sha256 ebad4a94cd22be44c5c697c9db51ddc0c91ba40b39996208eb80baaf450b0d44
  - size 10623456 bytes (11 MB), ABIs arm64-v8a + x86_64
- Host cargo test for bridge-android was skipped (--skip-host-tests).
