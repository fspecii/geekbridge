# Desktop first-run onboarding (Home, no phone paired)

Walked with the headless golden harness (private Xvfb, private service state; never the live service, real config or Hyprland).

## What a new user saw before

- Home showed six tiles, "Phone link: No phone paired / Click to pair a phone", and nothing else. No install step, no word about the Android app.
- A blocking firewall was only visible after a failed pairing, in `geekbridge-ctl doctor`/`firewall`. Home never mentioned it.
- The computer's name and the update status lived only in Settings. Start at login was not explained anywhere in the GUI; a stopped service showed "Service not running / Click for what to do".
- Bug found: the phone card printed the "why is it disconnected" lines twice for an unlinked phone (duplicate block). Removed.

## What Home shows now

A "Get started" card, shown while no phone is paired (or the service is down; hidden while the first service answer is pending, and once a phone is paired):

1. Install the Android app: QR code and link to the latest release page, "Open releases page", "Copy link".
2. Pair: explains same Wi-Fi + Pair in the app, names phones already announcing themselves ("Found nearby: ..."), "Pair a phone" opens the Pair view.
3. Optional features: battery, notifications, messages, clipboard, file sharing stay off until allowed.

Plus, in the same card:

- Firewall: ufw on with readable rules and no rule for 1716-1764 (TCP and UDP) is "blocking": names the problem, shows the exact `sudo ufw allow ...` lines in a box and a "Copy firewall commands" button. ufw on with root-only rules, or firewalld running, is a softer "is on, cannot verify" note with the same copy (`sudo firewall-cmd ...`). GeekBridge never runs sudo; the card says so.
- Service not running: leads with `systemctl --user enable --now geekbridge-service.service` (non-Linux: `geekbridge-ctl autostart enable`) and "Copy start command".
- This computer's name, with "Rename" (opens Settings), and why phones show it.
- Update status line (same text as Settings).
- Start at login: reads whether the systemd user unit is enabled (`default.target.wants` link, user or /etc) or an XDG autostart entry exists; on Linux when off, explains and offers the enable command to copy.

## Code

- `crates/bridge-core/src/firewall.rs` (new): `Firewall`, `ufw_allows`, `read()`, `verdict()`, `commands()`. Moved out of `doctor.rs`, which re-exports them, so doctor, ctl and Home share one decision. A test in `autostart.rs` keeps `geekbridge-ctl firewall` text in sync with the copied lines.
- `crates/bridge-apps/src/onboarding.rs` (+ `onboarding_tests.rs`): the card as a pure function of `Facts`; `HomeApp::onboarding_facts` builds the facts. Firewall and autostart are re-read every 5 s (two tiny file reads).
- Strings: `onboarding.*` in `crates/bridge-apps/i18n/en.properties` (i18n allowlist test unchanged; shell commands carry `// i18n-ok`).
- Accessibility: every control has a distinct name ("Copy link", "Copy firewall commands", "Copy start command", "Open releases page", "Rename", "Pair a phone"), step numbers read "Step N", the QR has a name. Verified by the AccessKit tree in the tests.
- Test/screenshot fixtures: `GEEKBRIDGE_FIREWALL_FIXTURE=none|ufw-blocking|ufw-unverified|firewalld`, `GEEKBRIDGE_AUTOSTART_FIXTURE=on|off|unknown`.

## Tests (one per state)

Default three steps with name, update and autostart; blocking ufw with exact sudo lines; unverified ufw and firewalld; service down; autostart off and unknown; nearby phones (none, one, many); "Copied" label; visibility rules (checking, unpaired, paired, down); autostart detection from a temp dir; the QR encodes the releases link; verdict table and command shape in bridge-core; HomeApp facts and copy timeout.

## Golden screenshots

`scripts/golden-desktop.sh` now starts a second never-paired service ("fresh") for first-run screens and pins the two fixtures for all screens. New: `home-first-run`, `home-first-run-firewall` (light/dark, std/min). Changed: `home-service-off`. Full check: 84 screens, 0 failing. The script also carries another agent's uncommitted `unset CARGO_TARGET_DIR` line, kept as it was.

## Not done / notes

- Windows and macOS get the card but not the firewall or autostart rows (Linux-only detection; Defender/macOS prompt text stays in `geekbridge-ctl firewall`).
- The sidebar dot says "Background service running" in the service-off golden because it checks the API port, not the token; pre-existing.
- Rename is in Settings, not inline on Home.
