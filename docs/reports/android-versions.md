# Android version coverage (minSdk 26 – targetSdk 36)

Date: 2026-11-02. Scope: does the app behave on Android 8–15, not only on the API 36 emulator used so far?

## Status: emulator matrix NOT run (disk), static audit done

`/home` was at 100 % with 3.5 GB free (not the ~45 GB assumed). The API 26 `google_apis;x86_64` image alone is 3.2 GB
unpacked (`system.img` 2.6 GB). After installing it, `emulator -avd gbapi26` refused to start
("not enough disk space to run avd", 797 MB free). I removed the image and the AVD again so the shared disk is not
squeezed further; nothing of the 26/30/34 images is left. Other agents' AVDs (`gb36*`, 1.8–5.7 GB each, ~35 GB total) were not touched.

To finish the matrix: free ~10 GB (e.g. delete idle `gb36*` AVDs), then per API
`sdkmanager --install 'system-images;android-<N>;google_apis;x86_64'`, create an AVD with `hw.ramSize=2048`,
`disk.dataPartition.size=2G`, start it with `GB_EMU_AVD=gbapi<N> GB_EMU_PORT=<free> GB_EMU_ADB_PORT=<free>
scripts/emulator-call-launch.sh`, and run `E2E_AVD=gbapi<N> E2E_EMU_PORT=… E2E_ADB_PORT=… E2E_CTRL_PORT=… E2E_API_PORT=…
E2E_DISC_PORT=… E2E_LISTEN_PORT=… scripts/e2e-android.sh --no-build` (all of these are already env-overridable).
Install one image at a time and delete it after the run; do 26 first, then 30, then 34.

## Matrix (scenario × API)

| Scenario | 26 | 29 | 31 | 33 | 34 | 36 |
|---|---|---|---|---|---|---|
| Setup wizard, battery dialog | SKIP | SKIP | SKIP | SKIP | SKIP | PASS (earlier e2e) |
| QR link pairing | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| Clipboard both ways | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| Files both ways, Share sheet | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| Call answer / reject | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| SMS / contacts / calendar | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| Widgets, tile, boot restart, update check | SKIP | SKIP | SKIP | SKIP | SKIP | PASS |
| Android unit tests (437) + `lintDebug` (incl. NewApi) | PASS | PASS | PASS | PASS | PASS | PASS |

SKIP = not run (disk), not "works". Unit tests/lint are API-independent, so one run covers the row.

## Static audit findings

| Area | Result |
|---|---|
| **SmsManager lookup** | **BUG, fixed.** `getSystemService(SmsManager::class.java)` returns `null` on API 26–30 (class lookup exists from 31), so sending an SMS from the computer would NPE at `divideMessage`. Now uses `SmsManager.getDefault()` below 31 (`PhoneDataAndroid.kt`, test `SmsManagerLookupTest`). |
| Notification channels | Created before posting on all three paths (`ShareNotifications`, `LinkService`, `ClipboardAndroid`) with the API 26 `NotificationChannel`; fine from minSdk. |
| POST_NOTIFICATIONS | Only requested/checked behind `SDK_INT >= 33`; below that it counts as granted. OK. |
| Foreground service | `connectedDevice` type passed on 29+, plain `startForeground` below; manifest declares `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_CONNECTED_DEVICE` and the prerequisite `CHANGE_WIFI_MULTICAST_STATE` (Android 14). `startForegroundService` and `startForeground` refusals (API 31+) are caught. `BOOT_COMPLETED` may start it on 14/15 (`connectedDevice` is not on the blocked list). |
| Known gap | After an app update (`MY_PACKAGE_REPLACED`) Android 12+ does not allow a background FGS start, so the link service stays off until the app is opened; the failure is swallowed, no crash. Not changed. |
| Received files | 29+: `MediaStore.Downloads` + `IS_PENDING` under `Download/GeekBridge`. 26–28: app-specific external dir (`Android/data/<pkg>/files/Download/GeekBridge`), no storage permission needed. The "received" notification then just opens the app (no FileProvider for that folder). Acceptable; files are reachable via a file manager. |
| PendingIntents | Every one uses `FLAG_IMMUTABLE` (API 23+), including the QS tile (34+ uses the PendingIntent overload). OK. |
| Exported components | Only `HomeActivity`, `QrPairActivity`, `ShareActivity`, `SendToComputerActivity`, the QS tile (permission `BIND_QUICK_SETTINGS_TILE`) and `SmsReceivedReceiver` (permission `BROADCAST_SMS`) are exported, all with intent filters or system permissions. OK for 31+. |
| Dynamic receivers | `RECEIVER_NOT_EXPORTED` on 33+; below, SMS-status actions carry an unguessable token and the phone-state receiver requires the system-only sender permission. OK. |
| Telecom | `acceptRingingCall` (26+) unguarded is fine; `endCall` (28+) guarded by `CallRules.canEnd`; phone-state listener picks `TelephonyCallback` (31+) / executor listener (29+) / looper listener (26–28). |
| Clipboard | Reads happen only in focused activities (tile, notification, button), which is what Android 10+ requires; unrestricted on 26–9, so nothing to change. |
| Insets / edge-to-edge | `WindowInsets.Type` on 30+, `systemWindowInset*` and `SYSTEM_UI_FLAG_*` below; cutout mode 28+. OK. |
| Widgets | Plain `RemoteViews`, no AndroidX; `AlarmManager.set` (inexact), so no exact-alarm permission or Doze special case. |
| **ABI** | The APK packages `arm64-v8a` and `x86_64`. A 32-bit-only phone gets `INSTALL_FAILED_NO_MATCHING_ABIS` from the installer and Play hides the app: a clean refusal. Feasibility of `armeabi-v7a` checked: `rustup target add armv7-linux-androideabi` works and `cargo build --profile android -p bridge-android --target armv7-linux-androideabi` links in 23 s (stripped `.so` 1.6 MB, +~0.7 MB compressed, correct `Machine: ARM`, JNI symbol exported). **Decision: not added now.** It cannot be tested here (no 32-bit emulator image, no disk), and it changes `build-android.sh` verification, `android-env.sh` linker flags and `docs/android-contract.md`. Phones that are 32-bit-only and run Android 8+ are rare. If wanted: add the triple + linker/16 KiB flags and an `elf_check ... "ARM"`. |

## Fixes in this change

- `android/app/src/main/java/dev/geekbridge/android/PhoneDataAndroid.kt`: SMS manager lookup for API 26–30.
- `android/app/src/test/java/dev/geekbridge/android/SmsManagerLookupTest.kt`: new.

Verification: `gradle testDebugUnitTest lintDebug` on a scratch copy of `android/` — 437 tests, 0 failures, lint clean.
Nothing committed.
