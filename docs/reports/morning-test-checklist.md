# Morning test checklist (2026-11-05)

Build: head `508897ab5`. Phone APK versionCode 20199 (installed 10099, so it updates in place, pairings kept). Needs: the phone, an iPhone (to call the phone), a USB Bluetooth dongle, the PC.

## 1. Phone: install the APK
1. Phone on the home Wi-Fi. Scan `dist/android/apk-qr.png` (or open `http://192.168.1.150:8000/geekbridge-debug.apk`; if it does not answer: `cd dist/android/serve && setsid nohup timeout 43200 python3 -m http.server 8000 --bind 0.0.0.0 &`).
2. Install, choose Update. If Android says "app not installed / conflicts": `adb install -r -d dist/android/geekbridge-debug.apk` (signing key is the same as before; do not uninstall, it deletes pairings).

## 2. PC: new binaries, firewall, hotkey
From a Hyprland terminal in the repo:
1. `scripts/run-desktop.sh --restart` (new binaries are already in `target/release`; pairings kept).
2. `scripts/realdevice.sh firewall --apply` (opens the control/transfer ports; run `scripts/realdevice.sh check` first to see the dry run).
3. `geekbridge-ctl hotkey install` (dry run, read the diff), then `geekbridge-ctl hotkey install --apply`. Check with `geekbridge-ctl hotkey status`. Plain Ctrl+Shift+V is terminal paste; pick another key with `--key "Ctrl+Alt+P"` if you want to keep terminal paste.
4. `geekbridge-ctl doctor`: read it top to bottom; every red line is worth sending back.

## 3. Dongle and call audio
1. Plug the USB Bluetooth dongle in. `geekbridge-ctl call-audio` should list an adapter; "No Bluetooth" means the dongle is not seen (try `bluetoothctl list`, `rfkill list`).
2. Settings (home window) > Call audio on this computer > **Set up call audio**. It walks 7 steps: adapter, power on, visible for 2 minutes, six-digit code, connecting, pick headphones and microphone, test.
3. On the phone: Bluetooth settings, tap the computer's name, compare the six-digit code, accept on the phone, then press **Yes, same code** on the PC. On the phone allow **Phone calls** for the computer if asked.
4. Choose headset and microphone, **Use these devices**, wait for the tone and mic meter. Expected: "Call audio is ready". Terminal alternative: `geekbridge-ctl calling`, `geekbridge-ctl call setup`, `geekbridge-ctl call setup --fix-config`.

## 4. Overlay
`geekbridge-ctl call-overlay-test`: a card labelled "Test call" appears top right for 10 seconds and does not steal typing focus. Expected on Hyprland: floating, pinned, no buttons active.

## 5. Test calls from the iPhone
On the phone's Home turn **Answer from computer** on and allow Calls for the phone on the PC. Then:
1. Ring the phone and let it ring: the card should appear on the PC within about 2 s with the caller name (number is not shown if overlay is `name`). Answer on the PC: voice through the headset, microphone works (the iPhone caller hears you).
2. Ring again, Decline on the PC: the iPhone hears busy/voicemail, the card disappears.
3. Ring again and answer on the phone: audio stays on the phone, the card goes away.
4. Hang up from the PC mid-call (`geekbridge-ctl call hangup` or the card). Try `geekbridge-ctl call mute on|off` and `call audio phone` / `call audio`.
5. With two SIMs (the Fold5), ring each SIM once.
Known gaps: the Wi-Fi call-audio experiment is debug/experimental and off; Bluetooth is the supported voice path. Not in this build: commit `1e3b45894` (Windows/macOS call guidance).

## 6. Send back
Right after any failure, while the log is fresh:
`scripts/realdevice-report.sh` (read-only, redacted, writes under `~/.cache/geekbridge-reports/`; add `--serial SERIAL` if adb is attached). Attach the printed file, and say: which step failed, what the screen said, whether the dongle LED lit, and the time of each test call. If the phone is on a debug build, the report includes its phone-state and phone-log.
