# Desktop UI polish, pass 2 (first impression)

> Status: snapshot from 2026-11-03; see git history for later changes.

Scope: `crates/bridge-apps` empty states (service down, no phone, firewall/guest Wi-Fi), call keypad, Messages, Share,
Clipboard, Calendar, Contacts, keyboard/AccessKit, light and dark. Everything ran under private Xvfb displays, private state
directories and loopback ports; the live Hyprland session and the real `~/.config` were not touched.

## Method

Empty-state shots with the service down (no token) and with the service up but no phone, light and dark, every app window.
Goldens (`make golden-desktop`) were green before and after, so no existing golden image changed.

## Defects fixed

| # | Defect | Fix |
|---|---|---|
| 1 | Home tile and sidebar card said "Looking for the service / Checking the background service" forever when the service was down, while the sidebar footer said "not running" | A failed check now shows OFF / "Service not running" / "Click for what to do" in the accent colour (`home_app.rs`, unit test) |
| 2 | Service-down and no-phone texts named no next step; the first-run `geekbridge-ctl doctor` was invisible in the GUI | The service-down error (shared by Contacts, Calendar, Clipboard, Messages, Pair details, Share) and the Details panel guidance now end with "Run geekbridge-ctl doctor"; the no-phone-found guidance names the usual cause (firewall TCP/UDP 1716-1764, guest Wi-Fi) and the doctor. Strings live in `i18n/en.properties` (`doctor.*`, `help.*`, `home.service_off.*`) |
| 3 | Calendar sidebar showed a dangling "THIS COMPUTER MAY" heading with nothing under it when no phone is paired | Heading only when a phone is paired (`apps/calendar.rs`) |

## New goldens

`home-service-off` and `contacts-service-off`, light and dark, std and min (8 PNGs): the empty state is now protected
(`scripts/golden-desktop.sh`, uses an empty state dir so the token is missing). Caveat: the fixture still has a reachable
discovery API, so the Home footer reads "Background service running" next to the OFF tile; a real outage shows both as off.

## Checked, fine

- Keyboard: every custom control is focusable and Tab order is app first, connection strip last (existing a11y tests pass);
  Enter/Escape only bound on the incoming-call sheet.
- AccessKit: named controls, live-region strip, named scroll bars and text fields; no new unlabeled widgets added.
- Dark/light follow the system; both themes shot for every empty state; text wraps in the 380 px windows.

## Open

- Calls keypad blocker line stays a one-liner ("Can't call yet: no phone found."); the doctor hint is in Details because the
  320 px window has no room for a second line.
- Contacts footer repeats the empty-state sentence when the service is down (pre-existing duplication).
- 2x HiDPI and the real phone remain for the real-device run (`WINIT_X11_SCALE_FACTOR=2`).
- Share and the system tray were reviewed from code only; the tray is disabled under the sandbox (`GEEKBRIDGE_NO_TRAY`).
- Other agents have uncommitted edits in `calls.rs`, `call_ui.rs`, `call_feed.rs` and `scripts/golden-desktop.sh`
  (build-dir line); they were left out of this change.

## Verification

`cargo fmt -p bridge-apps -- --check` 0, `cargo clippy -p bridge-apps --all-targets -- -D warnings` 0,
`cargo test -p bridge-apps` all pass (176 tests incl. i18n catalogue), `make golden-desktop` 68/68 before, 76/76 with the new shots.
