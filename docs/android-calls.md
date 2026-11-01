# Android call state

The Calls card shows whether the phone is **Idle**, **Ringing** or **In call**, with the time the app last saw
the state change. It exists to prove that simulated or real cellular signalling reaches the companion app. It
does not carry call audio and does not send anything to a computer (the app still has no transport).

## What is observed

- Only the coarse `TelephonyManager` call state: `CALL_STATE_IDLE`, `CALL_STATE_RINGING`, `CALL_STATE_OFFHOOK`.
  Unknown values are ignored.
- Incoming numbers are **not** requested, shown, logged or stored. The app does not hold `READ_CALL_LOG` or
  `READ_PHONE_NUMBERS`; an incoming call is displayed as "Ringing (incoming call)" only. The legacy
  `PhoneStateListener` callback passes a number argument; the code ignores it.
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
adb -s emulator-5584 shell am start -n dev.geekbridge.android/.MainActivity
adb -s emulator-5584 emu gsm call 5551234      # Calls card: Ringing
adb -s emulator-5584 shell input keyevent KEYCODE_CALL     # In call
adb -s emulator-5584 emu gsm cancel 5551234    # Idle
```

(The `5551234` number is only the emulator console argument; the app never sees or shows it.)

## Emulator evidence (2026-11-01)

Verified on AVD `gbcall` (emulator-5584, API 36, x86_64) with the debug APK, using only the emulator's simulated
GSM signalling (`adb emu gsm call/cancel`, `KEYCODE_CALL`/`KEYCODE_ENDCALL`). Files are in `dist/android/logs/`:

- `calls-4-not-allowed.*`: permission revoked, card shows "not allowed yet", phone state "off", "Allow call state".
- `calls-5-permission-dialog.png`: system dialog; tapping Allow starts observing (`calls-6-after-allow-idle.*`: Idle).
- `calls-7a-idle`, `7b-ringing`, `7c-incall`, `7d-idle` (`.png` + uiautomator `.xml`): Idle -> "Ringing (incoming
  call)" -> "In call" -> Idle, each with a new "Last change seen" time.
- `calls-registry-listen.txt`: `dumpsys telephony.registry` shows the app's `listen` with `events=[6]` (call state)
  on start and `events=[]` on stop, so the listener is unregistered when the app leaves the foreground.

Facts worth knowing: the system dialog is titled "make and manage phone calls" because that is the label of the
whole Android phone permission group; the app only holds `READ_PHONE_STATE`. This is simulated cellular
signalling only: no Bluetooth HFP link and no call audio were exercised.
