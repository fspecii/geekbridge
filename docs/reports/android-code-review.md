# Android code review (production readiness)

Scope: `android/app/src/main/**`, `crates/bridge-android/src/jni_export.rs`.

Limits of this pass: the review was static and partial. Bash failed with EDQUOT for the whole session, and
Grep and Glob were not available. Files could only be read by exact path, so the directory could not be listed.
These were read in full:

- `AndroidManifest.xml`
- `LinkService.kt`, `ServicePolicy.kt`, `BootReceiver.kt`
- `TransportController.kt` (first 200 lines), `LinkRuntime.kt`, `GeekBridgeApp.kt`
- `TelephonyRegistrar.kt`, `CallControl.kt` (first 120 lines), `CallControlAndroid.kt`
- `PhoneDataAndroid.kt`, `RecentPhotos.kt`, `RecentPhotosAndroid.kt`, `ShareActivity.kt`
- `NativeBridge.kt`, `jni_export.rs`

Not reviewed: the other activities, `QrPairActivity` (an exported deep-link entry), `ClipboardRuntime`, `MirrorListenerService`,
`CoreClient`, `WidgetUpdater`, `AppState` and `UpdateFileProvider`. No gradle run, commit or push was possible, so
the edits below are uncommitted and untested. Run `testDebugUnitTest lintDebug` before committing.

## Fixed in the worktree (`PhoneDataAndroid.kt`)

1. **Medium: `SmsReceivedReceiver` could hold the broadcast open too long.** It called `goAsync()`, and `finish()` ran
   only when the provider polling ended. That polling runs on the shared single-thread `geekbridge-data-send` worker.
   If a long contacts, SMS or calendar read held the worker, the broadcast stayed open past about 10 s, which is an ANR
   ("Broadcast of Intent SMS_RECEIVED"). Fix: `finish()` now runs at most once through an `AtomicBoolean`, and an 8 s
   failsafe on the main looper releases it.
2. **Medium: the data waiter thread had no guard around `e.onEvents`.** An uncaught `RuntimeException` from a provider
   or the engine (`IllegalArgumentException`, `SQLiteException`) would kill the daemon thread `geekbridge-data-wait`
   and crash the process. The calls waiter already guarded `onRequests`. Fix: `try/catch (RuntimeException)` around
   `onEvents`.
3. **Low: a misplaced annotation.** `@SuppressLint("MissingPermission", "UnspecifiedRegisterReceiverFlag")` sat above
   the KDoc of `smsActionToken`, so it did not cover `AndroidSmsSender`. The annotation now sits on the class.
4. **Medium: the computer's unread count went stale after reading on the phone.** Only the MMS table was observed, so
   marking a text read changed nothing the computer heard about. Fix: `startObserving` also registers a
   `ContentObserver` on `Telephony.Sms.CONTENT_URI` (descendants included) when the SMS feature is active (READ_SMS
   granted), debounced 1.5 s on the main handler, running the existing `refreshConversations` on the worker. That
   re-sends the thread list with the unread map (threads with unread texts only; read threads are omitted, and the
   PC treats absent threads as zero). It is unregistered in `stopObserving`. Not built or tested here (no shell).

## Open findings, ranked

1. **Medium: `CallControlRuntime.ensureMonitor` runs on the main thread** (`CallControlAndroid.kt`, `onLinkChanged` →
   `startObserving` → `CallStateMonitor.start`).
   - It does binder calls into the telephony process (`registerTelephonyCallback`, `SubscriptionManager.activeSubscriptionInfoList`,
     one `createForSubscriptionId` and register per SIM). `AndroidCallSources.sync()` also runs on the main thread
     below API 30. These calls can stall for seconds just after boot or when the phone process is busy, which risks
     an ANR (StrictMode flags it only in debug).
   - Fix: run `ensureMonitor` and the first `sync()` on `sender` and keep only the bookkeeping on main. The
     `OnSubscriptionsChangedListener` below API 30 does need the creating thread's Looper, so keep that one on main.
   - The 30 s retry (`ListenerRetry.due`) is evaluated on every status refresh, a few seconds apart. That is cheap,
     but a failing registration is retried from the main thread each time it is due.
2. **Medium: `AndroidCallSources.register` can leak `defaultHandle`.** If anything after `registrar.registerOn(base...)`
   throws a non-`RuntimeException`, the callback stays registered. The `AutoCloseable` is never returned, so
   `CallStateMonitor` cannot close it. Fix: wrap the rest of `register` in try/catch and close `defaultHandle` before
   rethrowing.
3. **Medium (unverified): the link and call waiter threads depend on `CoreClient` swallowing every throwable.**
   - `LinkRuntime.waitLoop` and `CallControlRuntime.waitLoop` call `core.transportAction` with no try/catch.
   - The `NativeBridge` comment says `CoreClient` guards the call. The Rust side panics are contained
     (`EnvUnowned::with_env` plus `catch_unwind`, which throws a fixed-message `RuntimeException`).
   - `TransportController.run` catches only `RuntimeException`. An `UnsatisfiedLinkError` or `OutOfMemoryError` on
     the worker is not caught. Fix: confirm `CoreClient` catches `Throwable`, or add a catch for `LinkageError` and
     `RuntimeException` around the waiter bodies.
4. **Low-medium: `BootReceiver` does disk work on the main thread.**
   - `UpdateStorage.purge` deletes files, and `LinkService.sync` calls `AppState.init`, which reads SharedPreferences.
   - Both run in `onReceive`. A receiver gets about 10 s, and this is normally fast, but slow flash right after boot
     can hurt.
   - Fix: `goAsync()` plus a worker thread for the purge.
5. **Low: `AndroidSmsSender.send` registers two receivers before `sendMultipartTextMessage`.**
   - An exception from `getSmsManagerForSubscriptionId(subId)` (`IllegalArgumentException`, a stale SIM) or from
     `sendMultipartTextMessage` leaves the receivers registered. The 15 min timer posted before the send does clean
     them up.
   - Fix: wrap the send in try/catch and unregister immediately on failure.
6. **Low: `AndroidPhotoPort.latest` catches only `SecurityException`.** An OEM `IllegalArgumentException` for an
   unsupported query arg propagates to the engine. `thumbnail` catches `Exception` but not `OutOfMemoryError`. The
   sampled decode and `loadThumbnail` at 160 px keep the memory risk very low.
7. **Low: `PhoneDataRuntime.resync`** calls `CapabilityRedeclare.onChange` twice, which is harmless but redundant.
   `calendars()` constructs a port per call, so the caller must stay off the main thread (the doc comment says so).

## Checked and fine

- **Foreground service start.**
  - `LinkService.sync` catches `RuntimeException`, which covers `ForegroundServiceStartNotAllowedException`.
  - `promoteOrStop` also catches it inside `onStartCommand`, then stops and returns `START_NOT_STICKY`.
  - The `connectedDevice` type is declared in the manifest and passed in code from API 29.
  - `FOREGROUND_SERVICE_CONNECTED_DEVICE` and install-time `CHANGE_NETWORK_STATE` and `CHANGE_WIFI_MULTICAST_STATE`
    are declared.
  - `BOOT_COMPLETED` and `MY_PACKAGE_REPLACED` are exempt from background-start limits, and `connectedDevice` has no
    Android 15 boot restriction or timeout.
  - The notification channel is created before `startForeground`. `POST_NOTIFICATIONS` denial does not stop the
    foreground service.
- **Lifecycle.**
  - `LinkService` unregisters the screen receiver and the network callback and clears its handler in `onDestroy`.
  - The telephony callbacks, per-SIM callbacks, subscription listener and phone-state receiver are all closed by the
    `AutoCloseable` that `AndroidCallSources` returns.
  - The SMS-status receivers are removed on completion or after 15 min.
  - The content observers and the battery receiver are unregistered in `stopObserving`.
- **PendingIntents.** Every one reviewed uses `FLAG_IMMUTABLE`. The dial action uses `setAuthenticationRequired(true)`
  for Allow.
- **Exported components.** None of these is exploitable as reviewed:
  - `SmsReceivedReceiver`, `MirrorListenerService`, `GeekBridgeInCallService` and `ClipboardTileService` are protected
    by system-only permissions.
  - `ShareActivity` accepts only `content://` URIs and blocks the app's own authorities.
  - Widget receivers are `exported=false`.
  - Providers are `exported=false` with URI grants.
  - Receivers registered at runtime are `RECEIVER_NOT_EXPORTED` on API 33+ and permission-guarded below.
- **Permission-denied paths.**
  - Each call, contacts, calendar, SMS and photo path catches `SecurityException` and returns empty or null.
  - The `ANSWER_PHONE_CALLS`, `CALL_PHONE` and `READ_PHONE_STATE` paths are gated by `CallRules` and mapped by the
    engine.
  - `AndroidDialPrompts` handles disabled notifications.
- **Cursors.**
  - Cursors use `use {}` and bounded loops.
  - String columns use `getString(...).orEmpty()` or null checks.
  - Row counts are capped, queries are cancellable, and the MMS and OEM column failures are tolerated.
- **Large photos.** Thumbnails use `loadThumbnail` (API 29+) or a bounds-first `inSampleSize` decode below that. Size
  and dimension caps are enforced, and nothing is decoded at full size.
- **JNI.** `jni_export.rs` guards with `with_env` plus `catch_unwind` and throws one fixed-message `RuntimeException`.
  The crate denies `unwrap`, `expect`, `panic` and indexing outside tests. Requests are size-capped before decoding,
  and unpaired surrogates are rejected.
- **WorkManager and alarms.** Neither is used. Deferred work uses main-looper handlers or a scheduled executor, all
  bounded.
- **Doze.** The service nudges the core on the `DEVICE_IDLE_MODE_CHANGED` and `SCREEN_ON` events. It uses a Wi-Fi lock
  only while linked. The battery-optimisation prompt is user-initiated.
