# FAQ

Short answers. The full steps are in the [user guide](user-guide.md). For anything not listed, run `geekbridge-ctl doctor` on
the computer: it says what is wrong in plain words and prints the fix.

## My phone does not show my computer (or the computer does not show the phone)

Usually the computer's firewall is dropping the phone's broadcast, or the two are not on the same network. Use **Link a phone**
on the computer and scan the QR code from the phone (**Scan QR code from my computer** in the setup wizard): it works even when
broadcasts are blocked. Then allow TCP and UDP **1716-1764** from your LAN in the firewall (`geekbridge-ctl firewall` prints the
commands; `geekbridge-ctl doctor` checks them). If the QR code scan says "the computer did not answer", it is the firewall,
a guest Wi-Fi or a VPN. See [Cannot pair](user-guide.md#cannot-pair).

## Does it work on a guest Wi-Fi, a hotel network or over mobile data?

No. Phone and computer must be able to reach each other directly on one local network. Guest networks and many office or hotel
networks isolate clients ("AP isolation"), and a VPN that routes everything usually hides the LAN. Turn the VPN off or use split
tunnelling. Connecting from outside your network is not supported.

## Does it need the internet?

Not to work. Pairing, calls, Messages, clipboard and file transfer run directly between your two devices. You need the internet
to download the apps and, if you leave it on, for the optional update check.

## What leaves my network?

Nothing from your phone. The only internet traffic is the optional update check to `api.github.com` (and the download if you ask
for it); turn it off with `geekbridge-ctl update disable` or in the phone's App updates card. There are no accounts and no
GeekBridge servers. Everything that shares phone data (contacts, calendar, Messages, notifications, photos, battery, clipboard,
calls) is off until you allow it for a specific phone, on both ends. Details: [privacy](privacy.md).

## Messages is off or says the phone does not offer Messages

Messages is opt-in on both ends. On the computer press **Allow** in Messages (or `geekbridge-ctl consent DEVICE_ID sms on`). On the
phone open GeekBridge > Share with computer, switch **Messages** on and **Allow access** to SMS. If you installed the APK yourself
on Android 13 or newer, Android may refuse silently: open Settings > Apps > GeekBridge, tap the three dots at the top right, choose
**Allow restricted settings**, then Permissions > SMS > Allow. (The same step unlocks **Show phone notifications**.) MMS, group
chats and RCS are not supported. See [Messages is off](user-guide.md#the-phone-does-not-show-my-computer--messages-is-off).

## There is no sound from a call on the computer

Answering, declining and hanging up work over the GeekBridge link with no Bluetooth, but **the voice stays on the phone**.
To hear and speak on the computer, the computer must be the phone's Bluetooth hands-free device: a Bluetooth controller in the PC
(a USB adapter works), a one-time Bluetooth pairing of the phone with it as hands-free with Phone calls allowed, and PipeWire plus
WirePlumber on Linux.
Open **Settings > Call audio on this computer** for a checklist with a fix for each step, or run `geekbridge-ctl calling`. Call
audio over Wi-Fi exists only as an experimental, off-by-default option on debug phone builds (`geekbridge-ctl callwifi`;
whether a real phone allows it is not settled and it may not work on yours, see [the report](reports/call-audio-wifi-realphone.md)).
On macOS call audio is not available yet. Bluetooth voice is unverified on real hardware by the maintainers. See
[call audio](call-audio-options.md); when a step fails: [call audio troubleshooting](call-audio-troubleshooting.md).

## Where are my missed calls, and can I decline with a text?

The Calls window has a **Recents** tab with the last 30 calls since the service started (memory only, gone when it
restarts). A missed call also raises a banner with **Call back** and **Message**. A call you declined on the phone looks
like a missed one. **Decline with message** on the incoming-call card sends one of three preset texts (edit them in
Settings); it needs Messages allowed for the phone on both ends. If a call misbehaves, `geekbridge-ctl calls --trace` shows
a redacted event trace. See [Calls on the computer](user-guide.md#calls-on-the-computer).

## The keypad is missing during a call

The keypad (for phone menus) works over Bluetooth hands-free. Over the GeekBridge link Android only lets the phone's default
dialer app play keypad tones, so on a normal phone it is hidden there. Nothing is wrong with your setup.

## Why do I need the phone unlocked to paste its clipboard?

Android only lets an app read the clipboard while it is in the foreground. Copy on the phone, then press the paste-from-phone key
(or **Paste from phone**) on the computer; the phone flashes a window for a moment. Computer to phone is automatic once clipboard
sync is on for that phone on both sides.

## Files stall at "Waiting"

Receiving on the computer is off per phone until you allow it (Share window > Receiving), and file transfers use other ports in
1716-1764, so the whole range must be open, not only 1716.

## The connection drops when the phone's screen is off

Android's battery saver stopped the app. In the setup wizard's battery step choose **Allow unrestricted battery**, then follow the
step for your phone's maker (Samsung, Xiaomi, OnePlus and others hide a second switch). See
[Battery optimisation](user-guide.md#battery-optimisation-kills-the-android-service). Keep **Stay connected** on.

## How do I update?

Both apps show an "Update available" banner. **Download** fetches the file and checks its SHA-256; nothing installs automatically.
On the Linux per-user install, **Install update** (or `geekbridge-ctl update install`) then switches it in and keeps the old version;
package installs (`.deb`, Arch) update through the package manager, Windows and macOS are download only, and on Android you tap
Install in the system installer. Use **Check for updates** to force a check. The Android app only accepts an update signed with
the same key: the GitHub release APK updates to later release APKs, and a debug or development APK cannot update to a release
(uninstall first; that removes the pairing). Debug builds follow the *dev* channel, release builds the *stable* one
(`geekbridge-ctl update channel`). See [updates](updates.md).

## How do I uninstall it?

Phone: uninstall the app. Linux: `sudo apt remove geekbridge` (Arch: `sudo pacman -R geekbridge`), or `./install.sh --uninstall` (this keeps your pairings; add
`--purge-data` to delete them from `~/.local/share/geekbridge`). Windows: Settings > Apps. macOS: delete GeekBridge from
Applications. Also **Unlink** the other device on each side so neither keeps a stale entry.

## Does it work with an iPhone?

No. GeekBridge is for Android phones only. The computer side speaks the KDE Connect protocol, but the phone app is written for
Android and there is no iOS app.

## Does it work on Windows and macOS?

Linux is the main target and the only desktop that has been run on real hardware. Windows and macOS build and pass the automated
tests in CI, but nobody has run them on a real PC or Mac, the binaries would be unsigned and there are no prebuilt downloads yet:
you would build from source. Call audio on macOS is not available. See [platform status](platform-status.md).

## Can I run it next to KDE Connect or GSConnect?

Yes. They use the same ports but do not share identities or pairings, and the phone sees two separate devices. If another program
already owns UDP 1716 GeekBridge says so instead of taking over. See [coexistence](coexistence.md).

## Something else is wrong. What do I send?

On the computer run `geekbridge-ctl report` (a prefilled, redacted bug report; it sends nothing itself) or `geekbridge-ctl diagnose`.
On the phone use **Diagnostics & about > Copy report**. Read it before sharing. Open an issue as described in
[SUPPORT.md](../SUPPORT.md); report security problems privately as in [SECURITY.md](../SECURITY.md).
