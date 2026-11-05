# Privacy review

Review date: 2026-11-03, against `origin/main` ab4609f30. Scope: every kind of phone data that can reach the computer,
what the computer and the phone store, what is logged, what leaves the local network, and how to erase it. GeekBridge
has no account system and no server of its own.

How to read the marks. Every claim names the code or document it rests on.

* **[code]** I read the cited code for this claim.
* **[doc]** Stated in the cited document only; I did not re-check the code.
* **[test]** A named automated test enforces it.
* **UNVERIFIED** I could not confirm it from here (for example anything that needs a real phone or a packet capture).

This is a code and document review. No network capture of a running system was made, so "nothing else leaves the
network" is a statement about the code, not a measurement (see [What leaves the LAN](#what-leaves-the-lan)).

## Summary

* The phone shares nothing with a computer until three things agree: the feature switch in the Android app, a
  per-computer allow on the phone (for the eight features that have one), and a per-phone consent on the computer.
  Exceptions are listed under [Defaults](#defaults-and-who-consents-where).
* On the computer, phone data is held in memory only. Disk holds pairing keys, the consent and share lists, settings, files
  you chose to receive or save, a short-lived scratch copy of a photo sent to the clipboard, and, for a debug-build
  phone only, its log.
* The only traffic that leaves the local network is the update check to GitHub. It is **on by default** on both
  platforms. Everything else is on the LAN, TLS with pinned certificates once paired, except the UDP announcement.
* Findings to act on are in [Findings](#findings).

## Consent features in code

The list below is checked against the code by `crates/bridge-core/tests/privacy_doc.rs`: if a feature is added to or
removed from `bridge_core::ipc::Feature`, or to the phone's per-computer grants, that test fails until this table is
updated. Do not reformat the markers.

Desktop consents (`consent.json`, `geekbridge-ctl consent DEVICE FEATURE on|off`), from `Feature::ALL` in
`crates/bridge-core/src/ipc.rs`:

<!-- guard:desktop-consent -->
| Feature | Covers |
| --- | --- |
| `sms` | Messages: conversations, texts, sending |
| `contacts` | the phonebook |
| `calendar` | reading calendar events |
| `calendar_edit` | creating, changing and deleting one-off events |
| `calendar_alerts` | desktop notifications before events (local only; no phone packet) |
| `clipboard` | clipboard text, links, HTML, images |
| `notifications` | mirrored phone notifications |
| `telephony` | call state, answer, decline, hang up, dial |
| `debug` | debug-build phone log and state dump (default **on** for a debug build only) |
| `battery` | battery level and charging state |
| `photos` | recent photo list, previews and picture requests |
| `call_audio_wifi` | EXPERIMENTAL: the far party of a live phone call, played on this computer over Wi-Fi (debug phone builds only) |
<!-- /guard -->

Phone per-computer grants (`geekbridge_grants` preferences), from `FEATURES` in
`crates/bridge-android/src/transport/engine/permits.rs` and `GrantFeature` in
`android/app/src/main/java/dev/geekbridge/android/ComputerGrants.kt`:

<!-- guard:phone-grants -->
| Grant | Desktop feature it pairs with |
| --- | --- |
| `calls` | `telephony` |
| `contacts` | `contacts` |
| `calendar` | `calendar` |
| `calendar_edit` | `calendar_edit` |
| `sms` | `sms` |
| `battery` | `battery` |
| `notifications` | `notifications` |
| `photos` | `photos` |
| `callaudio` | `call_audio_wifi` |
<!-- /guard -->

Not in either list: **clipboard** has a phone switch and a desktop consent but no per-computer phone grant, so its phone
switch applies to every paired computer. **File receiving** is not a `Feature`: the computer keeps `receive` and
`auto_accept` per phone in `share.json`, and the phone keeps `auto_accept:<computer id>` in `share_policy`
([code] `crates/bridge-service/src/share.rs`, `android/.../Share.kt`).

## Defaults and who consents where

A packet is dropped unread on the computer when the consent is off ([code] `crates/bridge-service/src/gate.rs`,
`classify_incoming` and the module comments of `phone_data.rs`, `mirror.rs`, `photos.rs`; consent is "anything not
recorded is off", `crates/bridge-service/src/private.rs` `ConsentStore::is_enabled`). The computer also declares only
the packet kinds it has code for, and a stock KDE Connect peer is never offered the GeekBridge-only kinds
([code] `LOCAL_INCOMING` / `LOCAL_OUTGOING` in `gate.rs`).

| Data | Default | Phone side | Computer side |
| --- | --- | --- | --- |
| SMS | off | Messages switch + Android SMS permissions (`READ_SMS`, `SEND_SMS`, `RECEIVE_SMS`, requested when switched on) + per-computer grant `sms` | consent `sms` |
| Contacts | off | Contacts switch + `READ_CONTACTS` + grant `contacts` | consent `contacts` |
| Calendar (read) | off | Calendar switch + `READ_CALENDAR` + calendars you tick (default none) + grant `calendar` | consent `calendar` |
| Calendar (edit) | off | separate switch + `WRITE_CALENDAR` (asked only when the switch is turned on) + grant `calendar_edit` | consent `calendar_edit` (and `calendar`) |
| Calendar alerts | off | none | consent `calendar_alerts` |
| Call state, answer, decline, dial | off | call-control switch + `ANSWER_PHONE_CALLS`; caller number/name only if you also allow `READ_CALL_LOG`; dialing needs `CALL_PHONE` and "Allow placing calls from my computer" (optional on-phone confirmation, off by default) + grant `calls` | consent `telephony`; dialing also needs a confirmation sheet on the computer |
| QR scan (pairing) | only on the Scan screen | `CAMERA`, asked on first use of **Scan QR code from my computer**; the camera runs only while that screen is open, the picture is decoded in memory by the native core and never stored, sent or logged; pairing also works without it (camera app, scanner app, paste) | none |
| Bluetooth check for call audio (Answer calls step) | on request | `BLUETOOTH_CONNECT`, asked only when you tap **Allow Bluetooth check**; lists the Bluetooth hands-free devices that are connected right now (names shown on the phone only, never sent); no scanning, no pairing from the app | none; nothing leaves the phone |
| Battery | off | "Share battery level" switch + grant `battery` | consent `battery` |
| Notifications | off | switch + Android Notification access + an app list that starts empty + grant `notifications` | consent `notifications` |
| Photos | off | "Show recent photos on the computer" switch + `READ_MEDIA_IMAGES` or, on Android 14, only the photos you pick (`READ_MEDIA_VISUAL_USER_SELECTED`) + grant `photos` | consent `photos` |
| Call audio over Wi-Fi (EXPERIMENTAL, debug phone builds only) | off | Settings, Experimental switch + the user starts an ADB helper over Wireless debugging + grant `callaudio` for that computer; each call needs a press (on the computer card or on the phone); speaking back is a further separate switch; a notification shows while audio is shared | consent `call_audio_wifi`; each call needs the press of "Listen here"; the indicator shows while audio plays |
| Clipboard | off | clipboard-sync switch | consent `clipboard` |
| Files, links, text | receiving off | sending is always an explicit Share-sheet action; receiving a file asks unless `auto_accept` is on for that computer | `receive` per phone (default off); `auto_accept` per phone (default off) |
| Device name | on, no switch | the name you set | the name you set; part of pairing |
| Debug diagnostics | off, except a debug build | only a debug build advertises the packets | consent `debug`, switched on once for a phone that advertises it ([code] `ConsentStore::default_debug_on`, `daemon.rs`) |

Sources for the phone column: the permission comments in `android/app/src/main/AndroidManifest.xml` and
`docs/phone-data-link.md` [doc]. I confirmed the grant list in code (above) but did not re-read every Kotlin switch.
**UNVERIFIED on a real phone:** the Android permission prompts and the Android 14 partial-access path were exercised
on an emulator and by unit tests only ([README](../README.md) feature table).

The README and the packet gate describe the same rule. Two cases are "on" without a switch and are not secret: the
device name and the identity announcement (below).

## Android permissions in the release APK

Every `<uses-permission>` in `android/app/src/main/AndroidManifest.xml` has a row here, and the debug-only
permissions (`src/debug/AndroidManifest.xml`: `RECORD_AUDIO`, `WRITE_CONTACTS`; its `WRITE_CALENDAR` duplicates the main one) are
not in the release APK. `crates/bridge-core/tests/privacy_doc.rs` fails if the two lists differ.
"Install" means granted at install time with no prompt; "runtime" means an Allow button the user presses.

<!-- guard:android-permissions -->
| Permission | Kind | Why |
| --- | --- | --- |
| `INTERNET` | install | The LAN link to a paired computer (TLS, pinned certificates) and the optional update check to GitHub |
| `ACCESS_NETWORK_STATE` | install | Notice Wi-Fi coming and going so the link redials |
| `CHANGE_NETWORK_STATE` | install | Prerequisite Android 14+ lists for a `connectedDevice` foreground service |
| `CHANGE_WIFI_MULTICAST_STATE` | install | Receive discovery broadcasts while discovery listening is on; also a `connectedDevice` prerequisite |
| `WAKE_LOCK` | install | A Wi-Fi lock (no CPU wake lock) while a computer is linked |
| `FOREGROUND_SERVICE` | install | The quiet link service, only while a computer is paired |
| `FOREGROUND_SERVICE_CONNECTED_DEVICE` | install | Android 14+ type declaration for that service |
| `RECEIVE_BOOT_COMPLETED` | install | Restart the link service after a reboot or an app update, only if a computer is paired |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | install | The setup step "Allow unrestricted battery" opens the system dialog; Android asks the user |
| `REQUEST_INSTALL_PACKAGES` | install | "Install update" hands a SHA-256-verified APK to the system installer; Android asks and the user confirms each install |
| `POST_NOTIFICATIONS` | runtime (Android 13+) | Link status, clipboard tap request, transfers, dial questions; everything works without it |
| `CAMERA` | runtime | The in-app QR scan screen, only while it is open |
| `READ_CONTACTS` | runtime | The phonebook feature |
| `READ_CALENDAR` | runtime | Calendar (read) |
| `WRITE_CALENDAR` | runtime | Only when "Let the computer edit my calendar" is switched on |
| `READ_MEDIA_IMAGES` | runtime | "Show recent photos on the computer" |
| `READ_MEDIA_VISUAL_USER_SELECTED` | runtime | The same on Android 14+ when the user shares only chosen photos |
| `READ_EXTERNAL_STORAGE` | runtime, Android 12L and older (`maxSdkVersion` 32) | The same on those versions |
| `READ_PHONE_STATE` | runtime | Coarse call state (idle, ringing, in call), no number |
| `ANSWER_PHONE_CALLS` | runtime | Answer, decline or end a call from a computer you allowed |
| `READ_CALL_LOG` | runtime | Android's only way to give the app the caller's number; the log itself is never read |
| `CALL_PHONE` | runtime | Place a call a paired computer asked for, after the computer's confirmation |
| `READ_SMS` | runtime, hard-restricted | Messages sharing, off by default |
| `SEND_SMS` | runtime, hard-restricted | Sending from the computer's Messages app |
| `RECEIVE_SMS` | runtime, hard-restricted | New texts for the computer |
| `BLUETOOTH_CONNECT` | runtime (Android 12+) | "Allow Bluetooth check" in the call-audio setup: lists connected hands-free devices, never scans or pairs |
| `BLUETOOTH` | install, Android 11 and older (`maxSdkVersion` 30) | The same on those versions |
<!-- /guard -->

The SMS permissions are hard-restricted on Android 13+ for an app that no store installed: the switch stays greyed out until
the user chooses "Allow restricted settings" in the app's info page, and the app says so
(`SmsRestriction`, `SmsReadiness.kt`). Notification access is restricted the same way. The app declares no exact-alarm
permission (its one alarm, the Agenda widget tick, is inexact), no `SYSTEM_ALERT_WINDOW`, no storage-wide permission, no location and no
`QUERY_ALL_PACKAGES`.

## Per category

For each: what travels, where it is kept, how long, what is logged, how it is erased.

### SMS

* Travels: stock `kdeconnect.sms.*` packets plus the GeekBridge `geekbridge.sms.status` ([doc] `docs/phone-data-link.md`).
* Computer: memory only, bounded counts per thread and per conversation, dropped on consent-off, unpair and restart
  ([code] `crates/bridge-service/src/phone_data.rs` module comment; [doc] `docs/state-formats.md` "Not persisted").
  Read marks stay on the computer and are **memory only**: they live in the service's per-thread state
  (`read_through_ms`, `unread_at_mark`; [code] `phone_data.rs` `mark_read`), not in the Messages app, which only calls
  the service and writes no file (grep of non-test `fs::write` / `File::create` in `crates/bridge-apps/src`). They do
  not survive a restart of the service, and are dropped on consent-off and unpair. After a restart, the unread counts
  come from the phone again.
* Desktop banner names the sender only. Message text appears in a banner only if you switch on **Settings -> Privacy
  -> Show message text in notifications** (or `geekbridge-ctl privacy set message_text on`); it applies at once, no
  restart ([code] `crates/bridge-service/src/privacy.rs`, `daemon.rs` `privacy_set`; [test]
  `crates/bridge-service/tests/privacy_api.rs`). The older switches still force it on and the page then says so: the
  marker file `<state dir>/sms-notify-content` or `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show`. See
  [Display privacy](#display-privacy-what-a-banner-or-the-call-card-shows).
* The tray tooltip and the waybar module show a **count** of unread messages (no names, no text); it follows the
  per-conversation read marks, so reading a conversation on this computer lowers it ([code] `tray/mod.rs` `collect`,
  `phone_data.rs` `unread_total`). It is a number visible on screen all day: turn Messages off for the phone to remove
  it.
* Phone: the app keeps a bounded list of send-request ids (`sms_requests`, no message text) in its preferences
  ([code] `PhoneDataAndroid.kt`; contents of `RequestLedger` not re-read, **UNVERIFIED** that it holds no number).
* Logged: no message content. The desktop service has no logging framework; its stderr lines are startup, state-file
  notes and error kinds ([code] grep of `eprintln!` in `crates/bridge-service/src`). A release phone writes no log at all
  (`GbLog` is a no-op unless `BuildConfig.DEBUG`, [code] `android/.../GbLog.kt`).
* Erase: unpair, consent off, or restart the service.

### Contacts

* Travels: stock `kdeconnect.contacts.*`; the phone strips `PHOTO` lines and caps each vCard at 64 KiB ([doc]
  `docs/phone-data-link.md`).
* Computer: memory only (same hub as SMS). Read-only: the computer never writes contacts.
* Logged: nothing from the contents (as for SMS).
* Erase: unpair, consent off, restart.

### Calendar (read, edit, alerts)

* Travels: `geekbridge.calendar.*`. Window is 12 months back to 24 months ahead, only for calendars you ticked
  (default none). Includes titles, locations, notes, reminders, attendee count and up to six attendee names, and the
  account name of each calendar (for example a Google address) ([doc] `docs/phone-data-link.md`).
* Computer: memory only. Edits are written by the phone into its own Calendar Provider; GeekBridge never talks to Google
  ([doc]). The computer does not persist the edit ledger ([doc] `docs/state-formats.md`). The phone keeps a bounded
  ledger of operation ids and result codes, no event text (`calendar_ops`, [doc]).
* Alerts: `notify-send` banners generated locally from the in-memory calendar.
* Erase: unpair, consent off, restart; edits already applied on the phone stay on the phone.

### Call state, answer, decline, dial

* Travels: `geekbridge.calls.*`, signalling only: state, optional number and name (only if you allowed reading the
  caller), answer/decline/hang up, and a dial request with a plain number. Call audio never travels on the link
  ([code] `crates/bridge-core/src/calls_link.rs` module comment).
* Computer: call state is in memory only ([doc] `docs/state-formats.md`). The call popup shows the number or name.
  The incoming-call card shows the number unless you switch off **Show caller number on the call card** (Settings ->
  Privacy; then it shows the contact name, or "Unknown caller"). `GEEKBRIDGE_CALL_OVERLAY=name|on` or the
  `call-overlay` file still decide first.
* Dial protection on the phone: at most 3 requests per computer per 10 minutes, a visible notice, optional on-phone
  confirmation ([doc] CHANGELOG 0.1.0; tested with a stand-in phone only, **UNVERIFIED** on hardware).
* Logged: the `diagnose` report and logs mask numbers ([code] `scrub` in `crates/bridge-service/src/diagnose.rs`).
* Bluetooth hands-free audio is a separate path between phone and computer and is not part of this link.
* **Decline with message.** On the incoming-call card and the banner, three short texts (yours, in **Settings ->
  Decline with message**, else three built-in ones) decline the call and send the chosen one to the caller as an
  ordinary text through the phone's Messages path. It is shown only when Messages is allowed for that phone on this
  computer **and** the phone's own Messages switch is on (it then accepts text sends), the phone link is up, and the
  caller's number is a plain one (digits, optional `+`). It shares Messages' limit of 10 texts a minute per phone and
  its refusals; nothing is sent on a refusal. The service reads the number from its own call view and takes only the
  *index* of a text from the apps, so the text and number never travel through a command line or the banner action
  ([code] `crates/bridge-service/src/call_control.rs` `decline_with_reply`). Stored: only the texts you typed, in
  `call-replies` (state folder, 0600). No history of what was sent is kept beyond Messages' own in-memory send list, and
  the call trace records only "text after decline: handed to the phone / not sent" with no number or text. There is no
  automatic reply: every text needs a press on that call.
* **Call-event trace** (so a call that did nothing can be understood afterwards). The service records, with a time:
  call-state changes per source, the phone link going up or down, the Bluetooth hands-free state, card launches and
  stops, answer/decline/hang-up/mute/audio/dial requests and how they ended, the audio route, and the Wi-Fi call
  audio session. A call, phone or source is shown only as a 6-hex-digit hash salted per run; a number only as its last
  two digits; **no name, full number, message text or address is recorded**. It is local only (never sent to the phone
  or the network), capped (128 KiB in memory, 256 KiB in `call-trace.log`, oldest lines dropped), and cleared when the
  service starts or with `geekbridge-ctl calls --trace --clear`. `geekbridge-ctl calls --trace [--json]` prints it;
  `geekbridge-ctl report` and `diagnose` include the newest 30 lines (also scrubbed) and `doctor` points to it
  ([code] `crates/bridge-service/src/call_trace.rs`; [test] `call_trace` unit tests and `privacy_doc.rs`).

### Display privacy: what a banner or the call card shows

Three choices control what is drawn on screen where someone else may read it. They are switches in **Settings ->
Privacy** and `geekbridge-ctl privacy [show|set KEY on|off]`; the service holds them (control requests `PrivacyStatus`
and `PrivacySet`) and a change applies at once.

| Choice | Default | When off |
| --- | --- | --- |
| `message_text` (Show message text in notifications) | off | a new-message banner names the sender only |
| `caller_number` (Show caller number on the call card) | on | the card shows the contact name or "Unknown caller", never the number |
| `phone_notification_text` (Show phone notification text) | off | a mirrored notification banner shows app and title, not the text |

* Stored in `<state dir>/privacy` (`name=on|off` lines, owner-only, atomic write). It holds three booleans, no content.
* Backward compatible: `sms-notify-content` or `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` force `message_text` and
  `phone_notification_text` on; `GEEKBRIDGE_CALL_OVERLAY=on|name` or the `call-overlay` file force `caller_number`.
  A forced value is shown as such and a change made in the UI is stored but does not override it ([test]
  `crates/bridge-service/src/privacy.rs`).
* The tray icon and its tooltip never carry a caller, message text or clipboard content; the unread **count** is the
  only message-derived value there.

### Battery

* Travels: `kdeconnect.battery`: percent, charging, low flag ([code] `crates/bridge-core/src/battery_link.rs`). Stock KDE
  Connect uses the same packet, so a stock peer interoperates.
* Computer: in memory for the life of the link; cleared when consent is withdrawn
  ([code] `daemon.rs` `set_consent`: `link.status.battery = None`). Shown in tray, Home, `geekbridge-ctl status`.
* Erase: consent off, link loss, unpair.

### Notifications

* Travels: stock `kdeconnect.notification*`: app name, title, text, ticker, time, button titles. Typed replies are not
  offered; the icon payload is never fetched ([code] `crates/bridge-core/src/notification_link.rs`).
* Phone: reads notifications only after Android's Notification access is granted, the feature is on, the link is up
  and the app is on your list; keeps nothing and logs no text ([code] `MirrorListenerService.kt`, `NotificationMirror.kt`).
* Computer: memory only, at most 50 per phone, each for at most 3 hours; erased when consent is withdrawn, the link
  ends or the phone is unpaired ([code] `crates/bridge-service/src/mirror.rs` `MAX_PER_DEVICE`, `TTL`; `daemon.rs`
  `set_consent`, `forget_consent`). Titles and texts are never logged. A banner shows app and phone; the text only if
  you switched on **Show phone notification text** (Settings -> Privacy, `geekbridge-ctl privacy`; the old
  `sms-notify-content` file forces it on too).
* Threat note: mirrored notifications can contain one-time codes. The banner text is hidden by default for that reason,
  but the in-app list on the computer shows them while they are held.

### Call audio over Wi-Fi (EXPERIMENTAL, off by default)

* Debug phone builds only; a release build has no helper and never advertises the packets. Bluetooth stays the supported way.
* Travels: `geekbridge.callaudio.*` (GeekBridge-only, inside the pinned TLS link): the far party's voice as Opus, and, only
  when you turn "Speak (experimental)" on, the computer's microphone. Nothing is recorded, stored or logged; logs carry
  counts and levels only. The phone shows a notification and the computer an indicator while audio flows; revoking a
  consent, ending the call or losing the link stops it ([code] `crates/bridge-callwifi/src/gate.rs`).
* The helper runs as Android's `shell` user, started over Wireless debugging; that user can do far more than call audio.
  The design note lists the risks and what Samsung may block (`docs/design/call-audio-over-wifi.md`).

### Photos: previews and full pictures

* Travels: `geekbridge.photos.request|list|thumb` (GeekBridge-only; a stock peer sees nothing). Newest 24 photos: opaque
  id, file name, time, size, dimensions, and a JPEG preview of at most 24 KiB and 256 px, only when the computer asks
  ([code] `crates/bridge-core/src/photos_link.rs`). The phone honours only ids it listed recently.
* Previews on the computer: memory only, at most 8 phones and 2 MiB of previews in all, erased on consent-off, link end,
  unpair or 30 minutes after the last refresh ([code] `crates/bridge-service/src/photos.rs` constants and module comment).
  Names and bytes are never logged.
* Full picture: only when you press Save or Copy. It travels over the ordinary file-share channel (pinned TLS) and the
  computer accepts only the photo it asked for ([code] `photos_link.rs`, `share.rs` `expect_photo`).
  * **Save** writes to `~/Downloads/GeekBridge` (the download folder's `GeekBridge` directory) and stays there.
  * **Copy to clipboard** goes through a private scratch folder `<save dir>/.geekbridge-photo-copy`, emptied at service
    start and swept of files older than 10 minutes ([code] `share.rs` `COPY_DIR`, `COPY_KEEP`, `sweep_copy_dir`).
    So a copied picture can sit on disk for up to 10 minutes; this is not "memory only".
* Residual risks accepted in [security review 6](reports/security-review-6.md): EXIF/GPS is not removed from full pictures
  on Android 8-9, and a saved photo's content is not verified beyond type and size.
* Erase: consent off, unpair, or restart (previews); delete saved files and the scratch folder yourself.

### Clipboard

* Travels: stock `kdeconnect.clipboard` text and URLs, computer to phone automatically while enabled; HTML and images on
  request; phone to computer only when you press **Paste from phone** (the phone answers only with an item copied within
  the freshness window while unlocked) ([code] `crates/bridge-service/src/clipboard.rs` module comment).
* Computer: the current text is held in memory while sync is active. Items that were on the clipboard before sync was
  enabled are never sent. Password-manager hinted items are skipped where the platform exposes the hint. Only the
  settings (window, hotkey, image limit) are stored (`clipboard-settings.json`).
* Logged: nothing; status carries counters, never content.
* Erase: switch clipboard sync off; unpair.

### Files, links and text

* Travels: stock `kdeconnect.share.request`; bytes on a payload socket pinned to the paired certificate; SHA-256 checked
  ([code] `crates/bridge-service/src/share.rs` module comment). Between two GeekBridge devices the phone also adds a
  per-share number to its share packet and the computer answers with `geekbridge.share.result`: only the number, one of
  seven fixed outcomes (accepted, declined, receive off, too large, busy, saved, link saved) and a fixed reason code. No
  file name, text or link content is in the answer. It goes inside the paired TLS link only to a phone that declared it
  (never to stock KDE Connect), and its capability name is kept out of the plaintext identity like every `geekbridge.*`
  name. The phone keeps the answers in memory (at most 32); the computer keeps nothing extra ([code]
  `crates/bridge-core/src/share_result.rs`).
* Stored: received files go to `~/Downloads/GeekBridge` on the computer and `Download/GeekBridge` (through MediaStore)
  on the phone. This is the one thing the product keeps because you asked it to. Names are reduced to one safe component
  and an existing file is never overwritten. Text and links are held for you; never opened and never put on the
  clipboard automatically.
* Erase: unpairing forgets the per-phone `receive` / `auto_accept` entries ([code] `share.forget_device`) but never
  deletes received files. Delete them yourself.

### Device name and identity

* Travels: device id, device name, type, protocol version, the stock (`kdeconnect.*`) capability lists, and the TCP
  port, in the clear in a UDP broadcast and in the first plaintext TCP line (see [Threat notes](#threat-notes)). The
  `geekbridge.*` capability names and the port hint are left out of both and travel only inside TLS once connected
  ([code] `crates/bridge-kde/src/identity.rs` `for_broadcast`, `addressed_to`, `to_packet`;
  `crates/bridge-service/src/pairing_flow.rs` `announcement`; `crates/bridge-android/src/transport/discovery.rs`
  `encode_announcement`). Every decision about a peer (heartbeat, negotiation, collision handling) reads the identity
  inside TLS, never the broadcast.
* The computer's name is 32 characters or fewer, no control characters, stored in `device-name`. Change it with
  `geekbridge-ctl rename`. `--no-announce` stops the computer broadcasting (phones then cannot find it).
* The phone stops broadcasting while every paired computer is linked ([code] `discovery.rs` `announce_loop`).
* Unpaired peers see none of your data, only this identity.

### Debug diagnostics (debug builds only)

* A release phone never advertises the debug packets. A debug-build phone sends its own process log and a state dump,
  after redaction, to a computer whose `debug` consent is on ([code] `crates/bridge-core/src/debug_link.rs`,
  `crates/bridge-service/src/phone_debug.rs`).
* Phone redaction (`DebugRedactor` in `android/app/src/debug/.../DebugLogStreamer.kt`): everything after a
  content-bearing key (body, address, number, name, title, text, clipboard, vCard, location, attendees, and so on), JWTs,
  bearer tokens, credentials, e-mail addresses, IP addresses (except a paired computer's), MAC-like and UUID values,
  long hex runs, phone-number-like digit runs and storage paths. The state dump is built from structured facts only and
  passes the redactor again ([code] `DebugState.kt` header).
* Computer: lines are sanitised (control and bidi characters), then written to `<state dir>/phone-logs/<device id>.log`,
  owner-only, rotated at 5 MiB keeping three older files ([code] `phone_debug.rs`). The files are not read by `diagnose`,
  only their sizes are reported ([code] `diagnose.rs`). The state dump from `geekbridge-ctl phone-state` is printed and
  never written to a file.
* Erase: the live and rotated logs of a phone are deleted when it is unpaired (from the computer, from
  `geekbridge-ctl unpair`, or by the phone) and when its `debug` consent is switched off ([code] `daemon.rs`
  `forget_consent`, `set_consent`; `phone_debug.rs` `erase`).
* Not covered by this: redaction is pattern-based. A new log line with unexpected wording could slip through; the unit
  test `DebugDiagnosticsTest` covers the known patterns ([code] `android/app/src/testDebug`). **UNVERIFIED** against a real
  phone's logcat.
* Release builds: `GbLog` writes nothing and `CrashNote` stores class names and frames only, no messages ([code]).

### Support reports

* `geekbridge-ctl diagnose` and Android **Copy report** are redacted: no control token, certificates or fingerprints,
  device names, IP addresses, phone numbers, clipboard or file content; devices appear as `device-1`, `device-2`
  ([code] `diagnose.rs` module comment, `crates/bridge-core/src/report.rs` `redact`, test
  `crates/bridge-core/tests/report_redaction.rs`).
* **Report a problem** builds a GitHub issue link from that redacted report and hands it to your browser. Nothing is
  sent by GeekBridge; you press Submit on GitHub ([code] `report.rs` module comment).

## What is stored where

Desktop state directory: `$XDG_DATA_HOME/geekbridge/service` (Linux, default `~/.local/share/geekbridge/service`),
`~/Library/Application Support/geekbridge/service` (macOS), `%LOCALAPPDATA%\geekbridge\service` (Windows), owner-only
(0700 / protected ACL), files 0600, written atomically ([doc] `docs/state-formats.md`; [code] `crates/bridge-kde/src/fsutil.rs`).

| Where | What | Sensitive | Retention |
| --- | --- | --- | --- |
| `identity.pem` (+`.bak`) | this computer's certificate and private key | yes (private key) | until purge |
| `trusted_peers.json` | paired phones, pinned certificate, device name | yes | until unpair |
| `consent.json`, `share.json`, `clipboard-settings.json`, `update-state.json`, `whats-new.json`, `privacy` (three display switches), `call-replies` (your own decline texts), `device-name`, `tray-paused`, markers | settings and per-phone opt-ins | low | until unpair (per-phone entries) or purge |
| `ipc-token` | local control token | yes (local) | until purge; replaced if damaged |
| `call-trace.log` | redacted call-event trace (hashed call ids, last two digits of numbers, no names) | low | emptied when the service starts, capped at 256 KiB, `geekbridge-ctl calls --trace --clear` empties it |
| `phone-logs/<id>.log` | debug-build phone log (redacted) | medium | rotated at 5 MiB, 3 old files; removed on unpair and when `debug` consent is withdrawn |
| `~/Downloads/GeekBridge/` | files and photos you received or saved | yes | until you delete |
| `<save dir>/.geekbridge-photo-copy/` | photo sent to the clipboard | yes | emptied at start, files older than 10 minutes swept |
| updater download folder `geekbridge-update-<random>` in the OS temp directory | a downloaded installer, only after you click Download | no | until you delete |
| `~/.local/state/geekbridge` | updater notes | low | removed by `--purge-data` ([code] `packaging/linux/install.sh`) |

Phone: `filesDir/kde/` (identity key, trusted computers), SharedPreferences (switches, grants, selected calendars,
notification app list, ledgers), `last-crash.txt` (class names and frames), `cache/updates/` (a downloaded APK only
after you ask), received files in `Download/GeekBridge` ([doc] `docs/state-formats.md`). `allowBackup` is `false` and the
data-extraction rules exclude every domain from cloud backup and device transfer ([code] `AndroidManifest.xml`,
`res/xml/data_extraction_rules.xml`).

Nothing is persisted on the computer for contacts, calendar, messages, call state, battery, mirrored notifications or
photo previews ([code] module comments listed above; [doc] `docs/state-formats.md`).

## What leaves the LAN

Everything GeekBridge does is between the phone and the computer on the local network, except the update check and
the things you start yourself.

**Update check (the only automatic internet traffic).** On by default on both platforms
([code] `Persisted::default` has `enabled: true` in `crates/bridge-update/src/lib.rs`; Android `KEY_AUTO` defaults to
true in `UpdateSection.kt`).

| Request | When |
| --- | --- |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases/latest` | desktop: about 15 s after the service starts, then about daily; Android: when Diagnostics & about opens, at most once per 12 h; or when you press Check |
| `GET <SHA256SUMS asset>` on `github.com` / `*.githubusercontent.com` | only when a newer release has an installer for this system |
| `GET <installer or APK asset>` | only after you press Download |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases/tags/dev`, then the small `dev-manifest.json` asset | **instead of** `releases/latest`, only if you chose the **dev channel** (default on Android *debug* builds, otherwise off); same host, same headers, same schedule |

Headers sent ([code] `crates/bridge-update/src/http.rs` and `lib.rs` `get_bytes`; `UpdateChecker.kt`):
`Host`, `User-Agent: geekbridge-updater/<version>` (Android: `geekbridge-android-updater/<version>`),
`Accept-Encoding: identity`, `Connection: close` (desktop), `Accept: application/vnd.github+json` (or
`application/octet-stream` for assets), `X-GitHub-Api-Version: 2022-11-28`, and `If-None-Match: <etag GitHub gave>` after
the first answer. No cookie, no account, no device or install id. `Authorization: Bearer` is sent only if the operator
sets `GEEKBRIDGE_UPDATE_TOKEN` (desktop, tester use), and only to `api.github.com`. What GitHub necessarily sees: your
IP address, the timing, and the version you run (in the User-Agent). DNS lookups for those names are visible to your
resolver. The desktop uses rustls with bundled roots and ignores system proxy settings; the Android check uses the
platform stack ([doc] `docs/updates.md`).

Turn it off: desktop `geekbridge-ctl update disable`, the **Check automatically** box, or `GEEKBRIDGE_NO_UPDATE_CHECK=1`
([code] `crates/bridge-service/src/lib.rs`); Android: the switch in the App updates card. Manual checks still work.

**Started by you, not by GeekBridge.** The release-notes, licence and **Report a problem** buttons hand a URL to your
browser through `xdg-open` or an Android `ACTION_VIEW` intent ([code] `update_ui.rs`, `report.rs`, `HomeActivity.kt`,
`MainActivity.kt`, `ReportActivity.kt`). A link received from a phone is held for you and never opened automatically
([code] `share.rs` module comment).

**Search for other outbound code.** I searched the crates and the Android sources for network clients. The only
internet-capable code is `bridge-update` (desktop) and `UpdateChecker.kt` (Android). Other sockets are loopback (the
control and discovery APIs on 127.0.0.1), the LAN link and discovery, and a pairing probe to an address you gave. The
phone's manifest holds `INTERNET` for that update check and the LAN link. There is no analytics, crash upload or
telemetry code ([code] grep for `reqwest`, `ureq`, `hyper`, `TcpStream::connect`, `HttpURLConnection`, `WebView`).
**UNVERIFIED:** no packet capture of a running desktop or phone was taken for this review. A capture while idle and
while using each feature would turn this statement into a measurement.

## Redaction in diagnostics

* Release builds of the phone write no log (`GbLog` is dead code; R8 removes it) ([code]). The phone's **Copy report**
  and the desktop `diagnose` use structured facts only and a second scrub ([code] `diagnose.rs` `scrub`, `report.rs`
  `redact`: e-mail addresses, file paths, IP and MAC addresses, UUIDs, long hex ids, phone numbers).
* The pattern redactor on the phone's raw log stream (`DebugRedactor`) exists in debug builds only and is described
  under [Debug diagnostics](#debug-diagnostics-debug-builds-only).
* The desktop service log (stderr / journal) contains no phone content ([code] grep of `eprintln!` in
  `crates/bridge-service/src`). Control-API errors may name paths; they go to the terminal, not to clients ([code]
  `lib.rs` near line 335).

## How to erase

| Goal | Desktop | Phone |
| --- | --- | --- |
| Stop one feature | `geekbridge-ctl consent DEVICE FEATURE off`, or the switch in the app. Clears that feature's in-memory data at once (notifications, photos, battery shown immediately; SMS/contacts/calendar with `data.set_enabled`) | the feature's switch, or remove the computer's grant |
| Forget a phone | `geekbridge-ctl unpair DEVICE` (or the unpair control in the desktop app): clears notifications, photo previews, phone data, share and clipboard entries, a debug-build phone's logs and the consent record ([code] `daemon.rs` `forget_consent`) and tells the phone | Diagnostics & about > Unpair (forgets the computer) |
| Remove everything | Linux: `./install.sh --uninstall --purge-data` deletes the state directory and the updater notes ([code] `packaging/linux/install.sh`); Windows/macOS: uninstall and delete the state directory listed above (**UNVERIFIED** that the Windows/macOS uninstallers offer a purge) | Settings > Apps > GeekBridge > Storage > Clear data, or uninstall |
| Saved files | delete `~/Downloads/GeekBridge` | delete `Download/GeekBridge` |
| Stop the broadcast | start the service with `--no-announce` | keep a paired computer linked (the phone stops broadcasting) |
| Stop internet traffic | `geekbridge-ctl update disable` | App updates card switch |

Unpair does not delete received files or saved photos.

## Threat notes

* **LAN observer.** The UDP announcement (broadcast to `255.255.255.255` and each local /24, every few seconds while
  not linked) carries device id, name, type and the stock `kdeconnect.*` capability lists in clear text; those must be there
  for stock KDE Connect to list and dial a device. The GeekBridge-only `geekbridge.*` names (which reveal that, say,
  photos or the debug link are on) are not broadcast and are not in the plaintext TCP line either; they go only inside
  TLS. An observer can still see which stock packet kinds are declared. They get no data from it. Do not put personal names in the device name.
  mDNS is not used.
* **Hostile LAN peer.** Discovery never grants trust. Pairing needs a code compared on both screens or a single-use QR
  token; links are TLS with pinned certificates ([doc] `docs/pairing.md`, [code] `gate.rs`). An unpaired peer can do
  nothing but be paired or closed.
* **Another user on the same computer.** State directory 0700 and files 0600; the control API listens on loopback only
  and needs a per-user token with an HMAC challenge ([code] `crates/bridge-core/src/ipc.rs`, `docs/state-formats.md`).
  Another process running as you can read everything you can; this is not a sandbox.
* **Screen and shoulder.** Banners hide message and notification text by default. The Messages, Calls and Photos windows
  show content on screen while open.
* **Stock KDE Connect peers.** A paired stock peer receives the stock packets (battery, notifications, clipboard,
  SMS, contacts) in the stock format once you allow them, and none of the GeekBridge-only kinds ([code] `gate.rs`).
* **Other apps on the phone.** The phone's received files in `Download/GeekBridge` are readable
  by apps with storage access; the app itself stores no backup copy of its data (`allowBackup=false`).
* **Residuals taken from earlier reviews:** [security-review-6](reports/security-review-6.md) (EXIF/GPS on Android 8-9
  full pictures, content of a saved photo, scratch file up to 10 minutes).
* **Not reviewed:** Windows and macOS clipboard, storage and ACL behaviour on real hardware; the GeekBridge `goodbye`
  and port-hint packets (no dedicated review, see [go/no-go](reports/stable-go-no-go.md) item 4); anything needing a real
  phone beyond pairing and link behaviour.

## Findings

1. **Debug logs outlived an unpair. Fixed.** `phone-logs/<device id>.log` and its rotated files are now deleted on
   unpair (every path) and when `debug` consent is withdrawn; covered by `control_api.rs` tests.
2. **"Nothing is written to disk" is slightly too strong.** The user guide and README say nothing from the phone is
   stored on the computer except files you ask for. The photo clipboard scratch copy (up to 10 minutes) and the
   debug-build log are two more cases; the wording in `docs/user-guide.md` now says so.
3. **The update check is on by default.** The README says it is "optional", which is true, but a reader may assume it
   is off. It is the only default-on network call and is listed above with its exact request.
4. **`geekbridge.*` capability names were visible on the LAN. Fixed.** The UDP announcement and the plaintext TCP line
   now carry only stock capabilities. Stock `kdeconnect.*` names remain visible (stock interop needs them). The
   desktop Agenda "phone advertises calendar" hint, which read the broadcast, no longer sees it; nothing else used it.
5. **Messages read marks** are memory only and do not survive a service restart (verified by reading the code, see SMS).

## Keeping this document true

* `crates/bridge-core/tests/privacy_doc.rs` compares the two marked lists above with `Feature::ALL`, and the phone
  grant list with `FEATURES` in `permits.rs`, and checks that the README links here.
* When you add a data category: add its consent, add a row in the tables above, describe storage and erase, and run
  `cargo test -p bridge-core --test privacy_doc`.
