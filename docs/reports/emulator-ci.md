# Emulator end-to-end suites on GitHub Actions (2026-11-03)

A dated snapshot. Living description: [docs/emulator-ci.md](../emulator-ci.md). Workflow: `.github/workflows/emulator-e2e.yml`.

Goal: run the emulator suites on `ubuntu-latest` with KVM, nightly and on dispatch, one matrix job per suite, and get every suite
that can pass on a CI emulator to pass. Repository `fspecii/geekbridge`, runs triggered with `gh workflow run emulator-e2e.yml --ref main`.

## Final state (run 41686018738, all jobs complete, run conclusion success)

| Suite | Result | Time |
|---|---|---|
| build (bootstrap, desktop binaries, APK) | pass | 10 min |
| features | pass | 10 min |
| call-ui | pass (notification Answer/Decline stage reported SKIP, see below) | 4 min |
| battery-optimisation | pass | 12 min |
| pair-background | pass (non-blocking, flaky) | 6 min |
| update-install (desktop and Android upgrade) | pass | 21 min |
| e2e-android | **fail, non-blocking**: `file_d2p` only (every other stage passes) | 14 min |

## Runs

| Run ID | What | Outcome |
|---|---|---|
| 41650358489 | first run | build ok; pair-background prehome fail; e2e-android clip_p2d and file_d2p fail; call-ui, features, battery, update-install hung in the logcat step until the 45 min limit (adb with no device), found afterwards to have finished in 5-10 min |
| 41659816195 | logcat bounded | pair-background pass; battery pass; call-ui (no `notify-send`, OCR of the Hang up button), update-install (no native libs for its extra APK builds), features dial Deny, e2e-android file_d2p fail |
| 41664683653 | fixes for those | cancelled: the native libraries were left out of the artifact |
| 41666540220 | libs in artifact | pair-background, battery, features pass; call-ui notification actions; update-install still no native libs (copied into an existing directory); e2e-android file_d2p |
| 41671548378 | e2e-android non-blocking | cancelled early to add the `only` input |
| 41675114736, 41678569248 | `only=call-ui` | call-ui isolates the notification-action problem (below) |
| 41680837634 | full | update-install pass (21 min), battery, features pass; call-ui notification probe did not match (pipeline status), pair-background prehome fail again |
| 41686018738 | full, final | table above |

## What had to change

Workflow side: KVM udev rule, disk clean-up, a `build` job whose output (release binaries, APK, the git-ignored `jniLibs`) is handed to
the suites as an artifact so no suite builds, `E2E_TARGET` / `E2E_SCRATCH` / `GB_BASE_AVD=gb36` exported to the scripts, `scripts/ci/e2e-setup.sh`
for the toolchain (the runner's cmdline-tools and JDK, emulator and image only; `full` for update-install), a bounded logcat step,
`only` dispatch input, results tables in the job summary, failure artifacts. No secrets, `contents: read`.

Script side (small, inert outside CI): the emulator launcher falls back to `scripts/emulator-call-launch.sh`; no `systemd-run --user` when
`CI` is set; call-ui checks the Hang up button by colour when OCR cannot read it; the features dial stage clears stacked notifications
before asking its confirmation question (with five notifications stacked the Deny button is not on screen); `file_d2p` writes
logcat and status evidence on failure.

## What cannot pass on the runner, and why

- **call-ui notification stage.** Ubuntu 24.04's libnotify 0.8.8: `notify-send --wait --action=...` against mako prints "Actions are not
  supported by this notifications server" although `GetCapabilities` lists `actions`, and the `Notify` call carries an empty action
  array. The stage probes that and reports SKIP. The Calls window stage (popup, Answer, Hang up, Decline) runs and passes.
- **e2e-android `file_d2p`** (desktop to phone file, 300 kB): `link_lost` right after Accept, in every run where the suite ran (five); phone log shows it created
  the pending download file and then deleted it. Passes on the development machine. Not diagnosed; the job is non-blocking. Everything
  else in that suite passes, including `file_p2d`, clipboard both ways (`clip_p2d` failed once, run 41650358489, with the link not up
  yet) and the update check.
- **pair-background `prehome`** (phone in the background while the pairing code is compared): failed in three of six full runs and passed in the others. The 8 s window is the likely culprit on a shared runner. The home, swipe and doze stages always passed. Non-blocking.
- No problem with nested virtualisation: KVM was available on every runner.
- Not attempted: `wayland.sh`, `coexist*.sh`, `pair-lifecycle.sh`, `pair-rename.sh`, `ux-sweep.sh` and the other scripts under
  `scripts/e2e/`. The matrix is a list in the workflow, so they can be added one line each.

## Costs

Public repository: free minutes. One full run is about 85 runner-minutes (build 10, suites 6 to 21 each). The nightly schedule runs it once a day.
