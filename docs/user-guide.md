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

### The tray icon

The service shows a small icon in the notification area (Linux and Windows) so GeekBridge is a quiet background
presence. It is a dot-matrix **G** with a corner mark: a check (phone connected), a dash and a dimmed G (no phone), a
ring (a call is ringing), an up arrow (update available) or a down arrow (a file is being transferred). Left click opens
GeekBridge. The menu (right click) shows the phone name and a status line, then:

| Item | Does |
|---|---|
| Answer / Decline | only while a call rings, and only the controls the call backend really offers |
| Open GeekBridge | opens the app window |
| Paste clipboard from phone | the same as the paste-from-phone hotkey (needs clipboard sync on for a connected phone) |
| Send file... | opens the share window |
| Pause sync / Resume sync | switches the clipboard feature off for the phones that have it on, and back on for exactly those |
| Check for updates | asks GitHub for a newer release now |
| Quit | stops the service until the next login (or `geekbridge-service`) |

It shows no caller name, number, message or clipboard text. It is optional: with no tray host (no StatusNotifier
watcher on Linux) it silently does nothing, and it appears as soon as a bar starts. On Linux it needs a bar that hosts
StatusNotifier icons: waybar's `tray` module (see [Omarchy / Hyprland](omarchy.md)), KDE Plasma, or GNOME with the
AppIndicator extension. To turn it off for good: `geekbridge-ctl tray disable` (back on with `enable`; it takes effect at
the next start of the service), or start the service with `--no-tray` / `GEEKBRIDGE_NO_TRAY=1`. A service started with a
private `--state-dir` (tests, demos) shows no icon unless `--tray` is given. On macOS the menu-bar item is not active yet,
open **GeekBridge** from Applications.

## Pair a phone

Requirements: Android 8 or newer, phone and computer on the same network, the background service running, and the
computer's firewall open for TCP and UDP **1716-1764** from your LAN (see Troubleshooting).

**With a QR code (quickest, no code to compare).**

1. Open **GeekBridge** on the computer and choose **Pair with QR code**. The code lives two minutes and works once.
2. On the phone: GeekBridge > setup wizard (or Home) > **Scan QR code**, then point the phone's camera app at the code
   and tap the GeekBridge link it shows (or paste the link; the computer's QR window has **Copy link**).
3. The phone shows "Pair with <computer>?". Tap **Pair**.

**By name (nearby list).** The phone appears on the computer as "Found, not paired" and the computer in the phone's
setup step. Tap **Pair** on either side; both screens show an 8-character code. Confirm only if they match.

If either side cannot find the other, use **Test connection** (both apps have it) or enter the address by hand under
*Connect by address*. Then finish the setup wizard (notifications, contacts, calendar, phone state and the **battery**
step so Android keeps the link alive). Details: [pairing.md](pairing.md).

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

On Omarchy / Hyprland, `geekbridge-ctl hotkey install` binds the key for you, floats the call and share windows and
documents a waybar status line; see [omarchy.md](omarchy.md) (it also lists the GNOME, KDE and sway steps).

## Share files, links and text

Drop files on the GeekBridge window (they open in **Geekbridge Share** for the first connected phone), or open
Share and use **Choose files**. From the phone, use *Share → Send to computer*. Receiving on the computer is off per
phone until you allow it; received files land in `~/Downloads/GeekBridge`. Details: [sharing.md](sharing.md).

## Updates

A banner appears when a newer release exists: **Download** (checked against the published SHA-256), **Show file**,
then install it yourself. Nothing installs automatically. Turn checks off under *Settings*, or
`geekbridge-ctl update disable`. Details: [updates.md](updates.md).

## Troubleshooting

Start with `geekbridge-ctl doctor`: it checks the service, the firewall (ufw/firewalld ports 1716-1764), your desktop
session (Wayland/X11 variables), paired phones, clipboard, calls and Messages, and prints the fix for each problem
(it exits 1 if a check failed). Then the quick checks: `geekbridge-ctl status` (is the service up, which phones are paired and linked),
`geekbridge-ctl diagnose` and, on the phone, **Diagnostics & about > Copy report** (both are redacted and safe to attach
to a bug report).

### Cannot pair
* **QR code "expired" or "already used".** The code lasts 2 minutes and works once. Press **Pair with QR code** again.
* **"Different network".** Phone and computer must be on the same subnet. Turn the phone's mobile data off or its VPN
  off, and check that the computer is not on a VPN or a second Wi-Fi.
* **Code mismatch or "wrong computer".** Cancel on both sides and start again; never confirm a code that differs.
* Use **Test connection** on the computer and the phone: it names the likely cause (service not running, firewall or
  Wi-Fi isolation, another network). Nothing is paired until you confirm on the phone.
* **"Security warning: certificate changed".** The computer or phone was reinstalled. Forget the pairing on both sides
  (computer: unpair; phone: Forget) and pair again.

### The phone does not show my computer / "Messages is off"
* **No computer listed on the phone:** run `geekbridge-ctl doctor`. Usually the firewall drops UDP 1716 (the broadcast the
  phone listens for) or the service is not running; **Pair with QR code** works even when broadcasts are blocked.
* **Messages is off:** it is opt-in per phone. Click **Allow** in Messages, or `geekbridge-ctl consent DEVICE_ID sms on`
  (`doctor` prints the exact line). If it then says the phone does not offer Messages, switch on Messages and allow the
  SMS permission in the phone app.

### Phone not found
* The computer announces itself every 15 s and listens on UDP 1716. Routers that filter broadcast traffic (guest Wi-Fi,
  "AP/client isolation", some mesh systems) hide devices from each other; use **Pair with QR code** or
  **Connect by address** instead, which need no broadcast. There is no mDNS.
* Another program may own UDP 1716 (KDE Connect or GSConnect running on the same computer). GeekBridge reports the
  conflict rather than taking over: quit the other app or use the QR code.
* The Android service stops when no computer is paired or **Stay connected** is off: open the app once, switch
  **Stay connected** on.

### Firewall, VPN, guest Wi-Fi
* Allow TCP **and** UDP **1716-1764** from your LAN. ufw: `sudo ufw allow from 192.168.1.0/24 to any port 1716:1764 proto tcp`
  and the same for `udp` (use your own subnet). firewalld: `sudo firewall-cmd --add-port=1716-1764/tcp --add-port=1716-1764/udp --permanent`.
  Windows prompts once for "Private network"; accept it.
* VPNs that route all traffic (full tunnel) usually block LAN access; use split tunnelling or disconnect.
* Guest Wi-Fi and hotel/office networks isolate clients: pairing and transfers will not work across them.

### Clipboard not syncing (Wayland and X11)
* Sync is **off until you switch it on** for the phone, on both sides.
* Install the clipboard tool: `wl-clipboard` on Wayland (needs a compositor with the data-control protocol: Hyprland,
  Sway, KDE 5.27+; stock GNOME does not have it) or `xclip` on X11. The service needs your session's
  `WAYLAND_DISPLAY`/`DISPLAY`; if it started before login completed, run `systemctl --user restart geekbridge-service`.
* **Phone to computer is on demand**: Android 10+ lets an app read the clipboard only while it has focus, so copy on
  the phone, then press the hotkey (or **Paste from phone**) on the computer; the phone briefly flashes a window.
* Items from password managers are marked sensitive and skipped by design; text over 64 KiB is not sent.

### Hotkey does nothing
* Wayland has no global-hotkey API for apps; the compositor must bind it. On Omarchy/Hyprland run
  `geekbridge-ctl hotkey install` (dry run), then `geekbridge-ctl hotkey install --apply`. Elsewhere bind
  `geekbridge-ctl clipboard paste-from-phone --notify` in your keyboard settings ([omarchy.md](omarchy.md) has GNOME, KDE
  and sway steps).
* Plain **Ctrl+Shift+V** is the terminal paste key: pick another combination (`geekbridge-ctl clipboard hotkey "Ctrl+Alt+P"`)
  if a global bind would swallow it. `geekbridge-ctl hotkey status` shows what is bound.

### Transfers stall or fail
* Receiving on the computer is **off per phone until you allow it** (Share window); the phone shows "Waiting" until then.
  Offers expire after 2 minutes.
* A transfer needs a direct TCP connection to a payload port in 1716-1764: a firewall that allows only the first port
  makes small items work and files stall at "Waiting". Open the whole range.
* Switching Wi-Fi/VPN or the phone sleeping interrupts a transfer; resend it. At most 3 transfers run at once (the rest
  queue); the limit is 8 GiB per file. Received files go to `~/Downloads/GeekBridge` (phone: `Download/GeekBridge`).

### No call audio / no Bluetooth
* Answering, declining and hanging up work **over the GeekBridge link without Bluetooth**: switch **Answer from
  computer** on in the phone's Home and allow *Calls* for the phone on the computer. The voice then stays on the phone.
* Call **audio** on the computer needs the computer to be the phone's Bluetooth hands-free device: a PC Bluetooth
  controller (`bluetoothctl show` must list one; many desktops have none; a USB adapter works), the phone paired over
  Bluetooth with "Phone calls" enabled, and an HFP-capable PipeWire/WirePlumber stack. `geekbridge-ctl calling` shows
  the verdict; try `geekbridge-ctl call audio` to move audio. Call audio has not been verified on real hardware.
* No incoming-call popup on Hyprland/mako: see [omarchy.md](omarchy.md) (notification buttons, floating window rule).

### Battery optimisation kills the Android service
Android (and especially vendor skins) stops background apps. In the setup wizard's **battery** step choose *Allow
unrestricted battery*, then the vendor step:
* **Samsung:** Settings > Apps > GeekBridge > Battery: *Unrestricted*; Settings > Battery > Background usage limits: remove GeekBridge from *Sleeping apps* and *Deep sleeping apps*; turn off *Put unused apps to sleep* for it.
* **Xiaomi/Redmi/POCO:** Autostart: on; Settings > Apps > GeekBridge > Battery saver: *No restrictions*; lock the card in Recents.
* **Huawei/Honor:** Settings > Battery > App launch > GeekBridge > *Manage manually*; switch on Auto-launch, Secondary launch and Run in background.
* **OnePlus:** Settings > Battery > Battery optimisation: *Don't optimise*; allow background activity and auto-launch under the app's battery usage; lock in Recents.
* **OPPO/realme:** Auto-launch / Startup manager: on; Settings > Battery > GeekBridge: allow background activity; lock in Recents.
* **vivo:** Autostart / Background start-up manager: allow; Settings > Battery > Background power consumption: *Allow high background power consumption*.
* **Nothing OS and stock Android/Pixel:** *Unrestricted* app battery usage is enough.
Menu names vary by version; the wizard opens the closest screen it can find. Real vendor devices have not been tested.

### Update not offered
* Checks run about 15 s after the service starts and then daily; press **Check for updates** to force one (desktop
  Details panel, Android *Diagnostics & about*). Automatic checks may be switched off (`geekbridge-ctl update status`).
* Only published, non-prerelease GitHub releases are offered, and only if the release has an installer for your OS and
  a matching `SHA256SUMS` line. A version you chose to **Skip** stays hidden until a newer one appears.
* The check needs outbound HTTPS to `api.github.com`; a rate limit (403/429) pauses it for a while. The Android update
  installs only over an app signed with the same key and needs *Install unknown apps* allowed for GeekBridge.

### Other
* *"Service not running"*: `systemctl --user status geekbridge-service` (Linux), start it with
  `systemctl --user start geekbridge-service` or run `geekbridge-service` in a terminal to see errors.
* *Pairing unavailable / read-only*: the service cannot protect its state directory (wrong owner or mode); on
  Windows the control API is unavailable until the private-file support there is verified.
* *Port in use*: the service uses 17380/17381 on 127.0.0.1; see [service-integration.md](service-integration.md).
