# Call-audio backend: adversarial review against real BlueZ, PipeWire and WirePlumber

Date: 2026-11-04. Scope: `crates/bridge-calls` (`setup.rs` pairing agent and setup, `hear.rs` checklist and
WirePlumber fix, `audio/controller.rs` routing/mute/silence checks, `pipewire.rs` telephony calls),
`crates/bridge-service/src/call_audio_host.rs` (setup IPC, fix-config, reconnect), `crates/bridge-core/src/call_audio_flow.rs`
(the one-button flow). Before this review the backend had only been run against `scripts/e2e/call-audio/fake_bluez.py`
and a private PipeWire. The owner tests a real USB dongle and a Galaxy Z Fold5 next.

Method: each assumption the code makes about the real stack was checked against the source of the versions on the
owner's machine (BlueZ 5.87, PipeWire 1.6.9, WirePlumber 0.5.18; `pacman -Q`), not against memory. Sources read:

* BlueZ 5.87 (`git.kernel.org/.../bluez.git/plain/...?h=5.87`): `src/agent.c` (`REQUEST_TIMEOUT (60 * 1000)`,
  `agent_get()`: the caller's own agent first, then the head of `default_agents`, with fallback to the next default
  agent on unregister), `src/device.c` `pair_device()` (`InProgress` while `device->bonding || device->connect`),
  `doc/org.bluez.Agent.rst` (RequestConfirmation, RequestAuthorization = incoming just-works pairing, Cancel),
  `doc/org.bluez.Device.rst` (Pair errors, `ConnectProfile` takes the *remote* UUID, `Connect` connects every
  auto-connectable profile), `doc/org.bluez.Adapter.rst` (`SetDiscoveryFilter` `Transport` auto/bredr/le).
* PipeWire 1.6.9 `spa/plugins/bluez5`: `backend-native.c` (telephony AG is created when the HFP **hands-free** SLC
  completes, `hfp_hf_cmer`/`hfp_hf_chld`; `Activate` = `AT+BCC` or a CVSD SCO connect; mSBC needs USB alt 1 or 6 and
  an eSCO-capable adapter, logged as `bluez-monitor/hardware.conf: alt6:%d alt1/5:%d`, `no msbc/esco transport`,
  `does not support USB ALT6`), `media-source.c`/`media-sink.c` (with a phone as AG the SCO source is a
  `Stream/Output/Audio` playback stream and the SCO sink a `Stream/Input/Audio` capture stream, unless
  `bluez5.media-source-role = input`), `bluez5-device.c` (card profile `audio-gateway` = A2DP source + HFP/HSP AG,
  SCO nodes are dynamic: they exist only while the voice link is pending/active), `defs.h` (`api.bluez5.profile` =
  `headset-audio-gateway`), `bluez5-dbus.c` (`bluez5.roles` and `bluez5.auto-connect` parsing; auto-connect names
  the **remote** profile, default empty), `telephony.c` (`/org/pipewire/Telephony/ag%d`, `AudioGatewayTransport1`
  `State` s = error|idle|pending|active, `Codec` y, `RejectSCO` b default false unless
  `bluez5.telephony.default-reject-sco`), `quirks.c` (`bluez5.enable-msbc` forces mSBC on or off).
* WirePlumber 0.5.18 as installed: `/usr/share/wireplumber/wireplumber.conf` (no default `bluez5.auto-connect`;
  `bluetooth.autoswitch-to-headset-profile` default true), `scripts/monitors/bluez/name-node.lua` (node names
  `bluez_input.<addr>.<id>` / `bluez_output.<addr>.<id>`, `node.autoconnect = true` for `headset-audio-gateway`),
  `scripts/device/state-profile.lua` (profiles set by a client are saved; `off` is restorable).
* `spa-json-dump` (PipeWire 1.x): exit 0 on valid SPA-JSON, 1 with `syntax error in file ...` otherwise (tried in
  the sandbox on good and broken fragments).

Nothing here touched the live Bluetooth stack or the session's audio configuration (this PC has no adapter). All
tests ran against the fake BlueZ on private buses and the private PipeWire sandbox; TMPDIR was `~/.cache/gb-tmp`.

## Findings, ranked

| # | Severity | Finding | Status |
|---|----------|---------|--------|
| 1 | Blocker | The phone is looked for by the GeekBridge name, which Android sets to `Build.MODEL` (`SM-F946B` on a Fold5), while its Bluetooth name is `Galaxy Z Fold5`. No device ever matched; even when the user paired from the phone the setup ran into its 150 s timeout and reported failure although the phone was paired. | Fixed |
| 2 | High | Trust was set only after the agent was unregistered. Right after pairing Android connects its profiles; BlueZ asks the agent to authorize each connection of an untrusted device, so with no other agent the phone's own HFP/A2DP connections were refused during that window. | Fixed |
| 3 | High | `RequestConfirmation` blocked the agent connection's only executor thread for up to 60 s, so BlueZ's `Agent1.Cancel()` (phone cancelled, BlueZ timeout) could not be delivered: the code stayed on screen and the UI stayed in "confirm". | Fixed |
| 4 | High | Cancel during `Pair()` was ignored until `Pair()` returned (up to the 90 s call timeout). | Fixed (`CancelPairing`) |
| 5 | High | `fix-config` wrote the WirePlumber fragment and restarted WirePlumber with no validation and no way back: a fragment WirePlumber cannot parse stops it from starting, which takes all the user's sound. | Fixed (validated twice, verified after restart, rolled back automatically) |
| 6 | Medium | `RequestAuthorization` (incoming just-works pairing) was auto-accepted while the computer was discoverable: anything nearby could pair during the 2 minutes. | Fixed (asked like a code) |
| 7 | Medium | Discovery ran on both transports, so a phone's LE side (random address, often the same name) appeared as a second device that the setup could pick and try to pair; HFP is Classic only. | Fixed (`SetDiscoveryFilter Transport=bredr`, random addresses never paired from here) |
| 8 | Medium | `Pair()` was sent from a connection without an agent; BlueZ then uses whatever default agent is first (a desktop's own pairing dialog if `RequestDefaultAgent` lost). | Fixed (sent from the agent's own connection) |
| 9 | Medium | The checklist accepted `bluez5.roles` with only `hsp_hs`; PipeWire's telephony service exists only for HFP. | Fixed |
| 10 | Medium | "No sound from the phone (digital silence)" was reported from the first level check, 3 s after the call went live, which includes dialling before the ringback: a false alarm on most outgoing calls. | Fixed (reported from the second check, 8 s) |
| 11 | Low | The adapter's `Pairable` and `DiscoverableTimeout` were changed for good (BlueZ stores them per adapter); `Discoverable` was forced off even if the user had it on. | Fixed (put back as found) |
| 12 | Low | Multiple adapters: the first by path was used even if unpowered, devices of other adapters were considered, an unplugged adapter mid-setup ran on to a misleading timeout. | Fixed |
| 13 | Low | `Connect()` before `ConnectProfile(HFP)` also pulls the phone's media audio (A2DP) to the computer. | Fixed (HFP first; `Connect()` only as fallback). See open item B for what Android still does |
| 14 | Low | Confirm timeout (60 s) equalled BlueZ's own `REQUEST_TIMEOUT`, so BlueZ's cancel raced GeekBridge's answer. | Fixed (55 s; BlueZ's 60 s is the hard limit for any agent) |
| 15 | Low | `call-audio-sandbox.sh` stage 2 ignored `CARGO_TARGET_DIR`. | Fixed |

Verified correct (no change): Agent1 method names and signatures (zbus maps `request_pin_code` to `RequestPinCode`
etc.; errors are `org.bluez.Error.Rejected`/`Canceled`); capability `DisplayYesNo` (numeric comparison with an
Android phone); `ConnectProfile("0000111f-...")` (the remote AG UUID, per Device.rst); `RejectSCO` set through
`Properties.Set(ssv)` and `Activate()` on `org.pipewire.Telephony.AudioGatewayTransport1`; call streams found by
properties (`api.bluez5.address`, `api.bluez5.profile = headset-audio-gateway`, media class) instead of node names, so
the `bluez_input.`/`bluez_output.` naming does not matter; `pactl move-sink-input`/`move-source-output` (pipewire-pulse
turns them into `target.object` metadata, as `wpctl`/`pw-metadata` would); `bluez5.auto-connect` needs `hfp_ag` (remote
profile) to reconnect a phone's calls; WP 0.5 fragments merge objects and append arrays, so a `zz-` file's rule runs last.

### Fix details

* **1, 8, 12:** `setup::run` takes a phone that pairs from its own side during the setup whatever its name (the user
  compared the six digits, which is what binds it), and an already paired phone that the user connects from the
  phone during the setup. An already paired phone by another name is *not* guessed at (it may be the other phone).
  The first step line now names the adapter and the name phones see (`phones list this computer as "omarchy"`), and
  the instruction says to tap that name; after 20 s with no name match it lists the phones it can see. Pair runs on
  the agent's connection. A powered adapter is preferred; only its devices are used; its disappearance ends the setup
  with "the Bluetooth adapter went away".
* **2, 11:** trusted first, then discovery stopped, `Discoverable`, `DiscoverableTimeout`, `Pairable` put back, agent
  unregistered. A crash still leaves the computer visible for at most the 120 s discoverable timeout.
* **3, 4, 6, 14:** the agent methods are `async` and poll the answer every 50 ms, so `Cancel()` arrives while a code
  is shown; a new `setup::Event::Withdrawn` takes the code down (the service switches the setup back to "working" with
  "The pairing code expired or the phone cancelled it"). Cancel sends `Device1.CancelPairing`.
  `RequestAuthorization` becomes a `Confirm` with an empty passkey ("Allow X to pair with this computer?").
  Pair errors are put in words with BlueZ's own error kept: `AuthenticationCanceled/Timeout`, `Rejected`, `Failed`
  ("remove this computer from the phone's list and try again": the usual stale-bond case), page timeout.
  `ConnectProfile` errors: `profile-unavailable`/`NotAvailable` = no HFP-HF profile registered (WirePlumber).
* **5:** `hear::write_wp_dropin` checks the text with `hear::check_spa_json` (a small SPA-JSON checker that also
  accepts the shipped `/usr/share/wireplumber/wireplumber.conf`) and, when installed, with PipeWire's own
  `spa-json-dump` on the temporary file (named `.conf.tmp`, which WirePlumber never loads). `hear::apply_wp_dropin`
  then restarts WirePlumber and checks it is still `active` 3 s later with an unchanged `NRestarts`; otherwise it
  takes the fragment back (the previous GeekBridge copy from `.bak`, or no file) and restarts again. A stale `.bak`
  is removed when there is no current copy so an undo never resurrects old content. File:
  `~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf` (`$XDG_CONFIG_HOME` honoured), backup
  `zz-geekbridge-call-audio.conf.bak`. Undo by hand: `rm ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf && systemctl --user restart wireplumber`.

### Tests (failing first, then fixed)

* `crates/bridge-calls/tests/setup_fake_bluez.rs` (new; runs in plain `cargo test`, each test starts its own private
  `dbus-daemon` + fake BlueZ, skips with a note without python-dbus): phone pairs from its side under another name;
  pairing from here uses its own agent, filters BR/EDR, never touches the LE twin, trusts before unregistering, restores
  `Pairable=false`/`DiscoverableTimeout=180`; the phone cancelling withdraws the code within seconds; user cancel sends
  `CancelPairing` and returns in < 10 s; an already paired phone that connects from its side is taken without pairing;
  a phone connected under another name is not taken for the wanted one. Against the code before this review all five
  original tests failed (two hung for 60 s on the blocked agent).
* `fake_bluez.py` now models the bluetoothd behaviour above (`Calls()` log, own-vs-default agent, `CancelPairing`,
  `Agent1.Cancel`, `SetDiscoveryFilter` with an LE twin, phone-initiated pairing/connecting, adapter mode restore).
* Unit tests: `setup` (name matching, LE twin, adapter preference, error wording), `hear` (`hsp_hs`-only roles fail,
  every fragment GeekBridge can write parses, broken fragments are refused with a line number, rollback restores the
  previous copy or removes the file, nothing temporary left in `wireplumber.conf.d`), `controller` (silence in the first
  seconds is not reported; silence that lasts is).
* `scripts/e2e/call-audio-sandbox.sh`: stage 1 (6 tests) and stage 2 (25 service checks) pass.

## Open items (cannot be settled without the hardware; each has a diagnostic)

A. **SCO over the USB dongle** (most likely real failure). Known classes: cheap "CSR 8510" clones (`0a12:0001`, often
   not CSR) with broken or absent SCO; Realtek RTL8761B/BU (`0bda:8771`, `2550:8761`) need `rtl8761bu_fw.bin`
   (linux-firmware) and support mSBC only with the kernel's alt-setting quirks; Intel AX200/AX210 (`8087:0029`,
   `8087:0032`, internal USB) work with mSBC on 6.x. PipeWire disables mSBC by itself when the adapter lacks eSCO or USB
   alt 1/6 (log lines above, info level). Symptom: call connects, `State` goes `active`, streams exist, the level check
   says digital silence. Next step: `bluez5.enable-msbc = false` (narrowband CVSD; exact drop-in in [call-audio-troubleshooting.md](../call-audio-troubleshooting.md#wideband-off-msbc-for-a-silent-or-choppy-call)), then a different dongle. Diagnose
   with `scripts/realdevice-report.sh` (USB id, driver, btusb parameters, the alt6/alt1 lines when WirePlumber logs at
   info: `systemctl --user set-environment WIREPLUMBER_DEBUG='spa.bluez5*:4' && systemctl --user restart wireplumber`,
   undo with `systemctl --user unset-environment WIREPLUMBER_DEBUG` and a restart).
B. **Android also connects media (A2DP) to the computer** after HFP is up (its own "connect other profiles" policy), so
   phone media and notification sounds may play on the PC. Not a GeekBridge action; turn off "Media audio" for the
   computer in the phone's Bluetooth device settings if unwanted.
C. **Samsung "Calls" toggle** (Settings > Connections > Bluetooth > gear next to the computer). If off, `ConnectProfile`
   fails (expect `org.bluez.Error.Failed` / `br-connection-refused` or `profile-unavailable`) and no `ag` object appears.
   The error line already says where the toggle is.
D. **A Bluetooth headset as the call's output/input** would need a second SCO link on the same dongle while the phone's
   is open; WirePlumber's `bluetooth.autoswitch-to-headset-profile` (default on) switches the headset to HFP as soon as
   the call's capture stream links to its microphone. Many dongles carry one SCO link only. Use a wired/USB headset for
   tomorrow's test.
E. **fix-config drops the phone's HFP link** (restarting WirePlumber unregisters the HF profile). GeekBridge's
   reconnect (and the phone) bring it back; the one-button flow allows 20 s (`LINK_GRACE_MS`). If that proves short,
   raise it.
F. **The window's confirm screen with an empty code** (the rare `RequestAuthorization` case) shows the device and no
   digits; the service message reads "Allow X to pair...". Wording in `bridge-apps` not changed.
G. **Muting** mutes the real microphone device (restored at call end, at shutdown, and after a crash from the restore
   file). Muting only the call's capture stream (`pactl set-source-output-mute`) would leave other apps alone and need
   no restore; a design choice, not a defect.
H. **The phone's pairing dialog** may time out sooner than BlueZ's 60 s; then expect `Withdrawn` followed by
   "Pairing ... failed: the code was not confirmed in time (org.bluez.Error.AuthenticationCanceled...)".

## Expected outputs tomorrow (what "working" looks like)

Read-only commands; `XX_XX...` is the Fold's address with underscores. `bluetoothctl` lines abbreviated.

1. Dongle plugged in, before the setup:

   ```
   $ ls /sys/class/bluetooth
   hci0
   $ rfkill list bluetooth            -> Soft blocked: no / Hard blocked: no
   $ bluetoothctl show
   Controller AA:BB:CC:DD:EE:FF (public)
   	Alias: omarchy
   	Powered: yes
   	Pairable: yes
   	Discoverable: no
   	DiscoverableTimeout: 0x000000b4 (180)
   	UUID: Handsfree                 (0000111e-0000-1000-8000-00805f9b34fb)   <- PipeWire's HFP hands-free is registered
   	UUID: Audio Sink                (0000110b-0000-1000-8000-00805f9b34fb)
   $ busctl --system get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 Powered
   b true
   $ busctl --user status org.pipewire.Telephony | head -3   -> a PID/unique name (service present)
   ```

   No `Handsfree (0000111e...)` UUID = WirePlumber did not register HFP-HF: `geekbridge-ctl calling` names the
   setting; `journalctl --user -u wireplumber -n 200`.

2. During the setup (first 120 s): the service log/UI lines are, in order: `Using Bluetooth adapter hci0 (...);
   phones list this computer as "omarchy"`, `This computer is visible as a Bluetooth device for 120 s`, `Looking for
   "SM-F946B". ... tap "omarchy" when it shows up`, then either `Found ...; pairing` (name matched) or, after the user
   taps "omarchy" on the phone, the code `Does Galaxy Z Fold5 show NNNNNN?`.

   ```
   $ busctl --system get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 Discoverable DiscoverableTimeout Pairable
   b true
   u 120
   b true
   ```

3. After the setup:

   ```
   $ busctl --system get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 Discoverable DiscoverableTimeout Pairable
   b false
   u 180            <- whatever it was before
   b true           <- whatever it was before
   $ bluetoothctl info <FOLD-ADDRESS>
   Device <FOLD-ADDRESS> (public)
   	Name: Galaxy Z Fold5
   	Icon: phone
   	Paired: yes
   	Bonded: yes
   	Trusted: yes
   	Connected: yes
   	UUID: Handsfree Audio Gateway   (0000111f-0000-1000-8000-00805f9b34fb)
   	UUID: Audio Source              (0000110a-0000-1000-8000-00805f9b34fb)
   $ busctl --system get-property org.bluez /org/bluez/hci0/dev_XX_XX_XX_XX_XX_XX org.bluez.Device1 Paired Trusted Connected ServicesResolved
   b true
   b true
   b true
   b true
   $ busctl --user tree org.pipewire.Telephony
   └─ /org/pipewire/Telephony
     └─ /org/pipewire/Telephony/ag1          <- the number varies; one ag per connected phone
   $ busctl --user get-property org.pipewire.Telephony /org/pipewire/Telephony/ag1 org.pipewire.Telephony.AudioGatewayTransport1 State Codec RejectSCO
   s "idle"
   y 2              <- the HFP codec id once negotiated: 1 = CVSD, 2 = mSBC
   b false
   $ pactl list cards short | grep bluez      -> one line with bluez_card.XX_XX_XX_XX_XX_XX
   $ pactl list cards | grep -A1 'Name: bluez_card' ; pactl list cards | grep 'Active Profile'   -> audio-gateway
   $ geekbridge-ctl calling   -> every step ok; the setup remembered the phone in <state dir>/call-audio-phone.json
   ```

4. During a call answered on the computer:

   ```
   $ busctl --user tree org.pipewire.Telephony        -> .../ag1/call1 appears
   $ busctl --user get-property org.pipewire.Telephony /org/pipewire/Telephony/ag1 org.pipewire.Telephony.AudioGatewayTransport1 State Codec
   s "active"
   y 2              <- mSBC (wideband); 1 = CVSD (narrowband, also fine)
   $ pactl list sink-inputs | grep -E 'Sink Input #|api.bluez5.profile|api.bluez5.codec|node.name|Sink:'
   Sink Input #NN
   	Sink: <the picked output>
   		node.name = "bluez_input.XX:XX:XX:XX:XX:XX.0"     (exact form may differ; GeekBridge does not rely on it)
   		api.bluez5.profile = "headset-audio-gateway"
   		api.bluez5.codec = "msbc"
   $ pactl list source-outputs | grep -E 'Source Output #|api.bluez5.profile|Source:'   -> the picked microphone
   $ geekbridge-ctl calls      -> "Call audio: on this computer (speaker: ...; microphone: ...; codec mSBC, wideband 16 kHz)."
   ```

   `State` stays `idle` while the call is live = the phone did not open the voice link (open item C, or the phone's
   call screen has the audio on the phone; GeekBridge sends `Activate` twice). `active` but "digital silence" from
   8 s on = open item A.

5. Collect everything in one go if anything differs: `scripts/realdevice-report.sh --bt-trace 60` and reproduce within
   the 60 s (pairing or a call). It adds the BlueZ objects (`busctl --system tree/introspect org.bluez`), the dongle's
   USB id/driver/btusb parameters, `bluetoothctl --version`, `wpctl status`, PipeWire's bluez nodes from `pw-dump`,
   `journalctl -u bluetooth.service -n 200`, `journalctl --user -u wireplumber -n 200`, the SCO/mSBC log lines, a
   session-bus trace of `org.pipewire.Telephony`, and with passwordless sudo a `dbus-monitor --system` trace of
   `org.bluez` plus a `btmon -w` capture (kept outside the redacted report; share privately). Without sudo it prints
   the two commands to run by hand.
