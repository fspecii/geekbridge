# Battery optimisation onboarding and Doze survival

Date 2026-11-03. Scope: asking the user to exclude GeekBridge from battery saving, showing the status, vendor guidance,
a warning when restricted, and whether the link and the 60 s screen-off heartbeat survive Doze.
Script: `scripts/e2e/battery-optimisation.sh` (own AVD clone `gbbat`, console 5664, adb server 16130, ports 17790-17861,
Xvfb :96, scratch `~/.cache/gb-battery`; the clone and emulator are stopped on exit).

## Audit

| Item | Before | Now |
|---|---|---|
| First-run ask | Wizard Battery step fires `ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, falls back to the optimisation list, then app settings. Works (system dialog, Deny keeps the prompt, Allow shows Unrestricted). | Unchanged. Play policy note added to `docs/android-power.md`. |
| Status | Setup step and overview show Unrestricted / Optimised / Restricted; whitelist confirmed with `dumpsys deviceidle whitelist`. | Unchanged. |
| OEM guidance | Xiaomi, Samsung, Huawei, OnePlus, OPPO, vivo, Nothing. Samsung lacked the real name "Never sleeping apps" and Adaptive battery. Pixel/Google had no guide (and no emulator coverage). | Samsung guide rewritten; new Pixel/Google guide; Nothing/Pixel get a "if it still disconnects" heading instead of "your phone's maker adds a switch". |
| Restricted warning | Only the `RUN_ANY_IN_BACKGROUND` mode was detected; the button opened app details. A `restricted` standby bucket was ignored. | Restricted bucket (not Doze-exempt) is detected; the button opens the app battery screen with fallback. Home banner "Fix" opens the Battery step. |
| Doze | Link survives forced idle (old APK, emulator). 60 s ping is a tokio timer on the monotonic clock. | New `transport_heartbeat_now`, sent on every maintenance window / Doze end. |

## Doze finding

The heartbeat timer uses `CLOCK_MONOTONIC`, which does not advance while the CPU is suspended. On a real phone in deep
Doze the 60 s ping can therefore land far from the maintenance window, and a window (a few seconds to a minute, minutes
to hours apart) can pass without a ping, leaving a dead NAT mapping unnoticed. The service now listens for
`ACTION_DEVICE_IDLE_MODE_CHANGED`; when `isDeviceIdleMode` becomes false (window or end) it calls
`transport_heartbeat_now`, the engine loop sends a ping at once on every link, and a failed write starts the existing redial.

Emulator measurements (Android 16, forced idle, screen off, app not Doze-exempt, old APK): link stayed connected for
4 minutes, pings arrived every 60 s. Whitelisted with the new APK: 3 consecutive `deviceidle step deep` windows, the
desktop saw a ping within 8 s of each window while the last phone ping was only 15-40 s old, link connected, service
running. Without a screen-off Doze the gap between phone pings measured 59-60 s.

Limits: the emulator does not suspend its CPU, so the real clock stop, real network blocking for non-exempt apps in Doze,
and OEM screens are not reproduced. Not run on a physical phone.

## Results (`dist/android/logs/battery-optimisation-results.tsv`)

All PASS: battery (wizard, deny, allow, whitelist), ask-guide, ask-whitelist, restricted-bucket, restricted-appop,
restricted-clear, heartbeat, doze.

## Tests

* Gradle: `:app:testDebugUnitTest` and `:app:lintDebug` exit 0 (lint 0 errors). New: `PowerRules.classify`, Samsung and
  Pixel guide wording, `DozeRules`, `heartbeatNowRequest`.
* Rust: `bridge-android` tests pass, including `heartbeat_now_needs_no_running_transport_and_takes_no_arguments`.

## Script notes

The AVD clone is made from `gb36u` (`GB_BASE_AVD`); it may carry a newer app build, so the script uninstalls first. The
restricted stage needs a paired phone and the Doze stages re-dial, because a restricted app may have lost its service.
