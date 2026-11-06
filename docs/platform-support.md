# What works on which system

One table per area, feature by operating system. "Yes" means the code path exists for that system; how far each
path has been checked (type-check, unit, CI, real hardware) is in [platform-status.md](platform-status.md), and
nothing on Windows or macOS has been run by a person on a real PC or Mac yet. "No" and "Linux only" mean the
feature is switched off or says so in plain words; it never fails silently.

The Android column is the phone app, checked against the Android sources (`android/app/src/main`, its manifest and
`crates/bridge-android`); the three desktop columns against the desktop crates (`bridge-service`, `bridge-apps`,
`bridge-calls*`, `bridge-update`). The desktop columns are the same Rust apps and the same service. Features that
share phone data (contacts, calendar, messages, notifications, photos, battery, calls, clipboard) are **off by
default on both ends**: the phone switches the feature on and allows each computer, and the computer allows the phone.
"n/a" means the system has no such thing (the phone has no tray; the computer has no SMS radio).

## Features by operating system

| Feature | Linux | Windows | macOS | Android (phone) | Notes |
|---|---|---|---|---|---|
| Pairing, control API, file and text transfer | Yes | Yes | Yes | Yes | Certificate-pinned; receiving files is off until you allow it |
| QR pairing | Shows the QR code | Shows the QR code | Shows the QR code | Scans it with its own camera screen, or opens the `geekbridge://pair` link from another camera app | Android asks for the camera only on the scan screen; the desktop never scans. The phone still asks "Pair with X?" |
| Invite by email ("Send me the download link") | n/a | n/a | n/a | Yes | Phone only. Opens a draft in the mail app (`mailto:`, no recipient). Carries public release links only, never a pairing link, token or address |
| Share (send files, links, text) | Yes | Yes | Yes | Yes: Share sheet ("Send to computer"), text-selection menu, home-screen widget | Up to 8 GiB, SHA-256 checked. Computer to phone also works, drag and drop on the desktop |
| Clipboard text | Yes | Yes (Win32 clipboard) | Yes (`pbcopy`/`pbpaste`) | Yes | **Opt-in per phone.** Computer to phone is automatic; phone to computer is on demand (Android 10+ does not let background apps read the clipboard): hotkey, "Paste from phone", Quick Settings tile |
| Clipboard images (PNG/JPEG) | Yes (`wl-clipboard` or `xclip`) | **No** | **No** | Yes | Text only on Windows and macOS: the image read and write return "unsupported". HTML is Linux and Android only too. Limit 8 MiB by default (64 KiB to 32 MiB) |
| Clipboard files | No | No | No | No | Files go through Share, not the clipboard |
| Phone notifications on the computer | Banner via `notify-send`, buttons where the daemon shows them | PowerShell toast, no buttons | `osascript` banner, no buttons | Sends them (notification-listener access) | **Off by default** on both ends; only apps you pick; Dismiss and the phone's own buttons, no typed replies; memory only. On an APK you installed yourself, Android 13+ needs "Allow restricted settings" first |
| SMS: read, send, receive | Yes (Messages window, unread count) | Yes | Yes | Yes (`READ_SMS`, `SEND_SMS`, `RECEIVE_SMS`; your own messaging app stays the default) | Opt-in per phone; send limit 10 a minute. Group messages and RCS: not supported |
| MMS | Placeholder only | Placeholder only | Placeholder only | Reports "there is a picture message", never reads its parts | No MMS send, no attachments, no group threads |
| Contacts | Yes, read-only (search, call, message, copy) | Yes | Yes | Yes (`READ_CONTACTS`) | Needs "Share contacts" on the phone |
| Calendar | Yes: month, week, day, agenda | Yes | Yes | Yes (`READ_CALENDAR`; `WRITE_CALENDAR` for editing) | Google calendars through the phone's own sync. **Editing is a separate opt-in**, off by default; repeating events can only be changed on the phone. "Notify before events" is also off by default |
| Recent phone photos | Yes: "Phone photos" tab in Share; Save; Copy to clipboard (`wl-copy` or `xclip`) | Yes; Copy through PowerShell | Yes; Copy through `osascript` | Yes (`READ_MEDIA_IMAGES`; on Android 14+ you may pick "Select photos") | **Off by default** on both ends; newest 24 as small previews, full picture only when you press Save or Copy; GeekBridge phones only; memory only |
| Phone battery on the computer | Yes (tray, Home, `status`) | Yes (tray, Home, `status`) | Home and `status` only (no tray at run time) | Sends level and charging state | **Off by default** on both ends; event driven, no polling |
| Calls: see, answer, decline, hang up | Yes | Yes | Yes | Yes (`ANSWER_PHONE_CALLS`; the phone does the work) | Over the GeekBridge link, no Bluetooth needed. Needs "Answer from computer" on the phone and Calls allowed on the computer. Caller number needs `READ_CALL_LOG` |
| Dial from the computer | Yes | Yes | Yes | Yes (`CALL_PHONE`) | Separate opt-in "Allow placing calls from my computer"; each call is confirmed on the computer first; digits and a leading `+` only (no `*` or `#` codes); the voice stays on the phone |
| Keypad (DTMF tones) | Yes: Bluetooth hands-free, or the link | Yes: Bluetooth hands-free, or the link | **Link only** (no Bluetooth keypad path in the macOS helper) | Plays tones only if GeekBridge is the phone's default dialer (in-call service) | On a stock phone the keypad is normally hidden. Link tones: 20 per 5 s |
| Mute microphone | No (no telephony API for it) | Yes, over Bluetooth hands-free only | No | n/a | Offered only where the PC carries the voice |
| Hold / swap, second call | No | No | No | No | Not implemented: Android reports one current call over the link |
| Decline with message (3 presets) | Yes | Yes | Yes | Sends the SMS | Needs Messages allowed on both ends, a phone-link call and a plain number; subject to the SMS limit; no automatic reply |
| Call audio through the computer (Bluetooth) | Yes (BlueZ + PipeWire or PulseAudio) | Windows' own Bluetooth "Phone calls"; GeekBridge shows the checklist | **Not offered**: answer on the Mac, talk on the phone | Is the hands-free gateway; the setup step shows what it sees (needs `BLUETOOTH_CONNECT` on Android 12+) | **Requires Bluetooth**: a controller in the PC and a one-time Bluetooth pairing of phone and computer. **Not heard on real hardware by the maintainers yet** |
| Call audio over Wi-Fi (`ctl callwifi`, card button) | **Experimental**, Linux with PipeWire (`pw-cat`) | No: button absent; `ctl callwifi` and `doctor` print that sentence, exit normally | No: same | **Experimental, debug builds only** (the helper is in the debug source set; no release APK has it) | **Off by default**, needs an ADB helper, may not work on your phone (Samsung may block it). Not a way to carry a call on a stock release phone. Real-phone result is still open |
| Incoming-call card (always on top, no task-bar entry, does not take focus) | Yes (window rules / X11 type) | Yes (`WS_EX_NOACTIVATE`, tool window) | Yes (accessory app: no Dock icon; floating level) | Phone's own call screen | |
| Answer / Decline controls on the desktop | Card, banner buttons (daemon permitting), tray | Card, tray menu, Calls window | Card, Calls window | Phone | |
| Banners (call, file offer, text, phone notification) | `notify-send`, with buttons where the daemon shows them | PowerShell toast, no buttons | `osascript` banner, no buttons | n/a | |
| Quick call replies | Yes | Yes | Yes | n/a | |
| Privacy switches | Four display switches in Settings > Privacy: message text, caller number, phone notification text, respect Do Not Disturb | Same four (Do Not Disturb disabled) | Same four (Do Not Disturb disabled) | Per-computer opt-ins: calls, contacts, calendar, calendar editing, messages, battery, notifications, photos | A new computer has nothing until you allow it on the phone. Message and notification text are hidden in desktop banners by default |
| Do Not Disturb respected for calls | Yes (mako, dunst, SwayNotificationCenter, KDE, GNOME) | **No**: the switch is off and disabled, "Not supported on this system"; `ctl privacy set respect_dnd on` is refused | **No**: same | n/a | Off by default, even on Linux |
| Tray / menu-bar icon with Answer, Decline, Pause sync | Yes (StatusNotifierItem host needed) | Yes (notification area) | **No**: the code exists, it is inactive at run time | n/a | Unread count in the tooltip: Linux and Windows. Windows tooltip holds 127 characters; a long phone name drops the call-audio line and the "GeekBridge" prefix first, never the unread count |
| Autostart | XDG autostart file (and a systemd user unit) | `HKCU\...\Run` | LaunchAgent (written the first time the app opens) | Restores the background link after a reboot or an app update, if a computer is paired | `geekbridge-ctl autostart enable`; installers do it |
| Firewall help | ufw / firewalld text | `netsh advfirewall` text | `socketfilterfw` text | n/a | The computer needs TCP and UDP 1716-1764 open from your LAN; the phone needs nothing |
| Update: check | Yes | Yes | Yes | Yes (GitHub releases; stable channel on release builds, dev channel on debug builds) | Optional; `geekbridge-ctl update disable` turns the check off |
| Update: download and SHA-256 check | Yes | Yes | Yes | Yes | Never installs by itself |
| Update: install | `update install`, `rollback`, `restart`: per-user install only (not package-managed, not the dev channel) | **No**: download only; the commands print a refusal and change nothing | **No**: same | Hands the verified APK to the **system installer**; you tap Install | Android needs "Install unknown apps" allowed for GeekBridge. A debug APK and the release APK are signed differently and cannot update each other |
| Call trace (`call-trace.log`, last calls only, local) | Yes, mode 0600 | Yes; the file lives in the private state folder whose protected ACL (you and SYSTEM) it inherits | Yes, mode 0600 | n/a | |
| Settings files (`privacy`, `call-replies`, `update-state.json`) | Private, atomic replace | Private ACL where written by the private writer (`privacy`, `call-replies`); `update-state.json` is plain atomic replace inside the private state folder | Private, atomic replace | n/a | |
| Hyprland hotkey, waybar | Yes | n/a | n/a | n/a | |
| Android-only extras | n/a | n/a | n/a | Home-screen widgets (connection, quick actions, agenda), Quick Settings clipboard tile | |

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
