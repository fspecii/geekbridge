# Call audio on a computer without Bluetooth: the realistic options

Date: 2026-11-02. Scope: what to do when the voice of a phone call should come out of (and go into) this computer, and the computer has **no Bluetooth controller** (the Omarchy test machine: `/sys/class/bluetooth` is absent, no Bluetooth device in `lsusb`, `rfkill list` empty).

Evidence tags: **[OFFICIAL]** fetched from the vendor's own page on 2026-11-02; **[SRC]** read in a project source or installed package on this machine; **[INF]** inference, not verified; **[UNVERIFIED]** from general knowledge, no source fetched. Nothing in this document was exercised on real call audio: this machine has no controller and the repository has never carried real call audio.

## Short answer

| Goal | Possible? | How |
| --- | --- | --- |
| Answer, reject, hang up from the PC; voice stays on the phone (speaker, earpiece or the phone's own Bluetooth headset) | **Yes, works today** | GeekBridge link. No PC Bluetooth needed. |
| Hear and speak the call on the PC | **Only over Bluetooth hands-free (HFP)** | Add a USB Bluetooth adapter (section 1). |
| Carry call audio over Wi-Fi/the GeekBridge link | **Not possible on a stock, unrooted Android 10+ phone** | Android withholds call-audio capture and injection from normal apps (section 2). |
| What other phone-companion apps do | The same thing: Bluetooth HFP. It needs Bluetooth on the PC too (section 3). | |

GeekBridge now says which of these applies. `geekbridge-ctl calls` (a `source:` line), `geekbridge-ctl diagnose` (`call audio:`) and the desktop Calls window show one of:

* `no Bluetooth adapter found` – answering still works, the voice stays on the phone; add a USB dongle.
* `Bluetooth adapter found, but the phone is not paired as a hands-free device` – pair it and allow calls.
* `phone is paired but not connected as a hands-free device` – connect its Hands-free profile.
* `ready (phone connected as a hands-free device)`.

Detection lives in `crates/bridge-calls/src/readiness.rs` (sysfs `/sys/class/bluetooth/hci*` plus `bluetoothctl devices Paired` / `bluetoothctl info`, cached 15 s; tested against a fake sysfs tree and canned `bluetoothctl` output).

## 1. A USB Bluetooth dongle

### 1.1 What the software needs

The Linux backend (`crates/bridge-calls`, ) makes **this computer the HFP hands-free unit (HF) and the phone the audio gateway (AG)**. It needs:

1. `bluetoothd` running with a powered adapter (`org.bluez.Adapter1`).
2. PipeWire's native HFP backend with the **`hfp_hf`** role. WirePlumber's current Bluetooth page lists `bluez5.roles` values `hsp_hs, hsp_ag, hfp_hf, hfp_ag, a2dp_sink, a2dp_source, bap_sink, bap_source` with default `[ a2dp_sink a2dp_source bap_sink bap_source hfp_hf hfp_ag ]`, and `bluez5.hfphsp-backend` values `any, none, hsphfpd, ofono, native` with default `native` **[OFFICIAL]** (<https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/bluetooth.html>). So on current WirePlumber **no configuration is required**; the defaults already include `hfp_hf` and the native backend. Note: `hsp_hs` is *not* in the current default list; the older default (`hsp_hs hsp_ag hfp_hf hfp_ag`) appears in older docs. Add `hsp_hs` only if a phone refuses HFP (rare on Android).
3. PipeWire's `org.pipewire.Telephony` D-Bus service (what GeekBridge drives for dial/answer/hang-up). It is exposed by the native backend; its API is undocumented, which is why the probe derives capabilities from live introspection **[SRC]** (`spa/plugins/bluez5/telephony.c`, installed `libspa-bluez5.so`, pipewire 1.6.9 on this machine).
4. `pipewire-pulse` (GeekBridge routes through PulseAudio-protocol `module-loopback` / `module-echo-cancel`).

oFono is **not** required: it remains a selectable backend (`bluez5.hfphsp-backend = "ofono"`) **[OFFICIAL]**, and the older Collabora status note describes the native backend as the simple one that implements only a minimal AT-command set (<https://www.collabora.com/news-and-blog/news-and-events/pipewire-bluetooth-support-status-update.html>, 2022, so possibly dated). GeekBridge uses the native backend plus PipeWire Telephony; it does not use oFono, and I found no source saying oFono is deprecated.

Installed here: pipewire 1.6.9, wireplumber 0.5.18, bluez / bluez-utils 5.87 **[SRC]** (`pacman -Q`).

### 1.2 Which adapter

I could not fetch an authoritative per-chipset list: the ArchWiki Bluetooth page blocked automated fetches and neither BlueZ nor the kernel publishes a recommended-hardware list that I found. So the guidance below is **[INF]/[UNVERIFIED]** and should be confirmed on the actual purchase:

* Any adapter driven by the in-kernel generic `btusb` driver works with BlueZ; the ArchWiki confirms `btusb` is the generic USB Bluetooth driver **[OFFICIAL-ish: search excerpt]**. Prefer chips whose firmware ships in `linux-firmware` (Intel, Qualcomm/Atheros, MediaTek, most Realtek) so no out-of-tree driver is needed. An Arch forum thread shows Realtek RTL8761B(UV) dongles needing an AUR firmware package, and a user advising to avoid parts needing out-of-tree drivers (forum opinion, <https://bbs.archlinux.org/viewtopic.php?id=267891>). Check the package state before buying.
* Bluetooth 4.0 or newer is plenty; HFP is a classic (BR/EDR) profile, so **buy a dual-mode (Classic + LE) adapter, not an LE-only one**. [UNVERIFIED]
* Avoid unbranded "CSR8510" dongles of uncertain origin: many are clones with flaky firmware and are a known source of "shows in lsusb, will not power on" reports. [UNVERIFIED]
* Wideband speech (mSBC, 16 kHz) needs the controller to support transparent SCO data; it is hardware-quirk dependent in WirePlumber ("None of these work on all headsets, so they are enabled based on the hardware quirks database") **[OFFICIAL]**. Expect narrowband (CVSD, 8 kHz) at worst.
* Plug it into a **USB 2.0 port directly**, not a hub, and not next to a USB 3 device (2.4 GHz interference). [UNVERIFIED]
* Watch for combined Wi-Fi/Bluetooth cards that conflict (ArchWiki notes Broadcom conflicts) **[OFFICIAL-ish: search excerpt]**; a separate USB dongle sidesteps that.

Nothing recommended here has been tested by this project.

### 1.3 Exact setup (Arch / Omarchy)

```bash
# 1. Plug the dongle in, then confirm the kernel sees it.
lsusb | grep -i bluetooth
dmesg | grep -i -E 'bluetooth|btusb|firmware' | tail
ls /sys/class/bluetooth            # expect hci0

# 2. Firmware if dmesg says it is missing.
sudo pacman -S --needed linux-firmware

# 3. Services and radio.
sudo pacman -S --needed bluez bluez-utils
sudo systemctl enable --now bluetooth.service
rfkill list bluetooth && rfkill unblock bluetooth
bluetoothctl power on

# 4. Pair the phone (accept the prompt on the phone; allow calls/contacts if asked).
bluetoothctl
#   scan on
#   pair  <PHONE_ADDR>
#   trust <PHONE_ADDR>
#   connect <PHONE_ADDR>

# 5. Verify the phone offers HFP audio gateway and PipeWire registered it.
bluetoothctl info <PHONE_ADDR> | grep -i -E 'Handsfree Audio Gateway|Connected|Paired'
busctl --user tree org.pipewire.Telephony      # an .../ag0 object per connected phone
wpctl status | grep -i bluez
pactl list cards short | grep -i bluez         # card appears; voice profile appears during a call

# 6. GeekBridge's own check.
geekbridge-ctl diagnose | grep 'call audio'
geekbridge-call-probe                          # read-only evidence report
```

Only if a phone will not connect hands-free, force the roles in a WirePlumber fragment (`~/.config/wireplumber/wireplumber.conf.d/51-bluez-hfp.conf`; the exact file name is free):

```
monitor.bluez.properties = {
  bluez5.roles = [ a2dp_sink a2dp_source hsp_hs hfp_hf ]
  bluez5.hfphsp-backend = "native"
  bluez5.enable-msbc = true
}
```

then `systemctl --user restart wireplumber pipewire pipewire-pulse`. (Property names are from the WirePlumber page **[OFFICIAL]**; the fragment is the usual WirePlumber HFP-role fragment.)

Open items that need real hardware (unchanged from the repo's research): whether WirePlumber switches the card to the voice profile by itself after `Activate()`; audible quality; echo cancellation.

## 2. Without PC Bluetooth: what the phone side can do

### 2.1 Works today

* **PC controls, phone carries the voice.** Answer/reject/hang up through the GeekBridge link (`ANSWER_PHONE_CALLS`, call state reporting). The user talks into the phone, or into whatever the phone is already connected to (its own Bluetooth earbuds or car). The Calls window says this ("Answers on the phone. The voice stays on the phone unless this computer is also its Bluetooth headset.").
* **Phone speaker / headset.** Android lets the user choose the call route (speaker, earpiece, wired, Bluetooth) on the phone; that is a phone-side choice and the PC does not need to be involved. GeekBridge does not currently switch it from the PC; doing so would need the call-UI/`InCallService` role on the phone, which the app does not hold.
* I found no stock Android feature that sends *call* audio to another device over Wi-Fi (Cast and Nearby carry media, not telephony) **[UNVERIFIED]**.

### 2.2 A network audio bridge for call audio: what Android blocks

On stock Android 10+ it is **not possible for a normal installed app to capture the other party's voice, nor to inject audio into the call**. Precisely:

* **Capturing the call (downlink/uplink, `VOICE_CALL`, `VOICE_DOWNLINK`, `VOICE_UPLINK`, `AudioDeviceInfo.TYPE_TELEPHONY`)**: Android's audio-sharing rules state "The call always receives audio. The app can capture audio if it is an accessibility service. The app can capture the voice call if it is a privileged (pre-installed) app with permission `CAPTURE_AUDIO_OUTPUT`", and to capture uplink/downlink the app must use those sources/device **[OFFICIAL]** (<https://developer.android.com/media/platform/sharing-audio-input>). `CAPTURE_AUDIO_OUTPUT` is not grantable to a Play/sideloaded app. Open-source call recorders confirm that the system-app permission is what makes recording work, and that many devices do not support the `VOICE_CALL` source at all (BCR README, <https://github.com/c4pt00/BCR-call-recorder>) **[OFFICIAL-ish: search excerpt]**.
* **Playback capture (`AudioPlaybackCapture`)** is the API normal apps get for "what the phone is playing". The voice-communication usage is gated by a privileged permission in AOSP (`CAPTURE_VOICE_COMMUNICATION_OUTPUT`, per an AOSP framework commit "Fix voice communication audio playback capture", mirrored at <https://gitlab.e.foundation/e/os/android_frameworks_av/-/commit/287d330c85bc67d0b985eb8c927945ffd541d4e2>) **[SRC, mirror]**; telephony audio is not exposed to it **[INF]**.
* **The microphone during a call**: the same page says the accessibility-service exception exists for *input* capture. That gives, at most, the user's own microphone (and the room sound including the speakerphone), not a clean downlink **[INF]**. Apps using privacy-sensitive sources (`VOICE_COMMUNICATION`, `CAMCORDER`) take priority over others **[OFFICIAL]**. Many OEMs additionally silence the mic for third-party apps during cellular calls **[UNVERIFIED]**.
* **Injecting audio into the call uplink**: there is no public API. Audio-routing control (`MODIFY_AUDIO_ROUTING`) is privileged/partner-only. The only stock-Android way to put sound into the call is Bluetooth HFP (or the physical microphone).
* **Accessibility service / Assistant workarounds**: an accessibility service would be a policy and trust problem even where it technically captures the mic. Google Play only exempts apps whose primary purpose is helping people with disabilities (`isAccessibilityTool`) from the prominent-disclosure requirement; other apps need in-app disclosure and affirmative consent, and the API may not be used for autonomous action; voice recordings count as data collected through it, requiring declaration (<https://support.google.com/googleplay/android-developer/answer/10964491>) **[OFFICIAL]**. The default Assistant/Voice Interaction role does not unlock the call stream **[UNVERIFIED]**; and the page above says that with `setPrivacySensitive(true)` even a privileged Assistant cannot capture concurrently **[OFFICIAL]**. A speakerphone-plus-mic trick (phone on speaker, mic picks up both sides acoustically) is the only unprivileged "capture", and it is low quality, echo-prone and fragile; it is **not** something GeekBridge should ship.
* **Verdict: impossible without root (or a custom/OEM ROM that pre-installs the app as a privileged system app) for a network call-audio bridge on stock Android 10+.** With root, privileged-app tricks (Magisk modules granting `CAPTURE_AUDIO_OUTPUT`, BCR-style) exist for *recording*; injection into the uplink is much harder and I did not investigate it. GeekBridge targets unmodified phones, so **this option is out of scope**.

## 3. What Windows phone-companion apps do

* Calls use **Bluetooth HFP**: Microsoft states "Calling requires Bluetooth capability on both your PC and on your Android device".
* The PC drives Windows' in-box stack (`PhoneLineTransportDevice` with `PhoneLineTransport.Bluetooth`, `PhoneCall.ChangeAudioDeviceAsync(LocalDevice)`); audio flows over the SCO link to the PC's Hands-Free endpoint, not over the app's own network link.
* So a PC with no Bluetooth gets **no call audio from such apps either**; it is the same constraint GeekBridge has.

## 4. Recommendation

1. Keep the link path as the default: answering and call control with the voice on the phone. State it plainly (done in the status lines above).
2. For voice on the PC: buy a dual-mode USB Bluetooth adapter with in-kernel/`linux-firmware` support, follow section 1.3, and run `geekbridge-ctl diagnose` until it prints `call audio: ... ready`.
3. Do not build a Wi-Fi call-audio bridge or an accessibility workaround; Android blocks it for ordinary apps and Play policy discourages the workaround.

## Sources

* WirePlumber Bluetooth configuration: <https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/bluetooth.html>
* Android, sharing audio input: <https://developer.android.com/media/platform/sharing-audio-input>
* Google Play, AccessibilityService API policy: <https://support.google.com/googleplay/android-developer/answer/10964491>
* PipeWire Bluetooth status (Collabora, 2022): <https://www.collabora.com/news-and-blog/news-and-events/pipewire-bluetooth-support-status-update.html>
* AOSP playback-capture commit (mirror): <https://gitlab.e.foundation/e/os/android_frameworks_av/-/commit/287d330c85bc67d0b985eb8c927945ffd541d4e2>
* BCR call recorder README: <https://github.com/c4pt00/BCR-call-recorder>
* Arch forum thread on Realtek dongle firmware: <https://bbs.archlinux.org/viewtopic.php?id=267891>
