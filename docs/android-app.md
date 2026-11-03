# Android companion app

Project: `android/` (Gradle, single `:app` module, package `dev.geekbridge.android`). Pins follow
[android-contract.md](android-contract.md): AGP 8.13.2, Gradle 8.13, Kotlin 2.2.21, JDK 17, compile/target
SDK 36, min SDK 26. UI uses platform widgets only (no AndroidX, no Compose); the only dependencies are
JUnit and AOSP's `org.json` (`android-json`, Apache-2.0) for JVM tests.

## Screens and background connection

The launcher is `HomeActivity` (paired-computer status, quick actions, settings); the former single page is
`MainActivity`, opened from Home as **Diagnostics & about**. `SetupActivity` is the first-run wizard and permission
master (including the battery-optimisation exemption and vendor autostart guidance). A foreground service
(`LinkService`, type `connectedDevice`) keeps the link reachable while a computer is paired and restarts after boot.
Design, size and power numbers, and what is not verified: [android-power.md](android-power.md). The paragraphs below
describe the Diagnostics screen and may still say "development" where that is literally true. The visual design
is described under [Dot-matrix visual language](#dot-matrix-visual-language-nothing-inspired) below.

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
- **Calendar preview.** After the user taps "Allow" (runtime `READ_CALENDAR`), shows at most 25 events in the next
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
on-screen contacts and calendar; `onDestroy` shuts the worker down. Results are bounded (rows, field length, request
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

## UI polish pass (accessibility, layout, startup, size)

Measured on the `gbhome` emulator (Android 16 / API 36, x86_64, 1080x2400 @ 420 dpi, software GPU, host shared with
other jobs), debug APK, Home as the launch screen, `am start -W` after `force-stop`: **TotalTime 925-1114 ms over
6 cold starts (median about 950 ms)**. These are emulator numbers, not a phone; treat them as a regression
baseline only. Package size (`assembleDebug` / `assembleRelease`, `isMinifyEnabled = false`): **debug 9.9 MB, release
(unsigned) 6.7 MB**; the two native libraries are 2.7 MB (arm64-v8a) and 3.1 MB (x86_64), `classes.dex` 2.7 MB
(release) and resources are about 30 KB. R8 and resource shrinking are now on for release builds; current numbers and budgets: [android-perf.md](android-perf.md).

What changed and how it was checked (screenshots: `dist/screenshots/android/polish-*.png`):

- **Dark mode.** The Share sheet theme had no night variant, so it kept a light parent under dark colours; it now has one.
  Filled buttons were white on `#2E8BFF` (3.4:1); night blue is now `#4DA3FF` with dark label text. System-bar icon
  colour follows night mode on every screen (`SystemBars`). Light/dark pairs: `01/02` Setup, `03` Home, `12` Share,
  `13/14` Diagnostics.
- **TalkBack.** Custom clickable views announce as buttons; toggle rows are one focus stop that reads title, summary,
  "Switch" and state, and the whole row toggles (large target); the setup wizard sets an accessibility pane title
  ("Step 3 of 9: ...") so each page change is spoken; headings are marked; decorative icons are hidden. Focus order is
  the visual order (content, then the wizard footer). Not verified with a real TalkBack session; verified by code
  and view-hierarchy inspection only.
- **Touch targets.** Buttons, pills, rows and the gear are at least 48 dp; wizard and button heights are no longer
  fixed (they were 50 dp, which clipped at large font sizes).
- **Font scale 1.3x / 2.0x** (`05`, `06`, `07`): no clipped text; at 1.5x and above status chips stack under the row
  text instead of squeezing it.
- **Landscape and tablet** (`08`, `09`, `10`): content keeps a 560 dp column, now including the wizard footer;
  display cutouts are used in landscape.
- **RTL** (`11`, forced with a per-app Arabic locale; strings stay English): layout mirrors correctly (badges, chips,
  switches, back/next). Label padding now uses start/end.
- **Edge-to-edge and insets.** Applied explicitly (not only through the Android 15 default) so API 26-34 behave the
  same; IME insets are included in the bottom padding. Checked on API 36 only; API 26-34 paths compile and follow the
  documented flags but were not run.
- **Predictive back.** `enableOnBackInvokedCallback` is on; the setup wizard only intercepts Back while it can go to a
  previous page, so the system back-to-home animation plays on the first page.
- **Empty and error states.** Dimmed quick actions explain themselves and open the pairing step instead of doing
  nothing; the Share sheet says what to do when the link is off or no computer is paired; pairing "looking" text
  suggests checking Wi-Fi/VPN; a missing file picker has a plain explanation.
- **Strings.** User-facing text of Home, the setup wizard, the pairing panel and prompt, the Share sheet and the offer
  dialogs now lives in `res/values/gb_ui.xml` (with plurals). **Still in Kotlin:** text produced by the pure models
  (`HomeModel`, `SetupModel`: hero text, step titles/details, warnings), and the Diagnostics screen
  (`MainActivity`, `LinkSection`, `UpdateSection`, clipboard settings). Moving those needs the models to return
  string ids and is a separate change. Lint's `SetTextI18n` still flags `MainActivity.kt:275` for that reason.

## Home-screen widgets

Three small widgets, platform `RemoteViews` only (no AndroidX, no Compose), in the same Nothing-style tokens as the app
(white / `#121212` cards from `colors.xml`, ink dots, one red dot, launcher corner radius on Android 12+).

| Widget | Size | Shows | Taps |
| --- | --- | --- | --- |
| **Connection** | 2x1 | Paired computer name and `CONNECTED` / `OFFLINE` / `RINGING` (or `NOT PAIRED`) in dots, a red dot when connected | Opens GeekBridge (pairing when unpaired) |
| **Quick actions** | 2x2 | Send file, Send clipboard, Paste from computer, Answer: ON/OFF | Send file: file picker, then the Share flow to the computer. Send clipboard: the same focused read as the Quick Settings tile (opens Diagnostics' clipboard switch while sync is off). Answer: ON turns it off; OFF opens the setup step (turning it on needs the permission and the per-computer grant, so it is never switched on silently). Dimmed, and each button opens pairing, while nothing is paired |
| **Agenda** | 4x2 | Today's next three events (timed ones by start, ongoing ones included, then all-day ones), times in dots, a red today dot. Without READ_CALENDAR: "Allow calendar" | Opens GeekBridge, or the Calendar setup step when access is missing |

"Paste from computer" cannot pull: the protocol has no phone-initiated clipboard request, the computer pushes. The button
therefore explains that (a toast: send the clipboard from the computer, it lands on this phone, then paste) and, while
Clipboard sync is off, opens the switch for it.

**Updates, no polling.** `updatePeriodMillis=0`. `WidgetUpdater` pushes when something changes: the link observer
(`LinkRuntime`, deduplicated so an unchanged state costs nothing), the call state (`RINGING`, only while answer-from-computer
is on and phone-state access is granted), the answer switch and Setup screen, and for the Agenda a `ContentObserver` on the
Calendar provider (debounced 2 s) plus one inexact `AlarmManager.set` for the earlier of the next shown event's end and
00:01 local, so it also rolls over at midnight, on time-zone and clock changes (`TIMEZONE_CHANGED`, `TIME_SET`, `DATE_CHANGED`).
The observer and the alarm exist only while an Agenda widget does (`AgendaWidget.onDisabled` removes both; the observer is
restored at process start only if one exists). No foreground service; work is one daemon worker thread with no core pool
(it does not stay alive idle), requests coalesce, calendar reads are bounded (64 rows) and never logged; the only log line is
an exception class name. A widget that is not refreshed (process dead) keeps its last view; the next link or calendar event,
a launcher update, or boot refreshes it.

**Works with the app process dead.** Every tap is an immutable `PendingIntent` to a non-exported activity or receiver of this
app (`FLAG_IMMUTABLE`), so a widget can start the app cold. The three providers and the `WidgetSendFileActivity` helper are
`exported="false"`; the system still delivers updates and the time broadcasts.

**Dot text** is drawn once into an `ALPHA_8` mask (about 24 KB for `CONNECTED`) with the shared `DotFont` and tinted by the
`ImageView` with a colour resource, so light and dark follow the system without a second bitmap. TalkBack never hears dots:
each widget has one content description in words ("Connected to Studio PC. Tap to open GeekBridge.", "Today, Monday 2 November.
9:30 AM, Team sync. ..."). Text is in `sp` and clips with an ellipsis at font scale 2.0
(`dist/screenshots/android/widget-fontscale-2.0-light.png`); layouts use start/end for RTL. Resizable both ways; each widget has a
`previewImage` (`drawable-nodpi/widget_preview_*.png`, real captures) and `targetCellWidth/Height` on Android 12+.

Pure logic with JVM tests (`WidgetModelTest`): connection word mapping, answer-tap rule, day bounds (23/25-hour DST days,
other zones), all-day events as UTC dates, event selection and ordering, next-refresh time, dot/spoken time labels.

Emulator check (Android 16, light and dark, font scale 2.0, calendar insert refreshing the Agenda by itself):
`dist/screenshots/android/widget-light.png`, `widget-dark.png`, `widget-fontscale-2.0-light.png`; two are in
`docs/img/android-widgets-*.png`. Debug builds only: `adb shell am start -n dev.geekbridge.android/.DebugPinWidgetActivity -e w connection|quick|agenda`
asks the launcher to pin a widget (`-e demo connected|ringing|offline` pushes a fake state for screenshots). Not verified: a
real launcher other than the emulator's, RTL on a device, a real ringing call, and a paired computer showing `CONNECTED` outside the fake demo state.

## What does not work (by design, for now)

- **No unauthenticated surface.** Pairing, discovery and the link to a computer are done by the Rust core (setup
  wizard step 1 or Home); trust is the pinned certificate plus a code compared on both screens. `INTERNET` is
  declared for the computer link and the update check; the link stops when the app leaves the foreground unless
  "Stay connected" is on (see [android-power.md](android-power.md)), and the update check only makes outbound HTTPS
  requests to GitHub. Native validation success never implies trust or call readiness. Contacts, calendar and SMS are
  relayed to a paired computer only after a per-computer opt-in on both sides ([phone-data-link.md](phone-data-link.md)).
- **SMS only on explicit opt-in.** `READ_SMS`/`SEND_SMS`/`RECEIVE_SMS` are restricted by Android: stores generally
  permit them only for the default SMS app, and sideloaded installs on Android 13+ may need "Allow restricted
  settings". They are requested only when the user switches on *Share messages* for a computer, a runtime denial is
  final until changed in app settings, and nothing is ever sent automatically. No MMS, group or RCS.
- **No call audio, ever.** The Diagnostics card only reports Idle/Ringing/In call locally and sends nothing. The opt-in
  **call control** feature ([call-control.md](call-control.md), off by default) can report ringing/in-call (and, with
  `READ_CALL_LOG` / `READ_CONTACTS`, who is calling) to a paired computer and let it answer, decline or end the call via
  `ANSWER_PHONE_CALLS`; it cannot dial and it never carries audio. Audio stays on the phone's own Bluetooth HFP link
  ([emulated-calls.md](emulated-calls.md)); only simulated signalling is verified.
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

Prerequisites: JDK 17, the Android SDK (platform 36, build-tools 36.0.0, NDK 30.0.16248370) and the native libraries
in `src/main/jniLibs` (built by `scripts/build-android.sh`, or `make android`). `scripts/dev-setup.sh` checks all of it.

```sh
cd android
echo "sdk.dir=/path/to/Android/Sdk" > local.properties   # git-ignored; or export ANDROID_HOME
./gradlew --no-daemon :app:testDebugUnitTest :app:lintDebug :app:assembleDebug
# APK: android/app/build/outputs/apk/debug/app-debug.apk
```

`./gradlew` is the documented path: it fetches Gradle 8.13 and checks its SHA-256 (`gradle/wrapper/gradle-wrapper.properties`),
and `gradle/verification-metadata.xml` verifies every dependency artefact ([supply-chain.md](supply-chain.md)). On the
maintainer machine the project-local toolchain works offline instead: `source scripts/android-env.sh`, then
`gradle --offline :app:testDebugUnitTest ...` in `android/`. `gradle.properties` sets
`kotlin.compiler.execution.strategy=in-process`, which avoids Kotlin-daemon RMI noise. The unsigned
`assembleRelease` output is not signed or tested; release signing is described in [builds.md](builds.md).

## Validation performed

Run with `.tools` JDK 17.0.21 / Gradle 8.13 / SDK 36 in this checkout (the `assembleDebug` bullet below is from the first build; the test and lint figures are from the merged tree):

- `:app:testDebugUnitTest`: 192 tests, 0 failures, on the merged tree (2026-11-01, per
  [android-calls.md](android-calls.md)). They cover JSON request shapes, strict response parsing, bounds, error
  redaction, permission-state logic, formatting, `WorkRunner` cancellation, transport, share, update and setup
  logic, and 20 tests in `CallStateTest.kt` (state mapping, SDK listener-strategy selection, change tracking,
  formatting, monitor lifecycle with a fake registrar, executor bounds).
- `:app:lintDebug`: 0 errors, 9 warnings (merged tree, 2026-11-01; the earlier build had one informational
  `NewerVersionAvailable`).
- `:app:assembleDebug` with the Rust libraries built by the native agent: APK contains both ABIs, package
  `dev.geekbridge.android`, target SDK 36. That first build declared only `READ_CONTACTS` and `READ_CALENDAR`; the
  manifest now declares many more, including `READ_PHONE_STATE`, plus
  `INTERNET` and the other permissions listed in `AndroidManifest.xml` (update install, foreground service, boot,
  notifications, battery-optimisation exemption, multicast, SMS, calendar write, call answering, call log), each runtime
  permission requested only when its feature is switched on. Never requested: phone numbers, Bluetooth, storage, location.
- Guard check: in a copy of the project without the `.so` files, `assembleDebug` fails in `verifyNativeLibs`
  with the message above.

- **Emulator, Calls card only (2026-11-01).** On AVD `gbcall` (`emulator-5584`, API 36, x86_64, dedicated adb server
  on port 16037) the debug APK (the Calls card now sits under Home > Diagnostics & about) showed Idle -> "Ringing (incoming call)" -> "In call" -> Idle driven by the emulator's simulated GSM signalling
  (`adb emu gsm call/cancel`, `KEYCODE_CALL`); `dumpsys telephony.registry` showed the app's call-state listener
  registered while the card is shown and removed after leaving it. The permission walk-through (revoked -> "not allowed yet" -> system dialog -> allowed) was checked on an earlier build, not the merged one. Screenshots and uiautomator dumps are in `dist/android/logs/`
  (`calls-*`). This is simulated cellular signalling only: no Bluetooth HFP link and no call audio. Only the API 31+
  listener path ran (API 36); the API 29-30 and 26-28 `PhoneStateListener` paths are covered by selection tests only
  and were not run on a device. See [android-calls.md](android-calls.md).

Not tested: installing or running on a physical device, anything on an emulator other than the Calls card (so
the real JNI call, the contacts/calendar permission dialogs and provider reads, share and update flows are
unexercised there), release builds, and TalkBack/accessibility behaviour. Call state on a real phone with a real
carrier has not been observed.

## Dot-matrix visual language ("Nothing"-inspired)

The whole app uses one token layer and a small set of code-drawn dot-matrix components. Nothing is bundled: no font,
logo or glyph artwork; the 5x7 dot font is original (`DotFont.kt`) and drawn from code, so the APK grows only by
a few KB of code and vector line icons.

* **Tokens.** Colours: `res/values/colors.xml` (light: off-white `#F5F5F5`, white cards, ink `#0B0B0B`) and
  `values-night/colors.xml` (true black `#000000` OLED surface, `#121212` cards, white dots); a single red accent
  `#D71921` (dark `#E8242C`) for the live dot, "today", warnings and the stepper's current step. "Ok" is ink, not green:
  state is always carried by words plus a filled-vs-hollow dot. Legacy names (`gb_blue` = ink) keep older screens on the
  same palette. Shapes, spacing, type scale and dot pitches: `Tokens.kt` (`Tok`); `Motion.enabled()` honours the system
  "remove animations" setting.
* **Components.** `DotText` (any text in dot-matrix, wraps and shrinks its pitch to fit, speaks its real text to TalkBack),
  `DotProgress` (dotted progress/link strip with a range value), `DotStepper` (setup dots), `heroTile`, `widgetTile`,
  `dotHeader`, `iconButton` (`DotKit.kt`, `DotComponents.kt`); pill buttons, chips with a state dot, pill toggles and
  28 dp cards come from `Ui.kt`/`UiKit.kt`. `DotMatrixView` (the old banner) now shares `DotFont`.
* **Calendar/Contacts.** `DotCalendar` (month grid: grey dot = no events, ink = events, red = today, ring = selected;
  each day is a 48 dp focus stop read as the full date plus event count), `monthCard`, `agendaCard` (red first dot, dotted
  timeline, "Add event" pill) in `DotCalendar.kt`, with the pure `MonthLayout` unit-tested. Home tiles open the screens through
  the `ContactsAction` / `CalendarAction` hooks in `QuickActions.kt` (setup step until those classes exist).
* **Motion.** None runs continuously. Dot text can `reveal()` once with a short column wipe (skipped when animations are
  removed); progress and the link strip just redraw on state change. Everything is static between updates.
* **Accessibility.** Targets are 48 dp, text and tiles scale to font scale 2.0 (dot text caps its growth at 1.4x and wraps),
  dark/light follow the system, RTL aligns dot text to the start edge, decorative dot art is hidden from TalkBack.
* **Design gallery (debug builds only).** `DesignGalleryActivity` shows every component with synthetic data:
  `adb shell am start -n dev.geekbridge.android/.DesignGalleryActivity --es page status|calendar|controls`.
* **Screenshots.** `dist/screenshots/android/nothing-*.png` (light and dark); four are in `docs/img/android-*.png`
  (`android-status-dark`, `android-setup-light`, `android-share-dark`, `android-calendar-dark`), reduced from `tests/golden/android/`.
