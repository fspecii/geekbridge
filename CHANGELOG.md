# Changelog

All notable changes to GeekBridge. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/). `scripts/release.sh X.Y.Z` moves
**Unreleased** under the new version.

## [Unreleased]

### Added
- **Contacts** and **Calendar** desktop apps replace "Agenda". Contacts: searchable phonebook with letter sections, details,
  Call / Message / Copy. Calendar: month / week / day / agenda-list, Today, per-calendar colours and visibility, account
  names, reminders, attendees, recurring instances expanded by the phone, optional desktop notifications before events
  (`calendar_alerts` consent). Google Calendar is shared through the phone's own sync (no Google API keys or OAuth).
- Two-way calendar: create, edit and delete one-off events from the desktop (`geekbridge.calendar.write`, own per-phone
  consent `calendar_edit`, off by default; Android `WRITE_CALENDAR` is requested only when the user turns editing on).
  Operation ids make retries idempotent, uncertain results are reported as uncertain, repeating events stay read-only.
  `geekbridge-ctl calendar-add|calendar-delete|calendar-retry`. `geekbridge-agenda` remains the Calendar executable;
  `geekbridge-contacts` is new.
- Effortless first-time pairing: QR code / pairing-link pairing with a pinned certificate and a single-use two-minute token
  (no camera permission on Android), one-tap **Pair** from the lists of nearby phones and computers, the computer now
  announces itself on the LAN and listens for phones by default, plain-language errors, **Test connection** on both sides
  and a "what to do next" card. See `docs/pairing.md`. mDNS is not implemented.
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
### Fixed
- Desktop -> phone file transfers no longer fail at "Waiting" with a secure-channel or timeout error when the link arrived on
  a network other than the phone's default: the phone now dials the normal route first, and a route counts only after its TLS
  handshake finished. Tested with 500 MB both ways on the emulator.
- Unpairing on the computer now reaches the phone with the app closed: it forgets the computer, ends that computer's transfers,
  stops its background service and Home shows "No computer paired".
- Sharing a second time from the phone's Share sheet no longer shows the previous result instead of sending.

