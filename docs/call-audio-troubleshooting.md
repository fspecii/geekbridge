<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Call audio troubleshooting (Linux, PipeWire + WirePlumber 0.5)

For "Hear calls on this computer" (the phone is the Bluetooth audio gateway, the PC is its hands-free unit). The checklist is `geekbridge-ctl calling`; each failing step there prints a `more:` line that points at a section of this page. Background: [call-audio-options.md](call-audio-options.md), [reports/call-audio-backend-review.md](reports/call-audio-backend-review.md). No Bluetooth controller at all? The experimental Wi-Fi option (debug phone builds only, off by default) is described in [reports/call-audio-wifi-realphone.md](reports/call-audio-wifi-realphone.md); this page does not cover it.

How this page was checked: the keys and defaults below were read on this machine from WirePlumber 0.5.18 (`/usr/share/wireplumber/wireplumber.conf`), PipeWire 1.6.9 (`man pipewire-props`, `/usr/share/spa-0.2/bluez5/bluez-hardware.conf`, the strings of `libspa-bluez5.so`) and upstream's WirePlumber Bluetooth page <https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/bluetooth.html> and "The configuration file" page. Every snippet was parsed with `spa-json-dump FILE` (exit 0). No snippet has been tried against a live adapter yet (this PC had none); the first real run is the dongle test. What drops on a restart is derived from the design (the Bluetooth monitor and the native hands-free backend live inside WirePlumber), not measured.

## The WirePlumber drop-in

WirePlumber 0.5 reads every `*.conf` file in `wireplumber.conf.d/` directories, system first, then `/etc/wireplumber/`, then `~/.config/wireplumber/`; inside one directory in alphanumeric order. Objects with the same key are merged, later files win; **arrays from different files are concatenated** (so a fragment can add a role but cannot remove a default one by listing fewer). Files that do not end in `.conf` are ignored. A fragment that does not parse stops WirePlumber from starting, so check it before restarting.

Defaults (as installed, nothing configured): `bluez5.roles = [ a2dp_sink a2dp_source bap_sink bap_source hfp_hf hfp_ag ]` (already includes `hfp_hf`, which is the role this PC needs); `bluez5.hfphsp-backend = "native"`; `bluez5.enable-msbc` unset, which means "decided per adapter and headset from PipeWire's hardware list"; `bluez5.auto-connect` empty (WirePlumber does not ask the phone for anything on its own); `bluez5.telephony-dbus-service` on. Other valid role names: `hsp_hs`, `hsp_ag`, `bap_bcast_sink`, `bap_bcast_source`.

You normally do not write any of this by hand: `geekbridge-ctl call setup --fix-config` writes `~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf` only when something is wrong (roles without `hfp_hf`, a backend other than `native`/`any`, the telephony service switched off, or `bluez5.auto-connect` without `hfp_ag`), checks the text with `spa-json-dump` first, keeps an older copy as `.bak`, and never edits your own files. Its output was compared with the syntax above and matches (same keys, `bluez5.roles = [ a b c ]` without commas, quoted `"native"`, rules with `update-props`).

### Wideband off (mSBC) for a silent or choppy call

`bluez5.enable-msbc` overrides the hardware list: `false` forces narrowband (CVSD, 8 kHz), `true` forces wideband. Create a separate file so `--fix-config` never touches it:

```
mkdir -p ~/.config/wireplumber/wireplumber.conf.d
cat > ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-msbc-off.conf <<'EOF'
monitor.bluez.properties = {
  bluez5.enable-msbc = false
}
EOF
spa-json-dump ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-msbc-off.conf
systemctl --user restart wireplumber
```

`spa-json-dump` must print the JSON back and exit 0. Undo: `rm ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-msbc-off.conf` and restart. After the next call, `busctl --user get-property org.pipewire.Telephony /org/pipewire/Telephony/ag1 org.pipewire.Telephony.AudioGatewayTransport1 Codec` should say `y 1` (CVSD) instead of `y 2` (mSBC). The number in `ag1` can differ (`busctl --user tree org.pipewire.Telephony`).

### Everything in one file (the full set of keys, for reference)

```
monitor.bluez.properties = {
  bluez5.roles = [ a2dp_sink a2dp_source bap_sink bap_source hfp_hf hfp_ag ]
  bluez5.hfphsp-backend = "native"
  bluez5.enable-msbc = false
  bluez5.telephony-dbus-service = true
}
monitor.bluez.rules = [
  {
    matches = [ { device.name = "~bluez_card.*" } ]
    actions = {
      update-props = {
        bluez5.auto-connect = [ a2dp_sink a2dp_source hfp_ag ]
      }
    }
  }
]
```

`bluez5.auto-connect` names the **phone's** profile that PipeWire connects when the phone shows up (`hfp_ag` = the phone is the audio gateway). It lives in a rule, not in `monitor.bluez.properties`. A line starting with `#` is a comment.

If a phone refuses hands-free with `hfp_hf` alone, add `hsp_hs` to `bluez5.roles` (upstream's note: some headsets misbehave with both `hsp_ag` and `hfp_ag`, which is why only HFP is on by default).

## Restarting and what drops

```
systemctl --user restart wireplumber                              # after any wireplumber.conf.d change
systemctl --user restart wireplumber pipewire pipewire-pulse      # only if the sound server itself is stuck
systemctl --user status wireplumber pipewire pipewire-pulse       # all three "active (running)"
journalctl --user -u wireplumber -b --no-pager | tail -30         # parse errors show here
```

* `wireplumber` alone: the phone's hands-free link and any call in progress drop (the native backend and `org.pipewire.Telephony` live inside it, so GeekBridge loses the telephony service for a moment); the phone usually reconnects by itself, otherwise `bluetoothctl connect PHONE_ADDRESS`. Other audio keeps playing; device defaults and routes are restored from WirePlumber's saved state.
* `pipewire` + `pipewire-pulse`: everything that plays sound or records stops for a second (browser tabs, music, GeekBridge's call routing); most apps reconnect, some need a restart.
* Bluetooth pairings are stored by `bluetooth.service` and survive all of the above. Do not restart them mid-call.
* To log what the Bluetooth plugin decides (mSBC, SCO): `systemctl --user set-environment WIREPLUMBER_DEBUG='spa.bluez5*:4' && systemctl --user restart wireplumber`; undo with `systemctl --user unset-environment WIREPLUMBER_DEBUG` and a restart.

## Symptoms

Start with `geekbridge-ctl calling` (the first FAIL is the one to fix) and `scripts/realdevice-report.sh --no-adb` (read-only, redacted).

### Adapter not found

Diagnose: `bluetoothctl list` (empty), `ls /sys/class/bluetooth` (no `hci0`), `lsusb` (is the dongle listed?), `dmesg | grep -i -E 'bluetooth|btusb|firmware'`.
Fix: nothing listed in `lsusb`: another port (USB 2.0, directly on the PC, no hub) or cable. Listed but no `hci0`: see the next section. `lsmod | grep btusb` empty: `sudo modprobe btusb`. A built-in (laptop) adapter that vanished after resume: unplug power, or `sudo modprobe -r btusb && sudo modprobe btusb`.

### Dongle firmware missing

Diagnose: `dmesg | grep -i -E 'bluetooth|btusb|btintel|btrtl|btmtk|btbcm|firmware'`. Typical lines: `Direct firmware load for rtl_bt/rtl8761bu_fw.bin failed with error -2`, `Direct firmware load for intel/ibt-... failed`, `Failed to load firmware`, `Opcode 0x... failed`.
Fix: the module that names the file tells the vendor package. On Arch the firmware is split into vendor packages (checked here: `linux-firmware` depends on `linux-firmware-realtek`, `-intel`, `-mediatek`, `-broadcom`, `-atheros` and others): `sudo pacman -S --needed linux-firmware` (pulls the vendor parts as dependencies) or the one you need: `linux-firmware-realtek` (`btrtl`), `linux-firmware-intel` (`btintel`), `linux-firmware-mediatek` (`btmtk`), `linux-firmware-broadcom` (`btbcm`). Unplug and replug (a reboot is not needed). Check the file exists: `ls /usr/lib/firmware/rtl_bt/ | grep 8761` (on this machine `rtl8761b_fw.bin`, `rtl8761bu_fw.bin`, `rtl8852bu_fw.bin` are present). Very new chips need a new kernel as well as new firmware: `uname -r`.
BCM20702 sticks (0a5c:21e8 and similar) normally need `BCM20702A1-<vid>-<pid>.hcd`, which is **not** in `linux-firmware` here (only a few `BCM-*.hcd` files are); it comes from the AUR package `broadcom-bt-firmware`. That is one more reason to prefer Intel or Realtek.

### Radio blocked (rfkill)

Diagnose: `rfkill list bluetooth` (look for `Soft blocked: yes` or `Hard blocked: yes`).
Fix: soft: `rfkill unblock bluetooth`. Hard: a switch, an Fn key or a BIOS/firmware setting; software cannot clear it.

### Bluetooth service inactive

Diagnose: `systemctl status bluetooth.service`, `journalctl -u bluetooth -b --no-pager | tail -20`.
Fix: `sudo systemctl enable --now bluetooth.service`. Package missing: `sudo pacman -S --needed bluez bluez-utils`. `org.bluez was not provided` or `BlueZ system service is not available` in the WirePlumber journal means this service was down when WirePlumber started: start it, then `systemctl --user restart wireplumber`.

### Adapter powered off

Diagnose: `bluetoothctl show | grep -E 'Powered|PowerState'`.
Fix: `bluetoothctl power on`. To make it permanent set `AutoEnable=true` under `[Policy]` in `/etc/bluetooth/main.conf`, then `sudo systemctl restart bluetooth`.

### Pairing loops

(pairing fails, repeats, or the phone forgets the PC.)
Diagnose: `bluetoothctl devices Paired`, `bluetoothctl info PHONE_ADDRESS`, and watch `journalctl -u bluetooth -f` while pairing.
Fix: remove the pairing on **both** sides, then start clean: `bluetoothctl remove PHONE_ADDRESS`, on the phone "Unpair/Forget" the PC, `systemctl restart bluetooth` (as root: `sudo`), then `bluetoothctl` and `agent on`, `default-agent`, `scan on`, and tap the PC by name on the phone (pairing from the phone's side is more reliable than from the PC). After pairing: `bluetoothctl trust PHONE_ADDRESS`. If the phone shows a code on only one side, confirm on both. A stale key (after reinstalling the OS, or a dongle swap) is the usual cause of a loop; a failing adapter (`Failed to set mode`, `Opcode ... failed`) points at the dongle.

### Phone never connects HFP

The phone is connected (music works) but the checklist says "not connected as a hands-free device".
Diagnose: `bluetoothctl info PHONE_ADDRESS | grep -i -E 'UUID|Connected'` (the phone should list `Handsfree Audio Gateway`, `0000111f-...`); `busctl --user tree org.pipewire.Telephony` (an `ag` object appears only when it is connected).
Fix: on the phone, Settings > Connections > Bluetooth > gear next to the PC > turn on **Calls** (Samsung One UI; "Phone calls" on Pixel and stock Android). Then from the PC: `bluetoothctl connect PHONE_ADDRESS 0000111f-0000-1000-8000-00805f9b34fb`. For it to come back after every reconnect add `hfp_ag` to `bluez5.auto-connect` (the rule in "Everything in one file"). If the telephony service itself is missing: `bluez5.telephony-dbus-service = true` and `bluez5.hfphsp-backend = "native"` (what `--fix-config` writes), restart WirePlumber.

### No HFP profile on the phone card

`pactl list cards | grep -A40 'Name: bluez_card'` lists only `a2dp-sink` and `off`.
Fix: in order: (1) `geekbridge-ctl call setup --fix-config` (roles without `hfp_hf`, backend not native); (2) the phone side "Calls" toggle above; (3) `bluetoothctl disconnect PHONE_ADDRESS && bluetoothctl connect PHONE_ADDRESS`; (4) `systemctl --user restart wireplumber`. Profiles are created only after the phone has connected its hands-free profile, so a card with no HFP means step 2 or 3, not a missing package.

### Profile audio-gateway missing

`pactl list cards` shows the phone card, but no `audio-gateway` profile (or the card is in `a2dp-source`/`off`).
Diagnose: `pactl list cards | grep -E 'Name: bluez_card|Active Profile|audio-gateway'`.
Fix: with the phone as the gateway the profile is called `audio-gateway` (A2DP source plus HFP/HSP gateway). It appears once HFP is connected: do "Phone never connects HFP". To set it by hand: `pactl set-card-profile bluez_card.XX_XX_XX_XX_XX_XX audio-gateway`. GeekBridge switches it for calls answered on the PC and back afterwards, so it is normal to see another profile outside a call.

### Connected but silent

The call connects, `State` is `active`, the checklist says "digital silence".
Diagnose, in order: `busctl --user get-property org.pipewire.Telephony /org/pipewire/Telephony/ag1 org.pipewire.Telephony.AudioGatewayTransport1 State Codec RejectSCO`; `pactl list sink-inputs short` and `pactl list source-outputs short` during the call (the phone's streams must exist); `wpctl status` (default sink/source not muted: `wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 0`); `dmesg | grep -i -E 'sco|bluetooth' | tail`; `scripts/realdevice-report.sh --bt-trace 60` while placing a call.
Fix: (1) turn mSBC off (section above) and call again; (2) `RejectSCO` true means the voice link is refused: call from the phone's call screen, switch the phone's output to the PC and back; (3) try a different dongle (see below), because broken SCO is a dongle problem more often than a software one; (4) a Bluetooth headset as the PC's call device needs a second SCO link on the same dongle: use a wired or USB headset.

### Choppy or robotic audio

Wideband (mSBC) trouble: the voice sounds metallic, drops out, or stutters, but the link stays up.
Diagnose: Codec `y 2` in the property above means mSBC; with debug logging on (see the restart section) look for `alt6`, `alt1`, `no msbc/esco transport`, `does not support USB ALT6`.
Fix: `bluez5.enable-msbc = false` (above): narrowband is dull but steady. Also: put the dongle on a USB 2.0 port away from USB 3 devices and hubs, use a short extension cable to move it away from the PC's rear panel (2.4 GHz interference), keep the phone within a few metres, and `sudo modprobe -r btusb && sudo modprobe btusb enable_autosuspend=0` if the link drops after idle (`/sys/module/btusb/parameters/enable_autosuspend`).

### Call audio goes to the phone

The call is answered on the PC but the voice stays on the phone's earpiece/speaker.
Diagnose: `busctl --user get-property ... AudioGatewayTransport1 State` (not `active` means the link was never opened); `geekbridge-ctl calling` (the steps `card`, `devices`, `voice`); `pactl list cards | grep 'Active Profile'`.
Fix: the PC must have headphones and a microphone chosen: `geekbridge-ctl call audio-devices`. Answer from the GeekBridge call screen. If the voice still stays on the phone, try the phone's own audio-output button on its call screen and pick the PC (not verified on a real phone yet). Check the phone's Calls toggle is on and the card is `audio-gateway`. If the card is in another profile: `pactl set-card-profile bluez_card.XX_XX_XX_XX_XX_XX audio-gateway`.

## USB dongles

What the software needs: Bluetooth 4.0 or newer with Classic (BR/EDR) and LE (dual mode; hands-free is a Classic profile, so an LE-only adapter will not do), a `btusb` chip with firmware in `linux-firmware`, and a working SCO (voice) path over USB. wideband speech (mSBC) additionally needs eSCO and a USB alternate setting that the kernel and PipeWire both handle: PipeWire's own list (`/usr/share/spa-0.2/bluez5/bluez-hardware.conf`) enables the Realtek ALT1 mSBC workaround only for USB vendor `0bda` and turns it off for vendors `0489`, `0e8d`, `13d3`, `2c7c` (MediaTek-based modules); other USB adapters use the standard path; on kernels older than 5.8 (and some 5.10 point releases) PipeWire turns the ALT1 mSBC path off by itself. When it does not work, narrowband (CVSD) is the fallback and is fine for calls.

| Class | USB ids (from memory or reports; confirm with `lsusb`) | Notes |
|---|---|---|
| Intel AX200 / AX210 (module with USB Bluetooth) | 8087:0029, 8087:0032 | Firmware `intel/ibt-*.sfi` (on this machine in `linux-firmware-intel`). Normally the most trouble-free with `btusb`/`btintel`. Mostly found as a laptop M.2 card (its Bluetooth part is on USB inside), rarely as a plug-in dongle. Reported to work for HFP on 6.x kernels; not yet tried by this project. |
| Realtek RTL8761B / BU (TP-Link UB500, many "5.0/5.1 nano" sticks) | 0bda:8771, 2357:0604 (UB500), 2550:8761 | Firmware `rtl_bt/rtl8761bu_fw.bin` + `rtl8761bu_config.bin` (present in `linux-firmware-realtek`). Cheap, widely sold, a good first dongle. mSBC via the Realtek ALT1 workaround; if choppy, turn mSBC off. |
| Realtek RTL8852BE / 8852BU / 8852CU (combo Wi-Fi 6 cards and USB sticks) | 0bda:xxxx | Firmware `rtl8852bu_fw.bin`, `rtl8852cu_fw*.bin` present locally. Newer, needs a recent kernel (6.x). Treat as likely but unverified. |
| ASUS USB-BT500 | 0b05:190e | RTL8761B inside. Same notes as the Realtek row. |
| TP-Link UB500 | 2357:0604 | RTL8761B, Bluetooth 5.0. Same notes. |
| Broadcom BCM20702 (older "4.0" sticks, ASUS BT400, Plugable) | 0a5c:21e8 | Needs `broadcom-bt-firmware` from the AUR (not in `linux-firmware`), old, no new features. Avoid unless it is what you have. |
| "CSR8510 A10" clones | 0a12:0001 | Most are not real CSR chips; many have broken or absent SCO/eSCO, wrong buffer sizes (kernel parameters `force_scofix`/`disable_scofix` exist because of that) and fall off the bus. Known unreliable for voice. Avoid. |
| MediaTek MT7921 / MT7922 / MT7925 modules | 0e8d, 0489, 13d3 | Firmware `BT_RAM_CODE_MT79*` in `linux-firmware-mediatek`. mSBC works through the standard path; PipeWire carries special cases for them. Generally good in new laptops. |

What to buy: a plain Bluetooth 5.x USB adapter with an **Intel or Realtek RTL8761B** chip from a known brand (TP-Link UB500 or ASUS USB-BT500), USB 2.0 or 3.0 type A, not a "CSR 4.0" no-name stick, not a combo with a Wi-Fi radio that you also need. Check the chip before buying: sellers change chips under the same name, so confirm the ids above with `lsusb` after unboxing and return the stick if they differ. Plug it directly into the PC (not a hub), and note `lsusb` id and `dmesg` lines in the report if something fails: `scripts/realdevice-report.sh` records them.

Sources: Linux `drivers/bluetooth/btusb.c`, `btrtl.c`, `btintel.c`, `btmtk.c` (id tables and quirks; not read on this machine, no kernel source installed), PipeWire's `bluez-hardware.conf` (read), `modinfo btusb` (parameters read: `force_scofix`, `disable_scofix`, `enable_autosuspend`, `reset`), the Arch wiki page "Bluetooth" and the firmware files listed in `/usr/lib/firmware` (read). The table's "works" statements are other people's reports, flagged where untested here. **No adapter has been tested by this project.**

## Testing notes: `call setup` and the real Bluetooth adapter

BlueZ lives on the system bus, which a sandbox (private `HOME`, `GEEKBRIDGE_STATE_DIR`, a different control port) still sees. So a sandboxed `geekbridge-ctl call setup` used to change the real adapter's modes (`Discoverable`, `DiscoverableTimeout`, `Pairable`). Two guards now exist:

- **Sandbox guard.** `setup::run`, `reconnect`, `forget` and the adapter probe refuse to talk to the system bus when `GEEKBRIDGE_NO_SYSTEM_BLUETOOTH` is set (and not `0`), or when `GEEKBRIDGE_STATE_DIR` or `GEEKBRIDGE_CONTROL_PORT` is set. They return "this process is a sandbox or test ... does not touch the computer's real Bluetooth adapter". `GEEKBRIDGE_ALLOW_SYSTEM_BLUETOOTH=1` lifts the sandbox rule (not the explicit `NO_SYSTEM_BLUETOOTH`) for a deliberate run on real hardware. A private bus passed as `Bus(Some(address))` (the fake BlueZ below) is never blocked.
- **Restore guard.** `call setup` reads the adapter's modes before changing anything and puts them back when it ends, however it ends: success, failure, cancel, early error or panic (a drop guard). If it had to switch Bluetooth on and did not get as far as pairing, it switches it off again.

Tests use `scripts/e2e/call-audio/fake_bluez.py` on a private `dbus-daemon` (crates/bridge-calls/tests/setup_fake_bluez.rs): one test checks the modes after a setup that finds no phone, another runs a child process with each sandbox variable and the fake as the *system* bus and asserts the fake saw no call. Never run `call setup` against the real adapter from a test or an agent session.
