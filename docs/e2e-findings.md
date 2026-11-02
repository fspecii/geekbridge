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
| **File desktop -> phone** | PASS (after fixes E2E-5, E2E-5c, E2E-14) | Offer dialog, Accept, SHA-256 of the received file equals the sent one, 100 MB and 500 MB included; text share still works |
| **File phone -> desktop (Share sheet, `am start ACTION_SEND`)** | PASS (after fixes E2E-4, E2E-7) | System chooser -> "Send to computer" -> file arrives in the desktop save folder, SHA-256 equal |
| **Update check (mock release server)** | PASS (after fixes) | v99.0.0 offered; download and SHA-256 verified against `SHA256SUMS`; "Install update" opened Android's "Install unknown apps" page for GeekBridge. The installation itself was not completed here; completed in the self-update row below. See E2E-8 |
| **Self-update, Android: real upgrade install** | PASS | v0.1.0 (versionCode 10099) -> v0.2.0 (20099), same debug key, via the app's Install button and the system package installer; versionName/versionCode changed, private data intact. See "Self-update end to end" below |
| **Self-update, desktop (service + `geekbridge-ctl update`)** | PASS | check, SHA-256 verified download, skip persistence, nothing executed. See below |

### Re-run on the current tree, 2026-11-01 (pairing experience, call control, phone data, diagnostics report)

Private set so it ran beside the other agents: AVD `gb36q` (a copy of `gb36`, console 5614), adb server port 16050, desktop
state `target/e2e-full-run` (control 17391, API 17390, discovery 17446, listen 1735, redirect 17361), Xvfb `:99`, release binaries
in `target/e2e-full`, APK built fresh from a scratch copy of `android/` (sha256 `aaf58478c969...`, built 16:36). Scripts:
`scripts/e2e-android.sh` (old scenarios), `scripts/e2e-qr-pairing.sh`, new `scripts/e2e-new.sh` (everything below marked *new*).
`dist/android/logs/e2e-full-results.tsv`, `e2e-qr-results.tsv`, `e2e-new-results.tsv`; screenshots `dist/screenshots/android/{e2e,qr,new}-*.png`.

| Scenario | 2026-11-01 | Notes |
| --- | --- | --- |
| Build APK + desktop binaries, install | PASS | |
| Setup wizard walk, notification permission | PASS | wizard is 9 steps |
| Battery-exemption prompt (Deny, Allow) | PASS | |
| Pairing, code comparison (desktop dials phone) | PASS | same code both sides, ping ok |
| Reconnect, reconnect after app restart, trust listed | PASS | |
| Desktop -> phone clipboard | PASS | first attempt failed only in the harness (E2E-12) |
| Phone -> desktop clipboard (fresh item) | PASS | one attempt read "The phone's clipboard is empty" (flaky copy through the Settings search field); passed on another |
| Clipboard 30 s window | PASS | refusal text as documented (one run lost the link first, E2E-10) |
| Clipboard unlocked rule | FAIL (inconclusive) | the link was down before the screen was switched off in two runs, so the refusal was "no phone connected", not "locked"; see E2E-10 |
| Text share desktop -> phone | PASS | |
| File desktop -> phone | **FAIL** at the time, PASS since (E2E-14) | offer dialog appears, Accept tapped, the phone never dials the payload port (E2E-11) |
| File phone -> desktop | **BLOCKED / FAIL** | `tls_failed`: host ports 1739-1742 were held by another emulator (`emulator-5594`, adb server 16038) so the desktop's payload dial reached the wrong phone. Needs a quiet host (E2E-11) |
| Update check (mock release server) | PASS | v99.0.0 offered, SHA-256 verified, install handoff |
| *new* QR code expired (125 s) | PASS | "That QR code has expired. Show a new one and scan it again."; nothing paired |
| *new* QR pairing (link opened as the camera app would), no code comparison, next-steps card | PASS | `qr_refused`/`qr_accepted`/`paired` events; "What to do next" card shown |
| *new* QR token reuse | PASS | second use from a fresh phone identity refused: "That QR code was already used..."; nothing paired |
| *new* Pairing failure hint: nothing listening (QR address `10.0.2.2:1799`) | PASS | failure screen with Test connection; the check says "The computer refused: GeekBridge is not running there" (E2E-13: the headline above it is generic) |
| *new* Pairing failure hint: no answer (`10.0.2.77:1735`) | PASS (weak check) | Test connection ran and reported "Computer answers" failing; the assertion only looks for the word "answer" so confirm by screenshot `new-hint-noanswer-report.png` |
| *new* Nearby discovery | NOT TESTABLE | the emulator NAT drops UDP broadcasts |
| *new* Call ring visible on the desktop (`adb emu gsm call`) | PASS | `geekbridge-ctl calls` shows ringing; needs `consent DEVICE telephony on`, READ_PHONE_STATE + ANSWER_PHONE_CALLS, and the "Answer from computer" switch (written to prefs by the script) |
| *new* Call notification on the desktop | PASS | `notify-send --category=call.incoming` with actions (captured with a stub) |
| *new* Call reject over the link (`ctl call reject`) | PASS | modem back to idle |
| *new* Call answer over the link (`ctl call answer`) | PASS | modem off-hook |
| *new* Call hang up over the link | PASS | modem idle |
| *new* Call control refused without desktop consent | PASS | "nothing can answer a call right now", the call kept ringing |
| *new* Incoming SMS -> desktop notification (`adb emu sms send`) | PASS | "New message from <number>"; body not in the notification |
| *new* Incoming SMS in the desktop Messages view | PASS | |
| *new* Calendar event inserted on the phone shows in the desktop agenda | PASS | `content insert` into the Calendar provider; arrives via `agenda-refresh`; contacts list also present |
| *new* Diagnostics "Copy report" has no personal data | PASS | pasted back through the app: versions, permissions, counts only; no computer name, device ids, IPs, contact/SMS/calendar text |
| Self-update install (other agent), release dry run | not re-run here | see their sections below |

The call and phone-data scenarios use the emulator's modem and providers; they say nothing about real carriers or OEM
permission screens. The call rows exercise the link path (`geekbridge.calls.*`), not Bluetooth HFP (`scripts/emulate-call.sh` covers
that on the call emulator).

### Re-run after the Nothing OS reskin, 2026-11-02 (new tiles, dot-matrix status words, wizard, Home, QR, share sheet, Diagnostics)

Private set: AVD `gb36t` (copy of `gb36`, wiped; console 5624, adb server 16070), desktop state `target/perf-nothing/desk` (control 17701,
API 17700, discovery 17746, listen 1736, redirect 17661), Xvfb `:95`. APK built fresh from a scratch copy of `android/`
(`target/e2e-nothing-android`, native libs rebuilt from the same tree). Results: `dist/android/logs/e2e-nothing-results.tsv`,
`e2e-new-results.tsv`, `e2e-qr-results.tsv`; UX sweep screenshots `dist/screenshots/android/e2e-ux-*.png`
(plus the re-taken `e2e-14-paired.png`, `e2e-ux-diag-*`). Gradle unit tests and lint: green (`scripts/build-android.sh --skip-host-tests`).

| Scenario | 2026-11-02 | Notes |
| --- | --- | --- |
| Build APK (debug + R8 release), install | PASS | release 6.21 MB (budget 6.5 MB) |
| Setup wizard (9 steps), notification permission, battery Deny/Allow | PASS | selectors unchanged: step titles are still the headings |
| Pairing, reconnect, reconnect after restart, trust listed | PASS | |
| Clipboard desktop -> phone, phone -> desktop, locked-phone rule | PASS | |
| Clipboard 30 s window | **FAIL** (see E2E-17) | passed twice earlier the same day on the previous build, fails reproducibly on the last one |
| File desktop -> phone, text share, Share sheet -> desktop | PASS | needed harness fixes (below): stale `name (1).bin`, and `adb root` breaks the Share chooser's URI grant |
| Update check (mock server), Download and verify, Install handoff | PASS | harness now scrolls to the buttons (Diagnostics is taller) |
| `scripts/e2e/update-install.sh` desktop + Android (v0.1.0 -> v0.2.0, tamper/corrupt/no-sums, throttle) | PASS | 33 of 33 checks, one SKIP by design |
| `scripts/e2e-qr-pairing.sh` (expired, pair, reuse) | PASS | |
| `scripts/e2e-new.sh` (hints, call ring/reject/answer/hangup/consent, SMS, agenda, Copy report) | PASS | after E2E-18 (real bug, fixed) and the grants harness change |
| `scripts/android-perf.sh` smoke (size, setup, startup, reconnect x10, transfer 20 MB) | PASS | cold start median 830 ms of 9 (budget 1000; 765 ms before the reskin; 1.1-1.5 s while the host was at load 9-20), PSS growth 1.2 MB over 10 reconnects, fds/threads flat |
| UX sweep: Home / Diagnostics / QR pair / Setup wizard / Share sheet x light, dark, font 2.0, landscape, RTL | PASS after fixes | `scripts/e2e/ux-sweep.sh` + `a11y-audit.py`: no controls under 48dp, no unlabelled controls, nothing off-screen |

**Real regressions found and fixed**

* Light theme, right-edge scrollbar painted over the full-width Home tiles: the vertical scrollbar of the scrolling screens is off
  (`UiKit.Screen`, `MainActivity`).
* Red *text* below 4.5:1: the calendar month label used `gb_red` (#D71921: 4.76 on off-white but 4.05 on pure black, 4.27 on `gb_fill`).
  It now uses `gb_warn` (#C4141C light, #FF5A60 dark: 4.99 to 6.9 on every surface). `gb_red` stays for dots and rings only.
  Measured: text on surface 18.05 / 21.0, muted 5.27 / 7.56, `gb_warn` 5.56 / 6.88 (light / dark).
* Diagnostics still had the pre-reskin look and stale copy ("can only exchange a ping ... not yet tested", "no background service and
  nothing listens", "Send from computer (unavailable)", "not implemented"). Restyled with the same ink pill switches and hairline cards as
  Home, copy rewritten, and the ports / discovery / loopback / connect-by-address controls moved behind a collapsed **Developer options**
  expander (48dp header, announces expanded/collapsed). The harness opens it before the manual Connect.
* **E2E-18: any phone-data feature (contacts, calendar or SMS) stopped the background link from starting.** `MAX_FIELDS` in
  `crates/bridge-android/src/dispatch.rs` was 8, but a full `transport_start` (now with `permits`) has up to 11 keys, so the native core
  answered `invalid_request`; Home then sat at "Connecting..." and nothing listened on 1716 (Diagnostics' Start link worked only because
  it sends fewer keys). Limit raised to 16, regression test `widest_transport_start_request_is_not_refused_as_too_many_fields`.

**Open**

* **E2E-17 FAIL: clipboard 30 s window** (`scripts/e2e-android.sh --only clip_p2d`, stage `clip_p2d_window`). Repro: pair, Clipboard sync on,
  copy text in a system field, wait 40 s, `geekbridge-ctl clipboard paste-from-phone`, open the shade. Expected: "Nothing recent on the
  phone: copy again, then paste within the time window." within a few seconds. Seen: `paste-from-phone` prints nothing for at least
  7 s and there is no "Your computer wants your clipboard" notification (`e2e-21-clip-stale.png`); the "locked phone" refusal that follows
  works. Passed twice earlier the same day (before the last APK/native rebuild and before `e2e-new.sh` left call/SMS/calendar
  grants and notifications on the phone), so it is state- or timing-dependent; not root-caused, not obviously related to the reskin.

Harness changes: `scripts/e2e-android.sh` (scroll to Download/Install update, open Developer options before Connect, delete the old
`e2e-d2p-file*` on the phone), `scripts/e2e-new.sh` (`write_grants`: a newly paired computer holds no calls/contacts/calendar/SMS rights),
`scripts/e2e/update-install.sh` and `scripts/android-perf.sh` (the emulator, adb port, ports and directories are overridable, so they
can run beside other agents), new `scripts/e2e/a11y-audit.py` and `scripts/e2e/ux-sweep.sh`. Do not leave `adb root` on for the Share
stage: the chooser's URI grant then fails with a `SecurityException` ("Could not open this file").

### Incoming-call answering through the real desktop UI, 2026-11-02 (`scripts/e2e/call-ui.sh`)

Everything runs on a private `dbus-run-session` bus, a headless `sway` + real `mako`, a private Xvfb (`:97`; override with `E2E_XDISP`),
a private XDG tree, the emulator `gb36k` (console 5634, adb server 16080) and a desktop `geekbridge-service` paired with it (e2e topology,
telephony consent on, "Answer from computer" on). The emulator is rung with `adb emu gsm call`, the modem state is read from
`dumpsys telephony.registry` (`mCallState`: 2 = off-hook, 0 = idle). Results: `dist/android/logs/e2e-callui-results.tsv`.

| Check | Result |
| --- | --- |
| Notification arrives in mako with actions `answer`/Answer and `decline`/Decline (`dbus-monitor` on the Notify call) | PASS |
| `makoctl invoke answer` -> modem off-hook; `makoctl invoke decline` -> modem idle | PASS |
| Calls window: popup shown while ringing, red Decline and off-white Answer buttons | PASS |
| Click Answer (xdotool) -> modem off-hook; in-call panel with timer and Hang up | PASS |
| No HFP route: window says "Audio on the phone", service says "Answered on the phone. Call audio stays there until this computer is paired ... Bluetooth hands-free device" | PASS |
| Click Hang up in the panel -> modem idle | PASS |
| Second call, click Decline -> modem idle, popup gone | PASS |

13 of 13 PASS. Pictures: `dist/screenshots/desktop/call-popup-{idle,ringing,answered,declined}.png`, `call-notification-ringing.png`.

Notes and harness defects found (no product defect in the call UI or the action wiring):

* The Nothing reskin changed the popup (dot-matrix "INCOMING" headline OCR cannot read, off-white Answer pill, buttons at y 377, Hang up at
  (301,102)); the script's OCR/colour/coordinate probes were updated, and the popup is now recognised by its explanatory line.
* `scripts/screenshots-desktop-nothing.sh` hardcodes Xvfb `:94`, the old default of this script; two runs at once shared one display and
  clicked each other's windows. This script now defaults to `:97` (`E2E_XDISP`); the other script should pick its own.
* The in-call panel has no mute control: the link protocol has no mute message and audio is not routed here without HFP, so there
  is nothing to test. Not a defect.
* `/tmp` (tmpfs, 16 GB) was at 79 % with other projects' artifacts, and a Claude Code temp write once failed with EDQUOT; this script keeps its
  state in `target/` and only a tiny XDG runtime dir in `/tmp`.
* Checks: `cargo fmt --check`, `cargo clippy -p bridge-apps -p bridge-service --all-targets -D warnings`, `cargo test` on both: green.

## Not testable on an emulator

* UDP discovery / announce, mDNS-style "find each other automatically" (NAT drops broadcasts).
* Real Wi-Fi roaming, IP changes, sleep/Doze behaviour with a real radio; the battery step was only checked up to the
  system dialog and the Doze whitelist, not that the link survives a real overnight idle.
* Whether `PowerManager` exemption keeps `LinkService` alive for hours; OEM battery screens.
* Real clipboard-in-background restrictions on a physical device (the emulator's `google_apis` image behaves like
  stock Android 16, but vendors differ).
* Hyprland/GNOME/KDE global hotkey for paste-from-phone (the test uses `geekbridge-ctl clipboard paste-from-phone`).
* Interoperability with stock KDE Connect on a phone.
* Real GitHub TLS/redirect behaviour of the updaters and a release-keystore-signed, R8-minified APK upgrade (see the release-workflow list below).
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

### E2E-3 Phone stops listening after the computer unpairs it -- by design, see E2E-15
After `geekbridge-ctl unpair PHONE` the phone's link stops (no listener), and Home now says "No computer paired" with a
"Pair a computer" button.

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
   (the `SYN_SENT` capture above). **This fix was incomplete:** only a bound attempt that failed to *connect* fell back, and
   usually it connects and then dies. See E2E-14, which replaced it (plain route first, every route judged by its handshake).
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
Fixed 2026-11-02 (soak, `docs/soak-report.md` finding 2): the native core starts with clipboard sync off and nothing re-sent the saved
setting after the process was killed; `ClipboardRuntime` now pushes it once whenever its waiter starts.

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

### E2E-10 The link to the desktop drops without a visible cause during long idle runs -- Medium (NOT root-caused)
Three times in one session the desktop logged `link_lost: peer closed the connection` about 30 s to 3 min after a redial, with the
phone idle or the Settings app in front (e.g. during the 40 s "30 s window" wait). A 80 s idle watch with the app in the foreground,
and a 30 s screen-off watch, did **not** reproduce it. Repro attempt: redial, run the clipboard stages back to back (they open the
system Settings search field and tap notifications), watch `geekbridge-ctl events`. Next step: capture `adb logcat` of the app's
pid and `LinkService` state at the moment of the drop; check whether `ShareCoordinator`/`ActivityTracker` stops the link when the
foreground app changes (see E2E-1). Until then "Unlocked rule" cannot be confirmed in a full run.

### E2E-11 File transfers failed on the current tree -- High (desktop -> phone root-caused in E2E-14; port clash handled in the harness)
Desktop -> phone: `send-file` shows the offer dialog, **Accept** is tapped, the desktop stays at `Waiting 0/N`, and the phone never
opens a TCP connection to the desktop's payload listener (sampled `/proc/net/tcp` every 0.2 s for 12 s; the listener is reachable from
the phone: `nc 10.0.2.2 <port>` connects). The app's Accept path (`ShareCoordinator.accept`) deletes the destination file again, which
means `share_accept` returned an error or never ran; the app only reports it as a notification ("Could not start receiving"), which was
off in the scripted phone. Reproduced on two phones (battery-paired and QR-paired). Next step: log the `share_accept` result code.
Phone -> desktop: `tls_failed`, but this run shared the host with another emulator (`emulator-5594`) that owns the host redirects
`1739-1742`, so the desktop's payload dial reached the wrong phone. That run proves nothing about the code; repeat it when ports
1739-1742 are free (the redirect ports are fixed by the protocol, so two e2e emulators cannot share a host for file transfers).

### E2E-14 (fixed) Desktop -> phone payload: `Timeout(payload TLS handshake)` / `tls_failed` at `Waiting 0/N` -- was High
**Root cause.** `payload::dial` bound the receiver's socket to the control link's local address and accepted it as soon as it
connected. The control link arrives on `eth0` (`adb emu redir`, phone address `10.0.2.15`) while the default network is
Wi-Fi (`wlan0`, `10.0.2.18`). Android routes by network, not by source address, so the bound socket leaves through `wlan0`
with a source that network did not assign. That network's NAT answers the SYN, ACKs the ClientHello, closes its side towards
the desktop, and resets the phone. The phone's TCP sometimes ignores that reset (`seq 0` is out of window), so TLS sits idle
until the 4 s handshake timeout. The earlier fallback (E2E-5b) never ran, because the connect had succeeded.

**Evidence** (emulator `gb36d`, `tcpdump -i any` in the guest, 12 MB file, unfixed build):
```
wlan0 Out 10.0.2.15.38893 > 10.0.2.2.1743: [S]  seq 2362108599
wlan0 In  10.0.2.2.1743 > 10.0.2.15.38893: [S.] seq 3270720001 ack 2362108600     <- connect succeeds
wlan0 Out 10.0.2.15.38893 > 10.0.2.2.1743: [P.] seq 2362108600:2362108814         <- ClientHello
wlan0 In  10.0.2.2.1743 > 10.0.2.15.38893: [.]  ack 2362108814
wlan0 In  10.0.2.2.1743 > 10.0.2.15.38893: [R.] seq 0 ack 2362108814              <- 1.45 ms later, out of window: ignored
          (4.000 s of silence; /proc/net/tcp on the phone still says ESTABLISHED)
wlan0 Out 10.0.2.15.38893 > 10.0.2.2.1743: [F.]                                   <- rustls gave up
```
At the same moment the desktop logged the ClientHello (`22,3,1,0,209,...`, from `127.0.0.1`) and then `tls handshake eof`.
A native probe (`bind` + `connect` + 214 bytes, run as the app's UID, same destination, listener answers 1400 bytes):

| Network state | Source | Outcome |
| --- | --- | --- |
| Wi-Fi is default | bound `10.0.2.15` (eth0), what `dial` did | **15/15 fail:** 12 connection reset (`tls_failed`), 3 silence (`timeout`) |
| Wi-Fi is default | plain (OS picks `10.0.2.18`) | 15/15 reply |
| Wi-Fi is default | bound `10.0.2.18` (wlan0) | 5/5 reply |
| Wi-Fi off, default is eth0 | bound `10.0.2.15` | 8/8 reply |
| Wi-Fi off, default is eth0 | plain | 4/4 reply |

The two failing outcomes are the two codes the app showed ("secure channel failed" and "timed out").
**Inferred, not measured:** the 3 MB and 5 MB files that did complete ran while the bound attempt happened not to connect
(the `SYN_SENT` case, which did fall back) or while Wi-Fi was not the default network. No size dependence was found. Why the
NAT kills a foreign source is also not established (it is the emulator's, and its source was not read); the behaviour above is
what was measured.

**Candidates ruled out:**
* *TLS role inversion or ordering:* the same roles work on the plain route, and the desktop received a correct ClientHello.
* *Handshake timeout too short under load:* the RST comes 1.45 ms after the ClientHello, so nothing was slow. With 48 busy threads
  on this 24-thread host, three 2 MiB transfers took 1.07 s in total (`transfers_complete_on_a_saturated_host`). The timeout stays at 4 s.
* *Accept-loop starvation, pending-handshake limit:* the desktop accepted the connection and read its first bytes.
* *Backpressure or deadlock between control and payload tasks:* no bytes ever flow; 500 MiB passes both ways on loopback.
* *Half-open emulator redirect:* the payload socket does not use a redirect at all. "Control-link TLS over the same NAT works" was
  wrong: the control link is on `eth0`, the payload socket on `wlan0`.
* *Peer-IP filter:* the source the desktop saw was `127.0.0.1` on every route (listener log), which passes the filter.
* *Certificate pinning:* the desktop's error is an EOF, not a certificate failure, and the same certificates work on the plain route.
* *Stale "connected" link after a reinstall:* not reproduced. With a link up, `force-stop` and `install -r` both end the desktop's
  link within 2 s, and a redial then transfers normally, including without the "relaunch after the dial" the harness used to
  need. Those notes described this bug.
* *Noise on the listener:* something on this host connects to every newly bound port (`GET / HTTP/1.1`, then a 1.5 KB ClientHello).
  `ss -p` names the T3 Code app. It is rejected and harmless, and on a real LAN the peer-IP filter drops it first.

**Reproduction before the fix** (`crates/bridge-android/tests/payload_nat.rs`, real phone engine, real TLS): a redirect proxy gives
the control link the emulator's addressing, and a small NAT kills connections from the foreign source after their first data.
Unfixed code: `code "timeout" after 1 foreign-source dials` (silent mode) and `code "tls_failed"` (reset mode), 4/4 tests failing,
at any size. Fixed code: 4/4 pass with no foreign-source dial made. `GEEKBRIDGE_CHAOS=soak` runs 500 MiB both ways.

**Fix** (`bridge_kde::payload`): `dial_secure` tries the OS-chosen route first, then the bound address, then the plain route again with the
full connect budget. A route counts only once its TLS handshake finished. A pinned-certificate failure ends the search and is
now reported as `pin_mismatch`. Multi-homed hosts still work: the sender closes a connection from an address it does not accept,
so the bound attempt follows at once (`a_failing_route_moves_on_only_after_a_real_handshake_attempt`).

**On the emulator after the fix:** 100 MB and 500 MB both ways, SHA-256 equal; numbers in [android-perf.md](android-perf.md).
**Not verified:** a real phone whose control link is on a non-default network.

**Listeners after cancel or timeout: no leak found.** `PayloadListener::send` owns the socket, so it closes however the send ends.
Tests: `the_listener_is_closed_when_a_send_is_cancelled_or_times_out` and, through the daemon,
`cancel_and_unpair_close_the_payload_listener`. On the emulator the port closed after the 120 s accept timeout and after a transfer.
The 1743 + 1744 seen earlier were two failed offers inside their 120 s windows. An offer the phone never answers still holds its port for
those 120 s, because the protocol has no way to say "declined" or "failed".

### E2E-15 (fixed) Desktop unpair left the phone with `paired_count=1` and `LinkService` running -- was Medium
**Root cause.** The packet was never the problem: the desktop already sent `kdeconnect.pair {pair:false}` and the phone's engine already
removed its trust record. But the app learns about that only from a status snapshot, and nothing polls while the app is closed
(by design), so `AppState.recordPaired` never ran and the service kept going.
**Fix.** A new blocking action, `transport_wait`, wakes on any recorded event with no timer in between (bound 600 s); `LinkRuntime`
runs one thread on it and fetches one snapshot per change. The engine also cancels that peer's transfers on `{pair:false}`, which it did
only for a local unpair. This adds one parked thread to the process. Tests: `a_peer_unpairing_wakes_the_event_wait_and_ends_its_transfers`,
`stopping_the_transport_ends_the_event_wait`, `TransportWaitTest`, `eventsTheCoreReportsAreFetchedOnceAndAPeerUnpairIsSaid`; existing
desktop tests cover both directions on its side (`local_unpair_tells_the_phone_and_forgets_everything`, `the_phone_unpairing_removes_trust_and_the_link`).
**On the emulator:** desktop unpair with the app closed: `paired_count` 0, service records 1 -> 0, nothing listening on 1716. With Home open:
the same within 3 s and Home shows "UNPAIRED / No computer paired". The phone's own Unpair: service gone, desktop lists 0 paired.
**Limit:** the notice needs a live link. A computer that unpairs while the phone is unreachable cannot tell it, and the phone keeps its record
until it is removed there.

### E2E-16 (fixed) Every share after the first in a process showed the old result and sent nothing -- was High
`ShareActivity` renders the session view while `ShareCoordinator.items` is non-empty, and `items` was never cleared when a session ended.
`ShareCoordinator` lives as long as the process (which the background service keeps alive), so a later share from the system sheet showed
the previous result and never started. Seen on the emulator: the screen named the new file above the old item's error. The harness hid
this by force-stopping the app first. Fix: a new share clears a finished session (`forgetFinishedSession`).
Verified on the emulator (several shares in one process); it has no unit test, because the coordinator needs an Android context.

### E2E-12 (fixed in the harness) Home "Clipboard sync" row sits under the gesture bar
After the Home redesign `ui scrollto` leaves the row at the very bottom, where a tap lands on the navigation bar, so Clipboard sync
could not be switched on. `phone_clip_sync_on` now swipes once more before tapping.

### E2E-13 Pairing failure headline is generic when the port is closed -- Low
A QR address with nothing listening (connection refused) shows "The connection closed before pairing finished." The Test connection
check names the real cause ("not running there"), but the headline could already say so: `PairingHelp.explain` maps a refused
dial to `link_closed`. (`bridge_core::pairing_help` has the same mapping for the computer.) Not changed: the files are being edited.

## Self-update end to end (`scripts/e2e/update-install.sh`)

Both builds of the Android app were produced from scratch copies of `android/` with a patched `Cargo.toml` version
(`target/e2e-update/v0.1.0`, `v0.2.0`; `-Pgb.abis=x86_64`), signed by the same debug key (identical certificate
SHA-256 `4d711ab4...`), versionCode 1000 and 2000 (`aapt dump badging`, `apksigner verify`). The mock server is
`scripts/e2e/mock_release_server.py`, switchable while running through a mode file (`corrupt`, `tamper_sums`, `nosums`,
`tag=vX.Y.Z`). Own AVD `gb36u` (console 5624, adb server 16048), mock port 18891, desktop ports 17680/17681/17616/1930.
`scripts/e2e/update-install.sh [desktop|android|all]` runs everything below; results land in
`dist/android/logs/e2e-update-results.tsv`.

| Scenario | Result | Evidence |
| --- | --- | --- |
| Android: install v0.1.0, `appops set REQUEST_INSTALL_PACKAGES allow`, check -> "Version 0.2.0 is available" | PASS | |
| Android: Download and verify, Install update, package installer "Do you want to update this app?" -> Update | PASS | `dumpsys package`: versionName 0.2.0, versionCode 2000 after; app starts; check then says "up to date" |
| Android: data survived the upgrade | PASS | sha256 of every file under `files/`, `shared_prefs/` (setup-finished state + marker), `databases/` identical before/after, plus a marker file. A real desktop pairing was not repeated here (same private storage, but not exercised): run `scripts/e2e-android.sh --only pair` first if you want that proof |
| Android: hash mismatch (one flipped byte, genuine digest published) | PASS | "does not match the published SHA-256"; `cache/updates/` empty |
| Android: tampered `SHA256SUMS` (digest `000...0`) | PASS | same refusal, file deleted |
| Android: no `SHA256SUMS` in the release | PASS | offered, "publishes no SHA-256", no download button |
| Android: downgrade (`v0.0.9`) and same version (`v0.1.0`) not offered | PASS | "is up to date" |
| Android: 12 h throttle | PASS | last check now: 0 requests when Diagnostics opens; 13 h ago: 1; 11 h ago: 0; switch off: 0 |
| Android: "skip this version" | n/a | the Android app has no skip control (desktop only, documented in `docs/updates.md`) |
| Desktop: downgrade / same version not offered | PASS | `latest: v0.0.9 - you are up to date` |
| Desktop: tampered sums -> download refused, nothing left behind | PASS | "does not match the published SHA-256; it was deleted"; no file under the download root |
| Desktop: release without `SHA256SUMS` | PASS | "installer: ... no published SHA-256, so it will not be downloaded"; `update download` exits non-zero |
| Desktop: corrupted bytes after a good check -> refused and deleted | PASS | |
| Desktop: good download | PASS | dir mode 700, file mode 444, SHA-256 equals the published one; only the archive in the directory |
| Desktop: nothing executed | PASS | the test tarball holds an `install.sh` that would create a canary file; the canary never appears, nothing is extracted |
| Desktop: skip this version persists | PASS | `(skipped)` shown, `"skipped_version": "0.2.0"` in `update-state.json`, still skipped after a service restart; a newer v0.3.0 is offered again |
| Desktop: `update disable` persists across restart | PASS | |

### E2E-U1 Automatic update check runs only when the Diagnostics screen opens -- Low (docs fixed, code not changed)
`UpdateSection` is created by `MainActivity` (Diagnostics & about, not exported, opened from Home). Home, the launcher,
never constructs it, so "checks when the app opens" (docs) really meant "when Diagnostics opens"; a user who never
opens Diagnostics is never told about an update. `docs/updates.md` now says so. A fix (start a check from
`HomeActivity.onStart` using the same 12 h throttle and `UpdateChecker.forBuild`) was not made because the Home UI is
being edited by other agents; it is a ~10 line change.

### E2E-U2 (fixed) No way to run the desktop service against a mock release server -- Low
`bridge_update::Config.base_url` existed but `geekbridge-service` always used GitHub. `update_service()` now honours
`GEEKBRIDGE_UPDATE_BASE_URL` only for `http://127.0.0.1:` / `http://localhost:` URLs (the updater never contacts any other
plain-http host), `crates/bridge-service/src/lib.rs`.

### Notes
* `geekbridge-ctl update status` prints "automatic on" even when the service was started with
  `GEEKBRIDGE_NO_UPDATE_CHECK=1` (that variable only stops the scheduler; the setting is separate). Cosmetic.
* A manual desktop check within 30 s of the previous one is answered from the cache, so the script sleeps 31 s between
  checks that change the mock's answer (about 3 minutes for the desktop part).
* Test harness: the shared `/tmp` has a per-user quota that other agents fill (apksigner failed with "Disk quota
  exceeded"), so scratch files live under `target/e2e-update/`; with several emulators and Gradle builds running the host
  load reached 50+, uiautomator calls timed out and my emulator was killed three times; the script therefore polls for
  expected text rather than sleeping, and `adbui.py` honours `ADBUI_TIMEOUT`.

## Updater with real release artifacts (2026-11-02)
Full write-up, commands and the list of checks: [updater-e2e.md](updater-e2e.md). Two real versions of every desktop
package (installer tarball, `.deb`, Arch `.pkg.tar.zst`) and a debug-signed APK pair (versionCode 10099 / 10199, same key)
were built from scratch copies of this tree (0.1.1 bumped with `scripts/release.sh --skip-tests` in the copy), published
into a replica of GitHub's REST API / download hosts (`scripts/e2e/github_replica.py`, payload checked against the
`github/rest-api-description` schema by `scripts/e2e/validate_release_payload.py`), and the installed v0.1.0 service and the
v0.1.0 Android app were driven through check, offer, download, verification and install.

| Run | Result |
| --- | --- |
| `scripts/e2e/updater-real.sh all` (replica fidelity + installed v0.1.0 desktop service: auto check, 304, prerelease handling, offer, 5 download faults, verified download, manual install, state-dir survival, 404/403/429/502 states) | all PASS in the final run |
| `scripts/e2e/update-install.sh android` with `E2E_UPD_REPLICA` (real 4.7 MB APK pair; signer pre-check; "No public release found"; rate limit) | see the table in updater-e2e.md |

### E2E-R1 The Linux installer tarball outranked the documented update archive -- Medium (fixed)
With the real release asset set, `select_asset` tied `geekbridge-linux-x86_64.tar.gz` and
`geekbridge-0.1.1-linux-x86_64-installer.tar.gz` on extension and broke the tie alphabetically, so the offer pointed at
the installer tarball, contradicting `package-linux.sh`'s "deliberately differs" note and the documented naming. Mock
tests (one asset per platform) could not see it. Now the shortest (canonical) name wins; regression test
`real_release_asset_set_picks_the_canonical_archive`. See updater-e2e.md for the open product question.

### E2E-R2 (fixed) `release.sh` left `packaging/arch/PKGBUILD` at the old version -- Low
`build-local.sh` then refused ("pkgver != Cargo.toml version"). `release.sh` now bumps `pkgver` for stable versions.

### E2E-R3 (fixed) `ctl update status` said "unknown (no check has completed)" after a check that found no stable release -- Low

### Notes
* `0.1.1-test` is a semver prerelease: it can never be an update target (not offered by the app, not returned by GitHub's
  `/releases/latest`); the e2e uses 0.1.1 for the offer and `v0.1.1-test` as the negative case.
* The Android notes card shows release-body markdown literally.
* The Android card ignores a tap on **Download and verify** while a check is running (it still shows the previous
  result's text then); the script retries.

## Release workflow dry run (`scripts/e2e/release-dryrun.sh`)
`act` and `actionlint` are not installed. The script extracts each shell step from `.github/workflows/release.yml` with
PyYAML and runs it in a scratch directory: YAML structure and every action pinned to a commit SHA; the version gate
(accepts `v0.2.0`, `v0.3.0-rc.1`; refuses mismatching, no-`v`, two-part and four-part tags); the "tag is on main" check
against a throw-away bare repo (accepted on main, refused on a side branch); the Linux Package step with fake binaries
(and that each packaged binary is a workspace bin target); `scripts/package-linux.sh` (.deb + installer tarball; the
installer tarball name cannot be mistaken for the updater's `geekbridge-linux-x86_64.tar.gz`); the `SHA256SUMS` step
(`sha256sum -c` passes, two-space format the updaters parse, refuses a missing artifact); the Android signing step for
real (stable tag without keystore refused; prerelease debug-signed; a generated keystore through `zipalign -P 16` +
`apksigner`, certificate and versionCode 2000 checked). 31/31 pass.

Only verifiable on GitHub: the `GITHUB_*` context, `actions/*` behaviour and the pinned SHAs resolving, the Windows Inno
Setup and macOS `.dmg`/`zip` steps and runner toolchains, the real `ANDROID_KEYSTORE_*` secrets, `gh release create --draft`
followed by `--draft=false` (and that `releases/latest` never shows the draft), `--generate-notes`, `--verify-tag`,
the `contents: write` permission scoping, matrix `fail-fast`, artifact upload/download merge, the 60-minute timeouts,
GitHub's real redirects/TLS/ETag/rate-limit behaviour for the updaters, and upgrading from a release-signed, R8-minified
APK (the dry run only proves signing and alignment, and the e2e upgrade used debug-signed unminified builds).

## Pairing lifecycle and rename (`scripts/e2e/pair-lifecycle.sh`, 2026-11-02)

Private rig (AVD `gb36u`, adb server 16090, desktop control port 17591, own state dir and Xvfb, scratch APK build from the
tree of that day). Every scenario ran 3 times; "first attempt" means one pass through the pairing screens with no retry
and no manual unpair. Results file: `dist/android/logs/pair-lifecycle-results.tsv`, screenshots
`dist/screenshots/pairing/pair-*.png`.

| Scenario | What | Result 2026-11-02 |
| --- | --- | --- |
| A_qr | phone unpairs while the computer still trusts it, re-pair by QR link | PASS 3/3 first attempt (15-16 s) |
| A_code | same, re-pair by code comparison | SKIP: the phone must start the request, which needs UDP discovery (the emulator's NAT drops broadcasts). The phone-initiated request is exercised by A_qr; unit/integration tests cover the by-name path |
| B_qr | computer forgets the phone, phone still trusts it, re-pair by QR | PASS 3/3 (after E2E-P1/P2 fixes; failed before) |
| B_code | same by code (computer asks, phone confirms) | PASS 3/3 (about 45 s) |
| C_qr / C_code | both sides forget, re-pair | PASS 3/3 each |
| repeat | computer asks twice (second request on a new connection): only the new code is valid, the first challenge is refused as stale, approving the new one pairs | PASS 3/3 (same-connection replacement is covered by the `bridge-kde` `Machine` tests; docs/pairing.md) |
| rename_good | phone renamed in Settings, computer renamed with `ctl rename`; each name shows on the other side after reconnect; the computer name survives a service restart | PASS 3/3 |
| rename_hostile | computer: control characters and 40 characters refused, RTL marks stripped, `<b>` markup reduced to harmless text, emoji kept, name of only marks refused. Phone: over-long and control-character stored names fall back to the model name, RTL marks stripped, emoji kept, the Settings field is capped at 32 | PASS 3/3 |
| rename_notify | text shared from the renamed phone raises a desktop notification that names it; a hostile phone name arrives as plain text (`Evil bHi/b ＆ Co`) | PASS 3/3 |
| rename_report | the phone's Diagnostics "Copy report" and `geekbridge-ctl diagnose` hold neither custom name (the report prints only model, versions, permissions, counts; `diagnose` aliases devices `device-N`) | PASS 3/3 |

Harness note: under a host load average of 30-40 (other agents' emulators and builds) uiautomator timed out and systemui showed
an ANR dialog that blocked every tap; the script sets `hide_error_dialogs` and uses a 60 s uiautomator timeout. Those
failures were the host, not the product, and the affected scenarios were re-run to 3/3.

### E2E-P1 (fixed) A QR scan for a computer the phone remembers said "Paired" without telling the computer -- High

Computer forgets the phone (Forget in its app, or `ctl unpair`), phone still lists the computer. Scanning the new QR code
showed "Paired with X" at once, but nothing was sent (`QrPlan` returned `Paired` as soon as the phone's own store held the
device id). The computer never saw the request, so the phone believed it was paired and the computer did not (no events at
all on the computer). Fix: `QrPlan` now emits `QrStep.Forget` first (the scan means "pair again"), waits for the record to go
(5 s cap), dials, sends the request, and only reports `Paired` once trust was recorded by this scan. Tests:
`QrPairingTest.aRememberedComputerIsForgottenThenPairedForReal`, `aForgetThatNeverShowsDoesNotStallTheScan`.

### E2E-P2 (fixed) The QR screen stalled on "Connecting" after the phone forgot its last computer -- Medium

Forgetting the last paired computer stops the phone's link (E2E-3), and `QrPairActivity.tick` returned early while the link
was stopped, so with E2E-P1's forget step the scan never dialled. `tick` now restarts the link (`startLinkIfStopped`).

### E2E-P3 (fixed) Computer names kept invisible direction marks -- Low

`validate_device_name`/`sanitize_device_name` (`bridge-kde`) only stripped control characters, so a name with `U+202E` or
zero-width characters was accepted, stored, announced to phones, and shown by them (the phone app already stripped them
for its own name, and the notification path strips them on display). They are now removed on the computer too, for our
own name and for names that peers announce. Test: `identity::tests::names_lose_direction_marks_and_markup_stays_inert`.
Note: ZWJ (`U+200D`) is in that set, so a joined emoji sequence such as a family emoji is shown as its separate emoji.

### E2E-P4 Phone "Paired computers" keeps the name from pairing time -- Low (open)

After the computer is renamed, Home ("Connected to X", "To X") and the notifications show the new name after the next
connection, but Diagnostics > Paired computers (button "Unpair X...") and `geekbridge-ctl status` `paired[].name` keep the name from
pairing time on both sides: the trust record is written once, at pairing. Refreshing it on a trusted connect needs a
`TrustStore` update that cannot resurrect a record the user just forgot, so it was not changed in this pass.

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
scripts/e2e/pair-lifecycle.sh [--repeat N] [A_qr B_code C_qr repeat rename ...]   # pairing lifecycle + rename, private rig
scripts/e2e-qr-pairing.sh                     # QR expiry / pairing / reuse (needs env stage first)
scripts/e2e-new.sh                           # hints, call answer/reject, SMS notification, agenda, Copy report
scripts/e2e/update-install.sh all             # self-update, desktop + Android (needs target/e2e-update/gb-0.{1,2}.0.apk)
scripts/e2e/release-dryrun.sh                 # release.yml shell steps in a scratch dir
```

Needs the `.tools` toolchain (`scripts/bootstrap-android.sh`), AVD `gb36`, `xclip`, `Xvfb`, `rsync`, `python3`.
File transfers to the desktop write to the real `~/Downloads/GeekBridge`; the script removes what it creates.

## Android state survives an APK upgrade (2026-11-02)

`E2E_UPD_REBUILD=1 scripts/e2e/update-install.sh android` (emulator gb36u, API 36; v0.1.0 and v0.2.0 both built from the
current working tree, so both contain `StateMigrations`). Before the upgrade the script sets: clipboard sync on, the device
renamed to "Upgrade Test Phone", a paired computer (a real v1 `files/kde/trusted_peers.json` with a throwaway certificate, plus
its `geekbridge_state` / `geekbridge_grants` contacts opt-in / `share_policy` auto-accept entries), and pins the Quick actions
widget. It then deletes `gb_state.xml` so the data looks like it came from a build that predates versioning.

Result: 26 of 26 checks PASS (one SKIP by design). After the upgrade through the real package installer: clipboard sync still on,
name kept, pairing, the per-computer opt-in and the auto-accept kept (the app re-derives "paired" from the trust file and would
have dropped the opt-ins otherwise), the widget still registered, `gb_state.xml` now holds `schema_version=1`, and the file
hashes of `files/` and the other `shared_prefs/` were unchanged before the first start of the new build.

Harness lessons: two copies of the script on one emulator interleave their taps (a toggled clipboard switch, a reinstall that
removed the widget) and produce false failures; and `adb shell run-as pkg sh -c "a && b"` runs `b` as the shell user, so chains
need the single-quote form `adb shell "run-as pkg sh -c 'a && b'"`.
