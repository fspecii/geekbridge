# Changelog

All notable changes to GeekBridge. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/). `scripts/release.sh X.Y.Z` moves
**Unreleased** under the new version.

## [Unreleased]

### Added
- Calls: a keypad for the live call (`0-9 * #`) in the Calls window, a Keypad button on the in-call card and `geekbridge-ctl call tone KEYS`, for phone menus. Over Bluetooth hands-free it works where the backend accepts tones; over the phone link it is built (an extra `tone` request, a `canTones` capability, limits on both ends, nothing logged but a count) but Android only lets the default phone app play tones, so a stock phone does not offer it. Not run on a real phone.
- Calls: **Decline with message.** The incoming-call card and the Linux banner can decline a call and send one of three short texts ("Can't talk now, I'll call you back", "I'm in a meeting", "Call me in a few minutes", editable under Settings > Decline with message, stored on this computer) to the caller through the phone's Messages. Offered only when Messages is allowed for the phone on both sides and the caller's number is known; shares Messages' send limit. Nothing is ever sent automatically.
- Calls: a missed-call notification with Call back and Message, and a Recents list in the Calls window (calls since the service started, kept in memory only) with Call back and Message per row. Call back still asks for confirmation before dialling.
- Calls: an opt-in **Respect Do Not Disturb for incoming calls** switch (Settings -> Privacy, off by default). When the desktop is in Do Not Disturb (mako, dunst, SwayNotificationCenter, KDE, GNOME) a ringing call shows no card or banner, but the tray and Recents still record it. Not supported on Windows and macOS, where Settings says so.

## [0.1.0] - 2026-11-03

First version: a Rust phone bridge for Android and Windows, Linux and macOS desktops. Verified on Linux and an Android
emulator with scripted peers, and on one real phone (Samsung SM-F946B) for pairing and the link only; Windows and macOS
builds have not been run on real hardware (see "Known limitations").

### Highlights
- Copy on your phone, paste on your computer (and back), including images, with a hotkey you choose
- Send files and links between phone and computer over Wi-Fi, straight from the Android Share sheet
- Answer or decline phone calls from your computer
- Read and send texts, see your contacts, and keep your Google Calendar in sync with the desktop
- See recent photos, battery level and notifications from your phone on your computer, each off until you allow it
- Pair by scanning a QR code; updates are checked against GitHub and verified before you install

### Added

**Desktop apps and service**
- Separate native apps, each with its own executable, window and desktop identity, on one shared background service:
  Home (`geekbridge`), Calls, Messages, Contacts, Calendar (`geekbridge-agenda`), Clipboard and Share, plus the
  `geekbridge-ctl` command-line helper.
- `geekbridge-service`: authenticated loopback control API (per-user token, HMAC challenge), passive and announced LAN
  discovery, expiring device records, one TLS link per paired phone, per-phone opt-ins for every feature.
- Nothing OS-inspired design on desktop and Android: true black or warm off-white, a single red accent, code-drawn
  dot-matrix text, dot-calendar months, light and dark themes, keyboard navigation and contrast tests.

**Pairing and security**
- KDE Connect-compatible transport (TLS, certificate pinning) with an explicit code compared on both screens.
- QR code / pairing-link pairing: pinned certificate, single-use two-minute token, no camera permission on Android.
  One-tap **Pair** from the lists of nearby phones and computers, **Test connection** on both sides, plain-language
  errors and a "what to do next" card ([docs/pairing.md](docs/pairing.md)). mDNS is not implemented.
- Unpairing on the computer reaches the phone even with the app closed (the phone forgets the computer and stops its
  service); KDE-style unpair on both sides.
- Two security reviews with the findings fixed and covered by regression tests (replay, reconnect, leaks, per-computer
  opt-ins, QR token lock-out, SMS and notification handling): [docs/reports/security-review.md](docs/reports/security-review.md),
  [docs/reports/security-review-2.md](docs/reports/security-review-2.md), [SECURITY.md](SECURITY.md).

**Clipboard**
- Text, links, HTML and images; per-phone opt-in, freshness window, loop and echo suppression, sensitive items
  skipped. Computer to phone is automatic, phone to computer is on demand with the **Paste from phone** hotkey
  (default Ctrl+Shift+V) because Android 10+ only lets the focused app read the clipboard. Event-driven watching on
  the desktop ([docs/clipboard-sync.md](docs/clipboard-sync.md)).

**File, link and text sharing**
- Both directions, AirDrop-style: **Send to computer** in Android's Share sheet, drop files on the Home window,
  `geekbridge-share` and `geekbridge-ctl send-file`. Pinned-TLS payload sockets, SHA-256 verified transfers up to 8 GiB,
  receiving is off per phone until allowed, files land in `~/Downloads/GeekBridge` and the phone's `Download/GeekBridge`.
  500 MB transfers verified both ways on the emulator ([docs/sharing.md](docs/sharing.md)).

**Calls**
- Incoming-call popup with Answer, Decline, Hang up and Mute (where a backend can mute) on the desktop, with desktop
  notification actions on Linux.
- Two merged control paths: the GeekBridge link (Android `ANSWER_PHONE_CALLS`, opt-in, never carries audio) and
  Bluetooth hands-free backends (Linux PipeWire telephony, Windows WinRT, macOS helper) for call audio, which needs a
  Bluetooth controller on the computer ([docs/call-control.md](docs/call-control.md)).
- Emulated-call test rig (`geekbridge-phone-sim`, `geekbridge-call-e2e`, scenarios S1-S8) that runs without a phone
  or Bluetooth ([docs/emulated-calls.md](docs/emulated-calls.md)).

**Messages, Contacts and Calendar**
- Messages: SMS conversations, history paging, sending and new-message notifications over the stock
  `kdeconnect.sms.*` packets, with honest send status (handed to phone / accepted / sent / delivered / unknown). No MMS,
  group or RCS; Google Play restricts SMS permissions to the default messaging app and GeekBridge does not become one.
- Contacts: searchable phonebook with letter sections and details, with Call, Message and Copy actions. Read-only.
- Calendar: month, week, day and agenda-list views, per-calendar colours and visibility, reminders, attendees, recurring
  instances expanded by the phone, optional desktop notifications before events. Google Calendar is shared through the
  phone's own sync (no Google API key or OAuth).
- Two-way calendar: create, edit and delete one-off events from the desktop (`geekbridge.calendar.write`, separate
  per-phone consent, off by default, `WRITE_CALENDAR` requested only when switched on). Operation ids make retries
  idempotent; repeating events stay read-only. `geekbridge-ctl calendar-add|calendar-delete|calendar-retry`.
- Contacts, calendar and SMS are shared only after a per-phone opt-in on both sides; nothing is stored on the computer
  ([docs/phone-data-link.md](docs/phone-data-link.md)).

**Android app**
- Kotlin app on a shared Rust core over JNI (arm64-v8a and x86_64, 16 KiB aligned), Android 8+ (min SDK 26), no AndroidX
  or Compose. Setup wizard, Home with status tiles, Diagnostics with a redacted **Copy report**, Share sheet target,
  Quick Settings clipboard tile.
- Foreground link service (`connectedDevice`) restarted at boot, **Stay connected** switch, battery-optimisation step
  with guidance for Samsung, Xiaomi, Huawei/Honor, OnePlus, OPPO/realme, vivo and Nothing
  ([docs/android-power.md](docs/android-power.md)).
- R8 and resource shrinking on release builds; release APK about 6.2 MB with size budgets enforced by
  `scripts/android-perf.sh` ([docs/android-perf.md](docs/android-perf.md)).

**Updates**
- Update check against GitHub Releases on desktop and Android, SHA-256 verified download, never installs by itself;
  the Android self-update was verified end to end on the emulator ([docs/updates.md](docs/updates.md)).

**Linux and Hyprland / Omarchy integration**
- Installer (`install.sh`, `.deb`) with a systemd user unit and XDG autostart; application entries for every app and a
  **Paste from Phone** launcher entry.
- `geekbridge-ctl hotkey install|status|uninstall` binds the paste key in Hyprland (Lua and conf configs, backup and
  dry run), floating window rules for the call and share windows, mako/Quickshell notification actions and a waybar
  status module ([docs/omarchy.md](docs/omarchy.md)).

**Tooling and documentation**
- `scripts/bootstrap-android.sh`, `build-android.sh`, `build-desktop.sh`, `package-linux.sh`, `release.sh`,
  `perf-check.sh` and release workflows for Linux, Windows (Inno Setup) and macOS (`.dmg`).
- `geekbridge-ctl diagnose` redacted support bundle, e2e scripts for the emulator (QR pairing, call answer/reject, SMS,
  calendar, share, update), chaos and soak scripts, real-device test plan and quickstart, release checklist.
- User guide with symptom-keyed troubleshooting, architecture, protocol and per-feature docs under `docs/`.

**Since the first release candidate**
- Recent phone photos on the computer (gap 6 of the parity list): a *Phone photos* tab in the Share window shows the newest
  24 photos as small previews, with **Save** and **Copy to clipboard**, and `geekbridge-ctl photos`. GeekBridge-only packets
  (`geekbridge.photos.request`, `.list`, `.thumb`), so a stock KDE Connect peer sees nothing. Off by default on both ends,
  per computer on the phone; Android 14 partial photo access is supported; previews only (24 KiB each) until a full picture
  is asked for, which then travels over the ordinary file-share channel; memory only on the computer. MMS attachments
  (gap 5) are designed but not built. See docs/design/recent-photos.md and docs/reports/recent-photos.md.
- A manual disconnect on the computer is no longer undone by the phone's redial: a GeekBridge-only `geekbridge.goodbye`
  packet (reasons `user_disconnect`, `shutdown`, `unpair`) makes the phone stay off that computer for 5 minutes or until
  either side connects; a service shutdown makes it redial with the normal back-off. The computer's secure identity also
  carries its real listening port (`geekbridgePort`) so a phone that only knows its address no longer guesses 1716.
  Stock KDE Connect peers see neither. See docs/reports/goodbye-and-port-hint.md.
- Phone notification mirroring with dismiss and button presses (stock `kdeconnect.notification`, `.request` and `.action`
  packets, so stock KDE Connect interoperates; typed replies are not offered). Off by default on both ends: a switch,
  Android's Notification access (with guidance for the restricted-settings case), an app list that starts empty and a
  per-computer allow on the phone, and a per-phone consent on the computer. Shown as desktop banners with a Dismiss
  button, `geekbridge-ctl notifications`, a Home button and a tray item; kept in memory only (50 per phone, 3 hours).
  See docs/reports/notification-mirroring.md.
- `geekbridge-ctl doctor`: checks the service, the firewall rule for TCP and UDP 1716-1764 (ufw, firewalld), the desktop
  session, paired phones, clipboard, calls and Messages, prints the fix for each problem and exits 1 if a check failed.
- `geekbridge-ctl call dial NUMBER [--yes]` and a confirmation sheet behind the Calls keypad: the computer asks the phone
  to place a call (digits and a leading `+` only; needs *Allow placing calls from my computer* on the phone). Tested
  against a stand-in phone only. A PC without a Bluetooth controller is now reported as information: call control works,
  the voice stays on the phone ([docs/call-audio-options.md](docs/call-audio-options.md)).
- "Why is it disconnected": a reason code, a plain sentence and a suggested fix for every paired phone without a link, in
  Home, the tray, waybar, `geekbridge-ctl status` and `doctor`.
- Messages permission guidance on the phone for sideloaded Android 13+ ("Allow restricted settings") and the computer
  picking up a permission change without a manual reconnect. Checked on an emulator and by analysis; not yet confirmed on
  the real phone.
- `install.sh --uninstall --purge-data` (Linux) deletes the pairing and settings directory; plain `--uninstall` keeps it.
- Phone battery level and charging state on the computer (stock `kdeconnect.battery` packets, so stock KDE Connect
  interoperates). Off by default on both ends: a "Share battery level" switch plus a per-computer allow on the phone,
  and a per-phone consent on the computer. Shown in the tray tooltip, the Home phone card and
  `geekbridge-ctl status` / `status --waybar`; kept in memory only. See docs/kdeconnect-interop.md.
- Desktop first-run guide on Home while no phone is paired: install QR and link, the pairing steps, optional features,
  this computer's name, update status, start at login, and the firewall fix to copy. Windows and macOS parity for
  `doctor` and `geekbridge-ctl firewall`/`autostart`: Windows Defender Firewall and macOS application-firewall detection
  with copyable commands (nothing is run elevated), autostart status parsing, and a hint when the state directory
  inherited loose permissions.
- Phone-side protection for dialing from the computer: at most 3 dial requests per computer per 10 minutes, a visible
  notice when a call is placed, and an optional on-phone confirmation (off by default).

**Also in this release**
- "What's new" after an update: a dismissible card at the top of Home on Android and the desktop (once per version, never on a fresh install), `geekbridge-ctl whats-new`, and a link to the full release notes. The text is generated from this file by `scripts/gen-whats-new.sh` and checked for drift by tests.
- Tray / menu-bar icon in the service (Linux StatusNotifierItem + dbusmenu, Windows notification area; the macOS menu-bar
  item is type-checked only): dot-matrix icon with connected / disconnected / ringing / update / transfer states, a menu
  with Open, Paste from phone, Send file, Answer / Decline, Pause sync, Check for updates and Quit. Optional: no tray
  host means nothing is shown; `geekbridge-ctl tray disable` or `--no-tray` turns it off. Icons in `assets/tray/`
  (`scripts/gen-tray-icons.sh`), headless check in `scripts/e2e/tray-headless.sh`.
- Messages: unread badges, search, date separators, SIM choice (phones that list their SIMs), SMS segment counter
  (GSM/UCS-2), local-only read marks, per-conversation memory cap, MMS "Attachment" placeholders, notification Reply
  action (text hidden unless chosen), plain-language failure reasons and exact SMS-permission steps in the setup wizard.

**Added after the first draft of these notes (2026-11-04)**
- Settings -> Privacy: switches for **Show message text in notifications** (off), **Show caller number on the call card**
  (on) and **Show phone notification text** (off), applied at once through the service (`PrivacyStatus` / `PrivacySet`,
  `geekbridge-ctl privacy [show|set KEY on|off]`). The `sms-notify-content` file and the old environment variables still
  force a value, and the page says so. An unread-message count is in the tray tooltip (all platforms) and the waybar
  `unread` field, and falls when a conversation is read on this computer. Messages: the To field takes a contact name
  (list with Up/Down/Enter, a chooser for several numbers), a refused reply comes back into the box with the reason, and
  Enter that confirms an input-method composition no longer sends.
- The computer now tells a GeekBridge phone what became of its share (`geekbridge.share.result`, GeekBridge-only, never sent
  to stock KDE Connect, kept out of the plaintext identity): accepted, declined (or lapsed), receiving off, too large, busy,
  and for text and links saved (a link is kept, never opened). The Share screen says so in plain words and stops waiting at
  once on a refusal; an older computer keeps today's hint. See docs/reports/share-result-packet.md.
- Dev update channel (docs/updates.md#dev-channel, docs/reports/dev-channel.md): a rolling `dev` GitHub prerelease with the
  newest main build (debug-signed APK, Linux archive, `SHA256SUMS`, `dev-manifest.json`), published by
  `scripts/release-dev.sh` (dry run unless `--publish`) or the opt-in `dev-release.yml`. The Android app (debug builds
  default to Dev; **Update channel: Stable / Dev** in the App updates card) and `bridge-update`
  (`geekbridge-ctl update channel dev`, `GEEKBRIDGE_UPDATE_CHANNEL`, Details panel) can follow it. Debug APKs now have
  `versionCode = 20000 + commit count`, so each main build is accepted as an update of the previous one.
- Incoming-call card: the service starts `geekbridge-calls --overlay` on every ring, a small always-on-top Answer / Reject
  card in the top-right corner (not inside the Calls window), closing itself when the call ends. Hyprland rules in
  `geekbridge-ctl hotkey install`, `geekbridge-ctl call-overlay-test`, a `doctor` line, `GEEKBRIDGE_CALL_OVERLAY=off|name`.
  See docs/omarchy.md.
- Linking a phone is now like WhatsApp Web (docs/reports/pairing-ux-2.md). Computer: **Link a phone** shows a big QR code
  that renews itself before its 2 minutes are up, with a live line (Waiting for phone… / Phone scanned — confirm on
  phone / Paired ✓ with X); with nothing paired yet it is already on screen, and it is revoked when the view is left.
  Phone: **Scan QR code from my computer** opens an in-app camera screen (Camera2; the picture is decoded in memory by
  the native core, new action `pairing_decode_qr`, no scanner library, no Play services; new `CAMERA` permission asked on
  first use, optional), then **Connect to X?**, **Pair**, and Home says **Paired ✓ with X**.
- **Don't have GeekBridge on your computer yet?** on the phone: share sheet or mail draft with the public releases page
  and three install lines (no secrets), a copyable short address.
- **Unlink** replaces Forget/Unpair wording on both sides; the phone's pair step has an Unlink button per computer.

### Fixed
- Desktop to phone file transfers no longer stall at "Waiting" with a secure-channel or timeout error when the link
  arrived on a network other than the phone's default: the phone dials the normal route first, and a route counts only
  after its TLS handshake finished.
- Sharing a second time from the Share sheet no longer shows the previous result instead of sending.
- Reconnect, replay and leak bugs found in code review and chaos testing, each with a regression test.
- Idle footprint: event-driven clipboard watching and UI poll back-off on the desktop; no wake locks or timers in the
  Android link service ([docs/performance.md](docs/performance.md)).
- Home-network hostility (docs/reports/network-hostility.md): the LAN announcement now goes to each NIC's real
  directed broadcast (a /16 Wi-Fi was announced to `x.y.z.255`, which nobody listens on); "different network" in
  `test-connection` and the why-not-connected answer uses the real prefix (a /16 or /23 home network was reported as
  another network); `test-connection` answers within the control deadline when a router drops TCP silently (it used
  to end as "cannot reach the service") and names client isolation; the service also listens on `[::]:1716`, so
  the IPv6 address in an IPv6-only QR works; a wrong clock (year 2020, 1970) no longer stops the service, rotates
  nothing and no longer makes paired devices refuse each other's certificate (dates are not enforced; pinning is).
- Real-device upgrade safety (docs/reports/preflight-upgrade.md): `scripts/run-desktop.sh --fresh` no longer wipes a state
  directory that holds pairings (it needs `--forget-pairings` and then moves the old state aside instead of deleting it);
  a running service that is older than its binary is no longer reused silently (`--restart` runs the new binary and keeps
  every pairing); `scripts/realdevice.sh install` compares the signing key and versionCode of the APK with the app on the
  phone first and stops before Android would refuse the update (a build signed with `~/.android/debug.keystore` instead of
  the project key, or a downgrade), saying why and what it costs.

### Changed
- The service size budget is 6.0 MB (was 5.8); the Android libraries are linked with lld `--icf=all` (about -100 kB per
  library); the idle service drops its idle code pages after start (RSS about 3.5 MB instead of 8 MB; mappings with a
  privately written page are never touched).
- When the phone and the computer dial each other at the same moment, the connection dialed by the smaller device id wins
  on both ends (shared `bridge_kde::tiebreak`), so the two connections no longer cancel each other.
- Switching Clipboard sync on while a link is up reconnects once if the link had not declared the clipboard packets.
- The pairing trust record is written before the accept is sent.
- While the phone's screen is off its heartbeat is every 60 s (20 s with the screen on) and it stops broadcasting while
  every paired computer is linked. Code-audited and tested; no battery measurement on a real phone yet.
- Docs: one real phone (Samsung SM-F946B) has been paired; Windows and macOS are CI-tested only and have no prebuilt
  binaries. The Android update check never moves a debug build to a release build (uninstall once).
- The phone's redials and announcements stay on Wi-Fi when mobile data is the default network (One UI "Switch to mobile
  data"): the core binds its outgoing connections and the announcement socket to the Wi-Fi IPv4 address, with a fallback
  to the plain route. Wi-Fi lock claims corrected (it does not keep the radio awake with the screen off on Android 14+),
  and the debug state shows a link-health section (last close reason, network then, silence before it).
  See docs/reports/wifi-binding.md.
- Safety net for that binding (docs/reports/wifi-binding-safety.md): a bound dial whose handshake does not finish in 6 s,
  or a bound link that dies within 10 s without the computer being heard, makes the core dial unbound at once and for
  10 minutes; a `bind_wifi` preference (default on, debug builds can flip it) and `GB_BIND_WIFI=0` switch binding off;
  the debug Link health section shows the switch and the last fallback reason.

### Security
- Recent photos review (docs/reports/security-review-6.md): a photo offer larger than any photo fails the request, a photo
  claim is atomic, withdrawing photos cancels a download in progress and erases clipboard copies, the scratch folder is
  re-made private (links replaced) and emptied at start, only picture-named files are listed, clipboard copies are
  header-checked (type, 250 MP) first, ids are per computer and the phone ignores request floods, and file names lose
  more bidirectional/line-separator characters.
- Privacy review (docs/privacy.md): a debug-build phone's logs are deleted on unpair and when its debug consent is
  withdrawn, and the LAN broadcast and the plaintext TCP line no longer carry `geekbridge.*` capability names (they go
  only inside TLS).

### Known limitations
- One real phone (Samsung SM-F946B) has been paired, and only pairing and link behaviour were verified on it. Every other
  feature is verified against an Android 16 emulator and scripted peers; real-device steps are in [docs/real-device-quickstart.md](docs/real-device-quickstart.md). Emulator NAT drops UDP broadcast, so
  LAN discovery itself is covered by unit and integration tests only.
- Call answering has only been tested with simulated signalling. Bluetooth hands-free audio (SCO, codecs) is unverified
  on any hardware, and needs a Bluetooth controller on the computer.
- Windows and macOS binaries are unsigned and have never been run; they are type-checked and built in CI definitions
  only. Release builds are made on the maintainer's Linux machine (Linux and Android assets only).
- Vendor battery screens (Samsung, Xiaomi, Huawei, OnePlus, OPPO, vivo) are unit-tested for intent order only, not run on
  those devices.
- Android 10+ restricts clipboard reads to the focused app, so phone to computer paste is on demand.
- v0.1.0 is signed with the release key. The earlier v0.1.0-rc.1 APK was debug-signed, so a phone running it must uninstall
  it (losing app data) before installing this version; later updates install in place.
