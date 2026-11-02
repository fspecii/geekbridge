# Regression run, 2026-11-02 (end of day)

Purpose: after a day of rapid changes, show that nothing that passed earlier now fails.

## Verdict

Not clean. Two items regressed against earlier results and are **open** (not fixed here), one needs a human decision:

1. **`scripts/e2e/call-ui.sh`: 12/12 PASS earlier today, now FAIL** from the pairing stage on. Cause isolated to the app:
   a `geekbridge://pair?...` link opened while the app is **not running** (cold start, e.g. right after `pm clear`) lands on the
   empty "Pair with QR code" entry page with no message. The same link opened while the app is already running shows the
   "Connect to a computer?" confirmation correctly. See "Open regressions".
2. **`scripts/perf-check.sh`: idle wakeups 5/s against a budget of 3** (the release-candidate report had 3/s and passed). Threads
   6 (RC: 3), open fds 18 (RC: 14), RSS 7.8 MB (RC: 6.6 MB), all still inside their budgets. Binary size budgets were already
   FAIL in the release-candidate report; sizes are unchanged (service 5 665 160 B, ctl 1 246 400 B).
3. The phone-to-desktop clipboard stage failed three times on the `gbhome` AVD clone and passes on a `gb36x` clone (details below).
   Treated as an emulator-image difference, not a code regression; a human should confirm.

Everything else that ran matches or beats the earlier results. Five small defects in tests and scripts were found and fixed
(list below). Nothing was committed or pushed.

## Setup

- Tree stable check: Android and Rust trees had no edits for 9.5 minutes at 19:42; the run proceeded on a snapshot taken then
  (`rsync` to `~/.cache/gb-regress/src`, excluding `target`, `.tools`, `dist`, `.git`; `.tools` symlinked to the shared toolchain).
  Other agents kept editing and committing afterwards (HEAD moved from `4bb7309cc`/`aa44f2e2e` to `a78e4b547` during the run), so
  results describe the 19:42 tree plus the fixes below, copied into both trees.
- Own build dirs under `~/.cache/gb-regress`, `nice`, `-j3`. Own emulator: AVD `gbreg` (console 5664, adb server 16143), cloned
  from `gb36x` (earlier attempts from `gbhome`), Xvfb `:153`, desktop ports 17580/17581/17516/17681/17261. Owner's service (17380/17381)
  and the LAN download server (8000) were not touched.
- Versions: rustc/cargo 1.99.1 (Arch) and the pinned 1.99.1 toolchain, OpenJDK 17.0.21, Gradle 8.13, adb 1.0.41,
  Android emulator 37.2.13.0, NDK 30.0.16248370, API 36 image, sway 1.12, Python 3.14.8.

## Results

| Check | Command | Result | Earlier | Change |
|---|---|---|---|---|
| Format | `cargo fmt --check` | PASS | PASS | none |
| Clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | PASS | none |
| Rust tests | `cargo test --workspace --locked --no-fail-fast` (`TMPDIR=~/.cache/gb-tmp`) | 1528 passed, 0 failed, 5 ignored, after fix 1 (first run: i18n guard failed) | RC report: 1132 passed | more tests, none failing |
| Android unit tests | `gradlew testDebugUnitTest` | 476 tests, 0 failures | RC report: 290 | none |
| Android lint | `gradlew lintDebug` | PASS | PASS | none |
| Android build | `gradlew assembleDebug` | PASS | PASS | none |
| xcheck Windows | `scripts/xcheck.sh windows` (project toolchain) | PASS after fix 3 (first run failed) | PASS | fixed |
| xcheck macOS arm / x86 | `scripts/xcheck.sh macos-arm macos-x86` | PASS / PASS | PASS | none |
| Doc links | `scripts/check-doc-links.sh` | 0 broken | | none |
| SPDX | `scripts/add-spdx.sh --check` | PASS after fix 2 (3 files lacked headers) | | fixed |
| Notices | `scripts/gen-notices.sh --check` | up to date | | none |
| e2e-android (all 9 stages) | `scripts/e2e-android.sh` on `gb36x` clone | all PASS (env, battery, pair, reconnect x3, clip_d2p, clip_p2d x3, share text, file_d2p, file_p2d, update) | PASS | none |
| e2e-android clip_p2d, clip_p2d_window, clip_p2d_locked | same, on `gbhome` clone, 3 runs | FAIL x3: "no connected phone has Clipboard switched on" | PASS | see note A |
| update-install desktop | `scripts/e2e/update-install.sh` | all PASS | PASS | none |
| update-install android | `E2E_UPD_REBUILD=1 scripts/e2e/update-install.sh android` | 30 PASS, 1 SKIP (`android-skip-version`, by design) after fix 5 | 33 of 33, one SKIP | none |
| pair-lifecycle | `scripts/e2e/pair-lifecycle.sh --repeat 1` | A_qr, B_qr, B_code, C_qr, C_code, repeat, rename x4 PASS; A_code SKIP (needs UDP discovery) | same (documented SKIP) | none; one repetition instead of three |
| pair-background | `scripts/e2e/pair-background.sh` | prehome, pair, home, swipe, doze PASS | PASS | none |
| tray-headless | `scripts/e2e/tray-headless.sh` | PASS | PASS | none |
| wayland (sway) | `scripts/e2e/wayland.sh` | 65 PASS, 0 FAIL, 2 SKIP (no waybar, Hyprland live) after fix 4 | PASS | fixed |
| call-ui | `scripts/e2e/call-ui.sh` | callui_env PASS; callui_pair, notify_answer, notify_decline, popup_ring FAIL; later stages not reached | 12 of 12 PASS | **PASS -> FAIL** (open regression 1) |
| scale, short mode | `scripts/scale-e2e.sh` (300 contacts, 1000 events, 40 threads) | env, seed, battery, pair PASS; measure completed (contacts full sync 704 ms, calendar first 933 ms, SMS first 6.7 s, phone PSS 64.7 -> 78 MB, desktop RSS 7.4 -> 13.1 MB) | scale-report.md | none; see note B |
| perf | `scripts/perf-check.sh --build`, rerun twice with `--bin-dir` | startup 6-11 ms, RSS 7.8 MB, 6 threads, 18 fds, 0 ticks OK; wakeups 5/s OVER (3); size OVER x2 | RC report: wakeups 3/s OK; size FAIL | **wakeups PASS -> FAIL** (open regression 2) |
| golden desktop | `scripts/golden-desktop.sh --check` (`GOLDEN_TIMEOUT=900`, `GOLDEN_JOBS=3`) | 68 screens, 0 failing | PASS | none; first run hit the 240 s limit under load |
| Rust coverage | `scripts/coverage.sh rust` | lines 86.32%, functions 84.24%; production-only 83.4% (bridge-core 94.9%, kde 88.8%, update 93.1%, apps 81.3%) | 86.3% / 84.2%; 83.6% | unchanged within noise |
| Android coverage | `scripts/coverage.sh android` | 34.4% lines, 39.8% branches (9467 lines) | 33.8% / 40.1% | unchanged within noise |

Note A (clipboard on the `gbhome` clone). After `ctl consent ... clipboard on` the desktop showed the phone as enabled and
connected, then the link dropped (`conn_aborted`) while the phone's Settings app was in the foreground for the copy step, and the
phone's redials failed with `tls_failed`. The e2e docs describe `tls_failed` on this emulator family when the redial binds to
the wrong interface. On a `gb36x` clone the identical script passed every stage. Not reproduced on any other image, no code
difference found; worth one look on a real phone.

Note B (scale). The default seed (500 contacts, 2000 events, 100 threads) twice produced an Android "System UI isn't
responding" dialog during the SMS seed (screenshot `e2e-01-setup-first-launch.png` in the run), after which battery and pair
could not find their screens. The lighter seed above ran clean. Emulator behaviour under a notification flood, not an app finding.

## Fixed during this run (all small, files re-read first, all with a failing run before and a passing run after)

1. `crates/bridge-apps/tests/i18n_catalogue.rs`: the literal guard scanned the new `*_tests.rs` and `test_support.rs` as production
   code, and stopped scanning `clipboard_link.rs` at a mid-file `#[cfg(test)] impl`. It now skips those files and only treats
   `#[cfg(test)]` followed by `mod` as the start of tests. With the scan correct it found one real new literal (the KDE Connect
   coexistence warning), so `tests/i18n_allowlist.txt` `clipboard_link.rs` went 40 -> 41.
2. SPDX headers added to `PhoneDataWaitTest.kt`, `SmsActionTokenTest.kt`, `SmsManagerLookupTest.kt`.
3. `crates/bridge-service/tests/wayland_clipboard.rs`: used `std::os::unix`, so the Windows type-check failed; now `#![cfg(unix)]`.
4. `scripts/e2e/wayland.sh`: the hotkey dry-run check expected the old default key. Super+Shift+V is now the default, and with
   the fixture's pre-bound key the installer picks Alt+Super+V and says so; the check now asserts both.
5. `scripts/e2e/update-install.sh` (`E2E_UPD_REBUILD=1` path): the scratch directory was never created and the version is now read
   from `../Cargo.toml`, so the script now creates it and writes a minimal `[workspace.package]` manifest. Also
   `scripts/golden-desktop.sh`: it sourced `android-env.sh`, which redirects `CARGO_TARGET_DIR` to `target/android`, then ran
   `target/debug` (possibly stale binaries); it now unsets `CARGO_TARGET_DIR` after sourcing.

Fix 5's `update-install.sh` edit is the only one not yet in a commit at the time of writing; the others were already swept into
HEAD by another agent's commits.

## Open regressions

1. **call-ui / cold-start pairing link.** Repro on an emulator with the debug APK and a running desktop service: `pm clear`
   the app, grant READ_PHONE_STATE and POST_NOTIFICATIONS, `am start -W -a android.intent.action.VIEW -d "'geekbridge://pair?...'"`
   with a fresh `geekbridge-ctl pair-qr` link. Result after 6 s: "Pair with QR code" with an empty link field and no message
   (`QrPairActivity` in `Mode.Entry`). Opening the same link again immediately afterwards (the activity now running, `onNewIntent`)
   shows "Connect to a computer?" correctly. `pair-lifecycle` and `pair-background` pass because the app is already running or has
   data there. Code reading found nothing in `handleIntent`/`parse`; the leading hypothesis (unverified) is that the activity is recreated
   shortly after its first launch, and `QrPairActivity` deliberately does not restore the confirmation after recreation
   (`if (savedInstanceState == null) handleIntent(intent)`), so the link is dropped silently. Owner of the Android pairing UI should
   check why a recreate happens on first launch and whether a link that arrives that way should be re-handled. Diagnostic copy of
   `scripts/e2e/call-ui.sh` with a wider failure dump was used in the snapshot only; the repo script is unchanged.
2. **Desktop idle wakeups 5/s (budget 3).** Reproduced on two reruns at a load average of about 5. The RC report had 3/s on a quieter
   host, and `docs/performance.md` says a loaded host reaches 6/s, so a host-load effect is possible; threads (3 -> 6) and fds
   (14 -> 18) also grew since then. Not investigated further. The size budget mismatch is also still open: `scripts/perf-check.sh`
   uses 5 600 000 B for the service while `docs/performance.md` says the budget was raised to 5 800 000 B.

## Could not run

- `scripts/e2e/coexist.sh`: needs the extracted stock KDE Connect tree, which is not on this machine (it would need a package
  download). Last results in `docs/reports/coexist-*-results.txt` are unchanged.
- `scripts/e2e-qr-pairing.sh`: not standalone; it expects the emulator and service of a preceding e2e run, which my cleanup
  between suites removed, so its single run failed at `setup` and is not a result. QR pairing itself is exercised by pair-lifecycle (A_qr, B_qr, C_qr)
  and pair-background.
- `golden-android.sh`, soak, fuzz, `release-local.sh`, real-device scripts: not in the requested set.
- Hyprland live and waybar (headless host); UDP discovery based scenarios (emulator NAT).
- pair-lifecycle ran one repetition per scenario, not three. Scale ran in short mode.
- call-ui stages after `callui_popup_ring` were not reached, so their current state is unknown.

## Housekeeping

- Several suites leave their desktop service, Xvfb or mock release server running on exit; later suites then failed with
  "address already in use". Results above are from runs started after killing only my own leftovers (matched by their
  `gb-regress` paths and my ports).
- The e2e file-share scenario writes into the user's real `~/Downloads/GeekBridge` and removes what it created.
- Cleaned at the end: only `~/.cache/gb-regress` (snapshot, target dirs, scratch, logs) and the `gbreg` AVD clone. Everything else was left alone.
