# Changelog

All notable changes to GeekBridge. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/). `scripts/release.sh X.Y.Z` moves
**Unreleased** under the new version.

## [Unreleased]

### Added
- Pairing over the local network with certificate pinning and a verification code compared on both screens.
- Android app: setup wizard, Home, Diagnostics, battery-optimisation guidance for Samsung, Xiaomi, Huawei/Honor,
  OnePlus, OPPO/realme and vivo.
- Native desktop apps for Calls, Messages, Agenda and Clipboard on one shared background service
  (Linux, Windows, macOS builds; Windows and macOS have not been run on real hardware yet).
- Clipboard sync: text, links, HTML and images, per-phone opt-in, freshness window, paste-from-phone hotkey.
- File, link and text sharing in both directions, including the Android Share sheet.
- Incoming-call popup with answer, decline, hang up and mute where a Bluetooth calling backend exists.
- Update check against GitHub Releases with SHA-256-verified download (never installs by itself).
- `geekbridge-ctl diagnose` and Android Diagnostics **Copy report**: redacted support bundles.
- Real-device test plan (`docs/real-device-test-plan.md`), release checklist, `SECURITY.md`.
- Agenda: contacts (stock `kdeconnect.contacts.*`) and a read-only calendar (`geekbridge.calendar.*`, incremental with
  tombstones) from the phone, per-phone opt-in on both sides, memory only on the computer.
- Messages: SMS conversations, history paging, sending and new-message notifications over the stock
  `kdeconnect.sms.*` packets, with honest send status (handed to phone / accepted / sent / delivered / unknown).
  Android side needs the SMS permissions; no MMS, group or RCS. See `docs/phone-data-link.md`.

### Known limitations
- Call answering and audio have only been tested with simulated signalling, not a real call.
- Windows and macOS binaries are unsigned and untested on real systems.
- Messages and Agenda need an Android build with the SMS/contacts/calendar permissions; Google Play restricts SMS to the
  default messaging app, GeekBridge does not become one. Only tested against the emulator, not a real phone.
