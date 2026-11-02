# Real-device quickstart (10-15 minutes)

First hour with a real phone, highest value first. The full matrix is [real-device-test-plan.md](real-device-test-plan.md).
Fill the **Result** column as you go (PASS / FAIL / N/A + a note); on any FAIL run `logs` (step 0) right away,
Android's log buffer is small.

```bash
cd path/to/geekbridge
S=scripts/realdevice.sh
eval "$(scripts/run-desktop.sh --env)"      # puts geekbridge-ctl on PATH, pointed at this session's service
```

**Evidence note.** The Android app writes almost nothing to logcat on the happy path, so "proof" is the PC side
(`geekbridge-ctl ...`), the phone screen (`$S screens NAME` saves a PNG to `dist/realdevice/screens/`) and, for
crashes or OEM kills only, `$S logs` (look for `FATAL EXCEPTION`, `am_proc_died`, `Killing`).

## 0. Prepare (2 min, once)

| # | Do | Expect | Result |
| --- | --- | --- | --- |
| 0.1 | Phone: USB debugging on, plug in, accept the RSA prompt. `$S check` | phone listed as `device`, same-subnet PASS, no FAIL. Note the printed *battery wizard to expect* line. | |
| 0.2 | **Firewall.** `ufw` here is active with default DROP and no rule for 1716-1764, so the phone cannot connect until: `$S firewall --apply` (runs the two printed `sudo ufw allow from <LAN> ... 1716:1764 tcp/udp` lines; undo lines are printed too) | `check` shows firewall PASS | |
| 0.3 | `$S install` (add `--build` if `check` says sources are newer; `--no-grant` to see the first-run permission prompts) | `installed`, app opens | |
| 0.4 | `$S desktop` (service + home window + terminal QR; `--fresh` forgets old pairings). The very first start creates the identity key and can take 10+ s; wait for `service up`. | `geekbridge-ctl status` answers; the GeekBridge window opens on **Home** (no QR there, see step 1) and the terminal shows a QR | |

## 1. The checklist

| # | Do (exact action) | Expect | Proof | Result |
| --- | --- | --- | --- | --- |
| 1 | **Pair via QR.** Phone on the same Wi-Fi. PC: use the terminal QR from step 0.4, or in the GeekBridge window click **Pair a phone** (Ctrl+2) > **Pair with QR code** (the QR is not on Home; Home's first tile says "Click to pair a phone"). Phone, first launch: the setup wizard opens on step 1 "Pair with your computer" > **Scan QR code** > **Open the camera app** (or **Scan with a scanner app**), point it at the QR and tap the GeekBridge link. If your camera only shows the link as text, copy it, come back to this screen, tap **Paste from clipboard** > **Continue**. Then "Connect to a computer?" (only when the link came from another app: **Continue**), then "Pair with ...?" with **Pair** / **Decline**. QR lives 2 min (the window shows the countdown and renews it): reprint with `$S desktop`. You can **Skip** every wizard step and still pair: Home then shows **Pair a computer**. | Phone says "Paired with ..." and a "What to do next" card; PC window shows "Paired with ..." and its own "What to do next" card. No code to compare. | `geekbridge-ctl status`: phone under `paired`, link `connected`. Fail: tap **Test connection** on the phone's error screen, or `geekbridge-ctl test-connection`. Screenshot `$S screens paired`. | |
| 2 | **Clipboard phone to PC.** Enable Clipboard on both sides: phone Home > scroll to Settings > **Clipboard sync** (off by default); PC: tick the phone in the Clipboard window (Ctrl+8, scroll down) or `geekbridge-ctl consent DEVICE_ID clipboard on`. Phone *unlocked*: copy "alpha" in any app, then within 30 s click **Paste from phone** in the Clipboard window (or run `geekbridge-ctl clipboard paste-from-phone --notify`) and paste into a text field. | "alpha" pastes. Then copy again, wait 45 s, paste: refused ("copy again"). Copy, lock the phone, paste: refused ("unlock"). Without the PC-side tick the error says no connected phone has Clipboard switched on, and how to fix it. | `geekbridge-ctl clipboard status`: last pull `delivered`, then reason `expired`, then `locked`. | |
| 3 | **Clipboard PC to phone.** `wl-copy "bravo"`; `geekbridge-ctl clipboard push-to-phone`. Phone: long-press a text field > Paste. | "bravo" pastes on the phone. | `clipboard status` sent +1. | |
| 4 | **Hotkey (dry run only).** `geekbridge-ctl hotkey install` | Prints a diff for `~/.config/hypr/bindings.lua` (Super+Shift+V, window rules for the call/share windows); writes nothing. | The diff on screen. Note: the bound command is a bare `geekbridge-ctl`, which uses the *default* state dir and PATH. To use the key itself, restart with `scripts/run-desktop.sh --default-state` and symlink the binaries into `~/.local/bin`, then `hotkey install --apply`. | |
| 5 | **File PC to phone.** `geekbridge-ctl send-file DEVICE_ID /tmp/gb-test.bin` (make it: `head -c 5M /dev/urandom > /tmp/gb-test.bin`). Keep the app open on the phone. | Phone shows "GeekBridge-desk wants to send a file" with **Accept** / **Decline** (a notification instead when the app is in the background and notifications are allowed). Accept: completes, saved to `Downloads/GeekBridge`. | `geekbridge-ctl share status` done; hash: `adb shell sha256sum /sdcard/Download/GeekBridge/gb-test.bin` equals `sha256sum /tmp/gb-test.bin`. | |
| 6 | **File phone to PC.** First allow the phone on the PC: Share files window > Receiving > tick **Allow this phone to send me files, links and text** (or `geekbridge-ctl share allow DEVICE_ID on`; add `auto-accept ... on` to skip the popup). Without this the phone shows "Sending 1 of 1 · 0%" forever. Then Gallery: open a photo > Share > **Send to computer** (or Home > **Send file**). Repeat with 3 photos. The first send asks for the notification permission if you skipped it in setup. | PC offer (or auto-accept), files in the save folder, viewable. | `geekbridge-ctl share status`; open the file. | |
| 7 | **Share sheet from Chrome.** Chrome: any page > Share > **Send to computer**. Then long-press an image > Share > Send to computer. | Link arrives (`link: ...`); image arrives as a file. | `share status`. | |
| 8 | **Incoming call.** First pair Bluetooth hands-free: PC `bluetoothctl` > `power on`, `scan on`, `pair AA:BB:..`, `trust AA:BB:..`, `connect AA:BB:..`; on the phone accept the pairing and keep "Phone calls" enabled for the PC. Needs an HFP-capable PipeWire stack (`wpctl status` lists the phone under Bluez). In the app: grant Phone permission, switch call control on. Ring the phone from another phone. | PC popup (or notification) within 2 s with Answer / Decline. **Answer:** phone answers, popup shows Hang up. **Decline:** ringing stops, caller gets busy. Audio: it only moves to the PC if the HFP link carries it; if it stays on the phone, try `geekbridge-ctl call audio`. A missing audio route is expected on some stacks, note which. | `geekbridge-ctl calls` (ringing, then live, controls offered, reason if one is missing); `geekbridge-ctl calling` (backend verdict). Screenshot the popup. | |
| 9 | **Calendar + contacts.** Phone: allow Contacts and Calendar for this computer. PC: `geekbridge-ctl agenda-refresh`, then `geekbridge-ctl agenda` and open the Calendar/Contacts windows (`geekbridge-agenda`, `geekbridge-contacts`). | Real contacts and today's events show. | `geekbridge-ctl agenda` (prints personal data, nothing stored). | |
| 10 | **Battery exemption on this phone's OEM.** App > setup wizard > battery > **Allow unrestricted battery**, then follow the vendor step (the one `$S check` printed). | Dialog accepted, vendor screen is the right one, Home shows no battery warning. | `$S logs`, then in `dumpsys-deviceidle.txt` the package appears under *whitelist*. Screenshot the vendor screen. | |
| 11 | **Kill the app and reconnect.** Swipe it out of Recents; also `adb shell am force-stop dev.geekbridge.android`, then open the app. | Link back to `connected` without re-pairing (within 30 s of opening; after a swipe the foreground service should return on its own). | `geekbridge-ctl status` link `connected`; `geekbridge-ctl events 0` shows `link_lost` then `connected`. | |
| 12 | **Wi-Fi off/on.** Phone: Wi-Fi off 1 min, on again. Optional: mobile data only for 30 s. | No crash; reconnects within 30 s of Wi-Fi returning. | `status`/`events 0` as above; `$S logs` shows empty `logcat-crash.txt`. | |

### Unpair, re-pair, rename (verified on the emulator, see [first-run-audit.md](reports/first-run-audit.md))

- **Forget on the PC:** Pair a phone > **Forget...** > **Forget phone**. The phone still shows the computer as paired; scanning a fresh QR re-pairs at once.
- **Unpair on the phone:** Home > **Diagnostics & about** > **Unpair**. The PC keeps showing "Paired, offline" until you forget it there too. If you re-pair with only one side cleared, the **first** attempt times out ("Pairing timed out before it was confirmed") and clears the stale record; show a new QR and scan again, the second attempt pairs. To avoid the detour, forget on both sides first.
- **Rename:** there is no rename screen yet. The computer's name is set when the service starts (`scripts/run-desktop.sh --name NAME`); the phone shows its model name.
- **After a PC restart** the phone should reconnect on its own through discovery (UDP 1716 broadcast, firewall: step 0.2). The emulator cannot receive broadcasts, so this was **not** verified there: check it on the real phone (step 11). Meanwhile the phone's Home shows "Waiting for ...: Open GeekBridge on your computer, on the same Wi-Fi network" until then.

## 2. Wrap up (1 min)

1. `$S logs` and `$S screens final` for the record. The bundle (README inside) lands in `dist/realdevice/<timestamp>/`.
2. Optional battery run (needs the phone unplugged, see the hint it prints): `$S battery-baseline --minutes 60`, come
   back, `$S battery-report`. Control run with the app force-stopped: `--label control`.
3. Stop and clean up: `scripts/run-desktop.sh --stop`; remove firewall rules with the undo lines from step 0.2; revoke
   the battery exemption if you want the warning path tested.

Rehearse without a phone: `$S --dry-run install`, or point at an emulator with `--adb-port 15999 --serial emulator-5584`.
