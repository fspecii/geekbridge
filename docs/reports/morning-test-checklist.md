# Morning test checklist (2026-11-05)

Build: head `bb82cedc4` (CI check.yml green; the Performance budget job failed once on idle RSS 8156 kB vs 8000 and passed on rerun, run-to-run noise 7.8 to 8.2 MB). Phone APK versionCode 20207, sha256 `c2ca86b7c640faf24894284ce378fe1bb7fd35b92914738eca05e977a8b9bf07` (installed 10099 or 20199, so it updates in place, pairings kept; same signer `4d711ab4...a3c3` as the previous build). PC binaries in `target/release` are from the same head (service sha256 `cf7ff3d9...2589`, ctl `5a36bff6...8293`); the running service was not restarted. The release APK passed `scripts/check-release-isolation.sh`. Needs: the phone, an iPhone (to call the phone), a USB Bluetooth dongle, the PC.

## 1. Phone: install the APK
1. Phone on the home Wi-Fi. Scan `dist/android/apk-qr.png` (or open `http://192.168.1.150:8000/geekbridge-debug.apk`; the file server was restarted 2026-11-04 08:33 with a 12 h timer; if it does not answer: `cd dist/android/serve && setsid nohup timeout 43200 python3 -m http.server 8000 --bind 0.0.0.0 &`).
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
Known gaps: the Wi-Fi call-audio experiment is debug/experimental and off; Bluetooth is the supported voice path. The Windows/macOS call guidance (`1e3b45894`) is now in this build but only matters on those systems. Visual QA 3 changes to look for: a no-Bluetooth warning above the keys on the call audio page, new call audio headline, Decline and Link wording.

## 6. Send back
Right after any failure, while the log is fresh:
`scripts/realdevice-report.sh` (read-only, redacted, writes under `~/.cache/geekbridge-reports/`; add `--serial SERIAL` if adb is attached). Attach the printed file, and say: which step failed, what the screen said, whether the dongle LED lit, and the time of each test call. If the phone is on a debug build, the report includes its phone-state and phone-log.
