# Android runtime verification (emulator) and UI notes

Status of what was **actually run** for the Android companion, what was not, and what is left. Everything
here is a debug-signed development build; there is no real-phone testing.

> **Historical record (first emulator pass, 2026-11-01).** Statements below such as "ping only", "Discovery was not
> run" or "no network" describe that early build. Pairing, discovery, sharing, clipboard, SMS, calendar, calls and
> self-update have since been run on the emulator; see [e2e-findings.md](e2e-findings.md) for the current record.

## Environment (all project-local)

- Toolchain: `source scripts/android-env.sh` (JDK 17, Gradle 8.13, SDK 36, NDK 30, Rust 1.99.1) under `.tools/`.
- Emulator: `scripts/bootstrap-android.sh --with-emulator` installed `emulator` 37.2 and
  `system-images;android-36;google_apis;x86_64`. AVD `gb36` (pixel_6, 1080x2400, 2 GiB RAM, 2 cores) lives in
  `.tools/android-user/avd/`.
- Isolated adb: every adb call used `ANDROID_ADB_SERVER_PORT=15037` and `-s emulator-5594`; no other device was
  touched. The emulator was started headless: `-no-window -no-audio -no-snapshot -gpu swangle_indirect
  -memory 2048 -cores 2` (`-gpu swiftshader_indirect` crashed more often; see Emulator stability).
- All emulator processes, the private adb server and the test peer were stopped when this work ended.
- Test data: the emulator image is empty. Three synthetic contacts (`GB Test Alpha/Beta/Gamma`) and later a
  synthetic calendar with two events were inserted through `adb shell content`; screenshots that show them say so.
  Nothing real was read, no call was made, no SMS was sent, the clipboard was only read by tapping Paste.

## Build and tests (final tree, `scripts/build-android.sh`)

- Host `cargo test -p bridge-android`: pass. Both ABIs built, ELF/16 KiB alignment/JNI symbol checks pass.
- `testDebugUnitTest`: **174 tests, 0 failures** (this includes tests other agents added in `android/**`).
- `lintDebug`: passed in the final build (exit 0). One Gradle run failed earlier only because the test-executor JVM
  timed out connecting back to Gradle while the host was overloaded (`attempt-testexecutor-timeout` log); a rerun passed.
- APK `dist/android/geekbridge-debug.apk` sha256 `8069127533347ff2af4314c78ff7c057516cd20705d46ce699a1bc69cd89f77d`,
  both ABIs. Native libs (this build): arm64 `cfd49672…01de47f`, x86_64 `ccc6b5ca…3f1a`.
  Full hashes: `dist/android/logs/provenance-sha256.txt`.
- **Provenance caveat.** The Rust core (`crates/bridge-android`) and files in `android/**` were changed by other
  agents while this ran (transport actions, call state in `CallState.kt`/`TelephonyRegistrar.kt`, share transfers in
  `Share.kt` and `Transport.kt`, `UpdateModel.kt`). The final APK contains whatever compiled at build time. Behaviour
  below was verified on earlier builds; the table says which.

## Verified on the emulator (ART, Android 16 / API 36, x86_64)

| Check | Result | Build |
| --- | --- | --- |
| App installs, launches, no crash | yes | all |
| Native core loads under ART and answers `version` (`core_version 0.1.0`, limits, actions) | yes: status shows "Native core: available" | all |
| Clipboard: empty text rejected by the core (`empty_text`); typed text accepted ("18 bytes … Not sent") | yes | `35ef6e57` |
| Clipboard: Copy then Clear then Paste round-trips; Paste only reads when tapped | yes | `35ef6e57` |
| SMS validation: valid draft accepted; bad recipient rejected (`invalid_recipient`); nothing sent | yes | `35ef6e57` |
| Contacts permission: dialog -> Don't allow (state "denied, can ask again") -> second request -> Don't allow ("will not ask again" + Open app settings, which opens the real settings page) | yes | `35ef6e57` |
| Grant in settings -> back to app: state re-synced on `onStart` ("Access allowed") -> preview read 3 synthetic contacts | yes | `35ef6e57` |
| Revoke (`pm revoke`) kills the process (Android behaviour); relaunch shows "not allowed" again | yes | `35ef6e57` |
| Leaving the app (HOME) clears the contact preview; return shows it cleared | yes (asserted before/after) | `35ef6e57` |
| Rotation: activity recreates, typed text is cleared (by design), landscape keeps a 640 dp column, no crash | yes | `35ef6e57` |
| Status/navigation bar scrims: scrolled content no longer runs under the clock/gesture bar | yes (screenshot) | `35ef6e57` |
| Manifest permissions at install: `INTERNET`, `CHANGE_WIFI_MULTICAST_STATE`, `READ_CONTACTS`, `READ_CALENDAR`, `READ_PHONE_STATE` | yes (`dumpsys package`) | `6206cc92` |
| **Transport under ART:** tapping "Start link" generated an identity (`files/kde/identity.pem`) and the UI showed device id, certificate SHA-256 and `0.0.0.0:1716`; `/proc/net/tcp` showed a real listener on port 1716 | yes | `6206cc92` |
| Transport survives rotation (listener still open after rotate) | yes | `6206cc92` |
| Transport stops when the app leaves the foreground (listener gone after HOME; on return the UI says "Link stopped because the app left the foreground") | yes | `6206cc92` |
| Calls card renders the call-state controls ("not allowed yet / off / none yet") | rendered only | `6206cc92` |

## NOT verified (do not claim)

- **Pairing end to end.** Connect, the pairing dialog, approve/decline, trusted state and ping were never exercised
  on the emulator: the emulator crashed repeatedly (below) before a peer connection could be completed. The code
  paths are covered by JVM tests with a fake core (request shapes, "send back exactly what was displayed", late
  status ignored after stop, no approval from a response alone) and the Rust side by its own host tests, but there is
  **no screenshot of the pairing dialog**, and no run against a real KDE Connect device or the desktop apps.
- Discovery (UDP, `MulticastLock`) was not run. Announce was not run.
- `READ_PHONE_STATE` grant flow and a simulated call (`adb emu gsm`) were not run; READ_CALENDAR grant and the
  Calendar preview were set up (synthetic events) but not read back.
- Not tested: real phone, arm64 ART, Android versions other than 16, TalkBack, large font scale, dark theme on
  device (dark resources exist; no screenshot).

## Emulator stability (honest log)

The emulator's own `qemu-system-x86_64-headless` process segfaulted repeatedly (core dumps in `coredumpctl`,
`SIGSEGV`, crash in a qemu thread; logs in `dist/android/logs/emulator-run*.log`). It also crashed one of another
agent's instances. Contributing factors seen: host load average 30-50 and ~20 GiB swap in use from concurrent
builds, a qemu footprint that peaks at 9-12 GiB RSS during/after boot despite `hw.ramSize=2048`, and the
`swiftshader_indirect` renderer (`swangle_indirect` lived longer; `-gpu guest` crashed at start). A boot gave
roughly 4-8 useful minutes. I did not find a cause inside the app. **Do not run more than one emulator at a time on
this host.** The pairing test below is the exact work that remains.

## Defects found and fixed in this pass (within `android/**`)

- Content scrolled under the status bar and gesture handle (no scrim) -> opaque top/bottom scrims sized from insets.
- Status dot centred on multi-line text -> dot is now a real view pinned beside the first line.
- Stale copy that said "no internet permission / no connection" after the transport was wired -> Privacy card,
  header, Messages rows, clipboard result and button labels now say what is true (ping-only link, nothing synced).
- Lint: unused `gb_green`, `DotMatrixView` constructor note.
- Raw core JSON now monospace; long explainer paragraphs muted; wide screens capped to a 640 dp column.

## UI (macOS-inspired, restrained)

Monochrome base (`#1D1D1F` text, `#F5F5F7` surface, hairline borders, 14 dp groups, uppercase caption labels with a
dot), one red accent (`#D71921`) used only for the Messages section (dot-matrix "MESSAGES" banner, iTunes-style
striped capability rows, red primary button) and status dots. Native `Button`/`EditText`/`CheckBox`/`AlertDialog`
only, with `Typeface` weights and ripples; no AndroidX/Compose. State is never colour-only: every dot has words.
Dark resources exist (`values-night`); contrast was chosen by value, not measured with a tool.

## Computer link (transport) wiring

Files: `Transport.kt` (strict bounded parsing, request builders, wording), `TransportController.kt` (process-level
state machine, single worker thread, epoch-guarded so stale polls cannot resurrect a stopped link),
`LinkSection.kt` (card, pairing dialog, multicast lock), `Ui.kt`/`MainActivity.kt` glue. Behaviour:

- Nothing starts on its own. "Start link" is an explicit tap; the link stops on Stop, on leaving the foreground
  (not on rotation) and on process death. No foreground service, no reconnect loop.
- All `transport_*` JNI calls run on one `geekbridge-transport` thread; the main thread only renders. Status polls
  every 500 ms while a prompt/unpaired connection exists, else 2 s, only while the activity is started.
- Pairing: the dialog shows the code, peer id, **both** certificate fingerprints and the expiry, cannot be dismissed
  by Back or a stray tap, and sends back exactly the displayed code and peer fingerprint; Decline sends ids only.
  "Approved" is shown as "waiting for the other device"; "Paired" appears only from `connections[].trusted`/`paired[]`
  or the `paired` event. Unparseable prompts are never approvable. `pin_mismatch`, `identity_mismatch` and
  `peer_requested_repair` are shown as SECURITY messages, not generic errors.
- Debug-only checkbox "allow loopback peers" (gated on `BuildConfig.DEBUG`) exists for emulator testing.
- Advertised features at that time: **ping only** (the UI said so). The link now also carries clipboard, sharing,
  call control, contacts, calendar and SMS, each behind a per-computer opt-in.

## Handoff: the pairing test that still has to be run

1. Boot one emulator (see Environment), install the APK, open the app, tick "Testing only: allow loopback peers",
   tap Start link. Read the device id (Computer link card) and certificate.
2. `adb forward tcp:41717 tcp:1716`.
3. Host peer (real `bridge-kde`): `target/debug/geekbridge-pair --state-dir /tmp/gbpeer --name TestPeer pair
   --connect 127.0.0.1:41717 --device-id <phone id> --mode request` in a TTY. The phone shows the pairing dialog;
   compare its code with the one the tool prints, type the code into the tool, then tap **Pair** on the phone.
   Expect "Paired" only after both sides accept; then "Send ping" and check the tool reports it.
   (The reverse direction, phone dials `10.0.2.2:<port>` with `pair --listen`, failed here because the listener
   accepts one connection from `127.0.0.1` and a stray local probe consumed it.)
4. Capture the dialog and the paired state; label them "test peer: geekbridge-pair on the host".
