# Android build verification 14

Date: 2026-11-04. Head built: `508897ab5` (origin/main when the 150-minute settle wait ended; main never stayed quiet for 25 minutes, other agents kept pushing). Built from a clean worktree under `~/.cache` (removed afterwards), TMPDIR `~/.cache/gb-tmp`.

## CI state
* `bf50fc236` check.yml: success. `508897ab5` differs from it only by test-deadline changes (flake sweep 2) and a report; its own check.yml run was cancelled by the next push (`1e3b45894`), whose run was still in progress. `4c196e52b` had failed earlier and was fixed by later commits. No failure left to classify.
* `1e3b45894` (Windows/macOS call guidance, overlay without Wayland) landed after the build and is NOT in this APK or in the PC binaries.

## Debug APK
* `scripts/build-android.sh --skip-host-tests` (gradle unit tests + lint): BUILD SUCCESSFUL.
* versionCode **20199** (installed on the phone: 10099), so the system installer accepts it as an update. Verdict from `apk_update_verdict`: ok.
* Signing certificate SHA-256 `4d711ab4...ca3c3`, identical to the previously served APK.
* APK SHA-256 `8004df3766442946c0d32ef0e03bb2b3f722c0d26029f52549bb066e9535cc42`, 8.4 MB, abis arm64-v8a and x86_64.
* Copied to `dist/android/geekbridge-debug.apk` and `dist/android/serve/geekbridge-debug.apk`; `apk-qr.png` regenerated for `http://192.168.1.150:8000/geekbridge-debug.apk`. `curl -sI` returned 200 and the served bytes hash equal to the build.

## Release APK and isolation
* `gradle --no-daemon :app:assembleRelease`: ok.
* `scripts/check-release-isolation.sh` FAILED on the unmodified script: "classes.dex contains debug-only code or packet names". Cause: a false positive. The pattern `geekbridge-debug` matches `UpdateModel.DEV_APK = "geekbridge-debug.apk"`, the dev update channel's asset name (commit `4a9eff0ab`, a legitimate release-side constant). The check meant the debug thread names (`geekbridge-debug-logcat`, `-send`, `-state`, `-cfg`). With the pattern narrowed to `geekbridge-debug-` the check passes on this release APK (no debug classes, receivers, cleartext or permissions). The one-line fix is NOT committed here (scope of this task); patch saved at `~/.cache/isolation-fix.patch` (`s/|geekbridge-debug|/|geekbridge-debug-|/` in scripts/check-release-isolation.sh). Whoever owns the script should apply it.

## PC binaries
* `cargo build --release --locked -p bridge-apps -p bridge-service` from the same worktree (4 min). geekbridge, -agenda, -calls, -clipboard, -contacts, -ctl, -messages, -service and -share were copied over `target/release` of the main checkout through a temp file and `mv`. The running service was not restarted; it keeps the old inode until `scripts/run-desktop.sh --restart`.
* Binaries not in this package set (gb-fuzz, call-e2e, call-probe, pair, phone-sim) were left as they were.

## Left over
Worktree and build trees deleted (about 10 GB freed; 69 GB free after).
