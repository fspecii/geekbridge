# Real-device test plan

Everything GeekBridge claims so far was verified on an emulator, a mock D-Bus and simulated call signalling. This
plan is the checklist for the first run on **real hardware**. Run it before each public release; record results in
the table at the end and attach the diagnose output (section 0).

Legend: **Do** = what to do, **Expect** = pass condition, **Check** = command that proves it. Any step that fails
gets a bug report with `geekbridge-ctl diagnose` and the Android **Copy report** attached (both contain no names,
addresses, keys or content).

## 0. Setup and conventions

**Test rig**

- 1 real Android phone (Android 10+, ideally 13+ and 14+/15) with USB debugging for `adb`.
- 1 PC per OS under test: Linux, Windows 10/11, macOS (run the whole plan on each; mark N/A explicitly).
- A Wi-Fi router with a 2.4/5 GHz pair of SSIDs (or two APs) for the Wi-Fi switch test, client isolation **off**.
- A second phone for calls (to ring the test phone) and a Bluetooth-capable PC.
- Windows/macOS binaries are unsigned: expect SmartScreen / Gatekeeper prompts; note them, do not count as failures.

**Environment variables used below**

```bash
export PKG=dev.geekbridge.android
export STATE=${GEEKBRIDGE_STATE_DIR:-}          # empty = default; pass --state-dir "$STATE" to ctl if set
alias gbctl='geekbridge-ctl'                    # Windows: geekbridge-ctl.exe
```

**Log and diagnostic commands (use throughout)**

| Where | Command |
| --- | --- |
| PC status | `geekbridge-ctl status` (pairs, links, `inbound_listener`) |
| PC support bundle | `geekbridge-ctl diagnose > diagnose-$(date +%s).txt` |
| PC events | `geekbridge-ctl events 0` |
| Linux service log | `journalctl --user -u geekbridge-service -b --no-pager` (or the terminal if started by hand) |
| Linux service restart | `systemctl --user restart geekbridge-service` |
| macOS service log | `log show --last 10m --predicate 'process == "geekbridge-service"'`; restart: `launchctl kickstart -k gui/$(id -u)/dev.geekbridge.service` |
| Windows service | Task Manager > Details > `geekbridge-service.exe`; Event Viewer > Application |
| Phone log | `adb logcat -v time --pid=$(adb shell pidof $PKG)` (clear first: `adb logcat -c`) |
| Phone report | app > Diagnostics > **Copy report**, paste into a file |
| Ports | Linux `ss -tulpn \| grep -E ':17(1[6-9]\|[2-5][0-9]\|6[0-4])'`; Windows `netstat -ano \| findstr :1716`; macOS `lsof -iTCP:1716 -iUDP:1716` |

**Step 0.1 Baseline.** Install the release candidate APK and the PC build from the release artefacts (check
`SHA256SUMS`). **Expect:** app version equals the tag (`adb shell dumpsys package $PKG | grep versionName`), and
`geekbridge-ctl diagnose` shows the same `service:` version. Save both outputs as `baseline-<os>.txt`.

**Step 0.2 Firewall.** Follow *First run: allow GeekBridge through the firewall* in
[user-guide.md](user-guide.md). **Expect:** `geekbridge-ctl status` shows an `inbound_listener`; from another LAN
machine `nc -vz PC_IP 1716` connects.

## 1. Pairing

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 1.1 | Phone and PC on the same Wi-Fi. Start service on PC. Phone: open app, setup wizard > **Look for computers**. | PC appears by name within 15 s. | `adb logcat` shows discovery; PC `geekbridge-ctl status` lists no pairing yet. |
| 1.2 | Tap the PC. | Both screens show the verification code; codes are identical. | `geekbridge-ctl status` shows a pending challenge. |
| 1.3 | Confirm on the phone, type/confirm on the PC. | Both say paired; link state connected. | `geekbridge-ctl status`: device under `paired`, link phase `connected`. |
| 1.4 | Enter a wrong code on the PC (new attempt). | Pairing fails with a clear message; no pairing stored. | `geekbridge-ctl events 0` shows `pair_failed`; `status` has no new paired device. |
| 1.5 | `geekbridge-ctl ping DEVICE_ID`. | `ok`; phone is unaffected. | `status`: `pings_received` increases. |
| 1.6 | PC: `geekbridge-ctl unpair DEVICE_ID`; then pair again. | Phone drops the PC; re-pair works with a new code. | Phone Home shows no computer between steps. |
| 1.7 | Unpaired network: guest Wi-Fi with client isolation (or Linux `ufw` closed). | Phone says it cannot find / reach the PC; no crash. | Section 0.2 firewall fix then makes it work. |

## 2. Reconnect behaviour

After each action, **Expect** the link to return to `connected` without user input within the stated time and no
re-pair prompt. **Check** with `geekbridge-ctl status` and `geekbridge-ctl events 0` (`link_lost` then `connected`).

| # | Do | Expect (time limit) |
| --- | --- | --- |
| 2.1 | Phone screen off 10 min (not charging), then wake. | Still connected or back in <= 30 s. |
| 2.2 | Phone screen off 60 min overnight, Doze active (`adb shell dumpsys deviceidle get deep` shows IDLE), then wake. | Back in <= 60 s; note if the foreground notification vanished. |
| 2.3 | Phone reboot. | After unlock, link service restarts (boot receiver) and reconnects in <= 60 s. `adb shell dumpsys activity services $PKG` lists `LinkService`. |
| 2.4 | PC suspend (lid close) 5 min, resume. | Reconnect in <= 30 s after Wi-Fi is back. |
| 2.5 | PC reboot. | Service autostarts at login; reconnect in <= 30 s. |
| 2.6 | Restart only the PC service (command in section 0). | Reconnect in <= 15 s. |
| 2.7 | Wi-Fi switch: phone moves from 2.4 GHz SSID to 5 GHz SSID of the same LAN. | Reconnect in <= 20 s. |
| 2.8 | Phone Wi-Fi off 1 min, then on; also test Wi-Fi off + mobile data on (PC unreachable), then back. | No crash; reconnect in <= 30 s after Wi-Fi returns. |
| 2.9 | DHCP change: renew/forget PC IP (`sudo dhclient -r && sudo dhclient`, or reboot router). | Reconnect via discovery in <= 60 s; no stale IP prompt. |

## 3. Clipboard smart rules

Precondition: Clipboard enabled for this phone on both sides (`geekbridge-ctl consent DEVICE_ID clipboard on`; phone
toggle on). Window default 30 s: `geekbridge-ctl clipboard window 30`; `geekbridge-ctl clipboard status` to read.
Phone screen **unlocked** unless stated. "Paste" = press the hotkey (default Ctrl+Shift+V) or run
`geekbridge-ctl clipboard paste-from-phone --notify`.

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 3.1 | Phone: copy text "alpha". Within 30 s press the PC hotkey in a text editor. | "alpha" appears. | `clipboard status`: `last pull` delivered (text); device `received` +1. |
| 3.2 | Copy on phone, wait 45 s, paste. | Nothing pasted; message says copy again. | `last pull` reason `expired`. |
| 3.3 | Copy on phone, lock the phone, paste on PC. | Nothing pasted; message says unlock. | reason `locked`. |
| 3.4 | Copy a password from a password manager (marked sensitive). | Not sent. | reason `sensitive`; device `skipped` +1. |
| 3.5 | Copy a URL, then HTML (long-press > Copy from Chrome), paste. | Link / formatted text arrives intact. | `last pull` kind `url` / `html`. |
| 3.6 | Phone clipboard empty, paste. | "The phone's clipboard is empty." | reason `empty`. |
| 3.7 | Disable clipboard in the phone app, paste. | Message says sync is off. | reason `disabled`. |
| 3.8 | Change the window (`clipboard window 60`), repeat 3.2 at 45 s. | Now pastes. Restore 30. | `clipboard status` window 60 s. |
| 3.9 | Hotkey: PC without global-grab support (GNOME/KDE Wayland). | `hotkey_binding` shows `unsupported`; the shortcut you bind to `geekbridge-ctl clipboard paste-from-phone --notify` works. | `clipboard status`. |
| 3.10 | Hotkey change: `clipboard hotkey Ctrl+Alt+V`. | New combo works, old one does not. | `clipboard status` hotkey. |
| 3.11 | PC > phone: Clipboard app **Send to phone** (text, then URL). | Phone clipboard / notification receives it; no auto-write in the background. | Phone: paste into any field. |
| 3.12 | No history: reboot both ends. | No clipboard content persisted or shown after restart. | `diagnose` contains no clipboard text. |

**Images**

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 3.13 | Phone: copy an image (Gallery > Copy / screenshot > Copy), paste in an image editor on PC. | Image arrives, correct size and colours. | `last pull` kind `image`. |
| 3.14 | PC: copy an image, **Send to phone**, paste into Messages/Keep on the phone. | Image arrives. | Phone Diagnostics. |
| 3.15 | Image over the limit (`clipboard image-limit 1`, copy a 4 MB photo). | Refused with a "too large" message; restore the limit (default shown by `clipboard status`). | reason `too_large`. |

## 4. File and link sharing

Precondition: `geekbridge-ctl share allow DEVICE_ID on` (receive from the phone). Save folder is in
`geekbridge-ctl share status`.

**PC to phone**

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 4.1 | `geekbridge-ctl send-file DEVICE_ID small.txt` | Phone shows an offer; accept; file lands in the phone's save location with identical content. | `sha256sum` on both sides equal. |
| 4.2 | Send a 500 MB file; during transfer turn the phone screen off. | Completes or fails with a clear message; no crash; progress visible. | `share status`; battery drain in section 9. |
| 4.3 | Send 3 files at once, then decline one. | Declined file reported as declined, others arrive. | `share status`. |
| 4.4 | `send-text DEVICE_ID "hello"` and `send-url DEVICE_ID https://example.org` | Phone shows text/link; link opens only on tap. | |
| 4.5 | Cancel mid-transfer: `share cancel TRANSFER_ID`. | Both sides stop; no partial file left visible. | `adb shell ls` the save dir. |

**Phone to PC (including the Share sheet)**

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 4.6 | **Gallery:** open a photo > Share > **Send to computer**. Repeat with 3 photos and a video. | PC shows an offer (or auto-accepts if enabled); files arrive in the save folder, correct size. | `share status`; checksum vs. source (`adb pull` the original). |
| 4.7 | **Chrome:** Share a page > **Send to computer**. | PC receives the link (`link: ...`). | `share status`. |
| 4.8 | **Chrome:** long-press an image > Share > Send to computer. | Image file arrives. | |
| 4.9 | **Files app:** select a PDF and a ZIP > Share > Send to computer. | Both arrive; names preserved; no path traversal characters in names. | |
| 4.10 | Share text from Notes. | Text arrives (<= 8 KiB; larger is refused with a message). | |
| 4.11 | `share allow DEVICE_ID off`, share again from the phone. | PC refuses; phone shows "not allowed" without hanging. | |
| 4.12 | `share auto-accept DEVICE_ID on`, send. Then off. | Auto-accept only while on. | |
| 4.13 | PC offline or asleep while sharing. | Phone says the computer is unreachable and offers retry; nothing queued silently. | |

## 5. Incoming calls (answer / decline, Bluetooth HFP)

This is the highest-risk area: only simulated signalling has been tested. **Precondition:** phone and PC paired over
Bluetooth (PC Bluetooth settings), phone connected as hands-free device (HFP); Calls app on the PC; Android
permissions *phone state* and *answer calls* granted; call control enabled in the phone app. `geekbridge-ctl calling`
reports the backend (`verdict`) and a phone count >= 1. Use a second phone to ring the test phone. Never record
real calls without consent.

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 5.1 | Ring the test phone. | PC popup within 2 s shows caller (masked in logs) with Answer / Decline. | `geekbridge-ctl calls` shows the ringing call and offered controls. |
| 5.2 | Click **Answer** on the PC. | Phone answers; call state becomes live; popup shows Hang up / mute. | `geekbridge-ctl calls`. |
| 5.3 | Speak both ways with the audio on the PC headset/mic. | Caller hears you; you hear caller; no echo; latency acceptable. If audio stays on the phone, use `geekbridge-ctl call audio`. | Subjective; note codec (CVSD/mSBC) if shown by the OS. |
| 5.4 | `call mute on` / `call mute off` during the call. | Caller stops / resumes hearing the PC mic. | |
| 5.5 | **Hang up** from the PC. | Call ends on both phones; popup closes. | `calls` is empty. |
| 5.6 | Ring again, click **Decline** on the PC. | Phone stops ringing, caller gets busy/voicemail; no missed-call glitch on the PC. | |
| 5.7 | Ring, answer on the phone itself. | PC popup disappears; controls never claim a call is live on the PC. | |
| 5.8 | Ring, ignore until it stops. | Popup closes by itself. | |
| 5.9 | Call while phone screen is locked and while another app is foreground. | Same result as 5.1-5.2. | |
| 5.10 | Disable Bluetooth on the PC, ring. | Popup offers only controls that exist (or none), with a reason; no false Answer button. | `calling` verdict. |
| 5.11 | Revoke *answer calls* permission, ring. | Answer hidden / reason shown; Decline policy matches what the app states. | |
| 5.12 | Second call waiting during a live call. | Documented behaviour only; no crash. Note the outcome. | |
| 5.13 | Privacy: grep the PC log for the caller's number after the calls. | Number does not appear in `journalctl` / `diagnose` / `logcat`. | `journalctl --user -u geekbridge-service \| grep -c "<digits>"` is 0. |

Windows: the Calls backend is separate (`geekbridge-calls-windows`); macOS has no real backend yet; mark the rows
N/A where the verdict says unavailable, and record the verdict text.

## 6. Battery-optimisation exemption per vendor

For each phone available: complete the setup wizard's **battery** step, then lock the phone for 30 min and confirm
the link survives. Evidence: `adb shell dumpsys deviceidle whitelist | grep $PKG` and
`adb shell dumpsys package $PKG | grep -i ignoring` (exemption), and the Android **Copy report** line `battery:`
should read `Unrestricted`.

| Vendor | Do | Expect |
| --- | --- | --- |
| **Pixel / stock Android** | Wizard > battery > allow in the system dialog. | `battery: Unrestricted`; Home shows no warning; link up after 30 min locked. |
| **Samsung (One UI)** | Wizard grants exemption; then Settings > Battery > Background usage limits: make sure the app is not in *Sleeping apps / Deep sleeping apps*; add to *Never sleeping apps*. | Same as above; the wizard lists the vendor step. |
| **Xiaomi / Redmi / POCO (HyperOS/MIUI)** | Wizard exemption; then Security > Permissions > Autostart: enable; app info > Battery saver: **No restrictions**; lock the app in Recents. | Link survives 30 min locked and after clearing Recents (without swiping the app away). |
| **OnePlus (OxygenOS)** | Wizard exemption; Settings > Battery > Battery optimisation > **Don't optimise**; app info > Allow background activity / Allow auto-launch. | Same. |

For every vendor also record: did the wizard open the correct vendor screen (the right activity, or only the generic
fallback)? Revoke the exemption afterward; **Expect** a warning on Home after resume (`battery: Optimised`).

## 7. Update check and install

| # | Do | Expect | Check |
| --- | --- | --- | --- |
| 7.1 | With the *previous* release installed on PC and phone and a newer release published: `geekbridge-ctl update check`. | Reports the newer version, asset name, "SHA-256 published". | `geekbridge-ctl update` (status). |
| 7.2 | `geekbridge-ctl update download`. | Download completes; "verified download: PATH"; nothing installs itself. | `sha256sum PATH` equals the published value in `SHA256SUMS`. |
| 7.3 | Tamper: flip a byte in a copy of the asset on a local mock server, or test with a release lacking a checksum. | Refused / "no published SHA-256, will not be downloaded"; file deleted. | `update` status `problem:` line. |
| 7.4 | Install the downloaded PC installer by hand (Linux `./install.sh` / `.deb`, Windows installer, macOS dmg). | Starts, keeps pairings, `diagnose` shows the new version. | `geekbridge-ctl status` still lists the paired phone, no re-pair. |
| 7.5 | Phone: Home > Update available > download > install (allow "install unknown apps" for the app when asked). | APK installs over the old one (same signing key); pairings survive. | `adb shell dumpsys package $PKG \| grep versionName` new; Copy report shows `paired computers` unchanged. |
| 7.6 | `update skip VERSION`, then new check; `update disable`. | Skipped version not offered again; disabled means no automatic check (verify: no GitHub traffic with the PC offline note). | `update` status. |
| 7.7 | Offline: disconnect internet, `update check`. | Clear error, no hang > 60 s, no crash. | |

## 8. Memory measurements and thresholds

Warm up first: connect the phone and let both sides sit 5 minutes. Record device, build and OS in the results.

**PC service (Linux):** `scripts/perf-check.sh --build` (idle, no phone). With a phone paired and idle:

```bash
pid=$(pidof geekbridge-service)
grep -E 'VmRSS|Threads' /proc/$pid/status; ls /proc/$pid/fd | wc -l
```

| Metric | Pass |
| --- | --- |
| Service RSS idle, phone connected | <= 16 MB (idle no phone budget is 10 MB) |
| Service RSS after 1 h with the phone connected | growth <= 3 MB vs. minute 5 |
| Service RSS after a 500 MB transfer is finished | back to within 5 MB of the idle value in 30 s |
| Threads / fds | <= 8 threads, <= 24 fds |
| Windows/macOS | same ceilings measured in Task Manager *Private working set* / Activity Monitor *Real memory*; record the value, no 10 MB claim |

**Android app:**

```bash
adb shell dumpsys meminfo $PKG | sed -n '1,25p'      # TOTAL PSS, Java Heap, Native Heap
adb shell dumpsys meminfo $PKG | grep -E 'TOTAL|Native Heap|Java Heap'
```

| Metric | Pass |
| --- | --- |
| Total PSS, background service only (no activity visible), connected, idle | <= 60 MB (release build) |
| Total PSS with Home open | <= 120 MB |
| PSS growth over 1 h idle connected | <= 8 MB |
| After 20 clipboard pastes and 5 file transfers, then 1 min idle | back within 10 MB of the baseline |
| Process stays alive 24 h with the exemption | yes (`adb shell pidof $PKG` non-empty, `LinkService` listed) |

A growth above the limit is a leak candidate: capture `adb shell dumpsys meminfo -a $PKG` at both times and attach.

## 9. Battery drain measurement (Android)

Use the same phone, 100% to a known level not needed; compare relative drain. Do **not** keep the phone on USB
power (use `adb tcpip 5555` + `adb connect PHONE_IP:5555` and unplug).

```bash
adb shell dumpsys batterystats --enable full-wake-history
adb shell dumpsys batterystats --reset
# ... run the scenario below, screen off ...
adb shell dumpsys batterystats $PKG > batterystats-$PKG.txt
adb shell dumpsys batterystats --charged | grep -A12 "Estimated power use"
adb shell dumpsys batterystats --charged | grep -iE "wake|alarm" | grep -i geekbridge
adb shell dumpsys deviceidle get deep                 # IDLE means Doze reached
adb shell dumpsys battery | grep level
```

Scenario A (idle, connected): phone paired, screen off, Wi-Fi on, PC online, 4 hours (overnight 8 h preferred).
Scenario B (control): GeekBridge force-stopped (`adb shell am force-stop $PKG`), same duration, same phone.
Scenario C (active): 30 min of 5 clipboard pastes + 3 file transfers per 10 min, screen off between actions.

| Metric | Pass |
| --- | --- |
| Drain A minus drain B | <= 1.0 % of the battery per hour |
| App share in `Estimated power use` (Scenario A) | <= 2 % of total, never in the top 3 consumers |
| Partial wake lock time (`batterystats` Wake lock section for `$PKG`) in A | <= 2 min per hour; no wake lock held > 5 min continuously |
| Alarms / wakeups attributed to `$PKG` in A | <= 30 per hour |
| Doze | device reaches `deep` IDLE during A; link recovers within 60 s of the next maintenance window or wake |
| Scenario C, 30 min | <= 3 % battery above the control |

PC battery: on a laptop, compare 1 h idle with and without the service (`upower -i $(upower -e | grep BAT)` /
`powercfg /batteryreport` / `pmset -g batt`); the service should add no measurable draw (< 0.5 %/h).

## 10. Final regression and sign-off

- Re-run section 1.1-1.3 on a fresh install, then unpair everything and confirm `geekbridge-ctl status` and the
  phone show no paired devices.
- Run `geekbridge-ctl diagnose` and the Android **Copy report**; read both fully: no device names, IP addresses,
  certificates, phone numbers or clipboard text. If any appear, that is a release blocker.
- Capture every failure as an issue with the section number, build versions and the two reports.

**Results record** (copy per phone/PC pair)

| Item | Value |
| --- | --- |
| Date, tester | |
| App version / PC version (from `diagnose`) | |
| Phone (model, Android, skin) | |
| PC (OS, version, Bluetooth chip) | |
| Sections passed / failed / N/A | 1: 2: 3: 4: 5: 6: 7: 8: 9: |
| Measured: service RSS, app PSS, drain A-B | |
| Blocking issues | |
