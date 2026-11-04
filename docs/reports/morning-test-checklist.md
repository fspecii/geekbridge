# Morning test checklist (2026-11-05)

Build: APK head `bb82cedc4`, versionCode 20207, sha256 `c2ca86b7c640faf24894284ce378fe1bb7fd35b92914738eca05e977a8b9bf07` (installed 10099 or 20199, so it updates in place, pairings kept; same signer as before). PC binaries in `target/release` are from the same head; the running service was not restarted. Needs: the phone, an iPhone (to call the phone), a USB Bluetooth dongle, the PC.

Every command below was dry-run in a sandbox on this PC (see [morning-dry-run.md](morning-dry-run.md)). Two things differ from older notes: the binaries in `target/release` still have two small bugs fixed after `bb82cedc4` (doctor says "FAIL Firewall" although the rule exists; the hotkey does not find a service with a private state dir), so rebuild first (step 2.1); and `adb` is not on this PC's PATH, use `.tools/android-sdk/platform-tools/adb`.

## 1. Phone: install the APK
1. Phone on the home Wi-Fi. Scan `dist/android/apk-qr.png` or open `http://192.168.1.150:8000/geekbridge-debug.apk` (file server, 12 h timer from 2026-11-04 08:33; if it does not answer: `cd dist/android/serve && setsid nohup timeout 43200 python3 -m http.server 8000 --bind 0.0.0.0 &`).
2. Install, choose Update. If Android says "app not installed / conflicts": phone on USB with debugging on, then `scripts/realdevice.sh install --apk dist/android/geekbridge-debug.apk` (checks the signing key first; never uninstall, it deletes pairings).

## 2. PC: new binaries, firewall, hotkey
In a terminal inside Hyprland, in the repo. Do not `source scripts/android-env.sh` here.
1. Rebuild (only if `git log -1` is newer than `bb82cedc4`, or doctor shows the false Firewall FAIL): `cargo build --release --locked -p bridge-apps -p bridge-service`, then `scripts/run-desktop.sh --restart` (pairings kept). Then, in every terminal you use: `eval "$(scripts/run-desktop.sh --env)"` (puts `geekbridge-ctl` on PATH, pointed at this session's service; without it `doctor` says "Service: not answering").
2. Firewall. On this PC ufw already has the rule `1716:1764` tcp and udp from 192.168.1.0/24, so this is a check: `scripts/realdevice.sh firewall` (prints PASS, or the two `sudo ufw allow ...` lines). Only if it says FAIL: `scripts/realdevice.sh firewall --apply` from a terminal (it needs one for the sudo password and refuses without it). `realdevice.sh check` needs the phone on USB, do not use it for this.
3. Hotkey: `geekbridge-ctl hotkey install` (dry run, read the diff), then `geekbridge-ctl hotkey install --apply`, `geekbridge-ctl hotkey status`. The default key is Super+Shift+V (Ctrl+Shift+V is terminal paste); another key: `--key "Ctrl+Alt+P"`. The window rules (float, centre, pin, card top right) come from this step, so do it before section 4.
4. `geekbridge-ctl doctor`: read it top to bottom; every red line is worth sending back. Expected on this PC with no dongle and no phone paired yet: no red line; yellow lines for "No phone is paired" and "Hear calls here" (no Bluetooth).

## 3. Dongle and call audio
1. Plug the dongle in. `geekbridge-ctl call-audio` should list an adapter; "No Bluetooth on this computer" means it is not seen (`bluetoothctl list`, `rfkill list`, `dmesg | grep -i btusb`).
2. Settings (home window) > Call audio on this computer > **Set up call audio**: adapter, power on, visible for 2 minutes, six-digit code, connecting, pick headphones and microphone, test.
3. On the phone: Bluetooth settings, tap the computer's name, compare the six-digit code, accept on the phone, then press **Yes, same code** on the PC. Allow **Phone calls** for the computer on the phone if asked.
4. **Use these devices**, wait for the tone and mic meter. Expected: "Call audio is ready". Terminal route: `geekbridge-ctl calling`, `geekbridge-ctl call setup` (exit 1 with "no Bluetooth adapter" when no dongle), `geekbridge-ctl call setup --fix-config`.

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
| `doctor`: "FAIL Firewall" but `scripts/realdevice.sh firewall` says PASS | old binary bug (fixed after `bb82cedc4`): ignore, or rebuild (2.1) |
| `realdevice.sh firewall` says FAIL, phone cannot connect | `scripts/realdevice.sh firewall --apply` in a terminal; it prints the `sudo ufw allow ... 1716:1764 ...` lines and the undo lines first |
| `realdevice.sh check`: "adb not found" / "no usable phone" | plug the phone in (USB debugging), and put `.tools/android-sdk/platform-tools` on PATH or set `GEEKBRIDGE_ADB` |
| `run-desktop.sh`: "older than ... binary" | `scripts/run-desktop.sh --restart` |
| `run-desktop.sh`: "port 17381 is taken" | `scripts/run-desktop.sh --stop`, or set `GEEKBRIDGE_CONTROL_PORT` |
| phone does not find the PC | `geekbridge-ctl test-connection`, then `scripts/realdevice.sh firewall`; QR address wrong: `GEEKBRIDGE_QR_ADDRESSES=192.168.1.150` and `--restart` |
| hotkey does nothing | `geekbridge-ctl hotkey status`; installed from a shell without the `eval` of 2.1: run `hotkey install --apply` again from one that has it |
| `call-audio` / `calling`: "No Bluetooth" | dongle not seen: `bluetoothctl list`, `rfkill list`, `dmesg \| grep -i btusb`; replug |
| `call setup`: "no Bluetooth adapter" | same; plug the dongle in first |
| `call hangup` / `call mute` / `call audio`: "not available" | there is no live call; ring the phone first, then `geekbridge-ctl calls` shows what is possible |
| `call-overlay-test`: no card | `geekbridge-ctl doctor` line "Incoming-call overlay"; no WAYLAND_DISPLAY in the shell: run it inside Hyprland |
| `phone-state` / `phone-log`: "not a debug build" | phone runs the release APK: nothing to do, the report says so |
| `consent DEVICE_ID FEATURE on`: "unknown feature" | features: contacts, calendar, notifications, battery, photos, telephony, sms, clipboard, debug (also calendar_edit, calendar_alerts) |
| anything else red | `scripts/realdevice-report.sh --no-adb`, attach the file |
