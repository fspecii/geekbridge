# Call audio on the computer's headset (Linux, Bluetooth hands-free)

Date: 2026-11-04. Goal (owner): a call answered on the computer is heard and spoken through the computer's own headphones and microphone, like Phone Link and Continuity. Constraint: Android does not let apps capture or inject call audio, so the path is Bluetooth Hands-Free (HFP), with the phone as the audio gateway and the computer as the hands-free unit. The owner's PC has no Bluetooth controller yet; a USB dongle arrives tomorrow. This report covers what was found, what was built, how it was tested without hardware, and the exact test for tomorrow.

## 1. Findings that changed the design

Checked against the PipeWire 1.6.9 sources (`spa/plugins/bluez5/bluez5-device.c`, `media-source.c`, `media-sink.c`, `backend-native.c`, `telephony.c`), the installed WirePlumber 0.5.18 scripts, and then confirmed by running PipeWire's real bluez5 plugin against a fake phone (section 4).

1. **There is no `headset-head-unit` profile for a phone.** Profile names describe the *remote* device. A phone's card `bluez_card.<ADDR>` has the profile **`audio-gateway`** ("Audio Gateway (A2DP Source & HSP/HFP AG)", priority 256, the only one besides `off`). `headset-head-unit` is what a *headset* gets, where the computer is the gateway. The repository's earlier route planner (`audio/plan.rs`, `manager.rs`) assumed the opposite: it treated `audio-gateway` as "wrong role" and would have refused every real phone. It was never wired into the service. It is replaced (section 2).
2. **The call is two streams, not a source and a sink.** While the phone's voice link (SCO) is open, PipeWire adds *dynamic* nodes: the caller's voice as `Stream/Output/Audio` (a pulse sink input, `bluez_input.<ADDR>.<n>`) and the uplink as `Stream/Input/Audio` (a pulse source output, `bluez_output.<ADDR>.<n>`), with `api.bluez5.profile = headset-audio-gateway` and `api.bluez5.codec = cvsd|msbc|lc3_swb`. WirePlumber's `name-node.lua` sets `node.autoconnect = true` on them, so **they are linked to the default speakers and microphone automatically**. A `module-loopback`/`pw-loopback` design would have doubled the audio (both WirePlumber's link and the loopback). The correct routing is to *move* the two streams to the picked devices (`move-sink-input` / `move-source-output`, i.e. `target.object`). Resampling (8 kHz CVSD, 16 kHz mSBC, 32 kHz LC3-SWB to the device's 48 kHz) and mono-to-stereo are PipeWire's adapters' job; nothing to configure. Latency is the bluez node's own; no loopback buffer is added.
3. **No profile switch is needed for a call.** `audio-gateway` carries music (A2DP from the phone) and calls together. The only profile action is: if the card is `off` (a user once switched it off and WirePlumber restored that), switch it to `audio-gateway` for a call answered here and back afterwards.
4. **`RejectSCO` is the "don't steal" switch.** `AudioGatewayTransport1.RejectSCO=true` makes PipeWire refuse the voice link the phone opens (`backend-native.c::sco_listen_event`), so the audio stays on the phone. Android routes a call to a connected hands-free device even when answered on the phone; refusing the link at that moment keeps it on the phone.
5. **PipeWire creates the card only after the gateway picks a codec** (`+BCS`), or when A2DP connects. The telephony object (`/org/pipewire/Telephony/agN`) exists as soon as the service-level connection is up.
6. **The owner's own WirePlumber fragment blocks hands-free reconnects.** `~/.config/wireplumber/wireplumber.conf.d/bluetooth-a2dp-autoconnect.conf` sets `bluez5.auto-connect = [ a2dp_sink a2dp_source ]` for every `bluez_card.*`. `auto-connect` names the *remote* profiles to connect; a phone's calls profile is `hfp_ag`, which is not in the list, so PipeWire never asks BlueZ for it. Unless the phone connects hands-free by itself, only music comes back after a reconnect. The checklist flags it and `geekbridge-ctl call setup --fix-config` fixes it without touching the owner's file (section 3.4).
7. **Defaults are otherwise right on Arch/Omarchy today** (pipewire 1.6.9, wireplumber 0.5.18, bluez 5.87): the native backend's default roles are `hfp_hf` + `hfp_ag`, `bluez5.hfphsp-backend` defaults to `native`, and `org.pipewire.Telephony` is on the session bus by default (owned by `wireplumber`, also on this machine now, with no adapter). oFono and hsphfpd are selectable backends but GeekBridge needs the native one (its telephony service does not exist with the others).
8. **`hci_vhci` / `btvirt` are not usable here:** `/dev/vhci` is `crw------- root root`. A virtual controller needs root; the sandbox below does without it.

## 2. What was built

All Linux code is in `crates/bridge-calls` (data, decisions, BlueZ/telephony adapters, the `pactl`/`parec` sound-server adapter) and `crates/bridge-service` (`call_audio_host.rs`, the Linux host in `call_control.rs`, `doctor.rs`, the CLI).

* **Call-audio controller** (`bridge_calls::audio::controller::CallAudio`), run every poll (250 ms) on the Bluetooth thread:
  * *Answered or dialled here* (`call answer`, the Calls window, the call card, `call dial` over Bluetooth): `RejectSCO=false` is sent before the answer goes out; the card is switched to `audio-gateway` if it is `off`; if the phone has not opened the voice link 1.5 s after the call is live, GeekBridge asks for it (`Activate`, HFP `AT+BCC`), at most twice; the two streams are moved to the picked output and microphone.
  * *Answered on the phone*: while it rings and for 5 s after it goes live, `RejectSCO=true` (the audio stays on the phone); then the flag is handed back, so picking Bluetooth in the phone's call screen later still works, and that audio is routed like any other.
  * *`call audio phone`*: `RejectSCO=true` and the card `off` for the rest of the call, which closes the voice link; the phone takes the audio back. `call audio` (or `call audio computer`) brings it back here.
  * *Mute*: `call mute on|off` mutes the **real microphone** the call uses (pulse source mute), shows the state (`microphone: muted`) and undoes it at the end of the call. A microphone muted elsewhere (headset switch, mixer) is reported as a problem.
  * *Checks within 3 s* (and once more at 8 s): voice link open, streams linked, microphone not muted, and a level measurement on both the caller's stream and the microphone (digital silence is reported). Every problem is a line with a fix in `geekbridge-ctl calls`, the Calls window (`audio_note`) and `call audio-devices`.
  * *Undo*: card profile, microphone mute and `RejectSCO` are written to `<state>/call-audio-restore.json` **before** each change and undone at the end of the call, at service shutdown, after a phone leaves mid-call, and at the next start after a crash. A change someone else made since is left alone.
* **Device choice**: `<state>/call-audio.json` (`null` = follow the system default). `geekbridge-ctl call audio-devices [--output NAME|default] [--input NAME|default]`; IPC `CallAudioDevices`. An unplugged pick falls back to the default and says so; a typo is refused. A new pick moves a live call at the next poll.
* **Guided setup** (`bridge_calls::setup`, BlueZ D-Bus only, no sudo, no `bluetoothctl`): power on, pairable + discoverable for 120 s, our own pairing agent (`DisplayYesNo`) that shows the six-digit code to confirm here and on the phone, find the phone by name (default: the connected GeekBridge phone's name), pair, `Trusted=true`, connect, connect the hands-free profile, invisible again. The phone is remembered in `<state>/call-audio-phone.json`. `geekbridge-ctl call setup [--phone NAME]` (interactive), `--confirm yes|no`, `--cancel`, `--fix-config`. IPC `CallAudioPair { action }` / `HearCalls` for the Settings UI. Matching a Bluetooth device to a GeekBridge phone is by name plus the code compared on both screens: Android hides its own Bluetooth address from apps (constant `02:00:00:00:00:00` since Android 6), so the phone cannot simply send it.
* **Automatic reconnect**: when the remembered phone is not connected for calls, its hands-free profile is connected when its GeekBridge link comes up, and otherwise retried after 1 minute, backing off to 10 minutes (only with an adapter present).
* **Hotplug**: the adapter appearing (`/sys/class/bluetooth`) while the service runs gives a desktop notification "Bluetooth adapter found: set up call audio ..." and `HearCalls.offer_setup = true` for the UI.
* **Checklist "Hear calls on this computer"** (`bridge_calls::hear`): `geekbridge-ctl calling` prints it with a fix per step; `geekbridge-ctl doctor` has a "Hear calls here" line; IPC `HearCalls` gives the UI a stable struct (`ready`, `no_bluetooth`, `headline`, `steps[{id,title,state,detail,fix}]`, `setup`, `offer_setup`). Steps: adapter, rfkill, `bluetooth.service`, adapter power, WirePlumber role/backend, telephony service, phone paired+trusted, connected, hands-free connected (and `auto-connect` without `hfp_ag`), the phone's card in `audio-gateway`, devices for calls, voice link open. Without the service, `ctl` checks what needs no D-Bus/sound-server client and marks the rest "not checked".
* **No Bluetooth adapter**: the Calls window (via the service's `audio_note`), `geekbridge-ctl calls/calling/doctor` say: *"This computer has no Bluetooth. You can see and answer calls here, but talking needs Bluetooth (add a USB adapter). Messages, contacts and the other features work normally."* (`bridge_core::ipc::NO_BLUETOOTH_WARNING`, `HearCalls.no_bluetooth`). The Home tile and tray tooltip are the UI agent's; they can show the same constant (an i18n key is added with the UI, since the catalogue test requires every key to be used).
* **Telephony**: `Session::set_reject_sco`, `Session::activate_audio`, `Action::SetRejectSco` (a `Properties.Set` on the phone's transport, revalidated like every other action).

## 3. Stack requirements on Arch/Omarchy and how each is detected

| Need | Default today | Detected by | Fix GeekBridge offers |
| --- | --- | --- | --- |
| A Classic (BR/EDR) Bluetooth adapter | none on this PC | `/sys/class/bluetooth/hci*` | buy/plug a dual-mode dongle |
| Not rfkill-blocked | | `/sys/class/rfkill/*/{type,soft,hard}` | `rfkill unblock bluetooth` |
| `bluetooth.service` (bluez, bluez-utils) | active | `systemctl is-active` | `sudo pacman -S --needed bluez bluez-utils && sudo systemctl enable --now bluetooth.service` |
| Adapter powered | | `bluetoothctl show` | setup powers it on over D-Bus |
| WirePlumber running, role `hfp_hf`, backend `native` | yes | `systemctl --user`, WirePlumber files in load order | `call setup --fix-config` |
| `org.pipewire.Telephony` | on | telephony backend | `bluez5.telephony-dbus-service = true` (fix-config) |
| Phone paired + trusted + connected | | `bluetoothctl devices Paired` / `info` | `call setup` |
| Phone's calls profile connected | | an AG object with the phone's address | phone: Bluetooth > this computer > Calls on; `call setup` reconnects; `auto-connect` gets `hfp_ag` |
| Card `bluez_card.<ADDR>` in `audio-gateway` | | `pactl` cards | switched for calls answered here; `pactl set-card-profile ... audio-gateway` |
| Headphones and microphone | HyperX (USB) | `pactl` | `call audio-devices` |

### 3.1 Samsung One UI (Fold5)
Settings > Connections > Bluetooth > gear icon next to the computer: **Calls** must be on (One UI's per-device toggles are "Calls", "Audio", "Contacts and call history"; stock Android names it "Phone calls"). During a call, the call screen's Bluetooth/audio button picks where the audio plays. After pairing, One UI may ask to allow access to contacts and call history; contacts are not needed for audio.

### 3.2 What the HyperX does and does not change
The HyperX Cloud II Core Wireless is a USB audio device (its own 2.4 GHz dongle), not Bluetooth, so it is never confused with the phone, has no echo path (headset), and is today's default output and input; calls use it with no pick at all. The Bluetooth dongle only carries the phone's link.

### 3.3 Sound quality expectations
CVSD is 8 kHz narrowband; mSBC is 16 kHz wideband and needs the adapter to carry transparent SCO over USB. Some USB adapters fail at mSBC (silence or crackle); the 3-second check reports digital silence with the fix: `bluez5.enable-msbc = false` (in GeekBridge's fragment or a user one) and `systemctl --user restart wireplumber`. The phone's own echo cancellation (NREC) stays on.

### 3.4 What `call setup --fix-config` writes
`~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf`, only with the lines needed. On the owner's machine today that is the auto-connect rule, keeping the owner's two values:

```
monitor.bluez.rules = [
  {
    matches = [ { device.name = "~bluez_card.*" } ]
    actions = { update-props = { bluez5.auto-connect = [ a2dp_sink a2dp_source hfp_ag ] } }
  }
]
```

The file sorts after the owner's `bluetooth-a2dp-autoconnect.conf`, so its rule applies last; the owner's file is not edited; an earlier copy of GeekBridge's file is kept as `.bak`; deleting the file undoes it. WirePlumber is then restarted (`systemctl --user restart wireplumber`, a one-second gap in sound).

## 4. Testing without hardware

`scripts/e2e/call-audio-sandbox.sh` (needs pipewire, wireplumber, pipewire-pulse, pw-cat, pactl, dbus-daemon, python-dbus, python-gobject):

* **Private stack**: its own `XDG_RUNTIME_DIR`, `HOME`, config and state dirs (WirePlumber's stored state is private), its own session bus and its own "system" bus (no service activation), `PULSE_SERVER` at the private socket, `TMPDIR` inside the sandbox, every hardware monitor (ALSA, V4L2, libcamera, MIDI) off. Null sinks stand in for the headset and a speaker, a remap of a null sink's monitor for the microphone.
* **Fake BlueZ + fake phone** (`scripts/e2e/call-audio/fake_bluez.py`): `org.bluez` with an adapter, a paired phone and an unpaired one that appears on discovery; `ProfileManager1`, `AgentManager1`, `Media1`; `Pair()` through the registered agent's `RequestConfirmation`; `Connect`/`ConnectProfile`. When PipeWire registers its hands-free profile, the fake hands it one end of a socketpair through `Profile1.NewConnection` and speaks the phone's side of HFP (BRSF, BAC, CIND, CMER, CHLD, CLIP, CCWA, CMEE, VGS/VGM, BCS, RING/CLIP, ATA, CHUP, ATD, BCC). **PipeWire's real bluez5 code** then creates the real `bluez_card.AA_BB_CC_00_11_22` (profile `audio-gateway`) and the real telephony AG and call objects.
* **What is still a stand-in**: the SCO voice link (no kernel controller, so no SCO socket). The two voice streams are played/recorded by `pw-cat` with the properties PipeWire gives the real ones.
* **Stage 1** (`crates/bridge-calls/tests/audio_sandbox.rs`, 6 scenarios): answered here (card off then on, `AT+BCC` sent after 1.5 s, "no voice link" problem after 3 s, streams moved to the pick, level checks clean, real mic muted, re-pick mid-call, everything restored after hang-up); answered on the phone (RejectSCO held 5 s, then handed back; never activated; later "use computer" then "use the phone": card off, RejectSCO on, all restored); silence and an externally muted microphone named within seconds; a crash mid-call undone at the next start; setup pairs, trusts and connects a new phone with the code confirmed; the phone leaving mid-call.
* **Stage 2**: a private `geekbridge-service` driven only through `geekbridge-ctl` (25 checks): `calling` through the service, `doctor`, `call audio-devices` (list, refuse a typo, pick), ring, `call answer`, routed to the pick, `call mute on` (real source muted, shown), `call audio phone` (card off), hang-up (microphone unmuted, card restored, no restore file left), `call setup --phone "Setup Phone"` + `--confirm yes` (phone remembered), `--fix-config` (nothing to change).
* The sound-server adapter runs `pactl -f json`/`pactl move-*`/`set-*` and `parec` (level meter) only while a call is handled, never a shell, with a deadline per run. An in-process libpulse was tried first: it maps libpulse, libpulsecommon, libdbus, libsndfile and libsystemd into the service at start, and CI measured idle RSS 9 692 kB against the 8 000 kB budget (main: ~7 500 kB), so it was replaced. `pactl`/`parec` come with libpulse, which Arch's `pipewire-pulse` depends on.
* Unit tests over an in-memory server (`audio/controller_tests.rs`, route, prefs, model), the checklist over canned facts (`hear_tests.rs`, including the owner's real WirePlumber fragment), the setup matcher, a D-Bus mock test for the RejectSCO write, the IPC listing.

Results today: sandbox stage 1 6/6 and stage 2 25/25 (final runs listed in section 7); workspace `cargo fmt`, `clippy --workspace --all-targets -D warnings` and `cargo test --workspace` green; `scripts/tests/realdevice-kit-test.sh` green. Bugs the sandbox found and fixed: a mute forgotten after "use the phone" (now tracked per call), a card entry stuck in the restore file after the phone left, a restore replay that could undo a live call's changes, `systemctl` failures counted as "not active", the checklist picking the wrong of two phones, and the Calls view not offering mute until the telephony service reported its transport (the voice streams now count as the audio link).

Not verified (needs the dongle): real SCO audio in both directions, mSBC over the owner's adapter, Android's behaviour with `RejectSCO` (expected: the audio stays on the phone), whether the Fold5 connects hands-free by itself after pairing, and the time between answering and audio.

## 5. Hardware test script for tomorrow (owner)

Run as the normal user, in a terminal, with the GeekBridge service running as usual. `gbctl` = `geekbridge-ctl`. Nothing here needs sudo except the two package/service lines in step 1 (only if step 1 says so).

**Step 0, before plugging in (1 minute).**
```
geekbridge-ctl calling
```
Expected: `1. [FAIL] Bluetooth adapter present: no Bluetooth controller ...` and the line *This computer has no Bluetooth. You can see and answer calls here, but talking needs Bluetooth (add a USB adapter). ...*

**Step 1, plug the dongle into a USB port directly (not a hub).**
```
ls /sys/class/bluetooth                 # expect: hci0
journalctl -k -n 50 | grep -i -E 'bluetooth|btusb|firmware'
geekbridge-ctl calling
```
Expected: a desktop notification "Bluetooth adapter found" within a few seconds; `1. [ok] Bluetooth adapter present: controller hci0`, `2. [ok] ... not blocked`, `3. [ok] bluetooth.service running: active`. If 3 fails: `sudo pacman -S --needed bluez bluez-utils && sudo systemctl enable --now bluetooth.service`. If the kernel log says firmware is missing: `sudo pacman -S --needed linux-firmware`, replug. If 2 says soft-blocked: `rfkill unblock bluetooth`.

**Step 2, fix the WirePlumber setting found on this machine.**
```
geekbridge-ctl call setup --fix-config
```
Expected: `Wrote ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf (your own files are unchanged; delete it to undo it). WirePlumber restarted.`

**Step 3, pair (2 minutes).** On the Fold5 open Settings > Connections > Bluetooth and keep that screen open. Then:
```
geekbridge-ctl call setup
```
Expected lines: `This computer is visible as a Bluetooth device for 120 s`, `Looking for "<your phone name>" ...`, `Found ... ; pairing`, then the prompt `Does the phone show 123456? Accept there too. [y/N]`: check the phone shows the same six digits, tap Pair on the phone, type `y`. Then `Paired with ...; marking it trusted`, `Connecting the phone's calls (hands-free) profile`, `... is set up for calls.` If the phone's name differs from the GeekBridge name, add `--phone "Galaxy Z Fold5"` (any part of the Bluetooth name). If the phone asks about contacts/call history, either answer is fine. On the phone, open the computer's gear icon in Bluetooth settings and check **Calls** is on.

**Step 4, the checklist must be all ok.**
```
geekbridge-ctl calling
```
Expected: steps 1-11 `[ok]` (12 says no call right now), `Result: ready`. In particular `9. [ok] Phone's calls (hands-free) profile connected` and `10. [ok] Phone's audio card in PipeWire (audio-gateway): bluez_card.XX_..: audio-gateway`, `11. [ok] ... output: HyperX Cloud II Core Wireless Analog Stereo (system default); microphone: HyperX Cloud II Core Wireless Mono (system default)`.

**Step 5, answer from the computer.** Call the Fold5 from another phone. Answer on the computer (call card, Calls window, or `geekbridge-ctl call answer`). Within 3 seconds:
```
geekbridge-ctl calls
```
Expected: `active | ...`, `source: Call audio: on this computer (speaker: alsa_output.usb-HP__Inc_HyperX...; microphone: alsa_input.usb-HP__Inc_HyperX...; codec mSBC, wideband 16 kHz)` (or CVSD, narrowband 8 kHz), `microphone: on`, and no other `source:` problem lines. Talk both ways: you hear the caller in the HyperX, the caller hears you.

**Step 6, mute, back to the phone, back here.**
```
geekbridge-ctl call mute on      # caller hears nothing; `calls` says microphone: muted; the system's microphone indicator shows it muted
geekbridge-ctl call mute off
geekbridge-ctl call audio phone  # audio moves to the phone's earpiece within ~2 s
geekbridge-ctl call audio        # audio comes back to the HyperX
```
Hang up from either side. Then `pactl get-source-mute @DEFAULT_SOURCE@` says `Mute: no` and `ls ~/.local/share/geekbridge*/call-audio-restore.json` finds nothing (state dir: `geekbridge-service --print-token-path` shows where it is).

**Step 7, answer on the phone (must not steal).** Call again and answer on the phone itself. Expected: audio stays on the phone; `geekbridge-ctl calls` says `Call audio: on the phone.` Then `geekbridge-ctl call audio` moves it to the HyperX.

**Step 8, reconnect.** Turn Bluetooth off on the phone for 30 s and on again (or walk out of range and back). Expected: Android usually reconnects the hands-free profile itself within seconds; if not, GeekBridge does it as soon as the GeekBridge link to the phone is back (otherwise on its retry, after 1 minute, backing off to 10). `geekbridge-ctl calling` step 9 is `[ok]` again without doing anything.

**Optional, another output.** `geekbridge-ctl call audio-devices` lists outputs and microphones; `geekbridge-ctl call audio-devices --output alsa_output.pci-0000_03_00.1.hdmi-stereo` makes calls play on HDMI; `--output default` returns to the system default.

**If anything fails**: run, with the call still live if the problem is about audio,
```
scripts/realdevice-report.sh --state-dir "$(dirname "$(geekbridge-service --print-token-path)")" --no-adb
```
and send the file it names (under `~/.cache/geekbridge-reports/`, redacted: MACs, numbers and tokens are masked; read it first). It now includes a "Bluetooth and call audio" section: `calling`, `calls`, `call audio-devices`, adapter/rfkill/bluetoothctl state, the bluez card and call streams, the WirePlumber bluez settings, and the PipeWire/WirePlumber/kernel Bluetooth log lines. Also say which step failed and what you heard.

## 6. Not done / follow-ups

* Settings UI (picker for output/microphone, "Set up call audio" button showing the code, offer on hotplug, Home tile and tray warning): owned by the Settings checklist work (section 8); the extra data it can use is `Request::HearCalls`, `CallAudioPair`, `CallAudioDevices` and `NO_BLUETOOTH_WARNING`.
* PulseAudio without PipeWire is detected and reported; it routes calls to the defaults itself and GeekBridge does not touch it.
* Echo cancellation for open speakers plus a laptop microphone is not added (headsets have no echo path; the phone's NREC stays on). If needed later: load `module-echo-cancel` and move the two streams onto its nodes.
* Windows and macOS are out of scope here (separate guidance).

## 7. Runs on the final tree (rebased on origin/main, 2026-11-04)

* `scripts/e2e/call-audio-sandbox.sh`: stage 1 6/6, stage 2 25/25 with the final `pactl`/`parec` adapter (and before that with the libpulse adapter at a machine load average of about 185, and twice on the pre-rebase tree).
* CI on the first push (4c196e52b): Rust on ubuntu, windows and macos, the Windows/macOS type-check, Android, coverage and supply chain passed; the performance budget failed on idle RSS (9 692 kB against 8 000 kB, from linking libpulse). Fixed in 788b7b551 by driving the sound server through `pactl`/`parec` (notices regenerated in c42464cc9). CI run 41773601344 on main (bf50fc236, which contains all of this): every job green, including Rust on all three systems, the performance budget at the strict RSS budget, and supply chain.
* `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
* `cargo test --workspace --no-fail-fast` (pre-rebase) and the affected crates after the rebase (bridge-calls, bridge-core, bridge-service, bridge-phone-sim, bridge-apps, bridge-fuzz): all pass. Under load averages of 120-290 a few timing tests in crates this change does not touch timed out (bridge-android `chaos` pairing, bridge-service `share_api`/`battery_api` acks); each passed when re-run alone.
* `scripts/tests/realdevice-kit-test.sh`: pass (with the new Bluetooth section in the report script).

## 8. How this fits the Settings checklist (landed in parallel)

The "Call audio on this computer" window and `geekbridge-ctl call-audio` (docs/reports/call-audio-setup-ux.md) landed at the same time. They share the device choice: both read and write `<state>/call-audio.json` with the same `{"output": ..., "input": ...}` shape, so a pick made in Settings (or with `call-audio use`) is what a live call is routed to, and `call audio-devices` shows and changes the same pick. Their window keeps its own checklist and the tone/microphone test, and its "Set up call audio" button (landed after this) drives `CallAudioPair`/`HearCalls`; this work adds the routing, mute, the undo, the pairing over D-Bus (`CallAudioPair`), the WirePlumber fix and the step-level `HearCalls` checklist, which the window can show for the steps it does not cover (WirePlumber role and backend, telephony service, the hands-free connection, the phone's card, `auto-connect` without `hfp_ag`). This work's service module is `crates/bridge-service/src/call_audio_host.rs` (theirs is `call_audio.rs`).
