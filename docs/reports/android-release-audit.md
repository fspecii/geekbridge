# Android release-variant production audit

Date: 2026-11-05. Audited commit: 6a4122922 (HEAD of main when the audit started). Subject: the **release** variant
(`:app:assembleRelease`, R8 on), built in a scratch worktree with the pinned toolchain (JDK 17, Gradle 8.13, AGP 8.13, NDK r30,
Rust 1.99.1), native libraries built the way `scripts/release-local.sh` builds them (`--profile android`, no `callwifi`).
Runtime checks on a private AVD (`gbaudit`, API 36 google_apis x86_64, console port 5640, adb server port 16077); the user's
service, Hyprland, BlueZ and ports 1716, 17381, 8000 were not touched. Emulator numbers are a regression baseline, not phone
numbers, and the host load average was 15 to 97 during the timing runs.

## Verdict

No release blocker in the APK itself. Two defects were found and fixed (CI built a different native library than the one that
was measured, and `docs/privacy.md` did not justify 11 of the 27 permissions). One size budget is exceeded (see Size). Items that
need an owner or a real phone are at the end.

## Results

| Area | Result |
| --- | --- |
| `isMinifyEnabled` / `isShrinkResources` | both true for release (`android/app/build.gradle.kts`), rules `proguard-android-optimize.txt` + `proguard-rules.pro`. R8 and resource shrinking ran. |
| `debuggable` | not set in the merged release manifest; `dumpsys package` flags: `HAS_CODE ALLOW_CLEAR_USER_DATA` (no `DEBUGGABLE`, no `TEST_ONLY`). |
| Debug-only components | none in the merged release manifest: no `DUMP`-protected receivers (`ScaleSeedReceiver`, `DebugControlReceiver`, `CallWifiReceiver`), no `DebugBootProvider`, `DesignGalleryActivity`, `DebugPinWidgetActivity`, no `RECORD_AUDIO`, `WRITE_CONTACTS`. New test `debug_only_permissions_stay_out_of_the_release_manifest` keeps them out of `src/main`. |
| `minSdk` / `targetSdk` / `compileSdk` | 26 / 36 / 36. |
| Version code | `versionCode` 10099 for 0.1.0 (formula `(major*10000+minor*100+patch)*100+slot`, slot 99 final, prerelease below it); debug is `20000 + commit count`, documented in `docs/updates.md` (a stable 0.2.0 = 20099 is below today's dev codes, which that doc already states). |
| Signing | the release build type has no `signingConfig` by design: the APK is unsigned, then `zipalign -P 16 4` and `apksigner` v2+v3 (v1 off, correct for minSdk 26) with the release keystore in `release.yml` / `release-local.sh`. I signed a copy with a throwaway key: `apksigner verify` ok, `zipalign -c -P 16 4` ok. |
| Backup | `allowBackup="false"`, `fullBackupContent="false"`, `dataExtractionRules` excludes root, file, database, sharedpref, external for both cloud backup and device transfer. |
| Network security | the release manifest has no `networkSecurityConfig` and no `usesCleartextTraffic`: targetSdk 36 default, cleartext refused. The cleartext-allowing config is `src/debug` only. The LAN link is raw TLS sockets in Rust, not affected. |
| Exported components | exported: `HomeActivity` (launcher), `QrPairActivity` (`geekbridge://pair`, asks before connecting), `ShareActivity` (SEND), `SendToComputerActivity` (PROCESS_TEXT), `MirrorListenerService` (`BIND_NOTIFICATION_LISTENER_SERVICE`), `ClipboardTileService` (`BIND_QUICK_SETTINGS_TILE`), `SmsReceivedReceiver` (`BROADCAST_SMS`). Everything else `exported="false"`, both providers are not exported and grant by URI only. Every `PendingIntent` is `FLAG_IMMUTABLE`. Context receivers with custom actions use `RECEIVER_NOT_EXPORTED`; the others listen to protected system broadcasts. |
| Foreground service | `LinkService` declares `foregroundServiceType="connectedDevice"` and calls `startForeground(..., FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)` on API 29+; holds `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_CONNECTED_DEVICE` and the Android 14 prerequisites `CHANGE_NETWORK_STATE` and `CHANGE_WIFI_MULTICAST_STATE`. `connectedDevice` is allowed to start from `BOOT_COMPLETED`; a refusal (Android 12+ background start) is caught by `ServicePolicy.promoteOrStop`. No timeout type (dataSync / mediaProcessing) so Android 15's FGS timeout does not apply. Not running while nothing is paired. |
| Exact alarms | none declared; the one alarm (Agenda widget tick) is `AlarmManager.set`, inexact. |
| Notification permission | `POST_NOTIFICATIONS` is requested at runtime on API 33+ in the setup step and before a share; features work without it (`ShareNotifications`, `CallControl` check it). |
| Restricted settings (sideloaded SMS and notification access) | `SmsRestriction.likely` (API 33+, hard-restricted flag, installer not Play/Galaxy Store) switches the SMS and notification-access states to the "Allow restricted settings" wording (`SmsReadiness.kt`, `NotificationMirror.kt`). Behaviour on a real Android 13+ sideload is unit-tested only. |
| Android 16 | `enableOnBackInvokedCallback="true"` and `SetupActivity` registers an `OnBackInvokedCallback`; insets are applied by hand (edge-to-edge enforced on 35+); no orientation locks. |
| Lint (`lintRelease`, `abortOnError`) | 0 errors; 27 warnings, none security-related (InlinedApi, NestedWeights, PluralsCandidate, UseCompoundDrawables, StaticFieldLeak x2, UnusedResources x2). |
| Native ABIs | `arm64-v8a`, `x86_64` only (`native-code` in badging); `extractNativeLibs="false"`, libraries stored uncompressed. |
| 16 KB page size | both `.so`: every `LOAD` segment aligned `0x4000`; `GNU_RELRO` present; `NEEDED` only libdl, libm, libc; no debug sections. APK: `zipalign -c -P 16 4` verification successful, `.so` offsets are multiples of 16384. |
| JNI keep rules | `-keep class ...NativeBridge { native <methods>; }` keeps `nativeRequest`; confirmed `NativeBridge` class unrenamed in `mapping.txt` and `nativeRequest` present in the dex; the Rust library calls back into Java by no name (no `FindClass`). `QuickActions.load` reflection is covered by the `*Action.INSTANCE` rule (only `SendClipboardAction` exists in release; the others resolve to null by design). The app starts and runs on the release build with R8. |
| StrictMode at startup | release does not enable StrictMode (debug only, `GeekBridgeApp`). On a debug build: 3 cold starts logged 6 `DiskReadViolation`s (13 to 65 ms), all from `CallWifiRuntime.init` called by `DebugDiagnostics.boot`, which lives in `src/debug` and is not in the release dex (0 occurrences). No violation from production code. |

## Size and runtime (release, R8, emulator)

| Metric | Budget (`docs/android-perf.md`) | Now | Status |
| --- | --- | --- | --- |
| Release APK, phone + emulator ABIs, unsigned | <= 6.5 MB | 7 102 616 B (7.10 MB; 7 113 369 B aligned and signed) | **over by 0.6 MB** |
| Release APK, arm64-v8a only | <= 3.5 MB | 3 545 623 B | **over by 45 KB** |
| `classes.dex` | <= 0.5 MB | 576 680 B | **over by 77 KB** |
| `libgeekbridge_android.so` arm64 / x86_64 | 2.74 / 3.13 MB at the R8 measurement | 3 058 896 / 3 552 000 B | grew with call, photos and clipboard work |
| `resources.arsc` | 35 KB | 111 KB | more translations |
| Cold start `am start -W`, HomeActivity | <= 1000 ms (load 25-40) | 932 2361 1379 856 871 877 896 1683 857 ms, median 877 (host load 15 to 97); a first set on an incrementally installed, never-dexopted package (`run-from-apk`) was 1.6 to 2.6 s and is not representative | ok under load, but noisy |
| Idle PSS, unpaired, activity open | <= 35 MB | 21.3 MB (RSS 141 MB, mostly file-backed) | ok |
| Idle PSS, app backgrounded 30 s | <= 35 MB | 21.6 MB, 22 threads | ok |
| Alarms / jobs / wake locks / Doze whitelist owned while idle | 0 | 0 / 0 / 0 / no | ok |
| Link service when unpaired | not running | not running | ok |

The paired, connected idle figure (22.8 to 26.5 MB in `android-perf.md`) was not re-measured: it needs the desktop service and a
pairing, and the unpaired figure is the stricter statement about the app itself. Reproduce everything with `scripts/android-perf.sh`.

Size cause: the budgets were set before the in-call card, photos and clipboard-image work; the native library grew about 10%
and R8 dex about 25% since. `adb install` of the incremental kind leaves the package undexopted: measure cold start after
`adb install --no-incremental` (status `verify`) or with `cmd package compile`.

## Defects fixed

1. **CI shipped a different native library than the one measured.** `release.yml`, `dev-release.yml` and `check.yml` built
   `bridge-android` with `cargo build --release` (opt-level `s`, no ICF) and copied from `target/<triple>/release/`, while
   `scripts/android-env.sh`, `build-android.sh` and `release-local.sh` use `--profile android` (opt-level `z`) plus
   `-Wl,--icf=all` and `common-page-size=16384`. The published APK was therefore bigger than every number in
   `docs/android-perf.md` and had never been the binary tested on the emulator. The three workflows now build with
   `--profile android` and the same linker flags, copy from `target/<triple>/android/`, and `release.yml` / `check.yml` fail the
   build if any `LOAD` segment is aligned below 16 KiB (`llvm-readelf`). The local equivalent already had that check.
2. **`docs/privacy.md` justified only 16 of the 27 release permissions.** `INTERNET` (in prose only), `WAKE_LOCK`,
   `FOREGROUND_SERVICE*`, `ACCESS_NETWORK_STATE`, `CHANGE_NETWORK_STATE`, `CHANGE_WIFI_MULTICAST_STATE`, `RECEIVE_BOOT_COMPLETED`,
   `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, `REQUEST_INSTALL_PACKAGES`, `READ_PHONE_STATE`, `READ_EXTERNAL_STORAGE`, `BLUETOOTH`
   and `POST_NOTIFICATIONS` had no entry. New section "Android permissions in the release APK" with one row per permission,
   guarded by `privacy_doc_lists_every_release_android_permission_and_nothing_else`, which parses the main manifest
   (comments stripped) and fails when the table and the manifest differ. It also records that no exact-alarm, overlay, location,
   all-files or `QUERY_ALL_PACKAGES` permission exists.

## Recommendations (not changed here)

* Decide on the size budgets: either raise them to the measured figures (7.1 MB universal, 3.55 MB arm64, 0.6 MB dex) or trim.
  Cheap candidates: drop `x86_64` from the published universal APK (the arm64 APK is already published separately), and check
  the 0.3 MB arm64 growth with `cargo bloat`. The user-facing download for phones is the arm64 APK at 3.5 MB.
* There is no baseline profile (`compileReleaseArtProfile` has nothing to compile), so a sideloaded install runs at `verify`
  until background dexopt. Adding one means `androidx.profileinstaller`, which the project avoids; measure on a real phone first.
* Real-device checks still needed: the Android 13+ "Allow restricted settings" path for a sideloaded SMS grant, the Android 14
  partial photo access, the `connectedDevice` service surviving a reboot (`BOOT_COMPLETED`) on 14/15/16 OEM ROMs, and cold start
  on an arm64 phone.
