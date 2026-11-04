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

**Verified where.** The Linux packages and the Android app are the tested targets; one real phone (Samsung Galaxy Z
Fold5, SM-F946B) has been paired. The Windows installer and the macOS `.dmg` are built and tested in CI only, have never
been run on a real PC or Mac, and there are **no prebuilt Windows or macOS binaries yet** (build from source).

**Debug or release APK.** The release APK is signed with the project's release key and is the one to install. A debug
APK (from `scripts/build-android.sh`) uses a local debug key: it cannot be updated by, or update to, a release build, and
the app refuses such an update. Uninstall one before installing the other (this removes the phone's pairing; pair
again).

**Uninstall.** Linux: `./install.sh --uninstall` removes the program but keeps your pairings, identity and settings in
`~/.local/share/geekbridge` (`$XDG_DATA_HOME/geekbridge/service`); add `--purge-data` to delete that folder (and the updater's notes in `~/.local/state/geekbridge`) too (`./install.sh --uninstall --purge-data`). The
package route is `sudo apt remove geekbridge`. Windows: Settings > Apps. macOS: drag GeekBridge out of Applications. Android: uninstall the app. Also unlink on the other side (the computer's **Link a phone** page > **Unlink…**; the phone's setup step 1 > **Unlink**), or the other side keeps listing the pairing.

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
| Show phone notifications / Stop showing phone notifications | only offered when a connected phone declares notification mirroring (or it is on); switches the per-phone consent |
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

Requirements: Android 8 or newer, phone and computer on the same network (not a guest network), the background service running, and the
computer's firewall open for TCP and UDP **1716-1764** from your LAN (see Troubleshooting). Two names, one job: the computer says
**Link a phone**, the phone says **Pair a computer**; the result is a pairing, and **Unlink** removes it on either side.
Optional Linux programs: `wl-clipboard` or `xclip` (clipboard), `libnotify` / `notify-send` (call buttons in notifications),
`bluez`, `pipewire`, `pipewire-pulse` and `wireplumber` (call audio only).

While no phone is paired, the GeekBridge Home window shows a **Get started** card: a QR code and link for the Android
app, the pairing steps (it names phones already announcing themselves), which optional features exist and stay off,
this computer's name, the update status, whether the service starts at login, and, when it can see that a firewall
blocks 1716-1764 (ufw or firewalld on Linux, Windows Defender Firewall, the macOS application firewall), the exact
commands to copy. GeekBridge never runs them for you. `geekbridge-ctl firewall` prints the same commands and
`geekbridge-ctl autostart [status|enable|disable]` starts the service at login (XDG autostart, LaunchAgent or the per-user
Run key).

**With a QR code (quickest, no code to compare), like WhatsApp Web.**

1. On the computer, open GeekBridge and click **Link a phone** (the first time, with no phone paired, the code is
   already on screen). The code is big, works once, lives two minutes and **renews by itself** before it expires: there
   is no refresh button. Under it the computer says what is happening: **Waiting for phone…**, then **Phone scanned —
   confirm on phone**, then **Paired ✓ with <phone>**. The code is only kept while that view is open; leave it and the
   code is revoked.
2. On the phone: GeekBridge > setup wizard step 1 (or Home > **Pair a computer**) > **Scan QR code from my computer**.
   The phone's camera opens inside the app (the camera permission is asked only here, and only the first time); point
   it at the code. The phone's own camera app works too: scan, then tap the GeekBridge link it shows. Or paste the
   link (the computer's **Can't scan? Use the link** has **Copy link**).
3. The phone asks **Connect to <computer name>?** Check the name and tap **Continue**, then **Pair**. Home opens with
   **Paired ✓ with <computer>**.

**No GeekBridge on the computer yet?** On the phone, step 1 has **Don't have GeekBridge on your computer yet?**. Tap
**Send me the download link** and pick any app (email, a chat with yourself, notes): the text holds the public releases
page (https://github.com/fspecii/geekbridge/releases/latest) and three install lines, nothing about you or this phone.
**Email it to myself** opens a draft in your mail app instead. Or **Copy link**, or type the short address
`github.com/fspecii/geekbridge/releases/latest` on the computer. GeekBridge sends nothing itself and needs no account.

**Unlink.** Computer: **Link a phone** page > **Unlink…** next to the phone > **Unlink phone** (it stops being trusted;
link it again with a new code). Phone: setup wizard step 1 > **Unlink** next to the computer (or Diagnostics &
about > **Unlink**). Unlink on both sides to remove a pairing completely; a new QR code always pairs again.

**By name (nearby list).** The phone appears on the computer as "Found, not paired" and the computer in the phone's
setup step. Tap **Pair** on either side; both screens show an 8-character code. Confirm only if they match.

If either side cannot find the other, use **Test connection** (both apps have it) or enter the address by hand under
*Connect by address*. Then finish the setup wizard (notifications, contacts, calendar, phone state and the **battery**
step so Android keeps the link alive). Details: [pairing.md](pairing.md).

## Recent photos on the computer

Off by default, on the phone and on the computer. On the phone, open Settings and switch on *Show recent photos on the
computer*; Android then asks for access to your photos (on Android 14 and later you may choose *Select photos* and share
only those) and you allow the computer. On the computer, open **GeekBridge Share**: when a connected phone offers photos
the window gets a *Phone photos* tab, which first shows an *Allow photos from PHONE* button (or run
`geekbridge-ctl consent DEVICE_ID photos on`). After that, opening the tab asks the phone for its newest 24 photos and
shows small previews. Press a preview and choose **Save** (the full picture goes to the save folder) or **Copy to
clipboard** (needs `wl-copy` or `xclip` on Linux and a JPEG, PNG, GIF or WebP picture). Only the previews are sent to
the list; the full picture leaves the phone when you press one of those buttons. Nothing is uploaded in the background,
the list is kept in memory only (erased when you press *Stop showing photos*, when the link ends or after 30 minutes)
and `geekbridge-ctl photos` shows what is held. See [design/recent-photos.md](design/recent-photos.md). Checked with
automated tests and a stand-in phone; not yet confirmed on a real phone.

## Phone notifications on the computer

Off by default. On the phone, open Settings and switch on *Show phone notifications on the computer*, then follow the
three prompts it shows: open the Notification access screen and switch GeekBridge on, choose which apps may be shown
(none are chosen at first, so nothing is sent), and allow the computer (*Allowed computers...*). On Android 13 and later,
if GeekBridge was not installed from a store the switch is greyed out: open app settings, tap the three dots, choose
*Allow restricted settings* (on Samsung: Settings > Apps > GeekBridge > three dots), then try again. On the computer
switch on *Show phone notifications* (Home or the tray menu, or `geekbridge-ctl consent DEVICE_ID notifications on`). A
banner names the app; the text is shown only if you enabled message content in notifications. *Dismiss* on the banner
also clears it on the phone, and the phone's own buttons (never a typed reply) can be pressed. `geekbridge-ctl
notifications` lists what is held (memory only, at most 50 per phone for 3 hours). Ongoing, call, foreground-service and
GeekBridge's own notifications are never sent. Checked with automated tests; not yet confirmed on the real phone.

## Phone battery on the computer

The computer can show the phone's battery level and whether it is charging (tray tooltip, the Home phone card,
`geekbridge-ctl status`, waybar). It is **off by default on both ends**, because it reveals when a phone is idle or away:
on the phone switch on *Share battery level* in Settings and allow it for that computer; on the computer switch on *Show
phone battery* (or `geekbridge-ctl consent DEVICE_ID battery on`). The value is kept in memory only. It uses the stock KDE
Connect battery packets. Checked with automated tests and the emulator; not yet confirmed on the real phone.

## Disconnecting and the phone's redial

**Disconnect** (Home, the tray, `geekbridge-ctl disconnect`) keeps the pairing and closes the link. A GeekBridge phone
is told (the `geekbridge.goodbye` packet), so it does not dial straight back: both sides stay off each other for 5
minutes, or until either one connects by hand. Stopping the service tells the phone too, but then it keeps redialing
with its normal back-off and returns as soon as the service is back. A stock KDE Connect phone is not sent anything
and may reconnect on its own.

## Why is my phone not connected

When a paired phone has no link, Home, the tray tooltip, waybar and `geekbridge-ctl status` / `doctor` give one plain
sentence and a suggested fix instead of a generic "not reachable". Typical reasons: the computer has no network or is on a
different network from the phone; the service is not listening; you paused the connection; the pairing was removed on the
phone; the phone is still reconnecting or gave up after 12 tries; the phone is silent or its link timed out; or the
firewall blocks 1716-1764 (`geekbridge-ctl` can see this, the apps cannot). The reason is a best guess from what the
service knows. See [reports/why-disconnected.md](reports/why-disconnected.md).

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

## The command line

`geekbridge-ctl --help` is the authoritative list. Groups: **link** (`status`, `connect`, `pair`, `pair-qr`,
`test-connection`, `approve`, `deny`, `disconnect`, `unpair`, `rename`, `ping`, `events`); **permissions** (`consent DEVICE
FEATURE on|off` with FEATURE one of `sms`, `contacts`, `calendar`, `clipboard`, `notifications`, `telephony`, `debug`,
`battery`, `photos`, `calendar_edit`, `calendar_alerts`); **phone data** (`agenda`,
`agenda-refresh`, `calendar-add|delete|retry`, `messages`, `messages-refresh`, `messages-send`, `messages-resend`, `messages-read`, `notifications`, `photos [refresh|save|copy]`); **calls**
(`calling`, `calls`, `call answer|reject|hangup|mute|audio|dial|setup|audio-devices`, `call-audio`); **clipboard and sharing** (`clipboard ...`,
`send-file|text|url`, `share ...`); **this computer** (`doctor`, `diagnose`, `report [--preview|--open|--copy]`,
`autostart`, `tray`, `firewall`, `hotkey`, `update`, `whats-new`, `token-path`). `phone-log` and `phone-state` only work
with a debug build of the phone app. `report` shows exactly what a prefilled bug report carries (the redacted `diagnose`
output) and sends nothing itself.

## Updates

A banner appears when a newer release exists: **Download** (checked against the published SHA-256), **Show file**,
then install it yourself. Nothing installs automatically. Checks run shortly after the service starts and then daily;
**Check for updates** forces one. The Android app only accepts an update signed with the same key as the installed one, so
a debug build never updates to a release (debug builds follow the *dev* channel, releases the *stable* one: `geekbridge-ctl update channel`). Turn checks off under *Settings*, or
`geekbridge-ctl update disable`. Details: [updates.md](updates.md).

## Privacy

No accounts, no GeekBridge servers, no analytics. Everything stays on your local network. Contacts, calendar, Messages,
battery level, phone notifications, recent photos, clipboard, calls (answering and dialing) and file receiving are each **off until you allow them for a
specific phone**, on both the phone and the computer, and can be switched off again at any time. Nothing from the phone is
written to the computer's disk (contacts, calendar, messages, battery level, mirrored notifications and photo previews are held in memory and erased when the
service stops); received files and photos you save are what you ask it to keep (a photo copied to the clipboard sits in a private
scratch folder for up to 10 minutes, and a debug-build phone's redacted log is written to the state directory). Message text, and the text of mirrored phone notifications, is hidden in desktop banners by default, and
caller numbers and message text are masked in logs and in the redacted `diagnose` report. The only internet traffic is
the optional update check to `api.github.com` (turn it off with `geekbridge-ctl update disable`). Reporting problems:
[SECURITY.md](../SECURITY.md). The full data-flow review is [privacy.md](privacy.md).

## Troubleshooting

Questions people ask first are in the [FAQ](FAQ.md). Start with `geekbridge-ctl doctor` (run it as the same user as the service, in your desktop session): it checks the service, the firewall (ufw/firewalld ports 1716-1764 on Linux, Windows Defender Firewall, the macOS
application firewall), your desktop session (Wayland/X11 variables; Linux), paired phones, clipboard, calls and Messages, and prints the fix for each problem
(it exits 1 if a check failed). Then the quick checks: `geekbridge-ctl status` (is the service up, which phones are paired and linked),
`geekbridge-ctl diagnose` and, on the phone, **Diagnostics & about > Copy report** (both are redacted and safe to attach
to a bug report). `geekbridge-ctl report` builds a prefilled bug report and shows what it carries before you send anything.

### Cannot pair
* **QR code "expired" or "already used".** The code lasts 2 minutes, works once and the computer replaces it by itself: scan the one on the screen now. If the computer says nothing happened after you scanned, the phone did not reach it (next two items).
* **The phone cannot scan.** Camera permission denied: the scan screen says so and **Open app settings** turns it on;
  or use the phone's own camera app on the code, or paste the link (**Use another way**). The code must be fully in
  view, not too close to the glare of the screen. A QR code that is not GeekBridge's is ignored with a hint.
* **Scanned, but nothing happens / "The computer did not answer".** The phone cannot reach the computer: same Wi-Fi
  (not a guest network), no VPN, and the firewall open for 1716-1764 (the first-run card prints the commands). Run
  `geekbridge-ctl doctor` on the computer for the exact fix.
* **"Different network".** Phone and computer must be on the same subnet. Turn the phone's mobile data off or its VPN
  off, and check that the computer is not on a VPN or a second Wi-Fi.
* **Code mismatch or "wrong computer".** Cancel on both sides and start again; never confirm a code that differs.
* Use **Test connection** on the computer and the phone: it names the likely cause (service not running, firewall or
  Wi-Fi isolation, another network). Nothing is paired until you confirm on the phone.
* **"Security warning: certificate changed".** The computer or phone was reinstalled. Unlink on both sides
  (computer: **Unlink…**; phone: **Unlink**) and link again with a new QR code.

### The phone does not show my computer / "Messages is off"
* **No computer listed on the phone:** run `geekbridge-ctl doctor`. Usually the firewall drops UDP 1716 (the broadcast the
  phone listens for) or the service is not running; **Link a phone** (QR) works even when broadcasts are blocked.
* **Messages is off:** it is opt-in per phone. Click **Allow** in Messages, or `geekbridge-ctl consent DEVICE_ID sms on`
  (`doctor` prints the exact line). If it then says the phone does not offer Messages, do this on the phone: GeekBridge >
  Share with computer > switch **Messages** on > **Allow access** and accept the SMS permission (it covers read, send and
  receive together; GeekBridge never needs to be your default messaging app). On an app installed from an APK file,
  Android 13 and newer may refuse silently with no dialog: open Settings > Apps > GeekBridge, tap the **three-dot menu at
  the top right**, choose **Allow restricted settings** (it appears only after you have tried **Allow access** once), then
  Permissions > SMS > Allow. Then reopen GeekBridge; the computer picks up the change by itself. The permission steps are
  tested on an emulator; Samsung's flow on the real phone has been analysed but not yet confirmed. Details:
  [phone-data-link.md](phone-data-link.md).

### Phone not found
* The computer announces itself every 15 s and listens on UDP 1716. Routers that filter broadcast traffic (guest Wi-Fi,
  "AP/client isolation", some mesh systems) hide devices from each other; use **Link a phone** (QR) or
  **Connect by address** instead, which need no broadcast. There is no mDNS.
* Another program may own UDP 1716 (KDE Connect or GSConnect running on the same computer). GeekBridge reports the
  conflict rather than taking over: quit the other app or use the QR code.
* The Android service stops when no computer is paired or **Stay connected** is off: open the app once, switch
  **Stay connected** on.

### Firewall, VPN, guest Wi-Fi
* Allow TCP **and** UDP **1716-1764** from your LAN (`geekbridge-ctl firewall` prints the commands for your firewall,
  `geekbridge-ctl doctor` tells you whether they are in place; the checks read ufw, firewalld, Windows Defender Firewall or the macOS application firewall and say so when they cannot). ufw: `sudo ufw allow from 192.168.1.0/24 to any port 1716:1764 proto tcp`
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

### Hear and talk through your computer (call audio)
Open **Settings > Call audio on this computer** (or the Recents tab of Calls). It lists what is in place and what is not, each
failing step with a one-line fix and a **Copy command** button (GeekBridge never runs the command and never uses `sudo`).
**Test call audio** plays a short tone through the chosen headphones and shows the microphone level for 5 seconds, no call
needed. The full guide, with what to buy and the decision table per OS, is [call-audio-options.md](call-audio-options.md).

| Your computer | What you get |
| --- | --- |
| Linux with Bluetooth, phone paired as hands-free | Voice through the computer's headset (not yet verified on real hardware) |
| Windows 11 with Bluetooth | Same, through Windows' own hands-free (phone paired in Settings, Phone calls on; not yet verified) |
| macOS | Answer here, talk on the phone, or use a Bluetooth headset paired with the phone. Call audio on the Mac is off until it is verified |
| No Bluetooth | Answer here, talk on the phone. To talk through the computer add a USB Bluetooth adapter (any using the kernel `btusb` driver, Bluetooth 5.x) |

* Answering, declining and hanging up work **over the GeekBridge link without Bluetooth**: switch **Answer from
  computer** on in the phone's Home and allow *Calls* for the phone on the computer. The voice then stays on the phone.
* On the phone, the **Answer calls** setup step has a **Pair phone with your computer for call audio** button that opens
  Android's Bluetooth settings and shows whether the phone sees a connected hands-free device. Android asks for the
  Bluetooth permission only there, and GeekBridge never scans.
* **Dialing from the computer** works the same way: type a number on the Calls keypad (or
  `geekbridge-ctl call dial +15550100`), confirm it, and the phone places the call. It needs *Allow placing calls from
  my computer* in the phone's Answer calls step. Digits and a leading `+` only.
* No Bluetooth controller is reported as information, not an error (`geekbridge-ctl doctor`, `geekbridge-ctl call-audio`).
  Carrying call audio over Wi-Fi is not possible on a stock Android phone. Dialing was tested against a stand-in phone only.
* Call **audio** on the computer needs the computer to be the phone's Bluetooth hands-free device: a PC Bluetooth
  controller (many desktops have none; a USB adapter works), the phone paired as a hands-free device with *Calls*
  allowed, and PipeWire/WirePlumber (the defaults on Arch/Omarchy are right). `geekbridge-ctl calling` walks through
  every step with a fix; `geekbridge-ctl call setup` pairs, trusts and connects the phone (compare the six-digit code on
  both screens) and `call setup --fix-config` fixes the WirePlumber settings without sudo. A call answered on the
  computer then plays on the headphones and microphone chosen in Settings (or `geekbridge-ctl call audio-devices`; default: the
  system's); `call mute on|off` mutes the real microphone; `call audio phone` sends the audio back to the phone and
  `call audio` brings it here. A call answered on the phone stays on the phone. Details and the hardware test:
  [reports/call-audio-pc-headset.md](reports/call-audio-pc-headset.md). Verified in a sandbox with PipeWire's real
  Bluetooth code and a fake phone; not yet on real hardware.
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

### Phone battery drain and the screen-off heartbeat
While the phone's screen is off the app pings the computer every 60 s instead of every 20 s, and it stops broadcasting
its presence while every paired computer is already linked. The link still survives a locked phone; the computer keeps its
own 20 s check and never drops a sleeping phone. This was measured by code audit and tests, not by a battery run on a
real phone ([reports/android-battery-audit-2.md](reports/android-battery-audit-2.md)).

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
