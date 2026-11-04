# Call audio setup UX: hear and talk through the computer (all platforms)

Date: 2026-11-04. Branch `call-audio-setup-ux`, based on origin/main `c8d2d00de`. Scope: the user-facing guided experience and the non-Linux
platforms. The Linux routing internals in `crates/bridge-calls` (another agent) were not touched; this work uses only
`bridge_calls::readiness::{count_adapters, parse_devices, info_is_hfp_gateway}` and `bridge_calls::probe::run` (public).

**Nothing here carries real call audio.** This machine has no Bluetooth controller, so adapter, pairing, hands-free and the
voice itself are exercised only through canned data and stand-in programs. See "Verified and not verified".

## What was built

### Desktop
* `crates/bridge-core/src/call_audio.rs`: one platform-neutral model (no I/O). Ordered steps (adapter, unblocked, powered, paired,
  hands-free, headphones and microphone), each Ok / Todo / Waiting / Unknown, a one-line `FixKind` and an optional command to copy;
  Linux and Windows builders; parsers for `bluetoothctl show|info`, rfkill files, `pactl --format=json list sinks|sources` and
  PowerShell `Get-PnpDevice | ConvertTo-Json` (single object, array, empty, BOM); the test tone (a gentle 1.2 s two-note WAV) and the
  level meter maths. 22 unit tests, including "no command uses sudo except the one copy-only line".
* IPC: `CallAudioSetup`, `CallAudioChoose`, `CallAudioTest` requests, one `call_audio_setup` response (covered by the existing
  every-variant wire test). The daemon answers on a blocking thread with a 15 s bound; without the service option (tests) it refuses.
* `crates/bridge-service/src/call_audio.rs`: the host. Linux: `/sys/class/bluetooth`, `/sys/class/rfkill`, `bluetoothctl`, the
  existing calling probe (phones registered), `pactl`. Windows: `powershell.exe Get-PnpDevice -Class Bluetooth|AudioEndpoint`.
  macOS: fixed fallback. The choice of headphones and microphone is validated against what the host lists and saved in
  `call-audio.json` (0600) in the state directory. The test runs on its own thread: tone through `paplay` (or `pw-play`), then
  5 s of `parec` metered at 10 Hz; nothing is recorded or kept. Tested with stand-in programs (success, missing, failing, silent).
* `geekbridge-ctl call-audio [test | use --output N --input N]` prints the checklist; `geekbridge-ctl doctor` gets OS-specific call
  audio guidance for Windows and macOS.
* Window: `crates/bridge-apps/src/call_audio_ui.rs`, shown in Settings (a section after "This computer") and on the Calls window's
  Recents tab (scrollable now). State is words, not colour; a failing step shows the one-line fix and **Copy command**; **Test call
  audio** with a live meter and a polite live-region sentence; device pickers; the no-Bluetooth block says "This computer has no
  Bluetooth. Add a USB Bluetooth adapter to talk through this computer's headset; until then you can answer here and talk on the
  phone", names what to buy (kernel `btusb`, Bluetooth 5.x) and opens the guide. A11y names for every control and the meter. The
  worker polls only while the panel is drawn (3 s; 150 ms during a test).
* i18n: 70 keys in `crates/bridge-i18n/i18n/en.properties` (`callaudio.*`); the catalogue test also reads the keys named inside
  the model (`FixKind::key`, `StepId::key`).
* Goldens (light/dark, std/min): `settings` (now with the no-Bluetooth fixture, so a machine's real Bluetooth cannot change it),
  `settings-call-audio`, `-test`, `-nobt`, `-mac`, and `calls-audio-setup`. Env hooks for screenshots only:
  `GEEKBRIDGE_CALLAUDIO_FIXTURE`, `GEEKBRIDGE_OPEN_CALLAUDIO`, `GEEKBRIDGE_CALLS_TAB`.

### Android
* **Answer calls** step gets "Call audio on your computer": explanation (on the computer add device, choose this phone; on the
  phone allow Phone calls, One UI: Bluetooth, gear, Call audio on), the button **Pair phone with your computer for call audio**
  (`Settings.ACTION_BLUETOOTH_SETTINGS`) and a status line from the HEADSET profile proxy (`getConnectedDevices`): connected
  (with names, on screen only), none, Bluetooth off, no Bluetooth, needs permission, unknown.
* `BLUETOOTH_CONNECT` (runtime, Android 12+) is requested only by **Allow Bluetooth check** with its reason on screen; `BLUETOOTH`
  is declared up to API 30. No scanning, pairing or connecting. `CallAudioModel` is pure and JVM-tested (5 tests).

### Windows and macOS
* Windows: detection by `Get-PnpDevice` with fixture-tested parsers; guidance in the window, `doctor` and the guide. The radio's
  on/off state and the default communications device are not read and are shown as "Cannot be checked from here". No local test
  (Windows owns routing).
* macOS: no checklist, never green. Honest wording below.

### Docs
`docs/call-audio-options.md` rewritten as "Hear and talk through your computer" with the per-OS decision table; the user guide
section, README feature row, `privacy.md` permission row and `android-calls.md` updated.

## Owner decision update: Bluetooth only, banner, hotplug card
* No-adapter **warning banner** (non-blocking, a11y name "Warning: no Bluetooth on this computer") with the owner's text: "This
  computer has no Bluetooth. You can see and answer calls here, but talking needs Bluetooth (add a USB adapter). Messages,
  contacts and the other features work normally." Shown in the Calls window (keypad and Recents), on the Home Calls tile
  (short form "No Bluetooth: talking needs an adapter") and in `geekbridge-ctl doctor`. Goldens `calls-keypad-no-bluetooth` and
  `home-no-bluetooth` (light/dark, std/min). The Calls keypad tab now scrolls so the banner is never clipped.
* **Hotplug card**: when the checklist goes from "no adapter" to "adapter found" while a window is open, the Calls window shows
  "Bluetooth adapter found. Set up call audio." with a button to the checklist (polling is 3 s; no udev watcher).
* Quickstart for the dongle day added to `docs/call-audio-options.md`.
* **Not done**: (a) the tray tooltip warning and `geekbridge-ctl calling` text (the tray status line is shared with the update and
  phone logic; left alone to avoid conflicts); (b) the seamless "single primary button that pairs, trusts and connects": the
  readiness/progress API from the call-backend agent was not available on origin/main when I built this, so the window shows live
  steps and fixes but does not drive pairing itself. When that API lands the primary button should call it and the steps auto-advance;
  the model (`Step`, `StepState`, `Fix`) is ready for it. Android needs no change (the phone cannot know the PC has no Bluetooth).

## Correction to the brief, macOS
The brief says macOS does not expose HFP-HF to third-party apps. The project's macOS research notes (which cite Apple's
documentation) say otherwise: Apple publishes `IOBluetoothHandsFreeDevice` (`connectSCO`, `transferAudioToComputer`). What is
true: Continuity calling is Apple-to-Apple only; no source shows a Mac carrying an Android phone's call voice; whether macOS
exposes the voice link as a CoreAudio device is undocumented; the only wrapper found has no hardware test. So the documents say
"not available yet, unverified" rather than "impossible", and the fallback is the same: answer on the Mac, talk on the phone, or
use a Bluetooth headset paired with the phone. The research notes themselves are private and are not linked from public docs.

## Verified and not verified
| Claim | Evidence |
| --- | --- |
| Checklist decisions, parsers, tone, meter, selection store, CLI text, wire format | Unit and service tests (all pass) |
| `geekbridge-ctl call-audio` against a real sandbox service on this machine | Ran: reports no Bluetooth, lists the real headset, refuses an unlisted device |
| Test tone and meter against real devices | **Not run** (would play and record on the owner's headset); verified with stand-in programs; `paplay`/`parec` flag syntax accepted by the installed tools |
| Linux adapter, rfkill, powered, pairing, hands-free steps | Only through canned output; no controller here |
| Windows detection | Parsers on canned output only. Instance-id shapes (`BTHENUM\{0000111F-...}`, `{0.0.0.`/`{0.0.1.`) are from general knowledge, never run on Windows |
| Android Bluetooth status on a phone | JVM model tests, Gradle unit tests and lint pass; **not run on a device** |
| Bluetooth voice, codecs, echo, any OS | **Not verified anywhere** |

## Checks run
cargo fmt --check, `clippy --workspace --all-targets -D warnings` (exit 0), `cargo test --workspace` (all pass except the items
below), Gradle `testDebugUnitTest lintDebug` (exit 0). Goldens updated in this worktree with `scripts/golden-desktop.sh --update`.

## Found on the way
* `apps::calendar::render_tests::stepping_and_jumping_move_the_shown_day_in_every_mode` fails on 2026-11-04 (a 27-day "month" step
  in `calendar_tests.rs:198`): date-dependent, unrelated to this change, skipped in my run.
* `parec --device=<unknown name>` silently records from the default source instead of failing. The service only passes names the
  host listed, but anything that passes an arbitrary name to `parec` would capture the default microphone.

## For the routing agent
The chosen headphones and microphone are stored as `bridge_core::call_audio::Selection` (`call-audio.json` in the state directory:
server device names, `null` = system default). `RouteRequest.local_output/input` can read it (`LocalSelection::Named` or
`DefaultAtPrepare`). Nothing reads it yet; the setup window only saves it.

## Open items
* Real hardware: a USB Bluetooth adapter on the Linux machine, then the full checklist, the tone/meter test and a call.
* Windows: run the PowerShell queries on a real PC and confirm the node shapes and `Status` meaning; read the default communications device.
* A real phone for the Android part (One UI wording of the "Call audio" switch is from the brief, not seen on the device).
