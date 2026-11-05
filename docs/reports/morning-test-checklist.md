# Real-device session script (Fold5 SM-F946B, Android 16, this Arch/Omarchy PC)

One ordered script, about 90 minutes, copy-paste from the repository root in a terminal inside Hyprland. Each step has a
**Send back** line; on any failure note the step, run the shown capture command, and move on (step 7 collects
everything). Needs: the Fold5 on the home Wi-Fi, an iPhone (or any second phone) to call it, the HyperX USB headset on the
PC. Older rounds' build notes are in git history of this file; the long Wi-Fi call-audio walk is
[call-audio-wifi-realphone.md](call-audio-wifi-realphone.md); the bring-up and troubleshooting tables are
[../real-device-quickstart.md](../real-device-quickstart.md); the generic 12-step plan is
[../real-device-test-plan.md](../real-device-test-plan.md).

What changed since the round 8 APK (20348) and round 9 binaries (`37af23dba`): self-update
(`geekbridge-ctl update install|rollback|restart|support`, the service restarts only when idle), the Android Home
**Update available** chip, accessibility fixes (call card announcements, TalkBack focus), first-run changes (Answer calls
step, QR-only step 1), Messages matching and memory-cap fixes, emulator-script hardening. The chip and the update commands
exist only in a build of current `main`, so **rebuild both sides first** (step 0).

## 0. Install the APK and restart the service

```
git pull --ff-only && git rev-parse --short HEAD                     # write the head down
cargo build --release --locked -p bridge-apps -p bridge-service      # PC binaries (all nine: scripts/release-binaries.txt)
scripts/build-android.sh --skip-host-tests                           # dist/android/geekbridge-debug.apk
sha256sum dist/android/geekbridge-debug.apk target/release/geekbridge-ctl
```

Phone, either way (same signer, so it updates in place and pairings stay; never uninstall, that deletes them):

* Cable: `scripts/realdevice.sh install --apk dist/android/geekbridge-debug.apk` (checks the signing key first).
* Wi-Fi: `(cd dist/android/serve && setsid nohup timeout 43200 python3 -m http.server 8000 --bind 0.0.0.0 &)` after
  copying the APK there, then open `http://<PC-IP>:8000/geekbridge-debug.apk` on the phone and choose Update.

Then the service (pairings kept) and the environment, in every terminal you use:

```
scripts/run-desktop.sh --restart
eval "$(scripts/run-desktop.sh --env)"
geekbridge-ctl --version && geekbridge-ctl doctor
```

Expect: the head's version line, `doctor` without red lines (yellow for call audio not set up yet is fine). If it says
"Service: not answering" you skipped the `eval`. **Send back:** head hash, both sha256 lines, red lines of `doctor`.

## 1. Pairing sanity

```
geekbridge-ctl status | jq '.paired, .links'
DEV=$(geekbridge-ctl status | jq -r '.paired[0].device_id'); echo $DEV
geekbridge-ctl ping $DEV
geekbridge-ctl consent $DEV telephony on; geekbridge-ctl consent $DEV sms on; geekbridge-ctl consent $DEV contacts on
geekbridge-ctl consent $DEV clipboard on; geekbridge-ctl consent $DEV photos on
```

Expect: the Fold5 under `paired`, link phase `connected`, `ping` answers. Still paired after the APK update (no QR
needed); if not, phone wizard > Pair with your computer > scan the QR from the PC window. Phone Home: **Answer calls from
my computer** on (the permission and switch are on the first screen; the rest is behind **More call options**).
**Send back:** `geekbridge-ctl doctor` if the link is not `connected`, and `geekbridge-ctl test-connection`.

## 2. Calls over the link (the iPhone rings the Fold5)

Hotkey and window rules once: `geekbridge-ctl hotkey install` (dry run), then `geekbridge-ctl hotkey install --apply`.
Start a clean trace: `geekbridge-ctl calls --trace --clear`.

| # | Do | Expect |
| --- | --- | --- |
| a | Ring, do not answer yet | Ring card top right within about 2 s, caller name/number, Answer / Decline, **Decline with message** |
| b | Answer on the PC | Becomes the in-call card: caller, running timer, Hang up, an audio line saying where the voice goes. Without Bluetooth call audio the voice stays on the phone (the card says so) |
| c | Hang up on the PC (card or `geekbridge-ctl call hangup`) | "Call ended mm:ss" for a few seconds, then the card goes; the iPhone side ends |
| d | Open the Calls window, **Recents** tab | The calls above are listed (in memory since the service started) |
| e | Ring and let it ring out | Banner "Missed call" with **Call back** and **Message**; Recents shows it as Missed |
| f | Ring, **Decline with message**, pick a quick reply | Card closes; the iPhone receives the SMS (note the delay). Needs the sms and telephony consents from step 1 |
| g | Answer a call on the PC, look for a keypad on the in-call card | **Hidden** on the link path (no Bluetooth call audio). Visible and greyed, or visible at all here, is a bug. It appears only after step 3 works |
| h | Do Not Disturb: `geekbridge-ctl privacy set respect_dnd on`, `makoctl mode -a do-not-disturb` (dunst: `dunstctl set-paused true`; swaync: `swaync-client -dn`), ring | **No card and no ringing banner**; the phone still rings; `geekbridge-ctl calls --trace` has `ring: kept quiet: Do Not Disturb`; answering in the Calls window still works. Undo: `makoctl mode -r do-not-disturb` (dunst `dunstctl set-paused false`; swaync `swaync-client -df`), `geekbridge-ctl privacy set respect_dnd off` |

Also once: `geekbridge-ctl call dial NUMBER` with your second phone (never an emergency number), confirm on the PC, end with
`geekbridge-ctl call hangup`. **Send back:** `geekbridge-ctl calls --trace` (redacted; from the first `ring` to the last
`ended` of the failing call), the time of each test call, a photo of the card if the layout looks wrong, and which row
(a to h) failed.

## 3. Bluetooth call audio (the PC is the hands-free unit)

Decision tree with every failure line: [call-audio-decision.md](call-audio-decision.md) section 1. Use the USB headset,
not a Bluetooth one. Phone: Settings > Connections > Bluetooth open and unlocked.

```
geekbridge-ctl calling                       # B0: steps 1-4 [ok] (controller hci0, not blocked, bluetooth.service, powered)
geekbridge-ctl call setup --fix-config       # B1: "The WirePlumber configuration needs no change." or "Wrote ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf ..."
geekbridge-ctl call setup                    # B2: add --phone "Fold5" if the phone is not found by name
geekbridge-ctl calling                       # B3: "Call audio: ready (phone: ...)."
geekbridge-ctl call-audio test               # two-note tone in the headset, then the microphone meter for 5 s
```

B1 pauses PC sound for a second and drops the phone link for up to 20 s: never in a call. B2 expected, in order:
`Using Bluetooth adapter hci0 ...` > `visible as a Bluetooth device for 120 s` > `Found <phone> ...` > `Does the phone show
NNNNNN? Accept there too. [y/N]` (compare, Pair on the phone, type `y`) > `Paired with ...` > `<phone> is set up for
calls.` If it fails with "did not accept the calls (hands-free) connection": phone Bluetooth > gear next to the PC >
turn **Calls** on, then `geekbridge-ctl calling`. Cancel: `geekbridge-ctl call setup --cancel`.

The real proof (B4): the iPhone rings the Fold5, answer **on the PC**, within 3 s `geekbridge-ctl calls` should show
`active` and `source: Call audio: on this computer (speaker: ...; microphone: ...; codec mSBC, wideband 16 kHz)` (or CVSD
narrowband), `microphone: on`, and both people hear each other. Then `geekbridge-ctl call mute on` / `off`,
`geekbridge-ctl call audio phone` (voice moves to the earpiece in about 2 s), `geekbridge-ctl call audio` (back), and the
keypad now appears: press digits against a phone menu. Silent but "voice link open": add `bluez5.enable-msbc = false`
as in [../call-audio-troubleshooting.md](../call-audio-troubleshooting.md#wideband-off-msbc-for-a-silent-or-choppy-call),
`systemctl --user restart wireplumber`, call again.

**Send back:** each `calling` and `call setup` output, the `calls` line from B4, whether each side heard the other, and
`scripts/realdevice-report.sh --no-adb --bt-trace 60` started just before a failing call (place the call within the 60
seconds).

## 4. Messages

Phone: Share with computer > Messages on and the SMS permission (Android 13+ sideload: Settings > Apps > GeekBridge >
three dots > Allow restricted settings). Then `geekbridge-ctl messages` or open Messages (`geekbridge-messages`).

* **Unread count:** have the iPhone send two texts. The tray/waybar count and the thread's unread badge go up by two. Read
  the thread **on the phone**: the count on the PC comes down after the next refresh (`geekbridge-ctl messages-refresh
  $DEV`). Opening the thread on the PC lowers it on the PC only (`messages-read`; the phone keeps its own state).
* **Group reply blocked:** open a group conversation. Replying is refused with "Group conversations can't be answered from
  the computer." and nothing is sent.
* **Compose by name:** New message, type a contact's first letters in **To**: suggestions with name and number (several
  numbers ask "Which number for NAME?"); same-name contacts stay separate rows. Send one SMS to your second phone only.

**Send back:** the three outcomes (yes/no), the screenshot of any wrong count, `geekbridge-ctl messages` (personal data:
skim it first) or just `geekbridge-ctl doctor` lines for Messages.

## 5. Share, clipboard, photos (about 10 minutes)

```
geekbridge-ctl share allow $DEV on                        # then on the phone: Gallery > a photo > Share > Send to computer
geekbridge-ctl send-file $DEV some-file.txt               # PC to phone; geekbridge-ctl share status shows both
geekbridge-ctl clipboard status                           # copy "alpha" on the phone, unlock, press the hotkey (or: clipboard paste-from-phone --notify) within 30 s
geekbridge-ctl photos refresh $DEV; geekbridge-ctl photos $DEV   # names only; photos save $DEV PHOTO_ID asks for the full file
```

Expect: the file arrives in the save folder both ways, "alpha" pastes, the photos list shows recent names and thumbnails.
**Send back:** `geekbridge-ctl share status`, `geekbridge-ctl clipboard status` (counters only), which one failed.

## 6. Update path (sandbox only)

Phone: Home shows the **Update available** chip only when a newer stable release is published; with none, **no chip is
the correct result** (Diagnostics has the update card with the last check). Do not tap Install on anything.
PC, read-only plus a sandbox prefix; the live install and the running service are never touched:

```
geekbridge-ctl update status                     # up to date / update available / channel; last install outcome
geekbridge-ctl update support                    # first word: user-install, package-managed or download-only (a build-folder run is download-only: expected)
P=$HOME/.cache/gb-update-sandbox; mkdir -p $P/bin $P/lib
geekbridge-ctl update install --dry-run --prefix $P
```

Expect `GeekBridge v0.1.0 is up to date; there is nothing to install.` when nothing newer exists, or, when a newer stable
release has a verified installer, `Dry run: vX passed every check (checksum, archive contents, executables, version).
Nothing was changed.` (it downloads the archive first). On the dev channel it says download only. Do **not** run `update
install` without `--prefix`, `update rollback` or `update restart` unless you decide to update the live install on
purpose. Clean up: `rm -rf $P`. Design and failure table: [../updates.md](../updates.md).
**Send back:** the three outputs and a screenshot of Home (chip or none).

## 7. If something fails: what to capture

```
geekbridge-ctl doctor > ~/gb-doctor.txt 2>&1
geekbridge-ctl calls --trace > ~/gb-calls-trace.txt 2>&1
geekbridge-ctl report --preview                # exactly what a public bug report would carry (redacted); nothing is sent
scripts/realdevice-report.sh --no-adb          # redacted bundle under ~/.cache/geekbridge-reports/; add --serial SERIAL with adb
```

Phone side, adb (`ADB=.tools/android-sdk/platform-tools/adb`; run `$ADB devices` first):

```
ADB=.tools/android-sdk/platform-tools/adb
$ADB logcat -d -v time -t 4000 > ~/gb-logcat.txt                                   # whole recent buffer; right after the failure
$ADB logcat -d -v time --pid=$($ADB shell pidof dev.geekbridge.android) > ~/gb-app.txt
scripts/realdevice.sh logs                                                         # filtered logcat, crash buffer, dumpsys
geekbridge-ctl consent $DEV debug on; geekbridge-ctl phone-state $DEV; geekbridge-ctl phone-log $DEV -n 300   # debug APK only
```

No cable: phone Settings > Developer options > **Wireless debugging** on > **Pair device with pairing code**, then
`$ADB pair HOST:PORT CODE` (the pairing port), then `$ADB connect HOST:PORT` (the port on the Wireless debugging screen;
it differs and changes whenever it is toggled; "connection refused" = stale port). Turn it off when done.
**Send back:** the files above, the step number, the clock time, and what the screen said.

## 8. Wi-Fi call audio probe (experimental, optional, last, only after step 3 is judged)

Debug APK only. Full procedure and verdict table: [call-audio-wifi-realphone.md](call-audio-wifi-realphone.md) (steps C, D
and the cleanup block); why it may be a dead end: [call-audio-decision.md](call-audio-decision.md) section 2.
**Warnings:** the probe with no call flashes the phone's microphone indicator and plays nothing to anyone; run in a call
it plays a **1 kHz tone into the call**, so do not run it in a call. Any beep test is **loud in the other person's ear**:
warn them, phone call volume near 30 %, no headphones on the Fold5, not on speaker, and set the PC output low first
(`wpctl set-volume @DEFAULT_AUDIO_SINK@ 25%`). Wireless debugging opens adbd on the LAN while it is on.

```
export ADB="$PWD/.tools/android-sdk/platform-tools/adb" ANDROID_ADB_SERVER_PORT=5038; $ADB devices      # isolated adb server; pair + connect as in step 7
$ADB shell 'CLASSPATH=$(pm path dev.geekbridge.android | sed s/^package://) app_process /system/bin dev.geekbridge.android.callaudio.CallAudioHelper --probe'   # no call, 8-12 s, let it finish
# read three fields of the JSON line: pstnInterceptable, telephonyTxDevice, uplinkInjectionTrack.outcome   (false + false = no uplink without root: close the Wi-Fi route)
# only if pstnInterceptable is true: continue with steps E to J of the linked procedure, with a person on the other phone who knows what to expect
geekbridge-ctl callwifi stop; geekbridge-ctl callwifi setup --adb "$ADB" --stop; $ADB shell am force-stop dev.geekbridge.android; $ADB shell settings put global adb_wifi_enabled 0; $ADB disconnect; $ADB kill-server   # cleanup, also after any aborted run
```

**Send back:** the whole probe JSON line and, if you went further, what the other person heard (1, 2, 3 beeps or nothing).
