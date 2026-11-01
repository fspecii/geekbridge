# Android call state

The Calls card shows whether the phone is **Idle**, **Ringing** or **In call**, with the time the app last saw
the state change. It exists to prove that simulated or real cellular signalling reaches the companion app. It
does not carry call audio. **By default call state is not sent anywhere**: the Diagnostics card itself never sends
it. A separate, opt-in feature (**call control**, below and in [call-control.md](call-control.md)) can report the call
state to a paired computer and let that computer answer, decline or end the call; it is off until the user turns on
"Share call status with my computer" in the setup wizard, and nothing is sent while it is off (the app declares the
`geekbridge.calls.*` packet types so a switch-on works without a reconnect, but its core refuses requests and sends no
report until the switch is on). The Calls card is in Diagnostics (`MainActivity`); Home shows the permission states and an
"Answer from computer" row.

## What is observed

- Only the coarse `TelephonyManager` call state: `CALL_STATE_IDLE`, `CALL_STATE_RINGING`, `CALL_STATE_OFFHOOK`.
  Unknown values are ignored.
- The Diagnostics card never shows, logs or stores an incoming number. The legacy `PhoneStateListener` callback passes
  a number argument; that code ignores it. Only the opt-in call control feature may read the number, and only with
  `READ_CALL_LOG` (see below).
- No direction, duration, contact name or SIM information.

## Permission

`READ_PHONE_STATE` (runtime). It is requested only from the "Allow call state" button, one permission per
button, like contacts and calendar. A denial is reported honestly; when Android will no longer prompt, the
button becomes "Open app settings". The grant is re-checked in every `onStart`, and a revocation clears the
state. Nothing else was added to the manifest by this change (`INTERNET` was added later for the computer link and update checks; still no service, no listening socket in the call feature).

## Listening lifecycle

`CallStateMonitor` (pure Kotlin, tested with a fake registrar) is started in `onStart` when the permission is
granted (and right after the user grants it) and stopped in `onStop` and `onDestroy`. `stop()` unregisters the
platform listener and invalidates anything still queued, so no report is delivered after it and the callback
does not outlive the activity. Reports are posted to the main thread before they touch the UI.

`TelephonyRegistrar` picks the platform API with `ListenerStrategy.forSdk(sdk)`:

| API level | API used | Callback thread |
| --- | --- | --- |
| 31+ | `registerTelephonyCallback(executor, TelephonyCallback.CallStateListener)` | bounded executor |
| 29-30 | `PhoneStateListener(executor)` + `listen(LISTEN_CALL_STATE)` | bounded executor |
| 26-28 | `PhoneStateListener()` created on the main thread | main looper |

The executor (`CallStateExecutor`) is one daemon thread with a queue of 8; when full it drops the oldest
pending report because only the newest state matters. Only the API 31+ path was exercised on an emulator (see
below); the two legacy paths are covered by selection tests only.

## Tests

`CallStateTest.kt` (JVM): raw-state mapping and constant check against `TelephonyManager`, SDK strategy
selection (26-28, 29-30, 31+), change tracking, formatting (labels, timestamp in a given zone, no digits in
state text), monitor lifecycle with a fake registrar (idempotent start, single unregister, stale reports dropped,
registration failure not echoing system text, failing unregister tolerated), and executor bounds.

## Verifying on the emulator

```sh
export ANDROID_ADB_SERVER_PORT=16037
adb -s emulator-5584 install -r android/app/build/outputs/apk/debug/app-debug.apk
adb -s emulator-5584 shell pm grant dev.geekbridge.android android.permission.READ_PHONE_STATE
adb -s emulator-5584 shell am start -n dev.geekbridge.android/.HomeActivity   # then Diagnostics & about (MainActivity is not exported)
adb -s emulator-5584 emu gsm call 5558802      # Calls card: Ringing
adb -s emulator-5584 shell input keyevent KEYCODE_CALL     # In call
adb -s emulator-5584 emu gsm cancel 5558802    # Idle
```

(The `5558802` number is only the emulator console argument; the app never sees or shows it.)

## Emulator evidence (2026-11-01, merged tree)

Validation on the merged tree: `:app:testDebugUnitTest` 176 tests in 26 classes, 0 failures (20 of them in
`CallStateTest.kt`); `:app:lintDebug` 0 errors, 9 warnings; `:app:assembleDebug` succeeds.

Verified on AVD `gbcall` (emulator-5584, API 36, x86_64) with that debug APK, using only the emulator's simulated
GSM signalling (`adb emu gsm call/cancel 5558802`, `KEYCODE_CALL` sent about 3 s after the call rang,
`KEYCODE_ENDCALL`). The Calls card is under Home > Diagnostics & about. Files in `dist/android/logs/`:

- `calls-1-idle`, `calls-2-ringing`, `calls-3-incall`, `calls-4-idle` (`.png` + uiautomator `.xml`):
  Idle -> "Ringing (incoming call)" -> "In call" -> Idle, with new "Last change seen" times 03:00:37, 03:00:50,
  03:00:57, 03:01:04. `dumpsys telephony.registry` `mCallState` agreed (0, 1, 2, 0).
- `calls-registry-listen.txt`: `dumpsys telephony.registry` shows the app's `listen` with `events=[6]` (call
  state) when Diagnostics is shown and `events=[]` after backgrounding (HOME) or leaving the screen, so the
  listener is unregistered.
- The earlier permission walk-through (revoked -> "not allowed yet" -> system dialog -> allowed) was done on the
  previous build; the system dialog is titled "make and manage phone calls" because that is the label of the
  whole Android phone permission group; the app holds only `READ_PHONE_STATE`.

This is simulated cellular signalling only: no Bluetooth HFP link and no call audio was exercised. Only the
API 31+ path ran on a device; the API 26-30 listeners are covered by selection tests only.

## Call control from the computer (opt-in)

Details, protocol and test matrix: [call-control.md](call-control.md). Summary of what this app does:

- **Off by default.** Nothing about calls leaves the phone until the user turns on "Share call status with my computer"
  in the setup wizard's *Answer calls* step. The computer must also have the "Calls" (Telephony) permission enabled for
  this phone, which is off by default there.
- **Permissions, each asked on its own step with an explanation:** `ANSWER_PHONE_CALLS` (runtime; answer, decline, end
  through `TelecomManager.acceptRingingCall()` / `endCall()`; `endCall()` needs API 28+, so API 26-27 phones can answer
  but report that they cannot decline or end), `READ_CALL_LOG` (optional; the only way Android supplies the caller's
  number; the call log is never read), and the existing optional `READ_CONTACTS` (name lookup on the phone).
- **What is sent:** the coarse state (idle / ringing / offhook), the number and name only if the permissions above allow it,
  and `canAnswer` / `canEnd` so the computer shows controls only when they would work. Requests from the computer are
  executed only if the call state fits and the permission is held; each gets an `ok` or a fixed failure reason back.
- **Audio:** no call audio flows through GeekBridge. Answering through this path leaves the audio on the phone unless the
  computer is also paired with the phone as its Bluetooth hands-free device; the desktop says "Answered on the phone"
  in that case.
- **Not verified on a device:** `acceptRingingCall()` / `endCall()`, the number broadcast and the new wizard steps have
  only JVM tests (`CallControlTest.kt`, fake `TelecomPort`); see the list in call-control.md.
