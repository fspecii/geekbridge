# Omarchy desktop review 2

Scope: how GeekBridge behaves on the owner's desktop (Omarchy/Hyprland, `omarchy-shell` tray module, notifications from
the `ambsd.notifications` Quickshell plugin, no waybar/mako). Method: source reading, the user's plugin and bindings
read-only, `/usr/share/omarchy` read-only, and a throwaway sandbox (private `XDG_RUNTIME_DIR` and D-Bus, headless sway
plus mako) for the `notify-send` behaviour. The live Hyprland session, real `~/.config` and the live service were never
touched. `scripts/e2e/wayland.sh` (someone else's uncommitted edits) was not edited or committed.

## 1. Notification banners

Checked against libnotify 0.8.9 `notify-send` and mako in the sandbox.

| Banner | app-name | icon (new) | urgency | expiry | actions |
| --- | --- | --- | --- | --- | --- |
| Incoming call | GeekBridge | `dev.geekbridge.Calls` | critical, `call.incoming` | 30 s | answer / decline / `default`=Open |
| File offer | GeekBridge | `dev.geekbridge.Share` | normal | 60 s | accept / decline / `default`=Open |
| File received | GeekBridge | `dev.geekbridge.Share` | normal | 20 s | open / folder / `default` |
| Update | GeekBridge | `dev.geekbridge.GeekBridge` | normal | 30 s | update / `default` |
| SMS, calendar | GeekBridge | `Messages` / `Agenda` | normal | 8 s / 15 s | none |
| Mirrored phone notification | GeekBridge | `dev.geekbridge.GeekBridge` | normal | 10 s | `dismiss` (when clearable), up to 3 phone buttons |

Findings and fixes:

* **Dismiss and the other buttons are not drawn by Omarchy's shell.** The `ambsd.notifications` card has no action
  buttons; `NotificationServer` advertises `actionsSupported`, but only the `default` action is invoked (a click on the
  card, `invokePopupDefault`). Answer/Decline/Accept/Dismiss are therefore reachable only through the Calls/Share windows,
  the tray and `geekbridge-ctl notifications`. The code already degrades correctly: with `--wait` a server that ignores
  actions just lets the banner expire, nothing is replied, the phone keeps ringing (the CI mako case; covered by
  `notify_send_stub.rs`). Documented in docs/omarchy.md section 3.
* **Replacement.** Mirrored banners relied only on the `x-canonical-private-synchronous` hint. Verified in the sandbox
  that mako replaces by it (same id 1, one banner). Quickshell has no such hint handling (the plugin only tracks
  `replacesId`), so on the owner's desktop every update of a phone notification would have stacked a second card.
  Fixed: mirrored banners now pass `--print-id`, the id is read while the banner is open (`run_banner`), remembered per
  slot (bounded to 256, generation-guarded so a replaced banner's process does not forget the live id) and the next banner of
  the slot passes `--replace-id=ID` as well as the hint. `choice()` ignores the numeric id line. Tests: argument shape,
  id/button parsing, id reported before exit, kill on timeout, ownership of the remembered id.
* **Icon.** No banner passed an icon. All now pass `--icon` with the installed hicolor app icon names.
* **Do Not Disturb.** The plugin lets only an app named exactly `notify-send` with critical urgency through DND. A
  GeekBridge call banner (app-name GeekBridge, critical) is stored but silenced under DND. Not changed (spoofing the app
  name would show the wrong source); documented. The tray still shows the ringing state.
* Observed, not repo-related: in this agent shell `notify-send` fails with a symbol lookup error because
  `LD_LIBRARY_PATH` points into the T3 Code AppImage's `libnotify`. A service started from such a shell would lose
  banners; the sandbox runs used `LD_LIBRARY_PATH=`. Not an issue for a unit started by systemd.
* Remaining: the call and share banner texts ("Incoming call", "Answer", "wants to send a file") are English literals
  in `call_control.rs` / `share.rs` rather than catalogue keys (pre-existing, other platforms use `banner.*`).

## 2. Tray (StatusNotifierItem)

* Menu: status line `Connected · clipboard sync paused · Battery 100% (charging)` is the longest fixed shape (about
  60 characters); phone-named parts are capped (headline 40, `+N` for more phones, update version 24). A test now asserts
  every label stays at or below 64 characters in the worst case.
* **There was no Photos entry.** Added `Phone photos…` (id 17, after `Send file…`; ids and relative order of the other
  items are unchanged). It is hidden and disabled until a connected phone offers photos, and starts the Share window with
  `GEEKBRIDGE_SHARE_TAB=photos`.
* **Defect fixed:** that environment switch (and so any "open on Photos") was dropped on the first frame, because the Photos
  tab only exists after the service has listed the phone, and the frame code reset the tab to Share. The request now
  waits up to 8 s for the page. Test in `share_app_tests.rs`.
* Not verifiable here: how omarchy-shell draws the dbusmenu (hidden items, widths); the SNI/dbusmenu side is covered by
  `tray_sni.rs` with a private bus (updated to the 17-row layout).

## 3. Window rules (Photos in the Share window)

* The Share window is still `app_id` `dev.geekbridge.Share`, so the generated rules (`^(dev\.geekbridge\.Share)$`, float,
  center, size 480x640, pin) still match; the dry-run against a copy of the user's `bindings.lua` produces the same blocks.
* Omarchy's defaults (`default/hypr/apps/localsend.lua`) have `o.window("(Share|localsend)", { float, center })`, an
  unanchored match that also catches our class, so Share floats and centres even without our rules.
* Sizes are logical pixels. 1920x1080: 480x640 is 25% of the width and 59% of the height; 4K at scale 2 is a 1920x1080
  logical desktop, so the same logical window (960x1280 physical). The window minimum is 380x460, the Calls window
  400x700 (minimum 320x560). Judged sensible for the Photos grid; not rendered on a real Hyprland.

## 4. Hotkeys

`Super+Shift+V` is unbound in `/usr/share/omarchy/default/hypr/bindings/*.lua` (they bind `SUPER+V` universal paste and
`SUPER+CTRL+V` clipboard manager) and in the user's `bindings.lua` (read-only copy used for a dry run, no `unbind` line
generated). Still free.

## 5. Autostart

Defect: the unit was `After=network-online.target` only and enabled in `default.target`, so on uwsm/Omarchy it could start
before `WAYLAND_DISPLAY` reached the user manager. The launcher then ran `systemctl --user start`, a no-op for a running
unit, so the service kept running without Wayland env and fell back to xclip (what happened on the owner's machine). The
service's own socket discovery cannot help when the socket does not exist yet.

Fixes:
* unit: `After=graphical-session.target network-online.target` (ordering only; no `Wants=` so console and non-systemd
  graphical sessions still start from `default.target`). `ImportEnvironment=` does not exist in systemd and
  `PassEnvironment=` is redundant for the user manager, which hands imported variables to every unit; neither was added.
* launcher: records whether the manager already had `WAYLAND_DISPLAY`; if not and the session now has it, it imports the
  environment and **restarts** the unit once.
* `geekbridge-ctl doctor`: new warning when the shell is on Wayland but the service's clipboard backend is `xclip`,
  with the `import-environment ... && restart` command.
* Tests: `tests/service_unit_launcher.rs` (unit ordering, launcher with a stub `systemctl` in four situations) and a
  doctor unit test. docs/omarchy.md section 1 updated, including the Omarchy default autostart that already runs
  `systemctl --user import-environment` at `hyprland.start` (so `exec-once` is not needed when the unit is used).

## Verification

`cargo fmt --all` exit 0; `cargo clippy --workspace --all-targets -- -D warnings` exit 0; `cargo test --workspace
--no-fail-fast` exit 0. One earlier full run hit the known timing flake in `clipboard_tools`
(`tool_trouble_is_surfaced_as_a_problem...`, see clipboard-watcher-flake.md) under load; it passed in isolation three times
and in the full rerun.
