# Real-device test plan

An ordered checklist for one session with **one real phone** (Samsung Galaxy Z Fold5 SM-F946B, One UI, Android 13+,
sideloaded APK) and **one Linux PC without Bluetooth** (Omarchy / Hyprland). About **2 hours**, 30 minutes of which
the phone just sits locked. Fill the Result column as you go. Every failure can be diagnosed afterwards from one file
(step 12), so do not stop to debug: note it, run the "If it fails" command, move on.

What is already proven and what is not: [real-device-quickstart.md](real-device-quickstart.md) (bring-up and
troubleshooting T0-T9) and [CHANGELOG.md](../CHANGELOG.md). Everything below is verified on an emulator and with
automated tests only, unless the quickstart says one real phone has been paired. The kit scripts are tested without a phone
by `scripts/tests/realdevice-kit-test.sh`.

## The kit

```bash
cd path/to/geekbridge
S=scripts/realdevice.sh            # check, firewall, install, desktop, logs, screens, battery-*, link-watch
R=scripts/realdevice-report.sh     # one redacted file with everything needed to diagnose remotely
eval "$(scripts/run-desktop.sh --env)"      # geekbridge-ctl on PATH, pointed at this session's service (after step 1)
DEV=$(geekbridge-ctl status | jq -r '.paired[0].device_id')    # after step 2
```

| Command | What it gives you |
| --- | --- |
| `$R --page URL` | `~/.cache/geekbridge-reports/geekbridge-report-<time>.txt` (+ `.tar.gz`): doctor, status JSON, events, phone-state and phone-log (debug build), firewall, service log tail, binary and APK versions and SHA-256, the APK on the served page compared with the installed one. Secrets, tokens, keys, clipboard text, message and notification content, names and phone numbers are removed. Read it once before attaching it to a public place. Add `--apk FILE` for the APK you sideloaded. |
| `$S logs` | adb side: filtered logcat, crash buffer, dumpsys (battery, deviceidle, meminfo, services). Run right after a failure: the log buffer is small. |
| `$S link-watch --minutes 20` | polls `geekbridge-ctl status` every 5 s, logs `UP`/`DOWN`/`STILL` with timestamps and the why-disconnected code, prints uptime %, outages and the longest outage. Needs no adb, works with the cable out. |
| `$S screens NAME` | PNG of the phone screen into `dist/realdevice/screens/` (screen must be on). |

The debug APK shows `phone-state` and `phone-log` (consent `debug`: `geekbridge-ctl consent $DEV debug on`). A release APK
answers that it is not a debug build; the report says so and carries everything else.

## Checklist

| # | Min | Do | Expect | If it fails run |
| --- | --- | --- | --- | --- |
| 1 | 10 | Build, firewall, install, start: `$S check`, `$S firewall --apply`, `$S install`, `$S desktop --fresh` (quickstart steps 1-7). | `Result: ... 0 fail`; the phone reaches `PC:1716` over TCP; QR shown. | `$S check` names the quickstart section (T0-T3). Then `$R`. |
| 2 | 5 | Pair: phone wizard > Pair with your computer > scan the QR. | Both show paired; `geekbridge-ctl status` has the phone under `paired` and a link in phase `connected`. | `geekbridge-ctl test-connection`, `geekbridge-ctl doctor`, `$S logs`, `$R`. |
| 3 | 3 | **Baseline report** now, while everything is fresh: `$R --apk path/to/app-debug.apk`. | Report written; `HASH` lines for 9 binaries and the installed APK; the installed APK hash equals `--apk`'s. | Read the printed error; `$R --no-adb` skips the phone part. |
| 4 | 8 | **Battery exemption (Samsung).** Wizard > battery > Allow unrestricted battery. Then Settings > Battery > Background usage limits: GeekBridge not in *Sleeping apps* / *Deep sleeping apps*, add to *Never sleeping apps*. Optional: *Share battery level* on the phone, `geekbridge-ctl consent $DEV battery on`. | `$S logs`: `dumpsys-deviceidle.txt` lists the package under the whitelist. With sharing on, `status` shows `links[].battery` with a plausible level and charging flag within about a minute. | `$S logs`, quickstart T8. `$R`. |
| 5 | 10 | **Notification mirroring.** Phone: Settings > *Show phone notifications on the computer*; open Notification access and switch GeekBridge on (if the switch is greyed out: Settings > Apps > GeekBridge > three dots > *Allow restricted settings*); choose one chat app; allow the computer. PC: `geekbridge-ctl consent $DEV notifications on`. Make the phone post a notification from that app (send yourself a message). | A banner on the PC names the app (text only if message content is enabled). `geekbridge-ctl notifications` lists it. *Dismiss* on the banner clears it on the phone. Ongoing and GeekBridge's own notifications never appear. | `geekbridge-ctl doctor`, `geekbridge-ctl notifications --json | jq '.items|length'`, `$R`. Banner missing but listed: the PC notification daemon (`notify-send test`). |
| 6 | 10 | **Messages permissions.** Phone: GeekBridge > Share with computer > Messages on > Allow access; accept the SMS permission (Android 13 sideload: Settings > Apps > GeekBridge > three dots > *Allow restricted settings*, then Permissions > SMS > Allow; reopen the app). PC: `geekbridge-ctl consent $DEV sms on`, open Messages (`geekbridge-messages`) or `geekbridge-ctl messages`. | Conversation list appears; "Messages is off" / "phone does not offer Messages" gone. Do not send a real SMS unless you mean to. | `geekbridge-ctl doctor` (prints the exact line), `$S screens sms`, `$R`. Note in the Result column exactly which dialog One UI showed. |
| 7 | 5 | **Dial from the PC (no Bluetooth).** Phone: Answer calls step > *Allow placing calls from my computer* (CALL_PHONE). PC: `geekbridge-ctl consent $DEV telephony on`, then `geekbridge-ctl call dial NUMBER` with your second phone's number (never an emergency number). Confirm on the PC. | The phone places the call; `geekbridge-ctl calls` shows it; `geekbridge-ctl call hangup` ends it. `doctor` reports the missing Bluetooth controller as information, not an error. With the phone's dial confirmation setting on, the phone asks first. | `geekbridge-ctl calls`, `geekbridge-ctl calling`, `$S logs`, `$R`. |
| 8 | 8 | **Clipboard and its reconnect.** Phone: Clipboard sync on. PC: `geekbridge-ctl consent $DEV clipboard on`. If the link had not declared the clipboard packets it reconnects once by itself (watch `geekbridge-ctl events 0` for `link_lost` then `connected`, no re-pair). Copy "alpha" on the phone, unlock it, press Ctrl+Shift+V (or `geekbridge-ctl clipboard paste-from-phone --notify`) within 30 s. | "alpha" pastes. After 45 s, or phone locked: the message says copy again / unlock. `clipboard status` counters move. | `geekbridge-ctl clipboard status`, `geekbridge-ctl doctor`, `$R` (counters only, never the text). |
| 9 | 6 | **Goodbye.** PC: `geekbridge-ctl disconnect $DEV`. Watch 2 minutes. Then `geekbridge-ctl connect $DEV`. | Link goes down and the phone does **not** dial straight back: reason `paused_by_user` in `status`/`doctor` for the 5 minutes. A hand connect brings it back at once. Then stop the service (`scripts/run-desktop.sh --stop`) and start it again: the phone redials with back-off and returns by itself. | `geekbridge-ctl events 0`, `$S logs` (look at the phone's dial attempts), `$R`. |
| 10 | 30 | **Screen-off heartbeat.** Charger unplugged, Wi-Fi on. Start `$S link-watch --minutes 20`, press the power button, put the phone down, do not touch it. | Uptime >= 99 %, no outage longer than 30 s. The phone pings every 60 s with the screen off (the PC judges silence at 70 s). Repeat once with 60 min or overnight (`--minutes 480`) if you can. | `$S link-watch` prints the next command per reason: `link_timed_out` = the link stopped answering, `phone_silent` = the phone stopped announcing. Then `$S logs` (`am_proc_died`, `Killing`, deviceidle IDLE) and `$R`. Compare with `battery-baseline`/`battery-report` for drain. |
| 11 | 10 | **Why-disconnected.** With the link up, provoke each cause once and read `geekbridge-ctl status` (stderr), Home and the tray: (a) phone Wi-Fi off for 1 minute; (b) `geekbridge-ctl disconnect $DEV` (step 9 already covered it); (c) unpair on the phone only. | (a) `phone_silent` or `link_timed_out`, then `reconnecting`/`gave_up` as tries grow, back within 30 s of Wi-Fi returning. (b) `paused_by_user`. (c) `pairing_lost`. Each shows one plain sentence and a fix. No code is `unknown`. | `geekbridge-ctl doctor`, `$S link-watch --minutes 3` while provoking, `$R`. |
| 12 | 5 | **Final report.** `$R --page URL --apk path/to/app-debug.apk` (URL = the page the phone downloads the APK from, if you use one). Fill the results table below. | One file; the "served APK" line says same as installed; nothing personal in it (skim it). | Send `~/.cache/geekbridge-reports/geekbridge-report-*.txt` and the Result table. |

Optional if time remains (each 5 min): file send both ways (`geekbridge-ctl send-file $DEV FILE`, Share sheet on the
phone; save folder in `geekbridge-ctl share status`); update check (`geekbridge-ctl update check`); unpair and re-pair
with a new QR (`geekbridge-ctl unpair $DEV`).

## Not in this session (needs other hardware)

Bluetooth call audio and HFP answer (no PC Bluetooth: control over the link is step 7), Windows and macOS PCs, a second
Android vendor (battery vendor screens other than One UI), the memory and battery-drain measurements (use
`scripts/perf-check.sh --build`, `adb shell dumpsys meminfo dev.geekbridge.android` and `$S battery-baseline` /
`battery-report` over 4-8 h for the drain figures: drain with the app minus control run <= 1 %/h, wake locks none).
Targets: service RSS idle with a phone <= 16 MB, app total PSS in the background <= 60 MB.

## Reading a failure remotely

1. `geekbridge-ctl doctor` section in the report: firewall, listener, session, paired phones, clipboard, calls, Messages.
2. `status (JSON)`: `links[].phase`, `disconnected[].code` (`computer_offline`, `not_listening`, `paused_by_user`,
   `pairing_lost`, `firewall_blocking`, `reconnecting`, `gave_up`, `link_timed_out`, `different_network`,
   `phone_silent`, `not_seen_yet`), `tries`, `last_seen_ago_s`.
3. `events`: the order of `connecting`, `connected`, `link_lost` with sequence numbers.
4. `phone-state`/`phone-log` (debug APK) for the phone's view; `service log` for the PC's.
5. `HASH` lines: is the binary or APK on the machine the one that was built?

## Results record (copy per session)

| Item | Value |
| --- | --- |
| Date, tester | |
| App version, PC version (`doctor` first lines) | |
| Phone, Android, One UI version | |
| Steps passed / failed / skipped | 1: 2: 3: 4: 5: 6: 7: 8: 9: 10: 11: 12: |
| link-watch uptime % and longest outage (step 10) | |
| One UI dialogs seen for notification access and SMS (steps 5-6) | |
| Report file names attached | |
