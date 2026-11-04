# Hear and talk through your computer

When a call comes in you can answer it from the computer, and the voice can come out of the computer's headphones and into its microphone, like Phone Link on Windows or Continuity on an iPhone and a Mac. The voice does not travel over the GeekBridge link. It goes over **Bluetooth hands-free (HFP)** between the phone and the computer, so the computer needs Bluetooth.

Date: 2026-11-04. **Nothing on this page has been tried on real call audio.** The signalling (answer, decline, hang up, dial) is tested against an emulator and a stand-in phone; Bluetooth voice, codecs and echo have never been exercised on any computer, because the test machine has no Bluetooth yet. Evidence tags: **[OFFICIAL]** read on the vendor's own page; **[SRC]** read in source or an installed package; **[INF]** inference; **[UNVERIFIED]** general knowledge, no source.

## What works where

| | Answer, decline, hang up, dial from the computer | Voice through the computer's headset | Setup help in GeekBridge | Verified |
| --- | --- | --- | --- | --- |
| **Linux** | Yes, over the link, no Bluetooth needed | Yes with a Bluetooth adapter (built in or USB), the phone paired as a hands-free device, PipeWire with the `hfp_hf` role | Live checklist in Settings and in the Calls window, `geekbridge-ctl call-audio`, tone and microphone test | Checklist logic and parsers: tests. Real voice: **not verified** |
| **Windows 11** | Yes, over the link | Through Windows itself: phone paired in Settings, Bluetooth & devices, Phone calls on; Windows plays call audio on the default communications device | The same checklist, filled from `Get-PnpDevice`; no local test (Windows owns routing); `geekbridge-ctl doctor` | Parsers: tests on canned output. Detection on a real Windows install and the voice: **not verified** |
| **macOS** | Yes, over the link | **Not available yet** (see below) | The window says so and offers the fallbacks; `geekbridge-ctl doctor` | The decision, not a feature |
| **Any, no Bluetooth** | Yes | No. Answer here and talk on the phone, or add a USB Bluetooth adapter | The window says so plainly and names what to buy | Yes (the message, not the voice) |

Always available, on every system: answer on the computer and talk on the phone (its speaker, earpiece, or a Bluetooth headset paired with the phone).

## Linux

The setup window lists six steps, each with a state in words and, when it fails, a one-line fix. A fix that is a command has a **Copy command** button; GeekBridge never runs it for you and never runs `sudo`.

1. **Bluetooth adapter found.** `ls /sys/class/bluetooth` shows `hci0`. No adapter: see "What to buy".
2. **Bluetooth not blocked.** `rfkill list bluetooth`. Fix for a software block: `rfkill unblock bluetooth`. A hardware block (switch, airplane-mode key) has no command.
3. **Bluetooth turned on.** `bluetoothctl show` says `Powered: yes`. Fix: `bluetoothctl power on`. If `bluetoothctl` cannot reach the service: `sudo systemctl enable --now bluetooth.service`.
4. **Phone paired as a hands-free device.** The phone is paired and offers the Handsfree Audio Gateway service (`0000111f`). On the computer add the device and choose the phone; on the phone allow **Phone calls** (One UI: Bluetooth, the gear next to the computer, Call audio on). The GeekBridge Android setup (Answer calls step) has a button that opens the phone's Bluetooth settings and shows whether the phone sees a connected hands-free device.
5. **Hands-free connected.** The phone is connected and PipeWire's telephony service registered it. Fix: `bluetoothctl connect <address>`.
6. **Headphones and microphone chosen.** Pick the output and the input in the window (default: the system defaults). The choice is stored in `call-audio.json` in the service state directory.

**Test call audio** (no call needed): plays a quiet two-note tone (about 1.2 s) through the chosen output, then shows a live level meter from the chosen microphone for 5 s and says whether it heard you. Nothing is recorded or kept. It works before any Bluetooth exists, so you can test the headset first. It needs `paplay` or `pw-play`, and `parec` (PulseAudio tools; `pipewire-pulse` provides the server).

### Software the voice needs
* `bluetoothd` with a powered adapter.
* PipeWire's native HFP backend with the `hfp_hf` role. WirePlumber's current defaults for `bluez5.roles` already include `hfp_hf`, and `bluez5.hfphsp-backend` defaults to `native` **[OFFICIAL]** (<https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/bluetooth.html>). Only if a phone refuses hands-free, add `hsp_hs` to the roles in a WirePlumber fragment and restart `wireplumber pipewire pipewire-pulse`.
* PipeWire's `org.pipewire.Telephony` D-Bus service, which GeekBridge drives for answer and hang up. Its API is undocumented, so it is probed live **[SRC]**.
* `pipewire-pulse`: GeekBridge reads the sound server and moves the call's voice to the chosen devices through `pactl` and `parec` (from libpulse, which pipewire-pulse depends on), run only during a call.

**How a call looks on PipeWire (verified against the 1.6.9 sources and in the sandbox, 2026-11-04):** the phone's card `bluez_card.<ADDR>` has the profile **`audio-gateway`** (profile names describe the *remote* device; `headset-head-unit` is what a *headset* gets and never carries a phone call). While the phone's voice link (SCO) is open, PipeWire adds two *streams* (not devices) with `api.bluez5.profile = headset-audio-gateway` and `node.autoconnect = true`: the caller's voice (`Stream/Output/Audio`) and the uplink (`Stream/Input/Audio`). WirePlumber links them to the default speakers and microphone by itself; GeekBridge moves them to the devices picked with `geekbridge-ctl call audio-devices`, mutes the real microphone on `call mute`, checks within 3 s that audio flows and undoes everything after the call. Details, the guided setup and the hardware test script: [reports/call-audio-pc-headset.md](reports/call-audio-pc-headset.md).

## What to buy (no Bluetooth on the computer)

Any USB adapter that uses the kernel's generic **`btusb`** driver and supports **Bluetooth 5.x**, dual mode (Classic plus LE; hands-free is a Classic profile, so not an LE-only adapter). Prefer chips whose firmware is in `linux-firmware` (Intel, Qualcomm/Atheros, MediaTek, most Realtek); some Realtek RTL8761B parts have needed an extra firmware package on Arch **[OFFICIAL-ish: forum thread <https://bbs.archlinux.org/viewtopic.php?id=267891>]**. Avoid unbranded "CSR8510" dongles of unknown origin **[UNVERIFIED]**. Plug it directly into a USB 2.0 port, away from USB 3 devices **[UNVERIFIED]**. Wideband speech (mSBC) depends on the controller and the headset quirks database; narrowband is the worst case **[OFFICIAL]**. **No adapter has been tested by this project.**

Check after plugging in: `lsusb | grep -i bluetooth`, `dmesg | grep -i -E 'bluetooth|btusb|firmware'`, `ls /sys/class/bluetooth`, then open the setup window.

## Windows 11

Windows has its own hands-free stack, so the voice goes to the Windows default communications device without GeekBridge touching audio. Microsoft says calling in its companion app "requires Bluetooth capability on both your PC and on your Android device" **[OFFICIAL]**.

1. Settings, Bluetooth & devices: add your phone; on the phone accept the pairing and allow Phone calls.
2. Settings, System, Sound: check the default communications output and input.
3. Open GeekBridge's setup window (or run `geekbridge-ctl doctor`).

Detection is `Get-PnpDevice -Class Bluetooth` (adapter present and working; a paired phone's `BTHENUM\{0000111F-...}` Hands-Free AG node, whose `Status` is `OK` while connected) and `-Class AudioEndpoint` (names only). The instance-id shapes come from general Windows knowledge **[UNVERIFIED]**: the parsers are tested on canned output, not on a real PC, and the radio's on/off switch and the default communications device are not read. The window marks those steps "Cannot be checked from here" instead of guessing.

## macOS

* **Continuity calling (iPhone Cellular Calls on a Mac) is Apple-to-Apple.** Nothing found says it works with Android, and GeekBridge does not use it.
* Apple does publish a hands-free interface (`IOBluetoothHandsFreeDevice`, with `connectSCO` and `transferAudioToComputer`), so "macOS does not expose hands-free to apps" would be too strong. But no source shows a Mac carrying an Android phone's call voice, whether macOS then offers the voice link as a CoreAudio device is undocumented, and the only open-source wrapper found ("claims iPhone and Android") has no hardware test **[verified against the project's macOS research notes, which cite Apple's documentation]**. Until a hardware test passes on two Android phones, GeekBridge keeps Mac call audio off and says so.
* Best today: answer on the Mac and talk on the phone, or keep a Bluetooth headset paired with the phone.

## Why not over Wi-Fi?

On stock Android 10+ an installed app cannot capture the other party's voice or inject audio into the call **[OFFICIAL]** (<https://developer.android.com/media/platform/sharing-audio-input>): `CAPTURE_AUDIO_OUTPUT` is for privileged system apps. Accessibility-service and speakerphone workarounds are fragile and against Google Play policy (<https://support.google.com/googleplay/android-developer/answer/10964491>) **[OFFICIAL]**, so GeekBridge does not do them. Bluetooth hands-free is the one route Android offers, and it is what Phone Link uses too.

## Commands

```
geekbridge-ctl call-audio             # the checklist as text
geekbridge-ctl call-audio test        # tone, then 5 s of microphone level
geekbridge-ctl call-audio use --output NAME --input NAME   # no name = system default
geekbridge-ctl calling                # backend summary and the step-by-step 'Hear calls on this computer' checklist
geekbridge-ctl call setup             # pair, trust and connect the phone as a hands-free device (compare the code)
geekbridge-ctl call audio-devices     # devices a call uses; --output/--input NAME|default
geekbridge-ctl call mute on|off       # the real microphone of the live call
geekbridge-ctl call audio phone       # send a live call's voice back to the phone (call audio: bring it here)
geekbridge-ctl doctor                 # includes the call audio line for your OS
geekbridge-call-probe                 # read-only evidence report
```

## Quickstart: the day the USB Bluetooth adapter arrives

1. Plug the adapter into a USB 2.0 port. Within a few seconds the Calls window shows "Bluetooth adapter found. Set up call audio." (and the warning "This computer has no Bluetooth..." disappears).
2. Open Settings, Call audio on this computer, and press **Set up call audio**. That is the whole setup; each step moves on by itself and the window says which of the 7 it is on:
   1. an adapter is there (if not, it waits and continues when you plug one in);
   2. Bluetooth is switched on;
   3. this computer is visible to phones for 2 minutes: **on the phone open Bluetooth settings, tap this computer's name and accept the pairing request**;
   4. compare the six-digit code on both screens and press **Yes, same code** (and accept on the phone);
   5. GeekBridge pairs, trusts and connects the hands-free profile (and writes a small user-level WirePlumber file if the sound server needs one; nothing is changed with sudo);
   6. choose the headphones and microphone, then **Use these devices**;
   7. a test tone plays and the meter shows the microphone; then "Call audio is ready".
   **Cancel** is on every step. A failed step says what went wrong and one thing to do, and **Try again** starts over. **Set up again** and **Forget Bluetooth phone** (it unpairs the phone from this computer) are in the same place. On the phone, allow Phone calls (One UI: Bluetooth, the gear, Call audio on) if the window asks for it.
3. The list of steps below the button is the detailed checklist; it is what to read when something fails. The tray icon's tooltip says "Call audio ready" once a phone is set up, or "No Bluetooth: calls can't carry voice" without an adapter; `geekbridge-ctl calling` starts with the same one-line answer.
4. Press **Test call audio** (any time): you should hear a two-note tone in the chosen headphones and see the meter move when you speak.
5. Call the phone from another number, answer on the computer, and talk. Write down what happened (voice both ways? echo? delay?) and run `geekbridge-ctl call-audio` and `geekbridge-ctl doctor` for the report.
6. Without the window: `geekbridge-ctl call setup` does the same steps in the terminal (`--confirm yes|no`, `--cancel`, `--forget`, `--fix-config`).

Open items that need real hardware: audible quality, whether the adapter carries wideband (mSBC) over USB, and echo with open speakers (the phone's own echo cancellation, NREC, stays on). There is no profile to switch for a call on PipeWire: `audio-gateway` carries both music and calls.

A call answered on this computer is routed to the chosen headphones and microphone automatically (no clicks) and everything is put back when it ends; a call answered on the phone stays on the phone. How it works, the sandbox tests and the hardware test script: [reports/call-audio-pc-headset.md](reports/call-audio-pc-headset.md).
