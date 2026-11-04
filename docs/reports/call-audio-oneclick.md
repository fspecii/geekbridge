# Call audio: one button (Linux, Bluetooth hands-free)

Date: 2026-11-04. Joins two pieces of work: the setup checklist UI (`call-audio-setup-ux.md`) and the call-audio backend
(`call-audio-pc-headset.md`: BlueZ pairing agent, trust, hands-free connect, WirePlumber fix, routing). The backend landed
on origin/main first (commit `4c196e52b`), so this is built on its real API; the UI-side fake was still needed for tests and
is documented below. Sandbox only: nothing here touched a live service, real config, Hyprland or a Bluetooth stack (this PC
has no controller).

## What the person sees

Settings, "Call audio on this computer" (and the Calls window's Recents tab) now start with one card and one primary button,
**Set up call audio**. It walks seven steps and moves on by itself; the card says "Step n of 7" in words (and a segmented bar):

| # | Stage | What the window says | Moves on when |
| - | - | - | - |
| 1 | `NoAdapter` | "Waiting for a Bluetooth adapter. Plug one in and the setup continues by itself." (+ what to buy) | the service reports an adapter (hotplug) |
| 2 | `PowerOn` | "Turning Bluetooth on..." | the service is past powering on |
| 3 | `Visible` | "This computer is visible to phones for 2 minutes. On your phone, open Bluetooth settings, tap this computer's name and accept the pairing request that appears." | the pairing code arrives |
| 4 | `Confirm` | the six-digit code, large, "Does your phone show this code? If it does, accept on the phone, then press Yes here." **Yes, same code** / **No, different** | the person answers |
| 5 | `Connecting` | "Pairing, trusting and connecting hands-free..." (WirePlumber user fix is sent once if the checklist says the config is broken) | paired + connected + hands-free are all ok |
| 6 | `Devices` | headphones and microphone pickers, **Use these devices** (waits for the person; nothing is guessed) | pressed |
| 7 | `Testing` | tone, then the live microphone meter | the test finishes and the microphone heard something |
| - | `Done` | "Call audio is ready. Answer a call on this computer and you talk through your headset." | |

**Cancel** is on every running step (it sends `Cancel` to the service, which stops the pairing between steps; nothing already
paired is removed). Unplugging the adapter mid-run cancels and goes back to step 1, waiting. Failures show their own words (the
service's sentence for service failures), one fix line chosen from the kind, and **Try again**; a silent or unopenable
microphone and a failed tone also offer **Test again** without redoing the pairing. Fix lines: start the Bluetooth service,
unblock Bluetooth, turn on Bluetooth on the phone, turn on Phone calls for this computer on the phone, choose devices, check the
microphone is not muted, update GeekBridge (service too old).

When a phone is already set up, the same place shows "Phone set up for call audio: NAME" with **Set up again** and **Forget
Bluetooth phone** (asks once: it unpairs the phone from this computer's Bluetooth, removes it from `call-audio-phone.json`; the
phone's own Bluetooth list is not touched). With no adapter and nothing pressed the card is not drawn (the existing banner,
buy hint and guide stay).

Tray tooltip, second line: **"No Bluetooth: calls can't carry voice"** when no adapter, **"Call audio ready"** when an adapter
exists and a phone is set up; nothing (as before) when an adapter exists but nothing is set up, and on Windows/macOS. It is
file-only (`/sys/class/bluetooth` and `call-audio-phone.json`), so the tray poll stays cheap.

`geekbridge-ctl calling` opens with one line: `Call audio: NO BLUETOOTH. Calls can't carry voice on this computer; add a USB
Bluetooth adapter ...`, `Call audio: ready (phone: X).`, `Call audio: set up, but not working yet...` or `Call audio: not set
up. One step: press "Set up call audio" in Settings, or run geekbridge-ctl call setup.` (also when the service is not running).
`geekbridge-ctl call setup --forget` is the terminal twin of Forget.

## Code

* `crates/bridge-core/src/call_audio_flow.rs`: the state machine. No I/O, no clock; the caller passes milliseconds and an
  `Observation { hear: Option<HearCalls>, setup: Option<Setup> }`; it returns `Act`s (`Pair(CallAudioSetupAction)`, `Choose`,
  `Test`). `Flow::{start, cancel, confirm, use_devices, retest, advance}`, `Stage`, `Failure`, `FixHint`. `trait Backend`
  (`observe`, `act`) is the seam to the service and `FakeBackend` (scripted `Beat`s: Working, Confirm(code), Done,
  DoneUnlinked, Fail, Heard) is the fake used by every test and by the worker tests in the window crate. **Integration points
  with the backend** are spelled only in `call_audio_flow::ids`: phases `idle|working|confirm|done|failed` of
  `HearCalls.setup.phase`, step ids `powered|paired|connected|hfp|wireplumber`, states `ok|fail`.
* `crates/bridge-apps/src/call_audio_ui.rs`: `ServiceBackend` (`CallAudioSetup` + `HearCalls` per poll; a service that does not
  know `HearCalls` gives `hear = None` and the one-button card is not offered), the worker (runs `Flow`, polls every 0.8 s while a
  setup runs, 3 s otherwise, 150 ms during the test; never idles while a setup is running), the card, `fixture_snapshot` for
  screenshots (`GEEKBRIDGE_CALLAUDIO_FIXTURE=flow-idle|flow-noadapter|flow-visible|flow-confirm|flow-connecting|flow-devices|
  flow-testing|flow-done|flow-failed`; the older names now also carry a step-level view).
* New IPC action `CallAudioSetupAction::Forget` (additive; `bridge_calls::setup::forget` = BlueZ `RemoveDevice`,
  `clear_remembered`; the service refuses it while a setup runs and forgets locally even if BlueZ cannot be reached, so a
  vanished adapter cannot keep a phone "remembered").
* `HearCalls::banner()`, `ipc::banner_line`, tray `CallAudioHint`.
* i18n: 42 `callaudio.flow.*` keys, 8 `callaudio.a11y.*` keys, `tray.call_audio_none`, `tray.call_audio_ready`.
* Accessibility names: the card sentence is a polite live region named "Call audio setup: ..."; the code is named "Pairing code
  N. Compare with the phone"; the progress bar is named "Step n of 7"; every button has a name that says what it does (start,
  cancel, same code / different code, use devices, test again, forget, yes forget, keep).

## Tests

* `call_audio_flow` (16): the whole path with one press, one confirmation and one "use devices"; step order monotonic 2..7;
  waits for an adapter and starts when it appears; unplug mid-run; cancel; refused code; each failure kind and its fix; hands-free
  link that never comes up (20 s grace); a service that never starts (8 s); WirePlumber fix sent exactly once; silent microphone
  then retest; a stale test result is not taken for the new one; old service = "update"; out-of-turn events ignored.
* `call_audio_ui` worker tests against `FakeBackend`: Start through Done on the real worker thread, Cancel, Choose, every flow
  fixture draws in egui, every failure and hint has catalogue words.
* tray model/collect hint, `HearCalls::banner`, `clear_remembered`, wire test for the new action (existing every-variant tests).
* `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p bridge-core -p bridge-apps
  -p bridge-calls -p bridge-service -p bridge-fuzz`: exit 0 (see the final section for the full-workspace run and CI).

## Goldens

New screens (light/dark, std/min), in a separate worktree: `settings-call-audio-{noadapter,visible,confirm,devices,testing,done,
failed}`, `calls-audio-confirm`. Existing screens that draw the panel with the `ready`/`connect` fixtures changed (the card, and
for `ready` the phone line with Set up again / Forget). Reviewed by eye; see the commit.

## Not verified

* Everything against real BlueZ, a real phone and a real adapter: the sequence above is tested against the scripted fake, and the
  backend's own sandbox (`scripts/e2e/call-audio-sandbox.sh`) tests the service half. The first real run is the owner's test
  tomorrow.
* The wording of the phone's prompt differs by phone (One UI: "Pair with this device?" with the same code). The window says
  "accept the pairing request that appears".
* The "2 minutes" visible time is the service's `discoverable_for`; the window does not count it down.
* Service-failure details are English sentences from the service and are shown as-is (not in the catalogue).
* `HearCalls` costs the service some probing per call (D-Bus, pactl); the window now asks it with every checklist poll (every 3 s
  while the panel is drawn, 0.8 s during a setup). If that proves heavy on real hardware, ask for it every other poll.
* Windows/macOS: no guided setup (the service refuses `CallAudioPair`); the card is not shown because the step-level view is empty
  there.
