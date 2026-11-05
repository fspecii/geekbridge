# Call audio on the Fold5: what to try, in what order, and what is left if it fails

Date: 2026-11-05. Goal (owner): use calls with the PC acting as the headset, ideally without depending on the PC's
Bluetooth. This PC now has an integrated Bluetooth controller (`hci0`), so Bluetooth hands-free (HFP, PC as the
hands-free unit) is the primary path. The Wi-Fi path (shell-uid helper over Wireless debugging) stays experimental.
Read-only design work: nothing here was run against the phone, the live service, Hyprland or real Bluetooth.

Inputs: [design](../design/call-audio-over-wifi.md), [integration](call-audio-wifi-integration.md),
[real-phone procedure](call-audio-wifi-realphone.md), [pre-review of that procedure](wifi-realphone-prereview.md),
[feasibility](call-audio-over-wifi-feasibility.md), [PC headset](call-audio-pc-headset.md),
[one-button setup](call-audio-oneclick.md), [gap analysis](call-ux-gap-analysis.md), [user guide](../call-audio-options.md),
`crates/bridge-calls/src/{setup,hear}.rs`, `crates/bridge-service/src/{call_cli,call_audio_host}.rs`,
`android/app/src/debug/java/dev/geekbridge/android/callaudio/CallAudioHelper.kt`.

Tags: **[CODE]** read in this repository's code; **[REPO]** stated in an earlier repository report (that report's own
tag applies); **[ANDROID-DOC]** public Android developer documentation, recalled for this report, not re-fetched or
re-checked in this session; **[UNVERIFIED]** general knowledge with no source; **[GUESS]** reasoned, to be measured.

## 0. The short answer

* **On a stock Fold5 there is no app-level way to carry a carrier call's voice to the PC without Bluetooth.** Android
  keeps call audio between the modem and the audio DSP; the permissions that read or write it
  (`CAPTURE_AUDIO_OUTPUT`, `CALL_AUDIO_INTERCEPTION`) are not grantable to an installed app [REPO, ANDROID-DOC]. Being the
  default phone app does not change that (section 3).
* **The only Bluetooth-free route is the debug-build Wi-Fi helper**, and it works only if Samsung's audio HAL declares
  the telephony TX/RX devices and lets the `shell` user use them. One ten-second probe with no call answers the
  "speak" half (section 2). Even if it passes, it needs Wireless debugging (or a USB cable) after every reboot, so it is
  a power-user feature, not a Phone Link equivalent.
* **So: do Bluetooth first (section 1), run the Wi-Fi probe in the same session because it is cheap, then decide.**

## 1. Decision tree: Bluetooth hands-free on the real Fold5

Prerequisites: the GeekBridge service running as usual, the HyperX (USB, not Bluetooth) as the PC headset, the Fold5
unlocked and unfolded. `ctl` below means `geekbridge-ctl`. All strings quoted are what the code prints [CODE] unless
marked; the phone's own wording is [REPO] (One UI) and may differ by version.

### Step B0. Baseline (30 s)

```
geekbridge-ctl calling
```

| You see | Meaning | Next |
| --- | --- | --- |
| `1. [ok] Bluetooth adapter present: controller hci0`, `2. [ok] ... not blocked`, `3. [ok] bluetooth.service running: active`, `4. [ok] Adapter powered on` | the controller is usable | B1 |
| step 2 `soft-blocked` | rfkill | `rfkill unblock bluetooth`, rerun B0 |
| step 2 `hard-blocked` | key/BIOS switch | enable Bluetooth in hardware/firmware; no command fixes it |
| step 3 `not active` | BlueZ not running | `sudo pacman -S --needed bluez bluez-utils && sudo systemctl enable --now bluetooth.service` (the window shows it; GeekBridge never runs sudo) |
| step 4 `powered off` | radio off | `bluetoothctl power on` (setup also powers it on) |
| step 1 `no Bluetooth controller` | `hci0` vanished (driver/firmware) | `journalctl -k -n 50 \| grep -i -E 'bluetooth\|btusb\|firmware'`; stop here and report |

### Step B1. WirePlumber settings (10 s)

```
geekbridge-ctl call setup --fix-config
```

| You see | Meaning | Next |
| --- | --- | --- |
| `The WirePlumber configuration needs no change.` | `hfp_hf` role, native backend, telephony service and auto-connect are fine | B2 |
| `Wrote ~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf (checked before use; ...). WirePlumber restarted and runs; to undo: rm ... && systemctl --user restart wireplumber.` | fixed (on this machine it is expected to add `hfp_ag` to `bluez5.auto-connect` [REPO]) | B2 (a one-second sound gap from the restart is normal) |
| `... Not restarted (...): systemctl --user restart wireplumber; ...` | file written, restart refused | run the restart yourself, then B2 |
| `WirePlumber did not keep running with the new ... so GeekBridge took it back ...` | the fragment broke WirePlumber; it was rolled back | stop; send `journalctl --user -u wireplumber -n 100` |

### Step B2. Pair (2 minutes)

On the Fold5: Settings > Connections > Bluetooth, keep that screen open. Then on the PC (or press **Set up call audio**
in Settings, which runs the same steps with the code shown large):

```
geekbridge-ctl call setup                     # if the phone is not found by name: --phone "Fold5" (any part of its Bluetooth name)
```

Success, in order: `Using Bluetooth adapter hci0 (...); phones list this computer as "<name>"` →
`This computer is visible as a Bluetooth device for 120 s` → `Looking for ...` → `Found <phone> (...); pairing: compare
the code on both screens` (or the phone starts it after you tap the PC's name) → `Does the phone show NNNNNN? Accept there
too. [y/N]` (compare, tap Pair on the phone, type `y`) → `Paired with ... ; marking it trusted so it reconnects by itself`
→ `Connecting the phone's calls (hands-free) profile` → `<phone> is set up for calls. Answer a call on this computer ...`
→ `Next: geekbridge-ctl calling (every step should be ok) ...`. If the phone asks about contacts and call history, either
answer is fine for audio [REPO].

| Failure line | Meaning | Do |
| --- | --- | --- |
| `Pairing with ... failed: the code was not confirmed in time on both screens` | BlueZ's 60 s agent window passed | rerun; answer within a minute |
| `... the phone refused; if it still lists this computer from an earlier pairing, remove it there (gear icon > Unpair) ...` | stale bond on the phone | Unpair on the phone, rerun |
| `... the phone did not answer; keep its Bluetooth screen open and bring it closer` | page timeout | keep the screen open, rerun |
| `no phone called "X" showed up or paired within 150 s ...` / `no phone showed up or paired within 150 s` | not discoverable, or a name mismatch (Android reports e.g. the model name) | on the phone tap the PC's name in its Bluetooth list (pairing from the phone is accepted whatever its name), or rerun without `--phone` |
| `Another Bluetooth pairing helper stays in charge (...)` | a desktop Bluetooth agent is the default | confirm the code in that helper's window if it pops up; otherwise close it and rerun |
| `this computer offers no hands-free profile to the phone (...)` | WirePlumber is not offering `hfp_hf` | back to B1, then `ctl calling` step 5 |
| `the phone did not accept the calls (hands-free) connection (...). On the phone: Bluetooth > this computer (gear icon) > turn on Calls / Phone calls` | the per-device **Calls** toggle is off on One UI | toggle it on, then `ctl calling` |
| `the setup is taking too long; geekbridge-ctl call setup --cancel` | the CLI's own 240 s limit | cancel, rerun |

### Step B3. Checklist all green (30 s)

```
geekbridge-ctl calling
```

Success: the first line `Call audio: ready (phone: <name>).` and `Result: ready. Answer a call from this computer and the
voice comes to the devices above.` In particular step `Phone's calls (hands-free) profile connected` is `[ok]` and
`Phone's audio card in PipeWire (audio-gateway)` says `bluez_card.<ADDR>: audio-gateway` [CODE, REPO].

| First FAIL | Do |
| --- | --- |
| `Phone connected: paired but not connected ...` | Bluetooth on the phone; `bluetoothctl connect <ADDR>` |
| `Phone's calls (hands-free) profile connected: the phone is connected but not as a hands-free device` | One UI: Bluetooth > gear next to the PC > **Calls** on |
| `PipeWire telephony service ...: not on the session bus` | `systemctl --user restart wireplumber` |
| `Phone's audio card ...: PipeWire shows no card` / `offers no audio-gateway profile` | the fix line printed (restart WirePlumber / disconnect and reconnect) |
| `Headphones and microphone for calls: ...` | `geekbridge-ctl call audio-devices` and pick the HyperX |

Then, no call needed: `geekbridge-ctl call-audio test` (two-note tone in the headset, then the microphone meter for 5 s).

### Step B4. The real call (the only step that proves anything)

Call the Fold5 from the iPhone, answer **on the PC**. Within 3 s:

```
geekbridge-ctl calls
```

| You see / hear | Meaning | Do |
| --- | --- | --- |
| a line starting `active`, `source: Call audio: on this computer (speaker: ...; microphone: ...; codec mSBC, wideband 16 kHz)` (or CVSD narrowband), `microphone: on`; both people hear each other | **Bluetooth works. Done.** | B5, then decide whether the Wi-Fi route still matters |
| a "no voice link" problem line after about 3 s [REPO: the 3-second check] | the phone did not open the audio link (SCO) | on the phone's call screen pick the PC as the audio device; then `ctl call audio`; report if it repeats |
| voice link open but digital silence / crackle | the controller fails at mSBC over its transport [REPO, UNVERIFIED for this controller] | add `bluez5.enable-msbc = false` (the fix line names the file), `systemctl --user restart wireplumber`, call again (narrowband) |
| one-way audio | stream routed to the wrong device, or the mic muted | `ctl call audio-devices`, `ctl call mute off`; report with the bundle below |
| the caller hears an echo | speakers, not the headset | use the HyperX (headset: no echo path) |

B5 (2 minutes, from [call-audio-pc-headset.md](call-audio-pc-headset.md) section 5): `ctl call mute on|off`,
`ctl call audio phone` (voice moves to the earpiece in about 2 s), `ctl call audio` (back); a second call answered **on
the phone** must stay on the phone; Bluetooth off on the phone for 30 s and back: the hands-free step must come back
`[ok]` by itself (Android usually reconnects; else GeekBridge on link-up, then at 1 min backing off to 10 min [CODE]).

Anything fails: `scripts/realdevice-report.sh --state-dir "$(dirname "$(geekbridge-service --print-token-path)")" --no-adb`
(redacted bundle with a Bluetooth and call-audio section), plus which row above you were on.

## 2. Wi-Fi route: the single cheapest experiment

**Question it settles:** does the Fold5's audio HAL declare the telephony TX/RX devices, and does Samsung's `shell` user
hold `CALL_AUDIO_INTERCEPTION`? That is exactly `AudioManager.isPstnCallAudioInterceptable()`, which in AOSP returns true
only when both `TELEPHONY_TX` and `TELEPHONY_RX` are present [REPO, SOURCE tag there]. Without TX there is no
framework-level way to put the PC's voice into a carrier call (routes 1-3 of the beep test all need it) [REPO]. Since the
owner wants a headset (two-way), not a listener, this flag alone decides whether the Wi-Fi route can ever meet the goal.

**Cheapest form: USB cable, no call, no install, nothing heard by anyone (about 15 s).** With USB debugging on (a cable
avoids the whole Wireless-debugging pairing walk and the debug-APK-over-release-APK signature problem in the
pre-review's finding 3):

```
ADB=.tools/android-sdk/platform-tools/adb
scripts/build-android.sh --skip-host-tests                      # debug APK with the helper (already built? skip)
$ADB push dist/android/geekbridge-debug.apk /data/local/tmp/gbprobe.apk
$ADB shell 'CLASSPATH=/data/local/tmp/gbprobe.apk app_process /system/bin dev.geekbridge.android.callaudio.CallAudioHelper --probe'
$ADB shell rm /data/local/tmp/gbprobe.apk
```

Why this should work without installing: the helper builds its own `AudioManager` on a shell-identity context and
reads nothing from the installed app [CODE]; scrcpy runs its server the same way from `/data/local/tmp` [UNVERIFIED that
nothing else in this helper needs the install; if the push form fails, use the documented `pm path` form from the
real-phone report, step D, after installing the debug build]. Side effects even with no call [REPO pre-review 14]: the
microphone indicator flashes (seven 0.6 s captures), and a 1.5 s VoIP-mix self-test registers an audio policy mix; no
sound is played to anyone outside a call. Do not interrupt it.

**Read three fields of the one JSON line:** `pstnInterceptable`, `telephonyTxDevice`, `uplinkInjectionTrack.outcome`.

| Outcome | Meaning | Next |
| --- | --- | --- |
| `pstnInterceptable: true` | HAL declares TX and RX and the shell holds the permission: speaking from the PC is **possible in principle** (on the emulator the same flag led to frames written into the call [REPO EMU]) | one real call: self-test step 2 (does listening work) and the beep test, step H (does the far party hear route 1) from [call-audio-wifi-realphone.md](call-audio-wifi-realphone.md) with the pre-review's corrections. Only "they heard the beeps" proves it |
| `pstnInterceptable: false`, `telephonyTxDevice: false` | no telephony output device: **no uplink at all without root** | Wi-Fi can at best be listen-only (if `VOICE_DOWNLINK` is not silenced in a call, which needs one more in-call test). Not a headset: treat the Wi-Fi route as closed for this goal |
| `pstnInterceptable: false`, `telephonyTxDevice: true` | TX exists, RX missing (or not reported as available) | routes 2/3 (`tx_voice`, `tx_music`) may still reach the far party; only the beep test can tell, and the pre-review warns that a refused preferred device plays the beeps out of the phone's speaker (false positive). Read `uplinkTest` for `preferred device refused` |
| `pstnInterceptable` is a quoted error (e.g. a `SecurityException`) | Samsung's `shell` does not hold `CALL_AUDIO_INTERCEPTION` (AOSP's Shell manifest does [REPO]) | route 1 closed; fall back to the `telephonyTxDevice` row above |
| `uplinkInjectionTrack.outcome: "track"` with no call | unexpected (AOSP hands it out only in a call [REPO]) | record it; still needs the in-call test |

An even cheaper but **non-decisive** hint, no APK at all: `$ADB shell dumpsys media.audio_policy | grep -i -E 'TELEPHONY_(TX|RX)'`.
A match can come from the declared module list rather than the available devices, so it can be a false positive
[GUESS about the dump format on One UI]; no match is a strong hint the probe will say false.

**What "settled" means for the product, either way:** even a full pass leaves the route depending on Wireless debugging
(or a cable) being on, a shell process that dies on reboot, debug-only builds, and Samsung's per-firmware HAL behaviour
[REPO]. It can become a documented power-user mode; it cannot become the default path.

## 3. If neither works: what is still seamless, and what is not possible

### 3.1 The companion-role routes, honestly

| Approach | What it gives | Call audio? | Verdict |
| --- | --- | --- | --- |
| Default phone app (`RoleManager.ROLE_DIALER` + a full `InCallService` UI) | Telecom binds the app for every call: answer, reject, hold, disconnect, `playDtmfTone`, `setMuted`, and routing among endpoints (`requestCallEndpointChange` with `CallEndpoint` types earpiece, speaker, Bluetooth, wired headset, streaming; the older `setAudioRoute`/`CallAudioState` deprecated in API 34) [ANDROID-DOC] | **No.** Routing picks among *existing* devices; no API returns or accepts call PCM. Recording sources `VOICE_CALL/UPLINK/DOWNLINK` still need `CAPTURE_AUDIO_OUTPUT`, which the role does not grant [ANDROID-DOC, REPO] | Would give native DTMF and control without the link quirks, at the price of replacing Samsung's Phone app for every call (incoming UI, emergency calls, Samsung features). Not worth it for audio, which it cannot deliver |
| `CallScreeningService` (`ROLE_CALL_SCREENING`) | allow/reject/silence an incoming call before it rings, skip the log [ANDROID-DOC] | No. (There is a screening response flag for audio processing used by system call screening; whether it is a SystemApi restricted to privileged apps is [UNVERIFIED]; it does not stream audio to another device either way) | No audio path |
| `CallStreamingService` / `ROLE_SYSTEM_CALL_STREAMING` (API 34) and the `STREAMING` call endpoint | Android's own hook for "continue this call on another device" [ANDROID-DOC, recalled] | Designed for this, but the role is a system role (not requestable by a store app) and it targets VoIP calls made through the Telecom/core-telecom APIs [UNVERIFIED in detail] | Confirms the platform reserves this feature for system/OEM apps. Not usable |
| `AudioManager.setCommunicationDevice` (API 31) | selects among `getAvailableCommunicationDevices()` for communication use cases [ANDROID-DOC] | No: it cannot create a device; during a carrier call Telecom owns routing [REPO] | No |
| Self-managed `ConnectionService` (VoIP) | the app's own calls appear as calls; the app owns their audio [ANDROID-DOC] | Only for GeekBridge's *own* VoIP calls; cannot adopt a carrier call [REPO] | Future GeekBridge-to-GeekBridge calling only |
| Accessibility-service or speakerphone tricks | | Fragile and against Play policy [REPO] | Rejected. **Speakerphone relay is explicitly not a feature** (owner) |
| Root / ALSA mixer (`incall_music`, `VOC_REC_DL`) | works on some Qualcomm devices [REPO, SECONDARY there] | Yes, with root | Out of scope (no root) |
| PC as a USB audio headset for the phone | phones route calls to a USB audio-class headset [REPO, GUESS for Samsung calls] | Yes in principle | x86 PCs cannot be a USB device [REPO]. A separate gadget board (USB audio gadget to the phone, network to the PC) would work in theory [GUESS]; it is hardware, out of scope |

Conclusion: on stock Android 16 with no root, **Bluetooth HFP is the only shippable two-way route**, the Wi-Fi helper is
the only Bluetooth-free one (conditional, power-user), and no Android role changes that.

### 3.2 Seamless fallbacks that are honest

1. **Answer here, talk on the phone.** Already the behaviour when hands-free is not connected, and the card already says
   "Answers on the phone. The voice stays on the phone unless this computer is also its Bluetooth headset." [CODE]. Make it
   impossible to miss *before* the click: the Answer button reads **Answer, talk on phone** when the voice cannot come
   here, and after answering a one-line toast **"Answered. Pick up your phone to talk."** On the phone, the GeekBridge app
   can post a heads-up "Answered from <PC>: the call is live" so the user does not leave a caller in silence [GUESS on
   One UI showing it over the in-call screen].
2. **Answer here, voice in earbuds paired to the phone.** No PC Bluetooth at all: the phone routes the call to its own
   active Bluetooth headset (Android's normal behaviour [UNVERIFIED on which device wins when several are connected]).
   The card should name it ("Voice: your earbuds") once the phone reports it (item 5 in section 4). This is the most
   Phone-Link-like experience available without PC Bluetooth.
3. **Listen-only over Wi-Fi** (if the probe and in-call self-test show downlink works but no uplink): hear the caller in the
   PC headset, speak into the phone. Not a headset; keep it debug-only unless the owner decides it is useful (e.g. for
   accessibility). Owner decision.
4. **A USB Bluetooth adapter** if the integrated controller misbehaves (mSBC, coexistence with Wi-Fi on 2.4 GHz
   [UNVERIFIED for this chip]): the existing hotplug flow picks it up; it is still "Bluetooth", but not the laptop's own
   radio.

## 4. Ranked product and UX changes for the Bluetooth path

Already built (verify on the real phone, do not rebuild) [CODE, REPO]: adapter hotplug with a "Bluetooth adapter found"
notification and a setup that waits for an adapter; the one-button 7-step setup with the code shown large; trust plus
automatic hands-free reconnect (on link-up, else 60 s backing off to 10 min); `--fix-config` WirePlumber drop-in with
rollback; the audio route line on the in-call card; mute; move to phone / to PC from the CLI; tray line
"Call audio ready" / "No Bluetooth: calls can't carry voice"; keypad tones over Bluetooth.

**Item 0, before any of the below: run section 1 once.** No real SCO voice has ever passed on this project [REPO];
every item below assumes it does.

| # | Change | Why | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Voice-destination chip on the ringing card, before Answer**: "Voice: this PC · HyperX" (hands-free connected) / "Voice: phone · Bluetooth not connected [Fix]" / "Voice: phone's earbuds" | the moment that matters is before the click; today the route line appears after answering, and the overlay's setup hint is computed from the post-answer route [CODE: `setup_hint: view.audio.route == AudioRoute::Phone`] | Low | data exists: `HearCalls` steps (`hfp`, `devices`) and the chosen devices |
| 2 | **Reconnect on ring**: when the link reports a ring and the set-up phone's hands-free is down, call `setup::reconnect` at once | today the reconnect is on link-up or on a 1 to 10 min backoff [CODE `maybe_reconnect`], so a phone that dropped HFP rings with the voice stuck on the phone | Low | must not delay the card; runs on its own thread like today's |
| 3 | **Truthful tray/Home state**: "Call audio ready" only while hands-free is connected; "Call audio: phone not connected" otherwise | today the tray hint is file-only (adapter present + phone remembered) [REPO oneclick], so it says ready when the phone is out of range | Low-medium | read the cached `HearCalls` the service already computes, not new polling |
| 4 | **Pair from the phone in one tap**: the PC sends its adapter address over the paired TLS link; the Android app calls `BluetoothAdapter.getRemoteDevice(addr).createBond()` (needs `BLUETOOTH_CONNECT`) so the system pairing dialog appears with the code, no list hunting | removes the "find this computer in the list" step that `setup.rs` documents as the hard part (Android hides its own address, so the match is by name and code today) [CODE] | Medium | `createBond` is public API [ANDROID-DOC]; One UI's dialog behaviour [GUESS]; the per-device **Calls** toggle stays manual (its setter is a system API [UNVERIFIED]); the code comparison stays on both screens |
| 5 | **Phone reports its hands-free state** over the link: connected HFP devices via the `BluetoothHeadset` profile proxy (`BLUETOOTH_CONNECT`) | lets the PC say *why* the voice is not here: Calls toggle off, out of range, or the phone's earbuds are the active device (fallback 2) | Medium | new packet + consent wording; numbers/names only |
| 6 | **One-click narrowband fallback**: when the 3-second check finds a silent voice link on mSBC, offer "Use standard quality" that writes `bluez5.enable-msbc = false` through the existing drop-in path, then asks to retry | the most likely controller-specific failure [REPO], fixed today only by hand | Low-medium | reuse `apply_wp_dropin` with rollback |
| 7 | **"Back to phone" on the in-call card** | the gap analysis lists it as missing on the card (#10) while `call audio phone` exists [REPO, CODE] | Low | verify current state first; the gap list may be stale |
| 8 | **Headset guidance in the device picker**: mark Bluetooth outputs ("a Bluetooth headset on the PC switches to low quality when the call opens its microphone, and may drop the call's output"), recommend wired/USB headsets; warn on speakers + built-in mic ("the caller may hear an echo") | the A2DP-to-HFP autoswitch failure [REPO pre-review 10] and speaker echo are the predictable bad first experiences | Low | wording only, plus a flag from the node name |
| 9 | **Echo cancellation for speakers**: optional PipeWire echo-cancel node for calls | headsets need none; laptop speakers + mic do; listed as not done [REPO] | Medium | off by default; only for non-headset outputs |
| 10 | **Answer wording when the voice cannot come here** (fallback 1): "Answer, talk on phone", post-answer toast, phone heads-up | keeps the no-Bluetooth case honest and smooth | Low | i18n keys |

Not recommended: automatic speakerphone on answer (owner: not acceptable), becoming the default phone app (section 3.1),
shipping the Wi-Fi helper in release builds.

## 5. Verified vs not

Verified here: nothing on hardware. The code paths, messages and backoff values quoted are read from the tree at
origin/main (9d6f95223). Unverified and decisive: real SCO voice on this PC and the Fold5 (section 1, B4), the Fold5's
`pstnInterceptable` (section 2), whether the probe runs from `/data/local/tmp` without installing, One UI's behaviour for
a phone-initiated bond (section 4, item 4), and every [ANDROID-DOC] claim, which was recalled rather than re-read today.
