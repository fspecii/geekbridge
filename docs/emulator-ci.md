# Emulator end-to-end suites in CI

`.github/workflows/emulator-e2e.yml` runs the Android emulator end-to-end suites on `ubuntu-latest` (hardware-accelerated
through KVM). It runs nightly (03:17 UTC) and by hand:

```
gh workflow run emulator-e2e.yml --ref main                 # all suites
gh workflow run emulator-e2e.yml --ref main -f only=call-ui # one suite (the others finish at once, skipped)
```

Read-only token, no secrets, nothing is published. One concurrency group per ref (a run queues behind a running one),
60 minutes for the build job and 45 for each suite.

## How it is put together

| Job | What it does |
|---|---|
| `build` | `scripts/bootstrap-android.sh` (project-local JDK, Gradle, Rust with the Android targets, SDK, NDK in `.tools/`), release desktop binaries (`bridge-service`, `bridge-apps`), the debug APK with the native core. Packs binaries, APK and the native libraries into the `e2e-build` artifact (3 days) |
| `suite` (matrix, `fail-fast: false`) | Enables KVM, frees disk, installs the desktop test dependencies (Xvfb, xclip, dbus, sway, mako, grim, xdotool, ImageMagick, tesseract), unpacks the build into `~/ci-e2e` and points `E2E_TARGET` / `E2E_SCRATCH` at it (the scripts then skip their own builds), runs `scripts/ci/e2e-setup.sh`, then the suite |

`scripts/ci/e2e-setup.sh light` reuses the runner's cmdline-tools and JDK 17 and installs only the emulator, platform-tools and the
`system-images;android-36;google_apis;x86_64` image into `.tools/`, then creates the base AVD `gb36` with
`scripts/emulator-call.sh create-avd`. Every suite clones its own AVD from it (`GB_BASE_AVD=gb36`; the scripts' default
names `gb36u`, `gb36k` and so on are only clone names). `full` runs the whole bootstrap (the update suite builds two more APKs).

Script changes made for CI, all inert on a developer machine: `start_emulator` in `scripts/e2e-android.sh` uses the in-repo
`scripts/emulator-call-launch.sh` when the untracked `.tools/gbcall/start.sh` is absent and skips `systemd-run --user` when
`CI` is set; the call-ui notification stage probes `notify-send` when `CI` is set (below); the Hang up check accepts a red pill
where OCR cannot read white-on-red text; the features dial stage clears stacked test notifications before the confirmation question.

Failure artifacts (7 days, `logs-<suite>`): `dist/android/logs`, `dist/screenshots`, the suites' run logs, a final logcat.
Every run also prints each suite's results table in its job summary.

## Suites and status

Measured on the runs in [reports/emulator-ci.md](reports/emulator-ci.md).

| Suite | Script | Time | Status |
|---|---|---|---|
| `features` | `scripts/e2e/features.sh` (battery sharing, notification mirroring, PC dial) | 10 min | blocking |
| `battery-optimisation` | `scripts/e2e/battery-optimisation.sh` | 11 min | blocking |
| `update-install` | `scripts/e2e/update-install.sh all` (desktop updater and Android upgrade, two extra APK builds) | 22 min | blocking |
| `call-ui` | `scripts/e2e/call-ui.sh` (headless sway + mako, egui Calls window) | 6 min | blocking; the notification Answer/Decline stage is skipped on the runner, see below |
| `e2e-android` | `scripts/e2e-android.sh` | 13 min | **non-blocking** |
| `pair-background` | `scripts/e2e/pair-background.sh` | 6 min | **non-blocking** |

### Known limits on the runner

- **KVM works.** GitHub-hosted Linux runners expose `/dev/kvm` once the udev rule in the workflow is applied; no nested-virtualisation
  problem was met. The emulator runs headless with `-gpu swiftshader_indirect`, as locally.
- **`e2e-android` / `file_d2p` was a product bug, fixed** (desktop to phone file: `Failed 0/300000 bytes (link_lost)` right after Accept).
  The previous link's 10 s grace timer failed a transfer that belonged to the newer link. Details and the regression test:
  docs/reports/file-d2p-ci.md. The job stays non-blocking until a dispatched run confirms it.
- **`pair-background` / `prehome`** now makes up to three attempts (re-dialling a phone whose listener is a moment behind) and keeps the
  desktop status/events and the phone log of every failed attempt (`pb-prehome-attempt-*.txt`); the PASS line says which attempt passed.
  It stays non-blocking until a few nightly runs show attempt 1 passing.
- **`call-ui` notification stage (`callui_notify_answer` / `_decline`) is skipped.** Ubuntu 24.04's libnotify 0.8.8 `notify-send`
  prints "Actions are not supported by this notifications server" against mako (although mako lists the capability) and sends
  the notification without Answer/Decline, so there is nothing to invoke. The check is a probe, so it runs again wherever the pair works.
  Reported as SKIP, not PASS.
- The emulator does not suspend its CPU in Doze, the same honest limit as locally (see `docs/reports/battery-optimisation-onboarding.md`).
- Not run in CI: the suites that need the real network, a real phone, Wayland compositors beyond the headless sway above
  (`wayland.sh`), Windows or macOS.
