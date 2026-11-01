# Android emulator <-> desktop end-to-end findings

Run on 2026-11-01 by `scripts/e2e-android.sh` (repeatable; see "How to repeat"). The Android app is the debug APK
built with the real native core (`scripts/build-android.sh`), running on AVD `gb36` (API 36, x86_64 emulator). The
desktop side is the unmodified `geekbridge-service` / `geekbridge-ctl` release binaries from this checkout. Nothing
here involves a phone, a LAN, Bluetooth or a real KDE Connect peer.

Logs: `dist/android/logs/e2e-*` (`e2e-results.tsv` is the machine-readable result table of the latest run).
Screenshots: `dist/screenshots/android/e2e-*.png`.

## Topology

* Emulator `emulator-5594` with its own adb server (`ANDROID_ADB_SERVER_PORT=16038`), separate from the call
  emulator (`gbcall`, 16037) and the UX agent's AVD, so concurrent agents do not interfere.
* Desktop service on control port 17481 / API 17480, state in `target/e2e-run/state`, `--dev-loopback`
  (the emulator's NAT makes every phone look like 127.0.0.1 / 10.0.2.2).
* **Desktop -> phone:** `adb emu redir add tcp:17161:1716`, then `geekbridge-ctl connect PHONE 127.0.0.1 17161`.
  The phone sees the NAT gateway 10.0.2.2 (a private address) so its default options apply (no loopback switch).
* **Phone -> desktop** (manual "Connect" in Diagnostics to `10.0.2.2:1730`) also reaches the desktop, but see E2E-2.
* UDP discovery/announce cannot cross the emulator NAT, so "Look for computers" finds nothing here. That path is
  **not testable on an emulator**.
* Desktop clipboard: a private Xvfb (`:97`) with `xclip`, the service started without `WAYLAND_DISPLAY`, so the
  real user clipboard is never read or written.
* Update server: `scripts/e2e/mock_release_server.py` on 127.0.0.1:18890, reached with `adb reverse`.

## Results

| Scenario | Result | Notes |
| --- | --- | --- |
| Build APK with native core, install | PASS | `scripts/build-android.sh` (x86_64 + arm64 libs, 16 KiB aligned) from a scratch copy of `android/` |
| Setup wizard walk, notification permission | PASS | Walked by step title (wizard grew from 7 to 9 steps during the run) |
| **Battery-exemption prompt** | PASS | "Allow unrestricted battery" opens `RequestIgnoreBatteryOptimizations`; **Deny** leaves the step at "To do"; **Allow** shows "Done / Unrestricted" and the app is on the Doze whitelist (`dumpsys deviceidle whitelist`) |
| **Pairing** | PASS | Desktop `connect` + `pair`; both sides showed the same 8-hex code and the same certificate fingerprints; approved on both; desktop `paired` lists the phone; `ping` ok |
| **Reconnect** | PASS | `disconnect` then redial: back to `connected` with no new code. App force-stopped and reopened: redial connects with no code. Desktop still lists the pairing |
| **Desktop -> phone clipboard** | PASS | Needs the phone's Clipboard sync on and `consent clipboard on`; text appeared on the phone clipboard (read with the app's Paste button). See E2E-6 |
| **Phone -> desktop clipboard** | PASS | Copied in a system text field (non-sensitive); `clipboard paste-from-phone` put it on the desktop clipboard. The phone answers at once only if it saw the copy while focused, otherwise it posts "tap to send"; the script taps it |
| 30 s window | PASS | An item copied 40 s earlier is refused: "Nothing recent on the phone: copy again, then paste within the time window." Desktop clipboard unchanged |
| Unlocked rule | PASS | Screen off before the pull: "Phone is locked: unlock it and copy again." Nothing was read |
| Text share desktop -> phone | PASS when the phone link is healthy | `send-text` shows "Text from E2E-Desktop" with Copy/Close |
| **File desktop -> phone** | PASS (after fixes E2E-5, E2E-5b, E2E-5c) | Offer dialog, Accept, SHA-256 of the received file equals the sent one; text share still works |
| **File phone -> desktop (Share sheet, `am start ACTION_SEND`)** | PASS (after fixes E2E-4, E2E-7) | System chooser -> "Send to computer" -> file arrives in the desktop save folder, SHA-256 equal |
| **Update check (mock release server)** | PASS (after fixes) | v99.0.0 offered; download and SHA-256 verified against `SHA256SUMS`; "Install update" opened Android's "Install unknown apps" page for GeekBridge. The installation itself was not completed. See E2E-8 |

## Not testable on an emulator

* UDP discovery / announce, mDNS-style "find each other automatically" (NAT drops broadcasts).
* Real Wi-Fi roaming, IP changes, sleep/Doze behaviour with a real radio; the battery step was only checked up to the
  system dialog and the Doze whitelist, not that the link survives a real overnight idle.
* Whether `PowerManager` exemption keeps `LinkService` alive for hours; OEM battery screens.
* Real clipboard-in-background restrictions on a physical device (the emulator's `google_apis` image behaves like
  stock Android 16, but vendors differ).
* Hyprland/GNOME/KDE global hotkey for paste-from-phone (the test uses `geekbridge-ctl clipboard paste-from-phone`).
* Interoperability with stock KDE Connect on a phone.
* Completing the APK self-update (the debug APK is not an upgrade: same signing key, lower versionCode than the mock tag).
* Call audio / HFP (not part of this app).

## Findings

Severity: **High** blocks a documented feature, **Medium** wrong or misleading behaviour, **Low** test-harness or polish.

### E2E-1 (fixed) Link is stopped right after "Start link" when navigating between screens -- High
`ShareCoordinator.onSnapshot` stops a running link when `controller.appVisible` is false. `appVisible` was a flag set in
each activity's `onStart` and cleared in `onStop`; on Home -> Diagnostics the new screen's `onStart` runs before Home's
`onStop`, so the flag ended up false, and `SetupActivity` never set it. "Start link" (Diagnostics) and "Look for
computers" (Setup) therefore showed "Link stopped: the transfers finished and the app is in the background." instead
of listening.
Repro (before the fix): fresh install, Home -> Diagnostics & about -> Start link: nothing listens on 1716.
Fix (`TransportController.appVisible`): the getter also returns true while `ActivityTracker.startedCount > 0`.
**Not fixed in:** `HomeActivity` / `SetupActivity` (UX agent): they should stop writing the flag.

### E2E-2 Phone -> desktop manual "Connect" does not show the connection in the phone UI -- Medium (NOT root-caused, not reproduced)
Originally: the desktop saw the TLS connection (`needs_pairing`, role `acceptor`) but the phone's Diagnostics "Connections"
box stayed empty and only later reported `park_timeout`. Not reproduced in the second session (Diagnostics layout has
changed; the scripted UI walk to the Connect fields no longer works, so the retry was inconclusive). Reading the code:
`Core::admit` registers the parked connection as soon as `endpoint.connect` returns, and `PairPanel`/`LinkSection` poll
every 0.5-2 s, so the box should fill. An in-flight dial (before the handshake finished) is not listed at all, which could
explain an empty box if the handshake took long. The loopback integration test `phone_dials_parks_then_requests_pairing`
passes. Next step: repeat with the new Diagnostics layout while watching `geekbridge-ctl events` and logcat.

### E2E-3 Phone stops listening after the computer unpairs it -- Low (probably by design)
After `geekbridge-ctl unpair PHONE` the phone's link goes to "stopped" (no listener). Re-pairing needs the user to
start it again. Worth a sentence in the UI ("start looking again to re-pair").

### E2E-4 (fixed, test-harness cause) Share sheet showed "The link is not running" -- was High
Root cause was **not** the app: the AVD still had a leftover build `dev.geekbridge.android.me` (E2E-9) and the chooser's
"Just once" picked that package's `ShareActivity` (`dumpsys activity top`: `dev.geekbridge.android.me/...ShareActivity`),
which has no running link. `scripts/e2e-android.sh` now uninstalls every other `dev.geekbridge.*` package before the share
stage and force-stops the app first (a stale singleTop Share screen would reuse its old session).
Defensive app fixes made while investigating (the original theory, not reproduced): `ShareCoordinator.onSnapshot` no longer
stops the link while `LinkService` runs (same rule as `ActivityTracker.leavingApp`), and `ShareActivity` starts the link once
if it opened while the link was still stopping.

### E2E-5 (fixed) Desktop -> phone file stayed at "Waiting 0/N" after Accept -- was High
Three independent causes, found with the emulator and a socket sampler (`/proc/net/tcp` on the phone, `ss` on the host):
1. **Random payload port (E2E-5a).** `--dev-loopback` sets `PayloadConfig.any_port`, and `PayloadContext::listen` then bound an
   *ephemeral* port. The phone (real config, ports 1739-1764 only) rejected it with `port_rejected`; the desktop never learns
   that and waits. Fix: `listen` always tries the standard range first and uses port 0 only as a fallback when `any_port`.
   Repro test (failed before the fix): `payload::tests::development_listener_announces_a_port_a_strict_receiver_accepts`.
2. **Bound source with no route (E2E-5b).** The phone binds the payload socket to the control link's local IP. In the
   emulator that is eth0 `10.0.2.15` while the default route is wlan0 `10.0.2.16`; the SYN sat in `SYN_SENT` for the whole 10 s
   connect timeout and the transfer failed with `timeout`. Fix: the bound attempt gets 3 s, then the plain route is used
   (`payload::dial` / `first_or_fallback`; test `a_blackholed_bound_source_falls_back_to_the_plain_route`; the real emulator
   repro is the `SYN_SENT` capture above, a loopback test cannot create a blackholed source).
3. **Offer dialog vanished after a Share screen was shown (E2E-5c).** `ShareCoordinator.onChange` was a single callback slot:
   `ShareActivity.onStop` set it to null and Home's `OfferDialogs` never heard about offers again ("no offer dialog" in the
   scripted run; only the notification appeared). Now a keyed listener set (`setListener(owner)` / `clearListener(owner)`).
Silent failures remain by protocol: the phone cannot tell the desktop it failed, so the desktop shows `Waiting` until its
120 s accept timeout.

### E2E-6 Clipboard sync can read "On" in the UI while the engine ignores the desktop -- Medium
After pairing and an app restart, the Home "Clipboard sync" switch showed "On. Your computer can ask..." but the
desktop's pushes were ignored and `paste-from-phone` answered "Clipboard sync is off in the phone app." Toggling the
switch Off and On again re-armed it. Repro: pair, force-stop the app, reopen Home, enable Clipboard sync, push text from
the desktop. The script always cycles the switch.

### E2E-7 (fixed in the harness) Phone -> desktop payload port unreachable through the emulator NAT
The phone listens on the first free port of 1739-1764 and the desktop dials `127.0.0.1:<port>`; no redirect existed.
`desk_dial` in `scripts/e2e-android.sh` now also adds `adb emu redir tcp:1739..1742`. The desktop's own payload listener
(desktop -> phone) takes the first free port after the redirected ones, so both directions work side by side.

### E2E-8 (fixed) The update-check loopback hook could not be used in the app -- Low
`UpdateChecker` accepts `http://127.0.0.1` for tests but nothing in the app could point it there, and Android blocks
cleartext traffic by default (`IOException`, shown as "Couldn't reach GitHub"). Added debug-only plumbing:
`UpdateChecker.forBuild` reads the `updates` preference `debug_base_url` (set through `adb shell run-as`) in debug
builds only, and `android/app/src/debug/` adds a network security config permitting cleartext to `127.0.0.1` only.
Release builds are unchanged.

### E2E-9 Observations for the other agents
* Setup wizard step count changed under the test (7 -> 9); the script walks it by title.
* `dev.geekbridge.android.me` is a leftover package on AVD `gb36`; the script ignores it.
* The `pm clear`ed phone gets a new device id, but the desktop keeps the old trust record; the script unpairs stale ones.
* Update auto-check ran against real api.github.com during the first launches (the emulator has internet); harmless.

## Files changed by this work

* New: `scripts/e2e-android.sh`, `scripts/e2e/adbui.py`, `scripts/e2e/mock_release_server.py`,
  `android/app/src/debug/AndroidManifest.xml`, `android/app/src/debug/res/xml/debug_network_security.xml`,
  `docs/e2e-findings.md`.
* Edited: `android/.../TransportController.kt` (E2E-1), `UpdateChecker.kt` + `UpdateSection.kt` (E2E-8, debug hook).
* File transfer fixes: `crates/bridge-kde/src/payload.rs` (+ tests), `ShareCoordinator.kt`, `OfferDialogs.kt`, `ShareActivity.kt`,
  `scripts/e2e-android.sh` (payload redirects, stale-service restart on `--rebuild`, leftover-package cleanup).
  Nothing was committed.

## How to repeat

```sh
scripts/e2e-android.sh                       # all stages, ~15 min, builds what is missing
scripts/e2e-android.sh --rebuild --only env,file_d2p,file_p2d   # file transfers both ways
scripts/e2e-android.sh --no-build --only pair,reconnect
scripts/e2e-android.sh --rebuild             # rebuild desktop binaries and the APK first
scripts/e2e-android.sh --stop                # stop the emulator, service and Xvfb it started
```

Needs the `.tools` toolchain (`scripts/bootstrap-android.sh`), AVD `gb36`, `xclip`, `Xvfb`, `rsync`, `python3`.
File transfers to the desktop write to the real `~/Downloads/GeekBridge`; the script removes what it creates.
