# Changelog

All notable changes to GeekBridge. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/). `scripts/release.sh X.Y.Z` moves
**Unreleased** under the new version.

## [Unreleased]

## [0.1.0] - 2026-11-02

First version: a Rust phone bridge for Android and Windows, Linux and macOS desktops. Everything below is verified on
Linux and an Android emulator with scripted peers; **no real phone has been paired yet** and Windows and macOS builds
have not been run on real hardware (see "Known limitations").


### Highlights
- Copy on your phone, paste on your computer (and back), including images, with a hotkey you choose
- Send files and links between phone and computer over Wi-Fi, straight from the Android Share sheet
- Answer or decline phone calls from your computer
- Read and send texts, see your contacts, and keep your Google Calendar in sync with the desktop
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

### Fixed
- Desktop to phone file transfers no longer stall at "Waiting" with a secure-channel or timeout error when the link
  arrived on a network other than the phone's default: the phone dials the normal route first, and a route counts only
  after its TLS handshake finished.
- Sharing a second time from the Share sheet no longer shows the previous result instead of sending.
- Reconnect, replay and leak bugs found in code review and chaos testing, each with a regression test.
- Idle footprint: event-driven clipboard watching and UI poll back-off on the desktop; no wake locks or timers in the
  Android link service ([docs/performance.md](docs/performance.md)).

### Known limitations
- No real phone has been paired. Everything is verified against an Android 16 emulator and scripted peers; real-device
  steps are in [docs/real-device-quickstart.md](docs/real-device-quickstart.md). Emulator NAT drops UDP broadcast, so
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
