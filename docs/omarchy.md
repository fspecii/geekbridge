# GeekBridge on Omarchy (Hyprland) and other Linux desktops

Omarchy is Arch + Hyprland + a Quickshell bar/notification layer (or mako/waybar on older installs). Everything below
also degrades cleanly on GNOME, KDE, sway and X11; see the last section.

## 1. Install and start the service

```sh
./install.sh                 # per user: ~/.local/bin, systemd user unit, XDG autostart, .desktop entries
geekbridge-ctl status        # the service answers
systemctl --user status geekbridge-service
```

Omarchy starts the session through uwsm, which exports `WAYLAND_DISPLAY` to the systemd user manager a little *after*
default.target. Two things make that harmless:

* the autostart launcher (`geekbridge-service-launch`) runs `systemctl --user import-environment WAYLAND_DISPLAY
  HYPRLAND_INSTANCE_SIGNATURE DISPLAY XDG_CURRENT_DESKTOP DBUS_SESSION_BUS_ADDRESS ...` before starting the unit;
* the service itself looks in `$XDG_RUNTIME_DIR` for the newest `wayland-*` socket, the Hyprland instance
  (`hypr/<signature>/.socket.sock`) and the session bus when those variables are missing.

The unit has no `PrivateTmp`: a private `/tmp` hides `/tmp/.X11-unix`, which the X11 clipboard tools need.
Check what the service sees: `systemctl --user show-environment | grep -E 'WAYLAND|HYPR|DBUS'`.

Launcher entries (walker, Omarchy's menu, rofi, fuzzel, ...) come from the installed `.desktop` files, with keywords:
type "phone", "clipboard", "paste", "share" or "call". **Paste from Phone** is its own entry, and GeekBridge /
GeekBridge Clipboard carry it as a right-click action.

## 2. Global "paste from phone" key

Wayland has no portable global-hotkey API, so the compositor binds it. One command edits your config:

```sh
geekbridge-ctl hotkey install            # dry run: prints the diff, writes nothing
geekbridge-ctl hotkey install --apply    # backs up the file, writes the marked blocks, reloads Hyprland
geekbridge-ctl hotkey status
geekbridge-ctl hotkey uninstall --apply  # removes exactly those blocks again
```

* **Where it writes.** The first existing of `~/.config/hypr/bindings.lua` (current Omarchy: Lua), `bindings.conf`,
  `hyprland.lua`, `hyprland.conf`. Omarchy's own defaults (`/usr/share/omarchy/default/...`) are never touched, and a
  file that resolves into one is refused. Other files in `~/.config/hypr` are not read for writing.
* **What it writes.** Two blocks fenced by `-- >>> GeekBridge ... >>>` / `-- <<< GeekBridge ... <<<` (`#` in `.conf`).
  Everything outside is left byte for byte. Installing again replaces the block (idempotent, no second backup when
  nothing changes); damaged or duplicated markers are refused rather than guessed at.
* **Backup.** `bindings.lua.bak.geekbridge.<unix time>` next to the file, before every write. The write is atomic
  (temp file + rename) and follows a symlink, so dotfile managers keep working.
* **The key.** `--key COMBO`, else the shortcut set in GeekBridge (`geekbridge-ctl clipboard hotkey`). The default
  `Ctrl+Shift+V` is how terminals paste, so a *global* bind on it would swallow terminal paste: on Hyprland the command
  uses **Super+Shift+V** instead (Omarchy leaves it free; Super+V is "Universal paste") and says so. Pass
  `--key Ctrl+Shift+V` to override. If the combination is already bound (Omarchy defaults or your own file), the diff
  shows an `hl.unbind(...)` / `unbind =` line first, as the Omarchy docs ask, and the command names what it replaces.
* **Validation.** After `--apply` inside Hyprland the command runs `hyprctl reload` and `hyprctl configerrors` and prints
  any error. Both Lua and `.conf` output was also checked with `Hyprland --verify-config` (Hyprland 0.56).

The bound command is `geekbridge-ctl clipboard paste-from-phone --notify`: it pulls the phone's latest eligible item
(copied recently, phone unlocked) onto the computer clipboard and shows a short notification with the result.
While clipboard sync is on, the service also binds the same key for the running session with `hyprctl keyword bind`;
the installed block makes it permanent and independent of the service. Both do the same thing.

### Window rules (same command)

`install` also writes the second block unless `--no-rules`: the Calls window (incoming call) and the Share window
(file offers, accept/decline) open **floating, centred, pinned** at a sensible size (Calls 400x700, Share 480x640):

```lua
hl.window_rule({ match = { class = "^(dev\\.geekbridge\\.Calls)$" }, float = true, center = true, size = { 400, 700 }, pin = true })
hl.window_rule({ match = { class = "^(dev\\.geekbridge\\.Share)$" }, float = true, center = true, size = { 480, 640 }, pin = true })
```

Pinned floating windows follow you across workspaces; Hyprland draws them above a fullscreen window (this could not be
checked live in the build sandbox, only that the rules parse: see Validation). `--no-pin` drops the pin (the window then stays on its
workspace), `--no-bind` writes only the rules, `--legacy-rules` uses the pre-0.53 `windowrulev2` syntax in `.conf`
files (newer `.conf` syntax is the default). To change the sizes, edit the numbers inside the block; reinstalling
resets them.

## 3. Notifications

Calls, file offers, received files, new SMS, calendar reminders and updates use `notify-send` (libnotify >= 0.8):

| Event | Urgency | Actions |
| --- | --- | --- |
| Incoming call | critical, `category=call.incoming` | **Answer**, **Decline** (only those the phone really offers) |
| File offer | normal | **Accept**, **Decline** |
| File received | normal | **Open**, **Show in folder** (highlights it in the file manager over D-Bus, else opens the folder) |
| Update available | normal | **Update** (opens GeekBridge's update panel; nothing is installed automatically) |

Clicking the notification body runs the `default` action: it **opens** the Calls/Share window, it never answers or
accepts. **mako** draws no buttons: use `makoctl menu` (or bind `makoctl invoke`) to pick Answer/Decline/Open;
dunst, swaync, GNOME and KDE draw buttons. If the notification daemon ignores actions, `--wait` returns when the
notification expires and nothing happens; the call keeps ringing on the phone and the in-app popup (the Calls window
shows Answer/Decline while it is open) stays the fallback. Everything that came from a phone (names, files, titles) has
markup, control and bidirectional characters neutralised and follows `--`, so it cannot become an option or HTML.

## 4. Waybar module

`geekbridge-ctl status --waybar` prints one JSON line (always exits 0; a stopped service prints state `offline`):

```json
{"text":" Pixel   3","tooltip":"Connected: Pixel\n3 unread messages","class":["connected","unread"],"alt":"connected"}
```

States (`alt`, and the first `class`): `offline` (service not running), `unpaired`, `disconnected`, `connected`,
`ringing`. Extra classes: `unread` (unread SMS, when the phone shares messages), `update` (newer release). It carries no
caller, number or message text. A battery level is not part of the phone link yet, so none is shown.

`~/.config/waybar/config.jsonc`:

```jsonc
"custom/geekbridge": {
  "exec": "geekbridge-ctl status --waybar",
  "return-type": "json",
  "interval": 10,
  "format": "{}",
  "tooltip": true,
  "on-click": "geekbridge",
  "on-click-right": "geekbridge-ctl clipboard paste-from-phone --notify",
  "on-click-middle": "geekbridge-share"
}
```

`style.css`:

```css
#custom-geekbridge.disconnected, #custom-geekbridge.offline { opacity: 0.5; }
#custom-geekbridge.unread  { color: #7aa2f7; }
#custom-geekbridge.update  { color: #e0af68; }
#custom-geekbridge.ringing { color: #f7768e; animation: blink 1s steps(2) infinite; }
@keyframes blink { to { opacity: 0.3; } }
```

Add `"custom/geekbridge"` to `modules-right`. Omarchy's own bar is the Quickshell shell (`~/.config/omarchy/shell.json`),
not waybar; the JSON shape is the common `text`/`tooltip`/`class` one, so any widget that runs a command can use it.

## 5. Clipboard under Wayland

Verified against a real headless compositor (sway, `wlr-data-control`), see `tests/wayland_clipboard.rs`:

* Watching uses `wl-paste --watch`, which follows the **regular clipboard only**; the primary selection (middle-click
  paste) is never read, watched or sent.
* Text is read as `wl-paste --type text`; images as `image/png` then `image/jpeg` (the magic bytes decide the type, a size
  cap applies, an oversize image is refused, not truncated); `text/html` is read when offered.
* `x-kde-passwordManagerHint` (set by password managers) marks a copy sensitive and it is never sent.
* Hyprland, sway, KDE Plasma and other compositors with the data-control protocol work in the background. **GNOME
  (mutter) has no data-control protocol**: `wl-paste --watch` exits, the service falls back to polling, and each read may
  flash a one-pixel window. Prefer the hotkey/Paste button there. Install `wl-clipboard` (`pacman -S wl-clipboard`).

Run the live test yourself against a throwaway compositor, never your own session (it overwrites the clipboard):

```sh
RT=$(mktemp -d); chmod 700 $RT; export XDG_RUNTIME_DIR=$RT
WLR_BACKENDS=headless WLR_RENDERER=pixman sway -c /dev/null & sleep 2
WAYLAND_DISPLAY=wayland-1 GEEKBRIDGE_TEST_WAYLAND=1 cargo test -p bridge-service --test wayland_clipboard
```

## 6. Other desktops

`geekbridge-ctl hotkey print` prints, for your key: the `gsettings` commands for **GNOME**, the System Settings steps
for **KDE Plasma**, `bindsym` plus `for_window` rules for **sway/i3**, and a pointer for other X11 window managers.
Notifications, `.desktop` entries and the waybar-style status line work on all of them; GNOME/KDE show their own
notification buttons.

## Changing or removing everything

`geekbridge-ctl hotkey uninstall --apply` removes both blocks; the backups (`*.bak.geekbridge.*`) stay until you delete
them. `./install.sh --uninstall` removes the program, units and `.desktop` entries.

## Tests

The config-edit logic is covered in a temp directory only (`hypr_config.rs`, `hotkey_cli.rs`, `tests/hotkey_ctl.rs`):
idempotent install, uninstall restores the original bytes, a backup is made, files other than the target and the text
outside the markers are untouched, symlinks and distribution-managed paths, damaged markers. The integration test
overrides `HOME` and `XDG_CONFIG_HOME` and removes `HYPRLAND_INSTANCE_SIGNATURE`, so it can never reach your real
configuration or reload your compositor.
