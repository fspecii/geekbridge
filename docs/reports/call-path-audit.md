# Phone calls from a PC without Bluetooth: call path audit

Date: 2026-11-03. Scope: the whole call path when the computer has no Bluetooth controller (the owner's PC: the real
probe says `verdict: no_bluetooth_controller`, `bluetooth_controllers: 0`), plus a "dial from the PC" feature that did
not exist.

## Verdict

Control of calls from the PC does not need Bluetooth, and now covers all four actions: answer, decline, hang up and
**dial**. Only the voice needs Bluetooth; without a controller it stays on the phone, and the UI, `geekbridge-ctl calls`
and `geekbridge-ctl doctor` now say so in words instead of printing a raw verdict.

## The path, step by step

| Step | Where | State before | State now |
| --- | --- | --- | --- |
| Phone permission | `AndroidManifest.xml`, `CallControl.kt` (`ANSWER_PHONE_CALLS`, `READ_PHONE_STATE`, `READ_CALL_LOG`) | Answer/decline/hang up; capability flags `canAnswer`/`canEnd` honest by API level (answer 26+, end 28+) | Added optional `CALL_PHONE` and flag `canDial`; its own button in the "Answer calls" setup step |
| Phone state | `CallStateMonitor` -> `CallControlEngine.onState` -> `transport_calls_report` -> `geekbridge.calls.state` | idle / ringing / offhook, number+name only with the extra permissions, opt-in switch off by default | unchanged, plus `canDial` |
| Consent on the PC | `Feature::Telephony`, per device, off by default (`gate.rs`, `CallHub::on_packet`) | reports ignored and nothing sent without it | same; dial is offered only through the same gate |
| Merge into one view | `bridge_core::calls_view::build` | rows with `answer/reject/hangup` targets, audio route | new `CallsView.dial` (`DialView`): first source whose own pre-flight accepts a dial, HFP first, only when no call is going on |
| Ringing / active / ended | `LinkCallReport::apply` | ringing -> Incoming, offhook -> Active, idle -> `CallEnded` | unchanged. Limit: an outgoing call placed over the link shows as `Active` with unknown direction (Android's coarse state has no "dialing") |
| PC call window | `geekbridge-calls` (`call_ui.rs`, `apps/calls.rs`) | popup (Answer/Decline), in-call panel (Hang up, mute only if real); keypad Call button was **inert** (empty click handler) | Call button opens a confirmation sheet, only its Call button sends `CallDial`; button disabled with a plain reason (no consent / phone has not allowed / call going on / `*#` digits) |
| CLI | `geekbridge-ctl calls`, `call answer|reject|hangup|mute|audio` | no dial | `call dial NUMBER [--yes]`; asks `[y/N]` on a terminal, refuses without `--yes` otherwise; prints why dialing is unavailable (consent / phone permission / call going on) |
| Service | `CallHub::dial` | none | re-checks the number and that the source is still the offered dial target before anything leaves; request ids and pending cap as for other actions |
| Phone execution | `bridge-android` `engine/calls.rs`, `CallRules.handle` | answer/reject/hangup | `dial` event carries the number; Kotlin checks `CALL_PHONE`, an **idle** phone and `+?[0-9]{1,20}`, then `TelecomManager.placeCall(tel:)` |
| No-Bluetooth explanation | `bridge_calls::readiness`, `CallsStatus.sources`, `doctor` | the readiness line existed but only in the Recents tab sources list; doctor printed `no_bluetooth_controller` verbatim | new `CallsStatus.audio_note` shown under the keypad and in Recents; doctor `Calls` check says control works over the phone link, audio stays on the phone, a USB adapter would bring it here |

## Safety rules for dialing

- Telephony consent per device (off by default) and the phone's `CALL_PHONE` both required; nothing is offered or sent otherwise.
- Digits with an optional leading `+`, at most 20 digits. `*` and `#` are refused everywhere (PC, wire parser, Android), so a
  request cannot be a USSD/MMI/supplementary-service code, and the number goes into a `tel:` URI built from parts.
- Confirmation before sending: modal sheet in the window, `[y/N]` or `--yes` in the CLI. The service never dials on its own.
- Only from an idle phone; a second call is never placed on top of a ringing or live one.
- Numbers are `Sensitive` (redacted `Debug`), the CLI masks them in output.
- Emergency numbers are ordinary digits and are not blocked: the confirmation is the only guard. Say so if you want them
  refused on this path.

## Tests (all Rust, against the stand-in phone, nothing dialed anywhere)

- `bridge-core`: wire parser (plain numbers only, malformed/USSD refused, dial without a number is malformed), capabilities,
  view offers dial only to an idle consenting phone.
- `bridge-service/tests/calls_api.rs` (real `Daemon`, loopback TLS stand-in phone): no consent -> nothing offered and nothing
  sent; consent but `canDial=false` -> reason in the sources, nothing sent; permitted -> bad/USSD numbers `BadRequest`, wrong
  source `NotFound`, exact number arrives as a `dial` request; live call -> no dial; phone refusal shown to the user.
  Also `geekbridge-ctl call dial` end to end (private state dir/port/HOME): explains without consent, rejects `*#06#`, refuses
  without `--yes` and no terminal, dials with `--yes`.
- `bridge-android/tests/calls_transport.rs`: `canDial` reported, dial becomes an event with the plain number, USSD never does.
- `bridge-apps`: dial request/blocker/confirmation text; `doctor::calls_check` for `no_bluetooth_controller`.
- Kotlin `CallControlTest.dialNeedsPermissionAnIdlePhoneAndAPlainNumber` was written but **not run** (see below).
- The HFP path (`bridge-phone-sim`, scenario with `Dial` -> `dialing` -> active) already existed and still passes; it needs
  Bluetooth only in the real world, not in the simulator.

Commands (TMPDIR=/home/ambsd/.cache/gb-tmp): `cargo fmt` on the touched files (exit 0), `cargo clippy --workspace
--exclude bridge-fuzz --all-targets -- -D warnings` (exit 0), `cargo test -p bridge-core -p bridge-calls -p bridge-android
-p bridge-apps -p bridge-service -p bridge-phone-sim` (exit 0). `bridge-fuzz` was excluded from clippy because another
in-progress change in the working tree (not part of this work) currently fails it with two lints.

## Not verified

- **Android build and Kotlin tests did not run**: `./gradlew --offline` cannot resolve the Android Gradle plugin here. The
  Kotlin, manifest and setup-screen changes are reviewed by eye only.
- No emulator run: `scripts/e2e/call-ui.sh` needs the debug APK, which could not be built. A `dial` stage there (emulator
  `gsm` network, not a real one) is the natural next check once the APK builds.
- No real phone, no real call, no audio. Android's `placeCall` behaviour across OEMs is untested.
- HFP dial on real Bluetooth is only "declared" capability (see `docs/call-control.md`).
- `CHANGELOG.md` was not touched (it has another change in flight).
