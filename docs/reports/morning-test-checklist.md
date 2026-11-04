# Morning test checklist (2026-11-05)

Build: head `c08893f6a` (built from a clean worktree; it includes the call audio backend review `0a480bc09`: the phone is found when its Bluetooth name differs from its GeekBridge name, WirePlumber fix validated and rolled back on failure, cancel handling, 8 s silence check). APK versionCode 20215, sha256 `b6ccdff5ddc72c608ac0e17ccb619c79c21412e981ad8afd5d29858e0ce42d8a` (gradle tests and lint passed; updates installed 10099 up to 20210 in place, pairings kept; same signer `4d711ab4...a3c3` as before). PC binaries in `target/release` are from the same head (all nine in `scripts/release-binaries.txt`; sha256 starts: service `e6794486`, ctl `a1b30894`, geekbridge `d01b4ffb`, calls `92bbc373`, messages `347fc119`, clipboard `eccfdb2f`, agenda `5144b535`, contacts `34e82674`, share `ce4c361d`; the full list is `sha256sum target/release/geekbridge*`). A sandbox `geekbridge-ctl doctor` and `calling` show 0 problems (only the no-phone and no-Bluetooth warnings); the running service was not restarted. Commits after `c08893f6a` (About section, security follow-ups) are not in this build. Needs: the phone, an iPhone (to call the phone), a USB Bluetooth dongle, the PC.

Every command below was dry-run in a sandbox on this PC (see [morning-dry-run.md](morning-dry-run.md)). Two things differ from older notes: the binaries in `target/release` now include the doctor firewall and hotkey fixes (built from `c08893f6a`), so a rebuild (step 2.1) is only needed for later commits; and `adb` is not on this PC's PATH, use `.tools/android-sdk/platform-tools/adb`.

## 1. Phone: install the APK
1. Phone on the home Wi-Fi. Scan `dist/android/apk-qr.png` or open `http://192.168.1.150:8000/geekbridge-debug.apk` (file server, 12 h timer from 2026-11-04 08:33; if it does not answer: `cd dist/android/serve && setsid nohup timeout 43200 python3 -m http.server 8000 --bind 0.0.0.0 &`).
2. Install, choose Update. If Android says "app not installed / conflicts": phone on USB with debugging on, then `scripts/realdevice.sh install --apk dist/android/geekbridge-debug.apk` (checks the signing key first; never uninstall, it deletes pairings).

## 2. PC: new binaries, firewall, hotkey
In a terminal inside Hyprland, in the repo. Do not `source scripts/android-env.sh` here.
1. Rebuild (only if you want commits newer than `c08893f6a`): `cargo build --release --locked -p bridge-apps -p bridge-service`, then `scripts/run-desktop.sh --restart` (pairings kept). Then, in every terminal you use: `eval "$(scripts/run-desktop.sh --env)"` (puts `geekbridge-ctl` on PATH, pointed at this session's service; without it `doctor` says "Service: not answering").
2. Firewall. On this PC ufw already has the rule `1716:1764` tcp and udp from 192.168.1.0/24, so this is a check: `scripts/realdevice.sh firewall` (prints PASS, or the two `sudo ufw allow ...` lines). Only if it says FAIL: `scripts/realdevice.sh firewall --apply` from a terminal (it needs one for the sudo password and refuses without it). `realdevice.sh check` needs the phone on USB, do not use it for this.
3. Hotkey: `geekbridge-ctl hotkey install` (dry run, read the diff), then `geekbridge-ctl hotkey install --apply`, `geekbridge-ctl hotkey status`. The default key is Super+Shift+V (Ctrl+Shift+V is terminal paste); another key: `--key "Ctrl+Alt+P"`. The window rules (float, centre, pin, card top right) come from this step, so do it before section 4.
4. `geekbridge-ctl doctor`: read it top to bottom; every red line is worth sending back. Expected on this PC with no dongle and no phone paired yet: no red line; yellow lines for "No phone is paired" and "Hear calls here" (no Bluetooth).

## 3. Dongle and call audio
Hardware tips first (from [call-audio-backend-review.md](call-audio-backend-review.md)):
* Use a **wired or USB headset** for the PC side (the HyperX dongle is USB, fine), not a Bluetooth headset: most dongles carry one voice (SCO) link, the phone's, and a Bluetooth headset would need a second one.
* On the phone, Settings > Connections > Bluetooth > gear next to the computer: the **Calls** toggle must be on. If it is off the connection fails with "connection refused / profile unavailable" and no call audio appears.
* On the phone, **tap the computer by its name** (the setup's first line says which name phones see, for example `phones list this computer as "omarchy"`). The phone's Bluetooth name (`Galaxy Z Fold5`) is not what GeekBridge knows it as (`SM-F946B`), so the setup accepts any phone that pairs from its own side during the setup; an already paired phone under another name is not guessed at.

1. Plug the dongle in. `geekbridge-ctl call-audio` should list an adapter; "No Bluetooth on this computer" means it is not seen (`bluetoothctl list`, `rfkill list`, `dmesg | grep -i btusb`).
2. Settings (home window) > Call audio on this computer > **Set up call audio**: adapter, power on, visible for 2 minutes, six-digit code, connecting, pick headphones and microphone, test.
3. On the phone: Bluetooth settings, tap the computer's name, compare the six-digit code, accept on the phone, then press **Yes, same code** on the PC. Allow **Phone calls** for the computer on the phone if asked.
4. **Use these devices**, wait for the tone and mic meter. Expected: "Call audio is ready". Terminal route: `geekbridge-ctl calling`, `geekbridge-ctl call setup` (exit 1 with "no Bluetooth adapter" when no dongle), `geekbridge-ctl call setup --fix-config`.
5. `call setup --fix-config` writes a WirePlumber drop-in so the PC offers the hands-free role, **restarts WirePlumber** and checks it came back; if the new file does not parse or WirePlumber does not start it is rolled back. The restart drops the phone's call link for about 20 seconds (the setup reconnects it), and PC sound pauses briefly. Do not run it in the middle of a call.
6. If a call connects but is **silent** ("digital silence" in `geekbridge-ctl calls` or the log): (a) set `bluez5.enable-msbc = false` for narrowband CVSD voice (a WirePlumber drop-in under `~/.config/wireplumber/wireplumber.conf.d/`, then `systemctl --user restart wireplumber`; exact file and syntax: [troubleshooting](../call-audio-troubleshooting.md#wideband-off-msbc-for-a-silent-or-choppy-call); the review, open item A, has the background), call again; (b) still silent: try another dongle (cheap CSR clones often have broken voice links; Intel AX200/AX210 and good Realtek RTL8761B ones work); (c) collect everything while reproducing it: `scripts/realdevice-report.sh --bt-trace 60`, then place the call within the 60 seconds.

## 4. Overlay
`geekbridge-ctl call-overlay-test`: a card with "Test call" and "This is a test. Nobody is calling." appears top right for 10 seconds, buttons greyed, no typing focus stolen. On Hyprland it is floating and pinned only after the hotkey rules from 2.3.

## 5. Test calls from the iPhone
On the phone's Home turn **Answer from computer** on and allow Calls for the phone on the PC. Then:
1. Ring the phone and let it ring: the card appears within about 2 s with the caller name. Answer on the PC: voice through the headset, the iPhone caller hears you.
2. Ring again, Decline on the PC: the iPhone hears busy/voicemail, the card disappears.
3. Ring again and answer on the phone: audio stays on the phone, the card goes away.
4. Hang up from the PC mid-call (`geekbridge-ctl call hangup` or the card). Try `geekbridge-ctl call mute on|off` and `call audio phone` / `call audio`.
5. With two SIMs (the Fold5), ring each SIM once.
Known gaps: the Wi-Fi call-audio experiment is off; Bluetooth is the supported voice path. Windows/macOS call guidance is in the build but only matters on those systems.

## 6. Send back
Right after any failure: `scripts/realdevice-report.sh --no-adb` (or `--serial SERIAL` with adb, using the `.tools` adb on PATH or `GEEKBRIDGE_ADB=.tools/android-sdk/platform-tools/adb`). It is read-only and redacted, writes under `~/.cache/geekbridge-reports/`. Attach the printed file and say: which step failed, what the screen said, whether the dongle LED lit, and the time of each test call. On a debug phone build the report includes phone-state and phone-log (needs `geekbridge-ctl consent DEVICE_ID debug on`; DEVICE_ID is in `geekbridge-ctl status`, under `paired`).

## If X then run Y

| If | Run |
| --- | --- |
| `doctor`: "Service: not answering ... no control token" | `eval "$(scripts/run-desktop.sh --env)"`, then `doctor` again; still red: `scripts/run-desktop.sh --restart` |
| `doctor`: "FAIL Firewall" but `scripts/realdevice.sh firewall` says PASS | old binary bug (fixed in `df23d137e`, included in the current build): only an older binary shows it; rebuild (2.1) |
| `realdevice.sh firewall` says FAIL, phone cannot connect | `scripts/realdevice.sh firewall --apply` in a terminal; it prints the `sudo ufw allow ... 1716:1764 ...` lines and the undo lines first |
| `realdevice.sh check`: "adb not found" / "no usable phone" | plug the phone in (USB debugging), and put `.tools/android-sdk/platform-tools` on PATH or set `GEEKBRIDGE_ADB` |
| `run-desktop.sh`: "older than ... binary" | `scripts/run-desktop.sh --restart` |
| `run-desktop.sh`: "port 17381 is taken" | `scripts/run-desktop.sh --stop`, or set `GEEKBRIDGE_CONTROL_PORT` |
| phone does not find the PC | `geekbridge-ctl test-connection`, then `scripts/realdevice.sh firewall`; QR address wrong: `GEEKBRIDGE_QR_ADDRESSES=192.168.1.150` and `--restart` |
| hotkey does nothing | `geekbridge-ctl hotkey status`; installed from a shell without the `eval` of 2.1: run `hotkey install --apply` again from one that has it |
| `call-audio` / `calling`: "No Bluetooth" | dongle not seen: `bluetoothctl list`, `rfkill list`, `dmesg \| grep -i btusb`; replug; firmware, rfkill, service: [troubleshooting](../call-audio-troubleshooting.md#adapter-not-found) |
| `call setup`: "no Bluetooth adapter" | same; plug the dongle in first |
| `call hangup` / `call mute` / `call audio`: "not available" | there is no live call; ring the phone first, then `geekbridge-ctl calls` shows what is possible |
| `call-overlay-test`: no card | `geekbridge-ctl doctor` line "Incoming-call overlay"; no WAYLAND_DISPLAY in the shell: run it inside Hyprland |
| `phone-state` / `phone-log`: "not a debug build" | phone runs the release APK: nothing to do, the report says so |
| `consent DEVICE_ID FEATURE on`: "unknown feature" | features: contacts, calendar, notifications, battery, photos, telephony, sms, clipboard, debug (also calendar_edit, calendar_alerts) |
| call connects but is silent | step 3.6: `bluez5.enable-msbc = false`, another dongle, `scripts/realdevice-report.sh --bt-trace 60` ([silent](../call-audio-troubleshooting.md#connected-but-silent), [choppy](../call-audio-troubleshooting.md#choppy-or-robotic-audio), [dongles](../call-audio-troubleshooting.md#usb-dongles)) |
| phone connected but no calls profile / `audio-gateway` missing | [phone never connects HFP](../call-audio-troubleshooting.md#phone-never-connects-hfp), [profile missing](../call-audio-troubleshooting.md#profile-audio-gateway-missing) |
| anything else red | `scripts/realdevice-report.sh --no-adb`, attach the file |

## Experimental: Wi-Fi call audio (optional, skip it if Bluetooth works)

Off by default, a debug phone build only, and **not proven on any real phone**: whether a Samsung Fold5 lets the `shell` user read the caller's voice is exactly what this section finds out. Bluetooth (section 3) stays the supported way; do this only after Bluetooth works or if you have no dongle. Design: [call-audio-over-wifi](../design/call-audio-over-wifi.md); status: [integration](call-audio-wifi-integration.md). It needs `pw-cat` (PipeWire's tools) on the PC and uses the same headphones you picked for Bluetooth calls (`geekbridge-ctl call audio-devices`).

1. **Phone side first** (needs the build from the phone agent's change): phone Settings > Experimental > "Call audio over Wi-Fi", read the Wireless debugging warning, then start the helper from the PC with `scripts/callaudio-enable.sh` (`--status` shows whether it runs, `--stop` stops it). In the phone's self-test, make or take a short call while the test runs: the result must say **works**. `silent` (Samsung blocks it), `refused` or `unsure` mean this phone cannot be heard here; stop, note the verdict, use Bluetooth.
2. **PC consent**: `geekbridge-ctl consent DEVICE_ID call_audio_wifi on`, then `geekbridge-ctl callwifi`. Expected: your phone listed, "ready to listen" (and "and to speak" only if the phone found an uplink route), or the reason it is not. `geekbridge-ctl doctor` has a "Wi-Fi call audio (experimental)" line per phone; it prints nothing at all when no phone offers the feature.
3. **Listen**: ring the phone, answer it, then press **Listen here (Wi-Fi, experimental)** on the live-call card in the Calls window (or `geekbridge-ctl callwifi listen`). Expected within a second or two: a red "Listening to this call on this computer over Wi-Fi (experimental)" box with a **Stop listening** button, the caller's voice on the headphones from step 3.4, and a notification on the phone. Check: does the caller sound clear, how late is it (clap or speak on the phone and compare), does it stutter when you walk away from the router (`geekbridge-ctl callwifi` shows frames lost and the buffer in ms)?
4. **Stop paths**: each must end the audio within about 2 seconds: the Stop button, `geekbridge-ctl callwifi stop`, hanging up the call, `geekbridge-ctl consent DEVICE_ID call_audio_wifi off`, switching Wi-Fi off on the phone, and Stop on the phone's notification. After each, `geekbridge-ctl callwifi` shows no session.
5. **Speak (experimental)** only appears when the phone said it has a way to speak into the call. Press it, say something, ask the caller whether they heard you; the box then also says "Your microphone is on: the caller can hear you". Expect that on most phones the caller will NOT hear you (design, section 2: a carrier call has no ordinary uplink); that is a valid result, write it down. If the caller hears an echo, use headphones, not speakers.
6. **Afterwards**: `scripts/callaudio-enable.sh --stop` and switch Wireless debugging off on the phone (it opens adbd on the LAN while it is on).

| If | Run |
| --- | --- |
| `callwifi`: "no connected phone offers it" | the phone app has no Wi-Fi call audio (release build, or the feature is off in its Settings > Experimental); nothing to do on the PC |
| `callwifi listen`: "Wi-Fi call audio is off for this phone" | `geekbridge-ctl consent DEVICE_ID call_audio_wifi on` |
| `callwifi listen`: "has not said its helper is ready" | `scripts/callaudio-enable.sh --status`, then start it; the phone then sends its state |
| `callwifi listen`: "no live call on this phone" | the call must be answered (not ringing) and the phone's call state shared (Setup > Answer calls) |
| `callwifi`: "not available on this computer: pw-cat ..." | install PipeWire's tools (Arch: `pipewire`); Wi-Fi call audio plays through `pw-cat` only |
| listening starts but nothing is heard | `geekbridge-ctl call audio-devices` (the output it names is the one used); `callwifi` frames should rise by 50 per second; if they do not, the phone sends silence or nothing: record the self-test verdict and `scripts/realdevice-report.sh --no-adb` |
| stops after about 5 seconds | no frames reached the PC (idle timeout); weak Wi-Fi or the helper died; start the helper again |
