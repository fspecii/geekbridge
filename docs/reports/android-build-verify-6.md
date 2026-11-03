# Android build verify 6

Recipe: `source scripts/android-env.sh; scripts/build-android.sh --skip-host-tests` (TMPDIR=/home/ambsd/.cache/gb-tmp).

- assembleDebug, testDebugUnitTest (549 test cases, 0 failures) and lintDebug: BUILD SUCCESSFUL in 2m48s.
  - The first two attempts failed in compileDebugKotlin: the working tree held uncommitted Android edits from other work in progress (Kotlin referenced string resources not yet present in gb_ui.xml; a concurrent Gradle daemon also tripped an incremental-state error). The third run, once the resources landed, passed. The tree was not clean at build time.
- APK copied to dist/android/geekbridge-debug.apk and dist/android/serve/geekbridge-debug.apk (dist is git-ignored).
  - sha256 1f1c0458edc12433e9fe8c25accf2875253492be2f9be574f5c1d42ef6f41f08
  - size 10601752 bytes
- `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk`: connection refused (curl exit 7); nothing listening on :8000 at verification time. The server was not started or restarted, so the Content-Length match could not be checked.
- Release isolation: assembleRelease succeeded with 95 GB free; scripts/check-release-isolation.sh passed.
- PC release binaries: `cargo build --release --locked -p bridge-service -p bridge-apps` finished in 2m59s (includes geekbridge-ctl). The live service was not restarted.
- Host cargo test for bridge-android was skipped (--skip-host-tests).
