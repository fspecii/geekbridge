# Android build verification 11

Build commit: 2120eeffd2fa3663e45c239c99f24f6bc9ce40c9 (origin/main, clean worktree)
APK sha256: fb0aab3add85ca8f9d176442cfa9e0fc87556ac7e663433cb92442ac20e30438 (8061301 bytes)

- `git merge-base --is-ancestor 660803c30 HEAD`: yes (photos security fixes included; photos e2e ff175f6d7 included).
- `scripts/build-android.sh --skip-host-tests`: succeeded, Gradle unit tests and lint passed.
- Debug APK copied to `dist/android/geekbridge-debug.apk` and `dist/android/serve/geekbridge-debug.apk`; `curl -sI http://192.168.1.150:8000/geekbridge-debug.apk` returned 200 with Content-Length 8061301. Replaces the earlier build 9a3f0423 (built at 06a6b1a17).
- Release build (`gradle --no-daemon assembleRelease`) succeeded; `scripts/check-release-isolation.sh` passed.
- PC release binaries rebuilt with `scripts/build-desktop.sh` and placed over `target/release` via temp copy and mv; the live service was not restarted.
