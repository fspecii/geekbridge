# Real-device quickstart (10-15 minutes)

What has and has not been done on hardware: one real phone (Samsung Galaxy Z Fold5, SM-F946B) has been paired, and pairing
and link stability were exercised on it. Everything else in this checklist (calls, Messages, battery sharing, dialing,
calendar, contacts, clipboard on a real phone) is still to be confirmed by you. Windows and macOS have only run in CI.

First hour with a real phone, highest value first. The ordered session checklist (with the one-file report and the lock-screen link watcher) is [real-device-test-plan.md](real-device-test-plan.md).
Fill the **Result** column as you go (PASS / FAIL / N/A + a note); on any FAIL run `logs` (step 0) right away,
Android's log buffer is small.

## Bring-up checklist in 10 minutes

**The phone is already paired and you only have a newer build?** Do not follow this table; `desktop --fresh` (step 6)
forgets the pairing. Use the upgrade steps in [reports/preflight-upgrade.md](reports/preflight-upgrade.md): `$S install`
(it compares the signing key and version with the app on the phone first) and `$S desktop --restart`.

Run everything from `path/to/geekbridge`, in a terminal **inside Hyprland**. Do not `source scripts/android-env.sh` in
that terminal (it points cargo at `target/android`, so the desktop binaries would not be in `target/release`).

| # | Step | Command | Expected output | If it fails |
| --- | --- | --- | --- | --- |
| 1 | Build the desktop once (skip if `target/release/geekbridge-service` is newer than your last change) | `cargo build --release --locked -p bridge-apps -p bridge-service` | `Finished release` | [T9](#t9-build-and-tool-problems) |
| 2 | Phone: Settings > About phone > tap *Build number* 7 times; Developer options > **USB debugging** on; plug in with a data cable, USB mode *File transfer*; tick **Always allow** on the RSA prompt | `adb devices` | your serial followed by `device` (not `unauthorized`, `offline`, `no permissions`) | [T0](#t0-the-phone-does-not-show-up-in-adb-devices) |
| 3 | Helper variable and a first health check | `S=scripts/realdevice.sh; $S check` | phone `PASS device authorised`, `PASS phone and this PC are on the same subnet`, Android line, a *battery wizard to expect* line. A `FAIL ufw` here is expected until step 4 | each WARN/FAIL names its section: subnets [T2](#t2-phone-and-pc-on-different-subnets-guest-wi-fi-client-isolation), VPN [T3](#t3-vpn-on-either-side) |
| 4 | Open the firewall for this PC (ufw, default DROP, no GeekBridge rule yet; details in [Firewall on this machine](#firewall-on-this-machine)) | `$S firewall --apply` | two `sudo ufw allow from 192.168.1.0/24 ... 1716:1764 ...` lines run; then `PASS ufw: ... rules for 1716:1764 tcp+udp present` | [T1](#t1-the-phone-cannot-connect-firewall) |
| 5 | Install and start the app | `$S install` | `PASS installed`, five `PASS granted ...`, `PASS process running` | [T0](#t0-the-phone-does-not-show-up-in-adb-devices) (install errors listed there) |
| 6 | Start the PC side and print the QR (first start can take 10+ s). First time only: `--fresh` is refused while the state holds pairings (add `--forget-pairings` to start over; the old state is kept aside) | `$S desktop --fresh` | `service up`, `the QR makes the phone try (in this order): <your LAN IP>:1716`, a QR in the terminal, the GeekBridge window on **Home** | [T4](#t4-many-network-interfaces-which-address-is-in-the-qr) if the address is wrong; [T9](#t9-build-and-tool-problems) if the service does not come up |
| 7 | The real reachability test, now that the service listens | `$S check` | section **Phone to this PC**: `PASS phone can ping ...`, `PASS phone reached <IP>:1716 over TCP`; `Result: ... 0 fail` | *timed out*: [T1](#t1-the-phone-cannot-connect-firewall)/[T2](#t2-phone-and-pc-on-different-subnets-guest-wi-fi-client-isolation)/[T3](#t3-vpn-on-either-side); *refused*: [T4](#t4-many-network-interfaces-which-address-is-in-the-qr) |
| 8 | Pair: phone wizard step 1 **Pair with your computer** > **Scan QR code from my computer** (camera allowed once), scan > **Continue** > **Pair** (details: checklist row 1 below) | `eval "$(scripts/run-desktop.sh --env)"; geekbridge-ctl status` | phone under `paired`, link `phase: connected`; phone lands on Home with "Paired ✓ with ..." | QR scan did nothing / "no address answered": [T5](#t5-discovery-mdns-and-udp-broadcast-blocked-pair-by-qr-only) and [T4](#t4-many-network-interfaces-which-address-is-in-the-qr); `geekbridge-ctl test-connection` explains the failing step |
| 9 | Battery exemption for this phone's OEM | app wizard > battery > **Allow unrestricted battery**, then the vendor step `check` printed | `$S logs`, then `dumpsys-deviceidle.txt` lists the package under *whitelist* | [T8](#t8-wi-fi-power-saving-doze-and-oem-battery-managers) |
| 10 | Prove the link survives: lock the phone for 5 minutes, then | `geekbridge-ctl status; geekbridge-ctl events 0` | link still `connected`, no `link_lost` | [T8](#t8-wi-fi-power-saving-doze-and-oem-battery-managers) |

Then work through the checklist in section 1.

```bash
cd path/to/geekbridge
S=scripts/realdevice.sh
eval "$(scripts/run-desktop.sh --env)"      # puts geekbridge-ctl on PATH, pointed at this session's service
```

**Evidence note.** The Android app writes almost nothing to logcat on the happy path, so "proof" is the PC side
(`geekbridge-ctl ...`), the phone screen (`$S screens NAME` saves a PNG to `dist/realdevice/screens/`) and, for
crashes or OEM kills only, `$S logs` (look for `FATAL EXCEPTION`, `am_proc_died`, `Killing`).

## 0. Prepare (3 min, once)

The same steps as the table above, with the detail:

| # | Do | Expect | Result |
| --- | --- | --- | --- |
| 0.1 | Phone: USB debugging on, plug in, accept the RSA prompt. `$S check` | phone listed as `device`, same-subnet PASS, "no VPN on the phone", no FAIL except the firewall. Note the printed *battery wizard to expect* line. | |
| 0.2 | **Firewall.** `$S firewall --apply` (runs the two printed `sudo ufw allow from <LAN> ... 1716:1764 tcp/udp` lines; undo lines are printed too). See [Firewall on this machine](#firewall-on-this-machine). | `check` shows firewall PASS | |
| 0.3 | `$S install` (add `--build` if `check` says sources are newer; `--no-grant` to see the first-run permission prompts). A Play Protect "Scan app?" dialog on the phone: **Don't send** / **Install anyway**. | `installed`, app opens on the setup wizard | |
| 0.4 | `$S desktop` (service + home window + terminal QR; `--fresh --forget-pairings` starts over, keeping the old state in `<state>.before-fresh-<time>`; `--restart` runs a rebuilt binary and keeps the pairings). The very first start creates the identity key and can take 10+ s; wait for `service up`. | `geekbridge-ctl status` answers; the GeekBridge window opens on **Home** (no QR there, see step 1) and the terminal shows a QR plus "the QR makes the phone try (in this order): ..." | |
| 0.5 | `$S check` again. Now the service listens, so the phone-to-PC TCP test runs. | `PASS phone reached <IP>:1716 over TCP` | |

## 1. The checklist

| # | Do (exact action) | Expect | Proof | Result |
| --- | --- | --- | --- | --- |
| 1 | **Pair via QR.** Phone on the same Wi-Fi. PC: use the terminal QR from step 0.4, or in the GeekBridge window click **Link a phone** (Ctrl+2; with no phone paired the big QR is already there and renews itself, with a live line *Waiting for phone…* / *Phone scanned — confirm on phone* / *Paired ✓ with ...*; Home's first tile says "Click to link a phone"). Phone, first launch: the setup wizard opens on step 1 "Pair with your computer" > **Scan QR code from my computer** (allow the camera once; the in-app camera reads the QR), or **Open the camera app** / **Use a scanner app instead** and tap the GeekBridge link. No computer app yet? **Don't have GeekBridge on your computer yet?** > **Send me the download link**. If your camera only shows the link as text, copy it, come back to this screen, tap **Paste from clipboard** > **Continue**. Then "Connect to (computer name)?" (**Continue**), then "Pair with ...?" with **Pair** / **Decline**. QR lives 2 min and renews itself: reprint with `$S desktop`. You can **Skip** every wizard step and still pair: Home then shows **Pair a computer**. | Phone lands on Home with "Paired ✓ with ..."; PC shows "Paired ✓ with ..." and its "What to do next" card. No code to compare. | `geekbridge-ctl status`: phone under `paired`, link `connected`. Fail: tap **Test connection** on the phone's error screen, or `geekbridge-ctl test-connection`. Screenshot `$S screens paired`. | |
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

- **Unlink on the PC:** Link a phone > **Unlink…** > **Unlink phone**. The phone still shows the computer as paired; scanning a fresh QR re-pairs at once.
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
An emulator is NAT-ed (`10.0.2.x`), so `check` warns about the subnet, its TCP test comes out of this PC itself (a firewall
cannot show up) and it never receives UDP broadcasts. Those three lines are the ones that only mean something on the real phone.

## Firewall on this machine

Facts checked on this PC (read-only, no root): **ufw** is the active firewall (`systemctl is-active ufw` = `active`;
`nftables`, `firewalld` and `iptables` services are inactive; ufw itself drives nftables underneath), default incoming
policy **DROP** (`/etc/default/ufw`), and `/etc/ufw/user.rules` has allows for SSH and a few dev ports from
`192.168.1.0/24` but nothing for 1716-1764. Nothing listens on 1716 until the service runs; with it running,
`ss -lntup | grep -E ':17(1[6-9]|[2-5][0-9]|6[0-4]) '` shows `tcp 0.0.0.0:1716` (pairing and data) and `udp 0.0.0.0:1716`
(discovery). So the phone cannot connect until you add (the script prints exactly these, with your detected LAN):

```bash
sudo ufw allow from 192.168.1.0/24 to any port 1716:1764 proto tcp comment 'GeekBridge'
sudo ufw allow from 192.168.1.0/24 to any port 1716:1764 proto udp comment 'GeekBridge'
sudo ufw status numbered | grep GeekBridge          # verify: two ALLOW IN lines
```

Remove them after the session:

```bash
sudo ufw delete allow from 192.168.1.0/24 to any port 1716:1764 proto tcp
sudo ufw delete allow from 192.168.1.0/24 to any port 1716:1764 proto udp
```

Tighter, one phone only (give it a DHCP reservation on the router first, otherwise its address changes):

```bash
sudo ufw allow from PHONE_IP to any port 1716:1764 proto tcp comment 'GeekBridge'
sudo ufw allow from PHONE_IP to any port 1716:1764 proto udp comment 'GeekBridge'
```

Those commands were checked for syntax against the ufw grammar only (applying them needs root and this rehearsal
does not change the firewall). `$S firewall` / `$S check` detect the result by reading `/etc/ufw/user.rules`. If you
use something else: firewalld `sudo firewall-cmd --add-port=1716-1764/tcp --add-port=1716-1764/udp` (runtime only,
`--permanent` to keep), plain nftables `sudo nft insert rule inet filter input tcp dport 1716-1764 accept` (and `udp`;
table and chain names vary, `sudo nft list ruleset` shows yours), iptables `sudo iptables -I INPUT -p tcp --dport 1716:1764 -j ACCEPT`
(and `-p udp`).

## Troubleshooting

Start with `$S check`: it prints the phone's view (ping, TCP to the service, VPN, Wi-Fi validation, Battery Saver)
and the PC's view (addresses, the QR contents, firewall) and names the section below for each WARN/FAIL.
For the computer side alone (service up, ufw rule for 1716-1764, Wayland/X11 variables, paired phones, why Messages or
clipboard is off) run `geekbridge-ctl doctor`; it prints the fix command under each problem and exits 1 if a check failed. If a paired phone
is not connected, `geekbridge-ctl status` prints a "Try: ..." line with the likely reason. Messages on a sideloaded
APK on Android 13+: Settings > Apps > GeekBridge > top-right menu > **Allow restricted settings** (appears after one
**Allow access** attempt), then Permissions > SMS > Allow. Also try, if you want: **Share battery level** (off by default, needs
the allow on both ends) and a dial from the PC (`geekbridge-ctl call dial NUMBER`, needs *Allow placing calls from my
computer*; it places a real call). A PC with no Bluetooth controller still controls calls; the voice stays on the phone.
The firewall rule is TCP and UDP 1716-1764 (step 4).

### T0. The phone does not show up in `adb devices`

| `adb devices` says | Cause and fix |
| --- | --- |
| (empty list) | Charge-only cable, or phone USB mode *Charging only*: use the phone's own cable, pull down the USB notification and pick **File transfer**. USB debugging off: Settings > System > Developer options. |
| `unauthorized` | The "Allow USB debugging?" prompt is waiting on the phone (screen on, unlocked). Tick **Always allow**. Still stuck: Developer options > *Revoke USB debugging authorizations*, replug. |
| `offline` | `adb kill-server`, replug; or toggle USB debugging. |
| `no permissions` | Missing udev rules: `sudo pacman -S android-udev`, then replug (and `sudo udevadm control --reload`). |
| `device` | Good. Several phones attached: `$S --serial SERIAL ...`. |

Wireless instead of a cable (Android 11+): Developer options > **Wireless debugging** > *Pair device with pairing code*,
then `adb pair PHONE_IP:PAIR_PORT` and `adb connect PHONE_IP:CONNECT_PORT`, and use `$S --serial PHONE_IP:CONNECT_PORT ...`.
This is also how you measure battery drain (the USB cable charges the phone, see step 2 of "Wrap up").

Install errors from `$S install`: `INSTALL_FAILED_UPDATE_INCOMPATIBLE` (another signing key is installed: `adb uninstall dev.geekbridge.android`,
which loses the pairing), `INSTALL_FAILED_USER_RESTRICTED` (Xiaomi/Oppo/Vivo: Developer options > *Install via USB* on),
`INSUFFICIENT_STORAGE`. `adb reverse` is not needed: that is for emulators and USB-only setups, the phone reaches the PC over Wi-Fi.

### T1. The phone cannot connect (firewall)

Symptom: `check` says *timed out connecting to IP:1716 ... packets are being DROPPED*, or the QR scan ends with
"no address answered", or the phone's Home keeps saying "Waiting for ...". A DROP (silent) is a firewall or Wi-Fi isolation;
a *refused* answer would mean nothing listens there. Fix: [Firewall on this machine](#firewall-on-this-machine), then
`$S check`. Confirm from the PC side that packets arrive at all: `sudo tcpdump -ni enp5s0 'tcp port 1716 or udp port 1716'`
while pairing (replace `enp5s0` with `ip route show default`'s `dev`): SYNs from the phone's IP and no reply = firewall.
No packets at all = the network between them (T2/T3).

### T2. Phone and PC on different subnets, guest Wi-Fi, client isolation

`check` prints `phone and this PC are on the same subnet` (PASS) or `different subnets` (FAIL), plus whether both use
the same router gateway. Typical causes, in order of likelihood: the phone is on the router's **guest network** or a
separate SSID/VLAN ("IoT", "Kids"); the router has **AP/client isolation** ("Isolate wireless clients") on; a mesh/repeater
puts the phone on another subnet (double NAT); the PC is on Ethernet and the router bridges Wi-Fi and LAN on separate
subnets. What to do: put the phone on the same SSID as the PC; turn off client isolation for that SSID; or reconnect the
repeater in bridge/AP mode. Quick proof from the phone side: `adb shell ping -c3 PC_IP` (the same test `check` runs).
Pairing needs only a routable TCP path phone -> PC (the QR carries the PC's IP); if the phone can reach the PC across
subnets (routed, no isolation, firewall allows that source) it works, but then widen the firewall rule from
`192.168.1.0/24` to the phone's subnet.

### T3. VPN on either side

- **On the phone** (`check` warns *a VPN is active on the phone*): most VPN apps send all traffic, LAN included, through
  the tunnel. Pause it, or enable the app's *Allow LAN / local network access* option. Android's *Always-on VPN* with
  *Block connections without VPN* blocks LAN entirely: turn it off while testing.
- **On the PC** (`check` warns *VPN-style interface(s) up*: `tailscale0`, `wg0`, `tun0`, `zt...`): the QR never contains
  tunnel addresses (they are not on the phone's Wi-Fi), so QR pairing is unaffected unless the VPN is *full-tunnel*: then
  the phone's SYN arrives on the LAN but the reply leaves through the tunnel and is lost. Check with
  `ip route get PHONE_IP` (must say `dev enp5s0`/your LAN interface, not the tunnel); fix by disconnecting the VPN or
  excluding the LAN (`tailscale set --exit-node=` to drop an exit node; WireGuard: keep `AllowedIPs` off your LAN subnet).
- Tailscale on both devices is a different, deliberate setup: use the 100.x addresses by hand with
  `GEEKBRIDGE_QR_ADDRESSES=100.x.y.z` (see T4); the firewall rule must then allow `100.64.0.0/10`.

### T4. Many network interfaces: which address is in the QR?

The QR holds the address(es) the phone dials, in order, each with its own timeout, so the first must be right and wrong
ones must not be there. This PC (verified with `ip -br a`):

```
lo       UNKNOWN  127.0.0.1/8 ::1/128
enp5s0   UP       192.168.1.150/24 fe80::8363:.../64        <- default route, the one
docker0  DOWN     172.17.0.1/16 fe80::58d9:.../64           <- container bridge, never offered
```

Selection rules (crates/bridge-service/src/lan_select.rs, unit-tested against a fake machine with `docker0 172.17.0.1`,
`virbr0 192.168.122.1`, `tailscale0 100.101.x.x`, `wg0`, `lo`, `enp5s0`, `wlan0`, link-local and CGNAT addresses):

1. Never offered: loopback, link-local (`169.254/16`, `fe80::/10`), CGNAT `100.64.0.0/10` (Tailscale and carrier NAT),
   interfaces that are down, container/VM bridges by name (`docker*`, `br-*`, `veth*`, `virbr*`, `vmnet*`, `vboxnet*`,
   `lxc*`, `podman*`, `cni*`) and VPN tunnels by name (`tailscale*`, `wg*`, `tun*`, `tap*`, `zt*`, `ppp*`, `nordlynx*`, ...).
2. Order: private address (`10/8`, `172.16/12`, `192.168/16`) on the **default-route interface** first, then private
   addresses on other interfaces (wired + Wi-Fi: the phone may be on either), then a public address, then Docker's
   default `172.17/16` as a last resort. At most 4 end up in the QR.
3. IPv6: a unique-local address (`fd..`) follows the IPv4 ones; a global one is offered only when the PC has no usable IPv4 at all (see T6).

Check what you got: `$S desktop` prints `the QR makes the phone try (in this order): ...`; `$S check` repeats it under
*the pairing QR will carry*; `geekbridge-ctl pair-qr` prints `Addresses: ...`. If it is wrong (for example the phone is on a
second network the PC is also on), name the addresses yourself, **before the service starts**:

```bash
scripts/run-desktop.sh --stop
GEEKBRIDGE_QR_ADDRESSES=192.168.1.150 scripts/realdevice.sh desktop      # comma list; ip alone = the service port, ip:port also fine
```

`desktop` warns when the variable is set but the service was already running without it.

### T5. Discovery (mDNS and UDP broadcast) blocked: pair by QR only

Some routers drop broadcast/multicast between Wi-Fi clients (and every guest network does). GeekBridge discovery is UDP
broadcast on port 1716, and KDE Connect's is the same; mDNS (`5353`) is not used for pairing. Symptoms: the phone's
list of computers stays empty, Home says "Waiting for ...", `geekbridge-ctl test-connection` says *No announcement
from a phone yet*. **QR pairing does not need discovery**: the QR carries the PC's IP, certificate fingerprint and a
one-time token, so it works over any routable path. Reconnecting after a **PC restart** relies on discovery
(that is the broadcast this section is about; the emulator cannot show it, step 11 on the real phone does): if the phone
stays on "Waiting for ..." with broadcasts blocked, scan a fresh QR (`$S desktop`), and give the PC a DHCP reservation on
the router so its IP stops changing. To confirm broadcasts are really blocked: on the PC `sudo tcpdump -ni enp5s0 udp port 1716` with the phone's app
open: lines from the phone's IP mean broadcasts do arrive (then it is the firewall's UDP rule, T1).

### T6. IPv6-only, dual-stack and link-local addresses

The service listens on `0.0.0.0:1716` (IPv4) **and** on `[::]:1716` (IPv6 only, a separate socket, so IPv4 phones still
arrive as plain IPv4 addresses). The QR is IPv4 first. Addresses starting `fe80::` (link-local) are never put in it
because they need a zone (`%enp5s0`) the phone cannot guess (the phone's address field takes no scope id either). A ULA
(`fd..`) address trails the IPv4 ones, so a phone whose Wi-Fi has no IPv4 can still dial; a global IPv6 address is
included only when this PC has no usable IPv4. **Limit:** the transport's address policy accepts only private,
link-local, CGNAT and ULA sources, so a network that hands out *only* global (`2001:..`) IPv6 addresses is refused with
"address is not on a permitted local network"; give the router a ULA prefix or use IPv4. Discovery is IPv4 UDP broadcast
only (no IPv6 multicast), so on an IPv6-only network the phone never appears in the list: pair by QR. If a mobile-data
connection is on, and the Wi-Fi shows "connected, no internet", Android may route app traffic over mobile data and the LAN
address will not answer: `check` warns about this ("Wi-Fi is connected, no internet"); fix the router's uplink or switch
mobile data off for the test. Reproduce in a sandbox: `scripts/netns-hostility.sh ipv6`.

### T7. Hostname resolution and Android Private DNS

Nothing in QR pairing uses a hostname: the QR holds IP addresses, and the phone's dialler accepts IP literals only (no DNS). Names matter only if you try `ping omarchy.local`
or add a computer by name. Android's **Private DNS** (Settings > Network > Private DNS) set to a provider hostname sends
all DNS to that provider, so `*.local` and router-local names stop resolving on the phone; `Automatic`/`Off` is fine for
GeekBridge. `check` reports it as information. On the PC, `getent hosts $(hostname)` shows what the PC calls itself; the name
phones see in the app is `GeekBridge-<hostname>` (`scripts/run-desktop.sh --name NAME` to change).

### T8. Wi-Fi power saving, Doze and OEM battery managers

The link survives only if the phone keeps its Wi-Fi radio and the app's service alive with the screen off.

- **Phone Wi-Fi power saving / Battery Saver**: Battery Saver (`check` warns when it is on) throttles background network.
  Turn it off for the test, and for the real thing exempt GeekBridge. Some phones also have *Wi-Fi power saving* or
  *Adaptive Wi-Fi* in Developer options / Wi-Fi advanced settings: turn it off, then compare.
- **PC Wi-Fi power saving** (only if the PC is on Wi-Fi; this one is wired): `iw dev wlan0 get power_save`;
  to turn off until reboot `sudo iw dev wlan0 set power_save off`; permanently with NetworkManager:
  `nmcli connection modify "<ssid>" 802-11-wireless.powersave 2` (2 = disable; syntax only, not run here).
- **Doze** (phone idle, screen off, unplugged, about 30+ minutes): network is suspended for apps that are not exempt. The wizard's
  **Allow unrestricted battery** is the exemption; verify with `$S logs` -> `dumpsys-deviceidle.txt` (package under whitelist).
  Test Doze on purpose: `adb shell dumpsys deviceidle force-idle`, watch `geekbridge-ctl status`, then
  `adb shell dumpsys deviceidle unforce`.
- **OEM battery managers** (Xiaomi, Samsung, OnePlus/OPPO, Huawei/Honor, Vivo) kill apps the stock exemption does not protect.
  `$S check` prints the vendor-specific step for the phone it sees ("battery wizard to expect"). Signs of an OEM kill: `$S logs`
  shows `am_proc_died`/`Killing` for the package, and `geekbridge-ctl events 0` a `link_lost` that never recovers until you open
  the app. Fix: do the vendor step (autostart on, battery "no restrictions", lock the app in Recents), then repeat step 10.

### T9. Build and tool problems

- `error: 'cargo-fmt' is not installed` or binaries missing from `target/release`: you are in a shell that sourced
  `scripts/android-env.sh`. Open a fresh terminal (or `env -u CARGO_TARGET_DIR cargo build --release --locked -p bridge-apps -p bridge-service`).
- `check` warns *desktop sources are newer than target/release/geekbridge-service*: rebuild, then `scripts/run-desktop.sh --stop`
  and `$S desktop`.
- `warn no WAYLAND_DISPLAY/DISPLAY`: you are not in a Hyprland terminal, the windows and clipboard cannot start; the service
  alone still works (use `--no-ui` and the terminal QR).
- `qrencode is not installed`: `sudo pacman -S qrencode`, or scan the QR in the GeekBridge window instead.
- `service did not come up`: `tail dist/realdevice/service.log`; port 17381 busy = another session's service (`GEEKBRIDGE_CONTROL_PORT=<other>`).
