# Cross-platform parity audit 3 (features of 2026-11-03/04)

Scope: call-chain review (card supervisor, watchdog), Privacy switches, tray unread line, contact search
and shortcuts in Messages, Wi-Fi call audio (Linux and PipeWire only by design), About, doctor and
firewall changes. No Windows or macOS machine and no cross toolchain (`rustup` is absent, so no
`--target` check) were available: the audit is code reading plus the CI logs of the last runs that got
as far as compiling (Windows and macOS clippy and build were green on 2026-11-04 13:36, covering all of
these features; the "Type-check Windows and macOS targets" job is green on the same commits).

## Fixed

| # | Platform | Finding | Fix |
|---|----------|---------|-----|
| 1 | both | `geekbridge-ctl` appended "Start it (systemctl --user start geekbridge-service, or scripts/run-desktop.sh)" to every failed control-port call. | `doctor::hint_for_error` names `geekbridge-service` and `geekbridge-ctl autostart enable` off Linux; the test is platform-aware. |
| 2 | both | The apps' "helper isn't running" line (`DiscoveryModel::guidance`) told Windows and Mac users to run `systemctl`. | Wording from the catalogue (`state.start_service.linux/other`). |
| 3 | Windows | Home guide firewall note ended "GeekBridge never runs sudo for you", but the Windows lines are `netsh` and need an administrator PowerShell. | `onboarding.fw.never_runs_admin` for Windows Defender, test. |
| 4 | macOS | Messages compose hint said "Ctrl+Enter sends" while the chord is Cmd+Enter (`Modifiers::COMMAND`). | Hint follows the platform (`messages.compose_hint.ctrl/cmd`), test. |
| 5 | Windows, macOS | About: "Licence text" / "Third-party notices" were handed to `explorer` / `open`. `LICENSE` has no extension (Windows: "How do you want to open this file?"; macOS may find no application). | Installed documents open with `notepad` / `open -t`; web pages and Linux keep the system opener. `about_ui::open_command` tested for all three. |
| 6 | Windows, macOS | `doctor` for a phone that offers Wi-Fi call audio on a computer that cannot play it said "Install PipeWire's tools (pw-cat)". | Fix line names PipeWire on Linux only, test. |

## Checked, no change needed

* **Call card supervisor and watchdog** (`call_overlay.rs`, `call_control.rs`): std threads, `Condvar` and
  `Instant` only; the `/proc/self` runtime-directory lookup is `cfg(unix)` and returns `None` on macOS
  (the display check is skipped there, see `windows-macos-call-features.md`). `geekbridge-calls` is a
  windowless-subsystem release binary on Windows, so the supervisor's spawn shows no console. The spawn
  names `geekbridge-calls` without `.exe`; `CreateProcess` appends it and searches the service's own
  directory first, so it works. `last_ring_failed` names `systemctl`/`WAYLAND` on Linux only (tested).
* **Privacy** (`privacy.rs`): the choices file goes through `bridge_kde::fsutil::atomic_write_private`
  (owner-only ACL on Windows, 0600 on Unix, refusal elsewhere) in the already private state directory;
  reads are plain `read_to_string`. Text banners on Windows and macOS (`os_notify`) honour the
  message-text and phone-notification-text choices (`IncomingText::body`, `MirrorBanner::text`); the
  caller-number choice acts on the card, and the plain ring banner behaves as on Linux.
* **Tray unread line**: shared `TrayInput::tooltip`; the Windows 128-unit `szTip` cut is whole characters
  and zero-terminated (tested). Limit, not fixed: the unread line is last, so with two long phone names
  on Windows it is the first thing cut. The macOS tooltip has no length limit.
* **Contact search in compose**: pure egui; shortcuts use `Modifiers::COMMAND`, so Cmd on macOS.
* **Wi-Fi call audio** (`callwifi.rs`): `pw-cat` code and its imports are `cfg(target_os = "linux")`;
  elsewhere `supported()` says "playing call audio over Wi-Fi works on Linux with PipeWire only",
  `open_playback`/`open_capture` return "not available on this system", `describe` is empty. The call
  card offers the button only when `supported`, so nothing appears on Windows or macOS; `ctl callwifi`
  and the doctor line carry the same sentence. No panics (errors only).
* **About**: `doc_dirs` also looks next to the binary, which is where the Windows and macOS archives put
  `LICENSE` and `THIRD_PARTY_NOTICES.md`; the Linux system paths are harmless elsewhere.
* **Firewall and doctor**: `lan_v4` and `own_lan_v4` exist on Linux only, Windows and macOS build
  `Firewall { ..Default::default() }`; Windows and macOS keep their own `netsh`/`socketfilterfw`
  verdicts and `guidance` text. The Home guide picks `autostart enable` and hides the systemd
  alternative off Linux.

## CI evidence

Latest completed Windows/macOS runs (2026-11-04):

* 09:25 (green): Windows tests 8.7 min, macOS 7.7 min. Slowest suites on Windows: fuzz
  `every_target_survives_2000_deterministic_inputs` 89 s (macOS 107 s), `chaos_transfer` 69 s
  (200 MB degraded-link transfer and a receiver cancel each over 60 s), a 52 s suite (12 tests), 35 s.
* 13:36 (red): Windows test step hit the 10 min limit while `an_unfetched_outgoing_offer_does_not_outlive_its_link`
  and `sender_vanishing_mid_transfer_fails_fast_and_leaves_no_partial` (clipboard/transfer API) were still
  running after 60 s; macOS failed two Wi-Fi call audio unit tests
  (`speaking_is_a_separate_step...` 6 != 5, `a_press_an_offer...` "timed out waiting for the tone"),
  fixed by the later "pace the far party's frames" commit. The step limit was raised to 20 min at 14:01.
  Not touched here (another agent owns the CI reds).
* Clippy and build on Windows and macOS: no warnings in the logs of the 09:25 and 13:36 runs.
* The 14:25 and later runs failed at the toolchain install step (network), not in code.

## Real-machine checks still open

Notepad/`open -t` for the About buttons; the Windows tooltip with two phones and unread messages; the
Cmd+Enter send in Messages; the Home guide firewall text on Windows.
