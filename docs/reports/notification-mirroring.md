# Phone notification mirroring (parity gap 1)

Closes gap 1 of `parity-gaps-2.md` ("Notification mirroring of other apps, dismiss both ways"). Protocol details,
caps and evidence are in `docs/kdeconnect-interop.md` ("Notification mirroring"); the user steps are in
`docs/user-guide.md`. The design follows `battery-status-feature.md` (per-computer grants, capability declared only when
enabled, consent on both ends, memory only).

## What it does

* **Stock packets only.** `kdeconnect.notification` (phone to computer: post and `isCancel`), `kdeconnect.notification.request`
  (`request: true`, or `cancel: id` to dismiss) and `kdeconnect.notification.action` (`key`, `action`). A stock KDE Connect
  peer interoperates both ways. **No typed replies** (`.reply` is never declared or offered) and **no icons** (see below).
* **Phone.** `MirrorListenerService` (a `NotificationListenerService`) feeds `NotificationMirror`, which filters, assigns
  opaque ids and sends through the native data relay. The switch "Show phone notifications on the computer" is off by
  default; the app declares the packets only while it is on **and** Notification access is granted; the native core
  refuses to send to a computer that is not on the per-computer list (`GrantFeature.Notifications`, permit key
  `notifications`).
* **Computer.** The daemon routes `kdeconnect.notification` (new `Incoming::Notification`), keeps the notifications in
  a bounded in-memory store (`crates/bridge-service/src/mirror.rs`), raises a banner through a new `MirrorNotifier`
  (Linux: `notify-send` with Dismiss and up to 3 phone buttons; Windows and macOS: plain banner) and exposes
  `Request::Notifications`, `DismissNotification`, `NotificationAction`.
* **Surfaces.** `geekbridge-ctl notifications [DEVICE_ID] [--json]`, `notifications dismiss DEVICE_ID ID`,
  `notifications action DEVICE_ID ID TITLE`; `geekbridge-ctl consent ID notifications on`; a "Show phone notifications"
  button on the Home phone card (offered only for a connected phone that declared the packet) and a tray item that toggles
  it for the connected phones that offer it. Nothing switches on without a click.

## Decisions

* **Conservative default.** Nothing is mirrored until the user picks apps: the allow-list starts **empty**, so turning the
  switch on and granting access still sends nothing. The picker lists launcher apps only (a `<queries>` entry for the
  launcher intent; no `QUERY_ALL_PACKAGES`). I did not ship a "messaging apps" preset: classifying apps by name is
  guesswork and messages carry codes; a preset can be added later as an explicit button.
* **Never mirrored, even for a picked app.** GeekBridge's own, ongoing, foreground-service, group-summary, `secret`
  visibility, categories call/progress/service/sys/transport/navigation/status/stopwatch/alarm, and empty ones. Calls have
  their own feature. Quiet (low importance) ones are sent `silent` so they are listed without a banner.
* **Redact nothing silently.** The phone sends title and text as Android shows them (cut to 160 and 600 characters,
  control characters replaced by spaces; the cut is a documented cap). The desktop banner *does* hide the text by default
  (same switch as texts) and says so in the user guide; `geekbridge-ctl notifications` shows it because it is an explicit
  local query. Titles and texts are never logged on either end.
* **Opaque ids.** The Android key (package, id, tag) is replaced by `gb<N>`, bounded to 128 and forgotten oldest first;
  the computer can only dismiss or press buttons on what it was given, and the phone re-checks that it is still showing,
  still from an allowed app and (dismiss) clearable, and that the button title is one it listed.
* **Icons: size zero.** A stock phone attaches an icon as a payload. This computer never fetches payloads for this packet
  (it judges the body) and this phone sends none, so the "small and size-capped" requirement is met by not transferring
  icons at all in this version. Showing app icons would need a bounded payload fetch (for example 32 KiB, PNG only,
  decoded size checked); left for a follow-up.
* **Bounds on the computer.** 50 per phone, 8 phones, 3 hours; consent off, link loss and unpairing erase. 6 banners
  burst then one per 2 s; replies to `request` are `silent`. Phone side: 15 per bulk reply, an identical re-post is not
  re-sent, and the core's packet rate limit still applies.
* **Android access guidance.** The switch row shows the next step in plain words (turn on, grant access, restricted
  setting, choose apps, pair, allow a computer). "Open Notification access settings" opens the detail page of this
  service on Android 11+ (`ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS`, falling back to the list). On Android 13+ for an
  install not made by a store (the same rule as SMS, `SmsRestriction`), the greyed-out switch is explained: app settings,
  three dots, Allow restricted settings (Samsung path named), then back to Notification access. Whether that matches a
  real Samsung device is a hardware check.

## Files

Rust: `crates/bridge-core/src/notification_link.rs` (new: parse, bodies, caps), `ipc.rs` (requests, `NotificationsView`),
`crates/bridge-service/src/{mirror.rs (new),gate,daemon,phone_data,os_notify,lib}.rs`, `tray/{model,mod}.rs`,
`bin/geekbridge-ctl.rs`, `crates/bridge-apps/src/home_app.rs`, `crates/bridge-apps/i18n/en.properties`,
`crates/bridge-android/src/transport/{mod.rs,engine/data.rs,engine/permits.rs}`, `crates/bridge-phone-sim/src/data.rs`,
fuzz target `notification_wire` (`crates/bridge-fuzz/src/targets.rs`, `fuzz/`, `docs/fuzzing.md`).
Kotlin: `NotificationMirror.kt` (new: rules, ids, mirror, readiness), `MirrorListenerService.kt` (new: listener, Android
port, settings intents), `PhoneDataProtocol.kt`, `PhoneDataEngine.kt`, `PhoneDataAndroid.kt`, `PhoneDataUi.kt`,
`ComputerGrants.kt`, `AndroidManifest.xml` (listener service with `BIND_NOTIFICATION_LISTENER_SERVICE`, launcher
`<queries>`), `res/values/gb_phonedata.xml`.

## Tests

* Negotiation: `crates/bridge-service/tests/android_negotiation.rs` updated deliberately (the real stock Android identity
  now negotiates `kdeconnect.notification` in and `.request` + `.action` out, never `.reply`); `gate.rs` unit test.
* Desktop: `notification_link` unit tests (stock shape with extras, strict ids, caps), `mirror` unit tests (cap, TTL,
  update, banner content hidden by default, rate limit, `notify-send` argument safety), tray and Home tests, end to end
  over pinned TLS in `crates/bridge-phone-sim/tests/data_link.rs`
  (`notifications_need_consent_are_bounded_dismissed_and_erased_when_withdrawn`).
* Phone core: `crates/bridge-android/tests/data_transport.rs` (declared only when enabled and without reply; per-computer
  permits; requests surface as events; off refuses sends). Kotlin: `NotificationMirrorTest` (empty default, gates, filters,
  stock field names and caps, opaque ids, dedupe, silent bulk, dismiss and button checks, strict parsing, readiness).
* Fuzz: `notification_wire` (bounded, control-free text; at most 3 buttons).
* Stock-peer interop: fixture level only. No live stock daemon or real phone capture of notification packets exists; the
  Samsung restricted-settings flow and the listener on a real device are hardware steps.

## Results

* `cargo fmt --all --check`: exit 0.
* `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
* `cargo test --workspace --no-fail-fast`: exit 0 (log `/home/ambsd/.cache/gb-tmp/nm-final2.log`).
* `./gradlew --offline :app:testDebugUnitTest :app:lintDebug`: exit 0 (log `/home/ambsd/.cache/gb-tmp/nm-gradle-full.log`).
* Not run: an APK on an emulator or phone, and the fuzz binaries themselves (the deterministic fuzz run inside
  `cargo test` covers the new `notification_wire` target).

## Not done

* Typed replies (`kdeconnect.notification.reply`) and inline reply: out of scope by request.
* App icons (see above), grouping/threads, mirroring from a stock phone's icon payload.
* Phone to computer "clear all", and closing an open banner when the phone cancels the notification (the entry is removed;
  `notify-send` cannot withdraw a shown banner, it expires after 10 s).
* A live capture against a stock KDE Connect daemon or a real phone.
