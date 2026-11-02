# Wayland end-to-end report

Scenario: `scripts/e2e/wayland.sh [clip|hotkey|apps|notify|tray]` (skips with a message when sway, mako, grim, wl-clipboard
or wtype are missing). Results: `dist/wayland-e2e/results.tsv`; pictures: `dist/screenshots/desktop/wayland-*.png`.
Last run: 64+ checks PASS, 0 FAIL, 2 SKIP (below).

## Isolation, and why sway instead of Hyprland

Everything runs in a 0700 runtime dir under `$GB_TMP/wl-xdg`, a private `HOME`/XDG tree, a `dbus-run-session` bus and a
headless `sway` (`WLR_BACKENDS=headless`, pixman, no input devices). The user's session, `~/.config` and compositor are never used.

**Hyprland 0.56.2 cannot run headless on this machine.** aquamarine's headless backend still needs a DRM allocator, and the
only KMS device is the user's real GPU. Hiding it (bwrap) gives `no allocator available`; exposing it makes the private
instance enumerate the real connector (DP-1), which is not acceptable. The nested `AQ_BACKENDS=wayland` mode fails the same
way against a pixman host. That attempt was stopped within seconds and only ever opened a seat-less (`noop`) session; nothing
was modeset. Consequences:

* Hyprland syntax is verified with `Hyprland --verify-config` (works without a display), for `.conf` and `.lua`, and the check is
  shown to fail on a broken file.
* Key binding and window-rule *behaviour* is verified on sway, using the lines `geekbridge-ctl hotkey print` gives for sway.
  `hyprctl dispatch`/`hyprctl reload`/`hyprctl clients -j` and pinning above fullscreen are **not** verified live. The Hyprland
  rules match `class`, which on Wayland is the xdg `app_id`; the apps' `app_id`s were read from the sway tree (below).
* `waybar` is not installed, so the tray host is the existing `tray-fake-watcher` (the real service registers an
  StatusNotifierItem, properties and the dbusmenu are read back; `tray-headless.sh`). The script uses real waybar when present.

## What was verified

**1. Clipboard (wl-clipboard, real compositor).** Text, PNG, JPEG, HTML and primary selection; password-manager hint. The service
(`ClipboardService` on the real backend) is event-driven: with a 20 ms poll configured, **0 `wl-paste` processes in a 4 s idle
window**, a handful per change; `wl-paste --watch` produces exactly one event per copy and none for the primary selection.
Apply-from-phone shows in `wl-paste` and is not echoed back; password-hint copies are never sent; an image over the cap is
refused, not truncated (3 MiB vs 1 MiB). New tests: `crates/bridge-service/tests/wayland_clipboard.rs`.
*Focus:* sway/wlroots implement `wlr-data-control`, so `wl-copy`/`wl-paste` work with no surface at all (the test compositor has
no window mapped). Compositors without data-control (GNOME/mutter) need a focused surface: wl-clipboard flashes a one-pixel
window per read there, and GeekBridge falls back to polling (docs/omarchy.md section 5). Not testable here.
Note: `wl-copy --type text/html` also offers `text/plain`, so such a copy is sent as text (browsers do the same).

**2. Hotkey.** On temp configs, `.conf` and `.lua`: dry run writes nothing and shows the diff; apply writes the marked blocks and
a backup; install again is a no-op; `Hyprland --verify-config` passes; uninstall restores the original **byte for byte**. On sway the
bound key (Super+Shift+V, typed with `wtype`) starts the real `geekbridge-ctl clipboard paste-from-phone --notify`; with no phone
the clipboard is unchanged and mako shows the reason. A pull that actually changes the clipboard needs a paired phone and was
not run here (the apply path is covered by the clipboard test above).

**3. Apps on Wayland (WAYLAND_DISPLAY, no DISPLAY).** app_ids: `dev.geekbridge.{GeekBridge,Calls,Share,Messages,Contacts,Agenda,Clipboard}`
match the rules. With the sway rules Calls and Share are floating, sticky, centred and 420x700; the other five stay tiled.
No winit/egui/accesskit warnings at startup (only Mesa's software-EGL notice of the pixman test compositor). Calls was rendered at
scale 1, 1.5 and 2 (`wayland-calls-scale-*.png`, output resized so the logical desktop stays 1280x800).

**4. Notifications (mako).** Actions as the service issues them (`--wait --action=...`) reach the sender through `makoctl invoke`
(Open/Show in folder, Decline); hostile text is inert; the notification screenshot is `wayland-notification.png`.

## Defects found and fixed

| # | Defect | Fix |
| - | - | - |
| 1 | **Home window panicked in egui** (`ui.set_max_width` with a negative width) as soon as a window was made very small (tiled drag, or a 0x0 first configure). All other apps survive 1x1 and 50x400. | `home_app.rs`: clamp to 0. Checked by `app_*_survives_tiny_sizes`. |
| 2 | `hotkey print` for sway chained `floating enable, sticky enable, resize set, move position` in one `for_window`; sway 1.12 runs only the first command on a window that just became floating (`No current container`), so the popups were not sticky, sized or centred. | `hotkey_help.rs` and docs: one command per `for_window` line. |
| 3 | `geekbridge-ctl clipboard paste-from-phone --notify` (the hotkey command) was **silent** when the service was not running / no control token, because the token was read before the notification path. | `geekbridge-ctl.rs`: notify "GeekBridge is not running". Notification text also now passed after `--`. |

## Not covered

Live Hyprland (dispatch, reload, `hyprctl clients`, pin over fullscreen), real waybar tray, GNOME's no-data-control fallback,
an actual phone-driven paste via hotkey, and the incoming-call popup with a ringing call (the Calls window itself was checked).
