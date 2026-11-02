# Real device: Messages tile OFF / `sms: not_offered` (Galaxy Z Fold5)

## What the audit found

The phone declares `sms` in its identity only when the app-wide Messages switch is ON **and** READ_SMS, SEND_SMS and
RECEIVE_SMS are all granted (`PhoneDataGate`). The per-computer switch (`ComputerGrants`, keyed by the paired
computer's device id) only filters requests in the native core; it does not change what is declared.
The flow was correct for the happy path, but had these defects:

1. **Stale declaration after Settings.** Permissions granted in system Settings (or "Allow restricted settings"
   followed by a grant) were only picked up if the Setup screen's `onRequestPermissionsResult` ran. Coming back to
   Home/Setup did not call `PhoneDataRuntime.resync()`, so a running link kept the old identity and the PC kept
   showing `not_offered` until a manual reconnect/restart. Now `HomeActivity.onResume` and `SetupActivity.onResume`
   call `resync()`, which reconnects only if the declared set really changed (`CapabilityRedeclare.needed`).
2. **"Allow access" button never recorded that it asked.** Only the switch path called `markAsked`, so after a
   permanent denial the app could not tell "never asked" from "blocked" and never pointed to Settings. The button
   now records it too.
3. **Home tile said only On/Off.** It used the phone-wide state and gave no reason. It now uses
   `SmsReadinessLogic` (switch, each permission, restricted setting, paired computer, per-computer allow) and shows
   *Off* (switch off), *On* (ready) or *Needs setup*. Tapping still opens the Messages step.
4. **Messages step explained nothing.** It now shows one plain sentence for the first thing missing, with the next
   action already on screen (Allow access / Open app settings / Allowed computers…):
   - switch off → turn on "Share text messages (SMS)"
   - some of read/send/receive missing → names exactly which ("read your texts, see new texts"), asks for all three in one request
   - asked before and Android no longer shows the dialog → open app settings › Permissions › SMS
   - same, on Android 13+ with the hard-restricted flag → "Allow restricted settings" steps
   - no computer paired / no paired computer allowed → pair, or tick the computer under "Allowed computers…"
5. **Per-computer switch (S2-5), by design:** a computer paired *after* the one-time migration gets no rights
   (`SecurityReview2Test` pins this). So a user who enabled Messages before pairing sees the phone permission OK but
   no computer allowed; this is now reported as such (item 4) instead of a bare "Off". The grant key is the paired
   device id from `AppState.pairedPeers`, the same id the core compares in `permits`.
   With exactly one paired computer, turning the switch on allows it automatically.

`geekbridge-ctl phone-state` (debug build) already had reasons for switch off / permission missing (with
restricted hint) / per-computer off / no computer paired; they were checked and left as they are.

## Tests

`SmsReadinessTest` (JVM): full decision table, partial grants (only READ, missing RECEIVE), blocked vs restricted,
words for every state, gate needs all three permissions, re-declare when permissions change in either direction,
grants keyed by device id. `./gradlew :app:testDebugUnitTest` and `:app:lintDebug` pass.

## Emulator

Own AVD `gb36x` (console 5654, adb server 16043), new debug APK, already paired with the e2e desktop. The
scripted `scripts/e2e-debug-diag.sh` run failed in its first debug step (the debug-log consent was not on, unrelated
to SMS and not investigated), so the SMS states were driven by hand with `pm grant/revoke` and the debug broadcast,
reading the `capability:` log lines and the Home tile:

| state | capability log | Home tile |
|---|---|---|
| switch on, no permission | `sms: not offered: READ_SMS not granted (restricted setting …)` | Needs setup |
| only READ_SMS | `SEND_SMS not granted …; RECEIVE_SMS not granted …` | Needs setup |
| all three, computer not allowed | `sms: not offered: per-computer Messages switch is OFF (no paired computer is allowed)` | Needs setup |
| all three + per-computer allow | `sms: offered` | On |
| SEND_SMS revoked again | `sms: not offered: SEND_SMS not granted …` | Needs setup |

Not verified on the emulator: the PC-side `geekbridge-ctl messages` flip (the scripted run did not get that far),
the resync-on-resume reconnect with a live link (Android restarts the process when a permission is revoked, so
that path is covered by the JVM test only), and the real "Allow restricted settings" UI of One UI.
The owner's phone and live service were not touched.

