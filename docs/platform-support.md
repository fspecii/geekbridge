# What works on which system

One table per area, feature by operating system. "Yes" means the code path exists for that system; how far each
path has been checked (type-check, unit, CI, real hardware) is in [platform-status.md](platform-status.md), and
nothing on Windows or macOS has been run by a person on a real PC or Mac yet. "No" and "Linux only" mean the
feature is switched off or says so in plain words; it never fails silently.

The Android column is the phone app. The three desktop columns are the same Rust apps and the same service.

## Desktop features

| Feature | Linux | Windows | macOS | Android (phone) |
|---|---|---|---|---|
| Pairing, control API, file and text transfer | Yes | Yes | Yes | Yes |
| Clipboard text sync | Yes | Yes | Yes | Yes |
| Clipboard images | Yes | No (text only) | No (text only) | Yes |
| Messages window, unread count | Yes | Yes | Yes | n/a |
| Unread count in the tray tooltip | Yes | Yes (tooltip holds 127 characters; a long phone name drops the call-audio line and the "GeekBridge" prefix first, never the unread count) | No tray at run time (see Tray) | n/a |
| Tray / menu-bar icon with Answer, Decline, Pause sync | Yes (StatusNotifierItem host needed) | Yes (notification area) | No: the code exists, it is inactive at run time | n/a |
| Incoming-call card (always on top, no task-bar entry, does not take focus) | Yes (window rules / X11 type) | Yes (`WS_EX_NOACTIVATE`, tool window) | Yes (accessory app: no Dock icon; floating level) | Phone's own call screen |
| Answer / Decline | Card, banner buttons (daemon permitting), tray | Card, tray menu, Calls window | Card, Calls window | Phone |
| Banners (call, file offer, text, phone notification) | `notify-send`, with buttons where the daemon shows them | PowerShell toast, no buttons | `osascript` banner, no buttons | n/a |
| Do Not Disturb respected for calls | Yes (mako, dunst, SwayNotificationCenter, KDE, GNOME) | **No**: the switch is off and disabled, "Not supported on this system"; `ctl privacy set respect_dnd on` is refused | **No**: same | n/a |
| Call audio through this computer (Bluetooth) | Yes (BlueZ + PipeWire/PulseAudio) | Windows' own Bluetooth "Phone calls"; GeekBridge shows the checklist | Not offered; answer here, talk on the phone | n/a |
| Call audio over Wi-Fi (`ctl callwifi`, card button) | Yes (PipeWire, `pw-cat`) | Linux with PipeWire only: the button is absent; `ctl callwifi` and `doctor` print that sentence, exit normally | same | Yes (phone side) |
| Quick call replies | Yes | Yes | Yes | n/a |
| Call trace (`call-trace.log`, last calls only, local) | Yes, mode 0600 | Yes; the file lives in the private state folder whose protected ACL (you and SYSTEM) it inherits | Yes, mode 0600 | n/a |
| Settings files (`privacy`, `call-replies`, `update-state.json`) | Private, atomic replace | Private ACL where written by the private writer (`privacy`, `call-replies`); `update-state.json` is plain atomic replace inside the private state folder | Private, atomic replace | n/a |
| Update check, download, SHA-256 check | Yes | Yes | Yes | Play / sideload |
| Self-update (`update install`, `rollback`, `restart`) | Per-user install only (not package-managed, not the dev channel) | **No**: download only; the commands print a refusal and change nothing | **No**: same | n/a |
| Autostart | XDG autostart file | `HKCU\...\Run` | LaunchAgent | n/a |
| Firewall help | ufw / firewalld text | `netsh advfirewall` text | `socketfilterfw` text | n/a |
| Hyprland hotkey, waybar | Yes | n/a | n/a | n/a |

## Where files go

| What | Linux | Windows | macOS |
|---|---|---|---|
| Service state (token, identity, trust, settings) | `$XDG_DATA_HOME/geekbridge/service` or `~/.local/share/geekbridge/service` | `%LOCALAPPDATA%\geekbridge\service` (local, not roaming: it holds secrets) | `~/Library/Application Support/geekbridge/service` |
| Downloaded updates and received files | `~/Downloads/GeekBridge` (`XDG_DOWNLOAD_DIR` honoured) | `%USERPROFILE%\Downloads\GeekBridge` | `~/Downloads/GeekBridge` |

Rules that make the paths safe on every system:

* Received file names and release asset names are single path components. Separators, control characters and
  `<>:"|?*` are replaced; trailing dots and spaces are trimmed; Windows device names (`CON`, `PRN`, `AUX`, `NUL`,
  `COM1-9`, `LPT1-9`, with or without an extension) are refused or prefixed. A release asset whose name is
  one of those or ends in a dot is never offered.
* Names that differ only by case (case-insensitive volumes on Windows and macOS) never overwrite each other.
* Generated settings files are written with `\n`; every reader accepts `\r\n` too, so a file edited in Notepad
  keeps working.
* The self-updater's tar unpacking exists on Linux only; Windows and macOS only download and verify.
