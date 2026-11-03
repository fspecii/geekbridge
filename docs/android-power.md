# Android: setup wizard, background connection, size and power

Status 2026-11-01. Code: `HomeActivity`, `SetupActivity`, `PairPanel`, `LinkService`, `BootReceiver`, `Power.kt`,
`SetupModel.kt`, `ServicePolicy.kt`, `AppState.kt`. Screenshots: `dist/screenshots/android/ui-*.png`.

## Screens

- **Home** (launcher): paired-computer card (never says "Connected" without a trusted connection), quick actions
  (Send file, Send clipboard; plug-in hooks in `QuickActions.kt`), status rows for Calls / Messages / Contacts / Calendar, settings
  (Stay connected, Setup & permissions, Diagnostics & about). A persistent warning banner appears whenever a required
  step is open or something that worked was revoked. Light and dark follow the system.
- **Setup wizard / permission master** (`SetupActivity`): first run, and any time from Home. One step per page:
  pair, notifications, contacts, calendar, phone state, SMS (information only), battery; then an overview with a
  state per step. Every state is recomputed on **every resume**. Denied-for-good permissions turn into "Open app
  settings".
- **Diagnostics & about** (`MainActivity`, no longer the launcher): everything the old developer page had (core JSON,
  manual link, previews, updates).

## Battery step

`PowerManager.isIgnoringBatteryOptimizations` decides Done; the button fires
`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` (permission `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`), falling back to the
optimisation list, then app settings. `ActivityManager.isBackgroundRestricted` is reported separately. For Xiaomi,
Samsung, Huawei/Honor, OnePlus, OPPO/realme and vivo the page lists the vendor's extra switch in plain steps and tries
the vendor settings screens in order (`OemIntents`), skipping any that is missing or protected and finally opening app
settings. Vendor switches cannot be read, so they stay "to do" until the user taps "I've turned it on".
Detection (`PowerRules.classify`): the user's "Restricted" background mode (`isBackgroundRestricted`) or, for an app that is
not Doze-exempt, the `RESTRICTED` standby bucket (45; `am set-standby-bucket ... restricted`) counts as
`BackgroundRestricted`; its step says so and its button opens the app battery screen
(`android.settings.APP_BATTERY_SETTINGS`, else app settings). Pixel/Google phones get their own short guide (App battery
usage, Adaptive battery); the Samsung guide names "Never sleeping apps", "Put unused apps to sleep" and Adaptive battery.
`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` is a restricted permission on Google Play (allowed only for apps whose core function
breaks without it); the app therefore falls back to the optimisation list, and for a Play listing the exemption request
should be dropped and the guide kept. Verified by `scripts/e2e/battery-optimisation.sh`.
If the exemption is later revoked, Home shows "Battery restriction was turned back on" and the service notification
adds "Battery optimisation is on".

## Background connection (`LinkService`)

- Runs only while `pairedCount > 0` **and** "Stay connected" is on (`ServicePolicy.shouldRun`). The count is persisted
  from every readable status snapshot (`AppState.recordPaired`), so the boot receiver can decide without loading the
  native core; a change from 0 to >0 (or back) starts or stops the service.
- Foreground service type `connectedDevice` (permissions `FOREGROUND_SERVICE`,
  `FOREGROUND_SERVICE_CONNECTED_DEVICE`; `CHANGE_WIFI_MULTICAST_STATE` satisfies the type's prerequisite).
  `dataSync` was rejected: it has a time limit on Android 15 and may not start from `BOOT_COMPLETED`.
- Notification: channel "Background connection", `IMPORTANCE_LOW`, ongoing, silent, with a Stop action.
- Holds one TCP listener and sends UDP announcements; it does not listen for discovery, so there is **no multicast
  lock**, and there are **no wake locks** in the new code. (Share transfers by the sharing code keep their own holds.)
- No polling or timers in the service. It watches the **Wi-Fi network** (`registerNetworkCallback` with
  `TRANSPORT_WIFI`; not the default network, which One UI moves between Wi-Fi and mobile data on its own while the Wi-Fi
  link stays fine) and restarts the transport (debounced 3 s, bounded retries) only for another Wi-Fi network or a
  changed IPv4 address; the first Wi-Fi network seen only nudges the core. Screen on also nudges the core
  (`transport_heartbeat_now`): every link pings at once, and a redial or announcement whose timer stood still while the
  CPU slept goes out now (a spent redial budget gets one more try per nudge). See
  docs/reports/link-stability-review-2.md. Dead peers are detected by kernel TCP keepalive that `bridge-kde` arms on every accepted and dialed socket
  (30 s idle, 10 s interval, 3 probes, `TCP_USER_TIMEOUT` 60 s; Android included), plus a 20 s application
  heartbeat (ping) in both directions. When Doze opens a maintenance window (or ends) the service sends
  `transport_heartbeat_now`, so the phone pings at once instead of waiting for a timer whose clock stopped with the CPU. While the screen is off and nobody uses the link the phone pings every 60 s
  instead (the computer keeps its own 20 s check, and silence is judged at 70 s), and it skips its UDP announcements
  while every paired computer is already linked; see docs/reports/real-device-link-stability.md and
  docs/reports/android-battery-audit-2.md. A manual Disconnect on the computer makes the phone stay off it for 5 minutes
  (`geekbridge.goodbye`, [kdeconnect-interop.md](kdeconnect-interop.md)).
- `BootReceiver` (`RECEIVE_BOOT_COMPLETED`, `MY_PACKAGE_REPLACED`) calls `LinkService.sync`.
- While Home/Setup is visible the UI polls `transport_status` every 1-2 s (the core API is poll-based); the service
  never does. Moving between the app's screens does not stop the link (`ActivityTracker`).

## Size

| | Before | After |
| --- | --- | --- |
| `libgeekbridge_android.so` arm64-v8a | 3 130 832 B | 2 664 456 B |
| `libgeekbridge_android.so` x86_64 | 3 546 824 B | 3 048 576 B |
| debug APK (both ABIs, uncompressed libs) | n/a | 9 919 968 B |

"Before" is the stripped release-profile library built at the start of this work; "after" includes other agents'
later additions (sharing, clipboard), so the real saving of the profile is larger. Cargo profile `android`
(`Cargo.toml`): `opt-level="z"`, fat LTO, `codegen-units=1`, `strip="symbols"`. **`panic=abort` is not used**: the
JNI boundary (`dispatch.rs`, `jni_export.rs`) catches panics and returns an error reply; with abort a single bad
request would kill the app. `scripts/build-android.sh` builds with `--profile android`. A release APK compresses the
libraries; the debug APK stores them uncompressed (16 KiB alignment), which is why it is larger than the sum of its
parts would suggest for a release.

## Measurements (Android 16 emulator, x86_64, debug build, app visible, screen on)

| State | Total PSS | Threads | Voluntary context switches |
| --- | --- | --- | --- |
| Setup wizard open, link stopped | ~40.0 MB (Java 10.3 MB, native 11.0 MB) | 23 | 208 / 60 s (3.5/s, UI) |
| Link running in pairing mode (TCP 1716, UDP discovery, UI polling every 1 s) | ~40.0 MB (Java 4.0 MB, native 4.4 MB; some pages swapped) | 25 (+2 Tokio workers) | 390 / 60 s (6.5/s; 602 of them on the UI thread) |

Caveats: debug build with the full Android runtime, so PSS is dominated by the runtime and not by the app; emulator
host was loaded. **The idle background service was not measured in steady state**: that needs a real paired
computer, and without one the app (correctly) stops the service as soon as the core reports no paired computer.
Expect it to be the second row minus the UI: two Tokio workers blocked in `epoll`, no timers. Not measured: alarms,
batterystats wakeups, doze behaviour over hours.

## Verified and not verified

Verified (Android 16 emulator): wizard pages and overview; real battery-exemption dialog and its result; revoked
exemption shows the warning after resume; notification permission dialog; dark mode; Diagnostics; foreground service
starts with type `connectedDevice` from `BOOT_COMPLETED` after a real emulator reboot (Android logged "Background
started FGS: Allowed"); TCP listener on 1716. JVM tests (176 pass), lint, `assembleDebug`.

Not verified: any OEM screen (no Xiaomi/Samsung/Huawei/OnePlus device; only intent order and fallback are unit
tested); pairing and a connected state with a real computer (the paired Home screenshot `ui-23-…simulated` uses
simulated state before the core corrected it); service steady state, network-change re-announce, TCP keepalive
behaviour; notification appearance on a physical phone; TalkBack. SMS access is not requested (Android restricts it);
the SMS step is information only.
