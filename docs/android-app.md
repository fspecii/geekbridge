# Android companion app

Project: `android/` (Gradle, single `:app` module, package `dev.geekbridge.android`). Pins follow
[android-contract.md](android-contract.md): AGP 8.13.2, Gradle 8.13, Kotlin 2.2.21, JDK 17, compile/target
SDK 36, min SDK 26. UI uses platform widgets only (no AndroidX, no Compose); the only dependencies are
JUnit and `org.json` for JVM tests.

## What works today

Confirmed by running the JVM tests, lint and `assembleDebug` against the `.tools` toolchain (see Validation):

- **Native core status.** `NativeBridge` loads `libgeekbridge_android`; a failed load is caught and shown as
  "Native core: UNAVAILABLE — reason" instead of crashing. Otherwise the `version` action reports the core
  version. Every JNI call goes through `CoreClient`, which converts missing symbols, runtime failures and
  malformed or oversized replies into a visible state and never echoes native text unbounded.
- **Clipboard (manual card in Diagnostics; sync is separate, see [clipboard-sync.md](clipboard-sync.md)).** Paste-from-clipboard, copy, and the Android share sheet are buttons; the clipboard is
  never read in the background or at startup. "Prepare" runs `validate_clipboard` in the native core (no
  clipboard access) and reports "Not sent". Copied text is flagged sensitive (honoured on Android 13+).
- **SMS validation.** `validate_sms` checks recipient, text and optional SIM subscription id with the shared
  Rust rules. It never sends or reads SMS and the app requests no SMS permission.
- **Contacts preview.** After the user taps "Allow" (runtime `READ_CONTACTS`), shows at most 25 contacts
  (name plus first number, fields clipped to 80 chars) read from the Contacts provider.
- **Agenda preview.** After the user taps "Allow" (runtime `READ_CALENDAR`), shows at most 25 events in the next
  7 days (title, time, location) from the Calendar provider.
- **App updates.** Checks GitHub's latest release when the app opens (at most every 12 h, switchable) and on
  "Check for updates"; shows version and notes; downloads the APK only on a tap, verifies size and SHA-256
  against the release's `SHA256SUMS`, and hands it to Android's installer through a non-exported
  `UpdateFileProvider` (`REQUEST_INSTALL_PACKAGES`; the user confirms). See [updates.md](updates.md).
- **Calls (state only).** After the user taps "Allow call state" (runtime `READ_PHONE_STATE`), the Calls card shows
  **Idle**, **Ringing** or **In call** with the time the app last saw a change. Only the coarse `TelephonyManager`
  call state is read: no phone numbers, call log, direction, duration or contact names are requested, shown, logged
  or stored (the app holds neither `READ_CALL_LOG` nor `READ_PHONE_NUMBERS`), and the call-state feature uses no
  network. Observing starts when the permission is granted and stops in `onStop`/`onDestroy`. The card also explains
  that call audio is the phone's normal Bluetooth HFP link and opens Android's Bluetooth settings; the app cannot
  capture or inject cellular audio. Nothing about calls is sent to a computer. Details, API-level strategy table and
  emulator evidence: [android-calls.md](android-calls.md).
Permission handling: each Allow button requests exactly one permission; a denial is reported honestly, and if
Android will no longer show the prompt the button becomes "Open app settings". Revocation is re-checked on
every start.

Threading and lifecycle: provider queries and JNI calls run on a worker thread (`WorkRunner`), one task per slot,
newer work cancels older work, and provider queries use `CancellationSignal`. `onStop` cancels all work and clears
on-screen contacts/agenda; `onDestroy` shuts the worker down. Results are bounded (rows, field length, request
and response size). Entered text is excluded from saved instance state, so rotating the device clears it.

## Share / Send to computer

"Send to computer" appears in the system Share sheet (`ShareActivity`, `ACTION_SEND` and `ACTION_SEND_MULTIPLE` for
`*/*`, `text/plain`, `image/*`) from Gallery, Browser, Files and so on. It uses the KDE Connect `share` plugin through
the `share_*` native actions (see [android-contract.md](android-contract.md)).

- **Sending.** Files: only `content://` Uris are accepted (a `file://` Uri could point at this app's private files).
  Each is opened with `ContentResolver.openFileDescriptor(uri, "r")`; the descriptor number goes to the Rust core,
  which duplicates it and streams from it, so file contents are never loaded into memory by Kotlin or passed through
  JSON. Names and sizes come from `OpenableColumns` (fallback `statSize`); a provider that cannot report a size
  cannot be sent. Up to 50 files per share, at most 3 transfers at once (the rest queue). Text is sent as text; a
  string that is exactly one `http(s)` URL is sent as a link. Several paired computers: the user picks; exactly one
  connected computer is used directly. A paired computer seen by discovery is connected automatically (TLS only; trust
  is the pinned certificate).
- **Progress.** The share screen lists each item's state from the polled `transfers` snapshot with a Cancel button,
  and an ongoing notification shows progress when the optional `POST_NOTIFICATIONS` permission is granted (asked once,
  on the first send; sharing works without it). The link is kept alive while a share is active
  (`TransportController.hold`/`needsLink`) even if the screen is left, and stops afterwards if the app is in the background.
- **Receiving.** An incoming file offer is never touched until accepted. Per paired computer there is an
  "Auto-accept files from this computer" switch (default off). Otherwise an Accept/Decline dialog (while the app is
  open) and notification buttons appear. Accepted files are written through `MediaStore.Downloads` to
  `Download/GeekBridge` as a pending row (no storage permission) and published when the transfer completes; failed or
  cancelled ones are deleted. **API 26-28** have no such collection, so files go to the app's own external
  `Downloads/GeekBridge` folder (no "open" action there). Received text is shown with a Copy button and a received
  link with an Open button (http/https only); the clipboard is only written when the user taps Copy.
- **Also** a "Send files to computer…" button on the clipboard card opens `ACTION_OPEN_DOCUMENT` (multiple) and routes
  to the same flow.

Code: `Share.kt` (pure, JVM-tested: request JSON, `transfers` parser, name/MIME rules, intent classification, receive
policy, target choice), `ShareCoordinator.kt` / `ShareActivity.kt` / `OfferDialogs.kt` / `ReceivedFiles.kt` /
`ShareNotifications.kt` / `ShareActionReceiver.kt` (Android glue).

Not tested: the share Android glue on a device or emulator, the real JNI share actions (depend on the Rust side),
MediaStore behaviour on real devices, notification permission flows, and interoperability with stock KDE Connect.

## What does not work (by design, for now)

- **No pairing, discovery, transport or sync.** The UI says so; "Pair" button is
  disabled. Sharing files, text and links to a paired computer is described above. The `INTERNET` permission is declared (the optional computer link and the update check use it; the
  link stops when the app leaves the foreground), but there is no service and no unauthenticated command
  surface: the update check only makes outbound HTTPS requests to GitHub and opens no listening socket. "Paired computers" is always "none" until a real backend exists. Native validation success never
  implies trust or call readiness.
- **No SMS read/send.** `READ_SMS`/`SEND_SMS` are restricted by Android: stores generally permit them only for
  the default SMS app, and sideloaded installs on Android 13+ may need "Allow restricted settings". A runtime
  denial is final until changed in app settings. The app requests neither, and nothing is ever sent automatically.
- **No call audio, call control or call numbers.** The app only reports Idle/Ringing/In call; it cannot answer, dial,
  hang up or read who is calling, and it never forwards call state to a computer (the optional computer link
  carries no call data; the card says so).
- **No background clipboard read**, accessibility, root or ADB use. Clipboard sync (off by default) is described in [clipboard-sync.md](clipboard-sync.md): the computer pulls, and the app reads the clipboard only while it has focus after a tap.
- No logging of contacts, calendar, SMS or clipboard contents (the app has no logging of them).

## Native library packaging

Libraries go in `app/src/main/jniLibs/{arm64-v8a,x86_64}/libgeekbridge_android.so` (git-ignored; produced by
`scripts/build-android.sh`). The Gradle task `verifyNativeLibs` runs before every `merge*JniLibFolders` task, so
`assemble*`/`bundle*` fail with an explanatory message if a library is missing, is not an ELF for its ABI, or
does not export `Java_dev_geekbridge_android_NativeBridge_nativeRequest`. Unit tests and lint do not need it.
There is no override: a missing-core APK is not produced. The arm64 library should be linked with 16 KiB page
alignment (already set in `scripts/android-env.sh`).

## Build and test

```sh
source scripts/android-env.sh
cd android
echo "sdk.dir=$ANDROID_HOME" > local.properties   # git-ignored
gradle --no-daemon :app:testDebugUnitTest :app:lintDebug :app:assembleDebug
# APK: android/app/build/outputs/apk/debug/app-debug.apk
```

There is no Gradle wrapper (the toolchain provides Gradle 8.13). `gradle.properties` sets
`kotlin.compiler.execution.strategy=in-process`, which avoids Kotlin-daemon RMI noise here. The unsigned
`assembleRelease` output is not signed or tested; release signing is not configured.

## Validation performed

Run with `.tools` JDK 17.0.21 / Gradle 8.13 / SDK 36 in this checkout:

- `:app:testDebugUnitTest`: 58 tests passed when the call-state work was verified (38 earlier tests: JSON request
  shapes, strict response parsing, bounds, error redaction, permission-state logic, formatting, `WorkRunner`
  cancellation; plus 20 in `CallStateTest.kt`: state mapping, SDK listener-strategy selection, change tracking,
  formatting, monitor lifecycle with a fake registrar, executor bounds). Transport, share, update and setup tests
  were added afterwards and this count has not been refreshed; re-run the command in "Build and test" for the
  current total.
- `:app:lintDebug`: no errors; one informational `NewerVersionAvailable`.
- `:app:assembleDebug` with the Rust libraries built by the native agent: APK contains both ABIs, package
  `dev.geekbridge.android`, target SDK 36. That first build declared only `READ_CONTACTS` and `READ_CALENDAR`; the
  manifest now declares the runtime permissions `READ_CONTACTS`, `READ_CALENDAR` and `READ_PHONE_STATE`, plus
  `INTERNET` and the other install-time permissions listed in `AndroidManifest.xml` (update install, foreground
  service, boot, notifications). Never requested: SMS, call log, phone numbers, Bluetooth, storage, location.
- Guard check: in a copy of the project without the `.so` files, `assembleDebug` fails in `verifyNativeLibs`
  with the message above.

- **Emulator, Calls card only (2026-11-01).** On AVD `gbcall` (`emulator-5584`, API 36, x86_64, dedicated adb server
  on port 16037) the debug APK showed "not allowed yet" with the permission revoked, then the system dialog, then
  Idle -> "Ringing (incoming call)" -> "In call" -> Idle driven by the emulator's simulated GSM signalling
  (`adb emu gsm call/cancel`, `KEYCODE_CALL`); `dumpsys telephony.registry` showed the app's call-state listener
  registered on start and removed on stop. Screenshots and uiautomator dumps are in `dist/android/logs/`
  (`calls-*`). This is simulated cellular signalling only: no Bluetooth HFP link and no call audio. Only the API 31+
  listener path ran (API 36); the API 29-30 and 26-28 `PhoneStateListener` paths are covered by selection tests only
  and were not run on a device. See [android-calls.md](android-calls.md).

Not tested: installing or running on a physical device, anything on an emulator other than the Calls card (so
the real JNI call, the contacts/calendar permission dialogs and provider reads, share and update flows are
unexercised there), release builds, and TalkBack/accessibility behaviour. Call state on a real phone with a real
carrier has not been observed.
