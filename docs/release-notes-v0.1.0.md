## GeekBridge 0.1.0, the first stable release

GeekBridge links an Android phone to a Linux, Windows or macOS computer over your own Wi-Fi: calls, texts, contacts,
calendar, clipboard, files, notifications and more, with no account and no server in between. It speaks the same
pairing and transport protocol as KDE Connect for the shared features, but it is an independent project with its own
code.

**Please read "Known limitations" before installing.** This is a first stable version from a one-person project. It
has been tried on exactly one real phone; large parts of it have only been checked on an emulator and with scripted
test peers.

### Highlights

- **Calls.** An incoming call pops up on the computer with Answer, Decline, Hang up and Mute. You can also type a number
  on the computer (or run `geekbridge-ctl call dial NUMBER`) and the phone places the call after you confirm; the phone
  limits how often this can happen, shows a notice when it does, and can ask for its own confirmation. Answering and
  dialing need no Bluetooth. Hearing and speaking *on the computer* needs a Bluetooth controller in the PC (see
  limitations).
- **Messages (SMS).** Read conversations, search, and send texts from the desktop, with honest delivery status and
  new-message notifications (with a Reply action). Text only: no MMS, group or RCS.
- **Contacts and calendar.** A searchable phonebook with Call, Message and Copy actions. Month, week, day and agenda
  calendar views for the calendars on your phone (Google Calendar works through the phone's own sync, with no API key).
  Creating, editing and deleting one-off events from the computer is a separate opt-in.
- **Clipboard.** Text, links, HTML and images. Computer to phone is automatic; phone to computer is on demand with a
  hotkey (default Ctrl+Shift+V), because Android only lets the app in front read the clipboard.
- **Share.** Send files, links and text both ways: "Send to computer" in the Android Share sheet, drag and drop on the
  computer, `geekbridge-ctl send-file`. Transfers are checksummed (up to 8 GiB). Receiving is off until you allow it.
- **Notifications.** Mirror notifications from the phone apps you choose as desktop banners, with Dismiss and the
  phone's own buttons. Kept in memory only.
- **Battery.** The phone's battery level and charging state in the tray, on Home and in `geekbridge-ctl status`.
- **Recent photos.** A *Phone photos* tab in the Share window shows the newest 24 photos as small previews, with Save
  and Copy to clipboard. Between GeekBridge phones and computers only.
- **Doctor and "why is it disconnected".** `geekbridge-ctl doctor` checks the service, firewall rule, desktop session,
  phones, clipboard, calls and Messages, and prints the fix for each problem. Every paired phone without a link gets a
  plain sentence and a suggested fix in Home, the tray and `status`.
- **Auto-reconnect.** The phone's link service restarts at boot and reconnects with back-off; when you disconnect on
  the computer on purpose, the phone stays away for a few minutes instead of redialing at once.
- **Updater.** The apps check GitHub for a newer release, download it, verify its SHA-256 against `SHA256SUMS`, and
  then wait for you. Nothing installs by itself. Only final releases are offered, never prereleases.
- **Pairing.** Scan a QR code (Android asks for the camera only on the scan screen, or use the phone's own camera app), or pick the phone from the nearby list and
  compare a code on both screens. Both sides pin each other's certificate.
- **Design and access.** One look on Android and desktop (black or warm off-white, a single red accent, dot-matrix
  type), light and dark themes, keyboard navigation.

### Known limitations

- **One real phone.** Only one physical phone (a Samsung Galaxy Z Fold5, SM-F946B) has been paired with GeekBridge. Other
  phones, and especially other manufacturers' battery and permission screens (Xiaomi, Huawei, OnePlus, OPPO, vivo), have
  not been tried.
- **Not yet verified on hardware.** These work on an emulator and against scripted stand-in phones, and have not been
  confirmed on a real phone: dialing from the computer, notification mirroring, recent photos, the Messages permission
  guidance for sideloaded Android 13+, the screen-off heartbeat and battery saving, the "why is it disconnected"
  wording, LAN discovery (an emulator drops broadcast packets), and the update check against the real GitHub. The
  README feature table says "not yet" for every row until a person has checked it on a phone.
- **Windows and macOS are CI-tested only.** Both build and pass the automated tests on GitHub's runners and start and
  answer a smoke test there. Nobody has run GeekBridge on a real Windows PC or Mac. The binaries are **unsigned** (no
  Authenticode, no Developer ID, not notarised), there is **no prebuilt Windows or macOS download in this release**,
  clipboard images and notification buttons are not supported there, and the macOS menu-bar icon is switched off.
- **No call audio on the PC without Bluetooth.** Answer, decline, hang up and dial work over the Wi-Fi link, but the
  voice stays on the phone unless the computer has a Bluetooth controller and acts as the phone's hands-free device.
  Bluetooth audio itself has not been tested on any hardware.
- **Android 14 partial photo access.** If you give GeekBridge "selected photos" only, the Phone photos tab shows just
  those photos. The Android 14 permission dialog has not been exercised on a real phone.
- **Phone to computer clipboard is on demand** (Android 10+ restriction), and MMS, group and RCS messages are not
  supported.
- **Vendor battery guidance** for Samsung, Xiaomi, Huawei/Honor, OnePlus, OPPO/realme, vivo and Nothing is
  unit-tested for the order of settings screens only.
- **Long-running behaviour.** A four-hour soak of the desktop service with a stand-in phone showed no linear leak, but
  also unexplained short stalls of the local control API early in the run and a slow memory creep that was levelling
  off. Real multi-day use has not been measured.
- **Linux is the main target.** It has been run on Arch/Omarchy with Hyprland. Other desktops should work but have had
  less attention (the clipboard needs `wl-clipboard` or `xclip`).

### Install

Always download all files from the same release page, and check them (next section) first.

**Android (8.0 or newer).** Download `geekbridge-android-arm64.apk` (most phones from the last several years; about 3 MB)
or `geekbridge-android.apk` (universal, 6 MB) and open it. Android will ask you to allow "Install unknown apps" for your
browser or file manager. Then follow the setup wizard. Notification access and a few other items are granted by you in
Android's settings; the wizard says where.

**Linux (x86_64).**
- Debian, Ubuntu and relatives: `sudo apt install ./geekbridge_0.1.0_amd64.deb`
- Arch and relatives: `sudo pacman -U geekbridge-0.1.0-1-x86_64.pkg.tar.zst`
- Any distribution: unpack `geekbridge-0.1.0-linux-x86_64-installer.tar.gz` and run `./install.sh` (per user, no root; the
  service starts at login). `geekbridge-linux-x86_64.tar.gz` holds the plain binaries.

Open your firewall for TCP and UDP 1716-1764 from your LAN; `geekbridge-ctl firewall` prints the exact commands and
`geekbridge-ctl doctor` checks them. To uninstall with the installer: `./install.sh --uninstall` (keeps your pairings) or
`./install.sh --uninstall --purge-data` (deletes them).

**Windows and macOS.** There is no prebuilt download in this release. You can build from source (Rust 1.88 or newer;
see the README), or, if you accept an unsigned and unreviewed build, download the short-lived artifacts of the
repository's *Build binaries (artifacts only)* workflow, which needs a GitHub login to download. Expect SmartScreen on Windows and Gatekeeper on macOS to warn
about an unknown publisher. Allow TCP and UDP 1716-1764 in the system firewall (`geekbridge-ctl firewall` prints the
commands). We have not run the installation steps on a real Windows PC or Mac.

### Verify your download

Every release file is listed in `SHA256SUMS`. In the folder with the files:

- Linux: `sha256sum -c --ignore-missing SHA256SUMS`
- macOS: `shasum -a 256 -c SHA256SUMS` (it will complain about files you did not download)
- Windows PowerShell: `Get-FileHash .\geekbridge-android.apk -Algorithm SHA256`, and compare with the line in `SHA256SUMS`

Both APKs are signed with the project's release key. The certificate SHA-256 fingerprint is printed at the bottom of
these notes; compare it with `apksigner verify --print-certs geekbridge-android.apk` if you want to be sure. The same
fingerprint is what Android checks before it accepts an update, so a different key will be refused.

### Upgrading from 0.1.0-rc.1

- **Android:** rc.1 was signed with a debug key and cannot be updated to this release-signed version. **Uninstall rc.1
  first** (app data and pairings are lost), install 0.1.0, and pair again; if the computer still lists the old phone,
  unpair it there first. From now on updates install in place.
- **Linux (and other desktops):** install the new package or run the new `install.sh`; your pairings and settings are
  kept. The in-app updater also offers 0.1.0 to an rc.1 install.
- After an update the apps show a dismissible "What's new" card once.

### Privacy

- There are no accounts and no GeekBridge servers. Your phone and computer talk to each other directly over the local
  network, encrypted with pinned certificates. Nothing is sent to us.
- The **only** internet traffic is the optional update check to GitHub (`api.github.com`, and the download if you ask
  for it). Turn it off with `geekbridge-ctl update disable` or with the switch in the "App updates" card of the Android Diagnostics screen.
- Everything sensitive is **off by default** and is enabled separately for each paired computer, on both the phone and
  the computer: clipboard, contacts, calendar (and calendar editing), SMS, calls and dialing, battery level, notification
  mirroring, recent photos and file receiving.
- Nothing from the phone is stored on the computer. Battery level, mirrored notifications and photo previews are held in
  memory only.
- Diagnostic reports (`geekbridge-ctl diagnose`, Android *Copy report*) are redacted and meant to be safe to share.

### Security

Several security reviews and a code review were done, with the findings fixed and covered by tests (see `SECURITY.md`
and `docs/reports/` in the repository). Report vulnerabilities privately through the repository's *Security* tab
(*Report a vulnerability*).

Security contact: **[OWNER: add the monitored security contact here before publishing]**

### Licence

GPL-3.0-or-later. Third-party components and their licences are listed in `THIRD_PARTY_NOTICES.md`. GeekBridge is
independent software, not affiliated with or endorsed by KDE; "KDE Connect" is named only to describe protocol
compatibility.
