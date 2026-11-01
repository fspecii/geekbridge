# GeekBridge user guide

## Install

| System | Package | Steps |
| --- | --- | --- |
| Debian/Ubuntu | `geekbridge_<version>_amd64.deb` | `sudo apt install ./geekbridge_*.deb` |
| Other Linux | `geekbridge-<version>-linux-x86_64-installer.tar.gz` | extract, run `./install.sh` (per user, no root) or `sudo ./install.sh --system`; `./install.sh --uninstall` removes it |
| Windows | `geekbridge-<version>-windows-x86_64-setup.exe` | run it; keep "Start the background service when I sign in" ticked |
| macOS | `geekbridge-<version>-macos-<arch>.dmg` | drag GeekBridge to Applications and open it once (right-click, Open: the app is unsigned) |
| Android | `geekbridge-android.apk` | sideload it on the phone |

All of these are on the GitHub release page. The installers start the background service
(`geekbridge-service`) at every login: a systemd user unit plus an XDG autostart entry on Linux, a Run key on
Windows, a LaunchAgent (written the first time the app opens) on macOS. Check with `geekbridge-ctl status`.
There is no tray icon yet; open **GeekBridge** from the application menu.

## Pair a phone

1. Phone and computer on the same network. Open GeekBridge on the phone.
2. Open **GeekBridge** on the computer. Under *Pair a phone* the phone appears as "Found, not paired" (or enter
   its address by hand).
3. Choose Connect, then Pair. Both screens show an 8-character code. Type the phone's code on the computer and
   confirm only if they match. There is no QR code: pairing is by comparing codes.

## Contacts and Calendar

Both are **off until you allow them**, separately for each paired phone: on the phone, open GeekBridge > Share with
computer and switch on *Share contacts* and *Share calendar* (Android asks for contacts and calendar read access), then on
the computer open **Contacts** or **Calendar** and press *Allow ... from <phone>* (or use the switches in the Calendar
sidebar). Nothing is stored on the computer; closing the service or turning a switch off erases it.

**Contacts** lists your phone's phonebook alphabetically with letter sections and a letter rail, searchable by name,
number or email. Pick someone to see their numbers and emails; **Call** opens the Calls app with the number typed in,
**Message** opens Messages with that conversation (or a new message to the number), **Copy number** copies it. Contacts are
read-only here: change them on the phone.

**Calendar** has Day, Week, Month and Agenda (list) views, a **Today** button and arrows. The sidebar lists every calendar
the phone shares, grouped by account (Google accounts are marked), with a colour and a tick to show or hide each one.
Pick an event for its details: time (in this computer's time zone; all-day events keep their date), calendar and account,
location, reminders, attendee summary and notes.

*How Google Calendar comes into this:* GeekBridge never signs in to Google and has no Google API key. A Google account that
is added to Android syncs its calendar into the phone's calendar storage; on the phone, tick that calendar in *Share
calendar > Choose calendars* and it appears here. Changes you make here are written to the phone's calendar and Google's
own sync on the phone sends them to Google (usually within a minute, when the phone is online).

*Editing is a separate permission, off by default.* To add, change or delete events from the computer: on the phone switch
on *Let the computer edit my calendar* (Android then asks for calendar write access; it is not requested before), and on
the computer switch **Edit the calendar** On in the Calendar sidebar. Then **New event** asks you to choose the calendar
explicitly. The sidebar's *Recent changes* tells the truth about each change: *Sent*, *Saved on the phone*, *Not done* with the
reason, or *Unknown whether the phone applied this* (for example the link dropped); **Ask again** is safe because the phone
applies each change only once. Repeating events can only be changed on the phone. **Notify before events** (also off by
default) shows a desktop notification shortly before an event starts.

## Clipboard

Switch the phone on under *Phone sync*. Copying on the computer then reaches the phone. To get the phone's
clipboard, press the shortcut (default **Ctrl+Shift+V**) or **Paste from phone**. Change the shortcut in the same
section or with `geekbridge-ctl clipboard hotkey "Ctrl+Alt+P"`. Most desktops cannot register a system-wide key
for the service: bind `geekbridge-ctl clipboard paste-from-phone --notify` in your keyboard settings (the window
shows the line). Details: [clipboard-sync.md](clipboard-sync.md).

## Share files, links and text

Drop files on the GeekBridge window (they open in **Geekbridge Share** for the first connected phone), or open
Share and use **Choose files**. From the phone, use *Share → Send to computer*. Receiving on the computer is off per
phone until you allow it; received files land in `~/Downloads/GeekBridge`. Details: [sharing.md](sharing.md).

## Updates

A banner appears when a newer release exists: **Download** (checked against the published SHA-256), **Show file**,
then install it yourself. Nothing installs automatically. Turn checks off under *Settings*, or
`geekbridge-ctl update disable`. Details: [updates.md](updates.md).

## Troubleshooting

* *"Service not running"*: `systemctl --user status geekbridge-service` (Linux), start it with
  `systemctl --user start geekbridge-service` or run `geekbridge-service` in a terminal to see errors.
* *Pairing unavailable / read-only*: the service cannot protect its state directory (wrong owner or mode); on
  Windows the control API is unavailable until the private-file support there is verified.
* *Phone not found*: same Wi-Fi, no client isolation on the router; enter the phone's address manually.
* *Clipboard "No clipboard access"*: install `wl-clipboard` (Wayland) or `xclip` (X11).
* *Shortcut does nothing*: see Clipboard above; Wayland blocks global keys.
* *Port in use*: the service uses 17380/17381 on 127.0.0.1; see [service-integration.md](service-integration.md).
