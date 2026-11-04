# Call audio over Wi-Fi (no PC Bluetooth): feasibility spike

**Status (2026-11-04):** experimental only. Bluetooth stays the primary call-audio path; any Wi-Fi/privileged path would be an opt-in debug-grade experiment, off by default and absent from release builds (`scripts/check-release-isolation.sh` passes with the probe in the tree). Emulator measurements are in the "Emulator results" section below; the downlink-capture and uplink-injection questions on a real phone are still unmeasured.

Date: 2026-11-04. Time-boxed desk spike plus a debug-only prototype. **No real cellular call was captured, so nothing here is a measured result on a phone.** The prototype has been unit-tested for its result classification and built; it has been run only against emulated calls (see "Emulator results"), never a real call.

Tags: **[DOC]** stated by a primary page I fetched (linked). **[SECONDARY]** project READMEs / forums, claims by others. **[RECALL]** from my memory of AOSP, not re-checked this session. **[GUESS]** inference, to be proven or killed on the phone.

## Verdict

1. **A normal GeekBridge app cannot capture a cellular call's downlink or inject the uplink.** Not with install-time permissions, not with `adb pm grant`. **[DOC]** for the permission wall; the "pm grant fails" half is **[RECALL]** and the probe script prints Android's own answer so the owner can confirm in one command.
2. **A process that runs as the `shell` uid probably can capture the downlink** (this is what scrcpy's `voice-call-downlink` source and the Shizuku call recorders do). That needs a helper started over Wireless debugging each boot, hidden APIs, and it covers **downlink/uplink capture only**. It does not give us a way to *play into* the call. **[SECONDARY]** for "works on Android 12-17 on some phones"; **[GUESS]** for Samsung One UI 6-8 and for a Fold5 specifically.
3. **Uplink injection (the PC mic into the call) has no non-Bluetooth route that I could find on stock or Samsung Android without root/system privilege.** **[GUESS]**, weakly supported (nothing found that does it; absence of evidence, not proof).
4. **Acoustic speakerphone bridge: downlink works, uplink does not in any useful way.** See section 3.
5. **USB gadget route: technically the cleanest "real" non-Bluetooth path on paper, but not viable for a desktop product.** Section 4.
6. **Every vendor that ships "calls on the PC" uses Bluetooth for the voice, or a private OEM/Apple stack.** None exposes a public Wi-Fi call-audio API. Section 5.

**Recommendation:** keep Bluetooth (a cheap USB BT dongle) as the default call-audio path, use the native Windows/macOS HFP paths already researched, and document speakerphone as a last resort for hearing the far end only. Run the probe once on the Fold5 so claim 2 gets evidence; do not build product features on it before that.

## 1. Capturing and injecting cellular call audio

### What the framework says [DOC]

- `VOICE_CALL`, `VOICE_DOWNLINK`, `VOICE_UPLINK` and `REMOTE_SUBMIX` capture each "requires the `CAPTURE_AUDIO_OUTPUT` permission. This permission is reserved for use by system components and is not available to third-party applications." ([MediaRecorder.AudioSource](https://developer.android.com/reference/android/media/MediaRecorder.AudioSource))
- Android 10+ input sharing: "The app can capture the voice call if it is a privileged (pre-installed) app with permission `CAPTURE_AUDIO_OUTPUT`", using `VOICE_UPLINK`/`VOICE_DOWNLINK` and/or `TYPE_TELEPHONY`; an accessibility service may capture audio during a call; "the call always receives audio". ([Sharing audio input](https://developer.android.com/media/platform/sharing-audio-input))
- Capture outside the foreground (or a foreground service) is silenced since Android 9 (same page). So a "silent" result must be checked against `isClientSilenced`; the prototype does.
- `InCallService` (the default-dialer API) offers routing and mute only: `requestCallEndpointChange`, `setMuted`; `setAudioRoute`/`CallAudioState` are deprecated in API 34. The page says nothing about recording. ([InCallService](https://developer.android.com/reference/android/telecom/InCallService)) So **being the default dialer gives no call audio**. A default dialer can route to a *Bluetooth* endpoint, which is the opposite of what we want.
- `MediaRecorder.AudioSource.VOICE_COMMUNICATION` is an ordinary *microphone* source tuned for VoIP. It never carries the far party of a cellular call. **[DOC]** (same AudioSource page). With the phone on speaker it would pick up the far party only acoustically.

### Which permissions the `shell` uid holds [DOC, with a caveat]

The AOSP Shell package manifest ([packages/Shell/AndroidManifest.xml](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/packages/Shell/AndroidManifest.xml)) declares, among others: `CAPTURE_AUDIO_OUTPUT`, `MODIFY_AUDIO_ROUTING`, `CAPTURE_VOICE_COMMUNICATION_OUTPUT`, `MODIFY_PHONE_STATE`, `CAPTURE_MEDIA_OUTPUT`, `RECORD_AUDIO` (fetched through a summariser, so it is a **[DOC]** reading of the file, but I did not read the XML line by line; AOSP `main`, not a vendor build). Samsung's Shell package is not AOSP's file; **[GUESS]** that it keeps these.

### Why an app cannot simply be granted them [RECALL]

The permissions are `signature|privileged` (some with `role`/`appop` extras). `pm grant` only works for runtime and `development`-flagged permissions, so for an ordinary app it fails with a "not a changeable permission type"-style error. The shell *holds* the permission itself; it does not hand it on. Hence Shizuku-style tools run the capture code **in a process started as the shell uid** (via `app_process`), not in the app's own uid. The probe script runs `pm grant` precisely to show the real message.

### Known working approaches

| Approach | What it shows | Evidence |
|---|---|---|
| scrcpy `--audio-source=voice-call`, `voice-call-uplink`, `voice-call-downlink` | The shell-uid server can request call sources. Audio forwarding needs Android 11+. Its default `output` and `playback` (Android 13+) are playback capture, which "Android apps can opt-out" of; call audio is a separate source. The doc gives no call-specific limitations. | [DOC] [scrcpy audio.md](https://github.com/Genymobile/scrcpy/blob/master/doc/audio.md) |
| ShizuCallRecorder (GPL-3, F-Droid) | A non-root call recorder using "an advanced list of permissions given to the shell application", separate uplink/downlink in stereo. Its table: Android 12+ minimum, 12-13 basic (Pixel may work for VoIP), 14-17 listed as supported. Uses hidden APIs, "may break on new releases or OEM modifications". Samsung not mentioned. | [SECONDARY] [README](https://github.com/kitsumed/ShizuCallRecorder). The fetched page did not name the audio source or permissions. |
| BCR (root/Magisk or privileged install) | Uses `VOICE_CALL` where the device supports it; "some devices don't support" it, with a mic+speaker fallback; needs foreground service. | [SECONDARY] BCR forks' READMEs from search results |
| Samsung's own call recorder, Cube ACR | System/privileged; Cube-style apps need accessibility or a mic+speaker workaround. | [GUESS] from the above; not verified |

None of these *injects* audio into the call. Nothing found does.

### What blocks it

- **Framework permission** (`CAPTURE_AUDIO_OUTPUT`): certain for an app uid [DOC].
- **audioserver policy**: AudioPolicy checks the capture permission and the privileged/foreground priority rules above [DOC]; a refusal surfaces as `SecurityException`, a failed `AudioRecord` init, or `IllegalStateException` on start [GUESS on which one, by version]. The prototype reports whichever comes.
- **Vendor HAL**: whether the modem/DSP exposes the voice RX/TX tap to the AP at all is a hardware/HAL matter. Many devices historically return silence or only the near-end (the BCR "some devices don't support `VOICE_CALL`" note and the 2019 StackOverflow reports of exceptions are the evidence; both **[SECONDARY]**). **[GUESS]** that Samsung Exynos/Snapdragon devices on One UI 6-8 vary by region and SoC. Voice over IMS (VoLTE/VoWiFi) decodes in the modem or in IMS client code depending on SoC, which may bypass a tap; unproven.
- **SELinux**: the shell domain's access to the audio HAL is policy-controlled; **[GUESS]** it is allowed because scrcpy works on at least some phones, but unverified per vendor.
- **Android 13-17, One UI 6-8**: ShizuCallRecorder lists 14-17 as supported, which is the only version evidence, and it is a README table, not a test log I can reproduce.

### Self-managed ConnectionService (VoIP) [DOC-level reasoning, [GUESS] on details]

A self-managed `ConnectionService` is for *the app's own* VoIP calls, with the app owning the audio. It cannot adopt a carrier call. It would serve a future GeekBridge-to-GeekBridge VoIP feature, not mirroring the iPhone/cellular call, so it is not an answer to this request.

## 2. Wiring this into GeekBridge if capture works [GUESS]

Even in the best case (shell helper started over Wireless debugging, downlink reads real audio): the phone would stream downlink to the PC, but the PC mic still has nowhere to enter the call. So the product would be listen-only. A "call" tool that cannot speak is a worse experience than the speaker feature in section 3. Do not build this beyond the probe unless an uplink route appears.

## 3. Speakerphone acoustic bridge [reasoned, not tested]

- Downlink to PC: phone on speaker, GeekBridge streams the phone mic (`VOICE_COMMUNICATION`, AEC) to the PC. The mic picks up the far party's voice via air; the phone's AEC is built to *remove* the speaker signal from the mic during a call, and Android gives a normal app's mic input to the call first ("the call always receives audio", [DOC]; a second ordinary app may receive silence). So even this half is unreliable. **[GUESS]**: low quality, may be silent on some devices.
- Uplink from PC: the only way in is to play PC-mic audio from the phone speaker into the phone's own mic. That is an acoustic loop with the cellular call's AEC fighting it; it also needs the speaker on and is audible to anyone nearby. Not workable as a product feature. **[reasoned]**
- Verdict: a documented last resort at most ("hear the far end through your PC while the phone sits on speaker"), not a calling feature.

## 4. USB route

- Android as host: Android acts as USB host for USB audio class headsets, UAC1 subset (PCM 16/24/32-bit, mono/stereo), automatically routing playback and capture to the peripheral by audio policy; UAC2 not targeted. ([AOSP USB digital audio](https://source.android.com/docs/core/audio/usb)) **[DOC]**. Whether the *telephony voice call* routes to a USB headset on a given phone (Samsung included) is not stated there. **[GUESS]**: it usually does (USB headsets work for calls), but test.
- So the phone would need the **PC to look like a USB audio class headset** (UAC gadget mode: Linux `f_uac1`/`f_uac2` via configfs, seen in the kernel sources returned by search, [SECONDARY]). That requires a **USB device controller (UDC)** on the PC: single-board computers have one; ordinary x86 desktops and laptops do not (their ports are host-only). **[GUESS, well established in general knowledge]**
- Windows/macOS have no built-in gadget mode at all. **[GUESS]**
- The AOSP page also describes the old Android "accessory mode" audio (16-bit/44.1 kHz stereo, output only to a host, "not currently recommended for new designs"), which is the wrong direction anyway. **[DOC]**
- Verdict: not viable for a desktop app. It could be a hobbyist feature with a Raspberry Pi or similar acting as the "headset", out of scope.

## 5. What vendors actually use

- **Microsoft Phone Link**: calls via Bluetooth; PC mic and speakers used; requires BT paired/connected and HFP support; no dual SIM, no iPhone headset relay. **[SECONDARY]** (community and third-party summaries from search; the Microsoft support URL I tried returned 404). The repo's own `docs/research/windows-calling.md` documents the Windows HFP API side **[MS]**.
- **Apple Continuity (iPhone calls on Mac)**: Apple's own relay; Bluetooth for proximity, Wi-Fi/iCloud for the data. One low-authority blog says Wi-Fi carries the call audio; **I could not verify that against an Apple source. [GUESS]** It is a closed Apple-to-Apple stack, which is the key point: it works because Apple owns both ends, and it is not available to a third-party app talking to an iPhone or Android.
- **Huawei/Honor Multi-Screen Collaboration**: answer calls on the PC with the PC speaker/camera; Bluetooth and WLAN must both be on; the support pages describe routing to earbuds but not the audio transport. ([Huawei: answering calls on PC](https://consumer.huawei.com/en/support/content/en-us15894561)) **[DOC]** for setup; the transport is **unknown**, and an OEM-private stack is the natural reading. **[GUESS]**
- **Samsung Flow, Intel Unison, Vivo/Oppo**: I found no primary source this session. Unknown; do not rely on any claim about them. Unison for iPhone is described elsewhere as Bluetooth-based **[GUESS]**.
- Pattern: the open, cross-vendor route is Bluetooth HFP. Wi-Fi call audio exists only inside vendor-owned stacks on both ends (OEM system apps with system privileges).

## 6. Prototype (debug builds only)

- `android/app/src/debug/.../CallAudioProbe.kt`: hidden switch `call_audio_experiment` (default off, stored in debug prefs, set through `DebugControlReceiver` with `--es what call_audio_experiment --ez on true`; `--es what call_audio_probe` runs one probe). It opens an `AudioRecord` on `VOICE_DOWNLINK` for 5 s, keeps only RMS/peak, sends nothing. Result codes shown in the debug state dump (`geekbridge-ctl phone-state`) under "Calls": `DISABLED`, `NO_RECORD_AUDIO`, `SECURITY_EXCEPTION`, `ILLEGAL_STATE`, `SILENCED_BY_POLICY` (client silenced by the foreground rule), `SILENT` (RMS 0), `AUDIO_PRESENT`.
- `RECORD_AUDIO` is declared in the **debug** manifest only; the release manifest is unchanged.
- `scripts/realdevice-call-audio-probe.sh`: walks the owner through Wireless debugging, tries `pm grant ... CAPTURE_AUDIO_OUTPUT` and prints the failure, arms the switch, waits for the call, runs the probe, reads the result with `phone-state`, and, if `scrcpy` is installed, also tries the shell-uid `voice-call-downlink` route into `~/.cache/geekbridge-call-audio-probe/`.
- Tested: `CallAudioProbeTest` (classification of silence / audio / policy silencing / no samples), `gradlew :app:testDebugUnitTest :app:lintDebug` both exit 0 (the first lint run caught an API 29 call, now guarded). Real call: not tested. Emulator calls: see the next section; they say nothing about a Samsung phone.
- Expected on the real phone: the app-uid probe reports `SECURITY_EXCEPTION` or `ILLEGAL_STATE` ([GUESS], strongly). If it ever says `AUDIO_PRESENT` the whole analysis above needs rewriting.

## Emulator results (measured 2026-11-04)

**The emulator's voice HAL is not Samsung's (or any phone's) modem. These rows show how Android's permission and policy layers behave and which results are false positives; they do not show that a real call can be captured.** Setup: fresh AVDs from the repo's SDK, own adb server (port 16101), console ports 5610/5612, headless, `-no-audio`; call state from `adb emu gsm call 5551234` + `gsm accept 5551234` (Telecom/telephony registry showed `mCallState=2`, offhook). App: this branch's debug APK, `RECORD_AUDIO` granted, `call_audio_experiment` ON, app in the foreground. Shell-uid route: scrcpy v5.0.1 prebuilt (SHA-256 checked against the release's SHA256SUMS), `--no-video --no-playback --audio-source=...`, 20 s, levels via `ffmpeg volumedetect`.

| Image | Probe in app uid, no call | Probe in app uid, call active | `pm grant ... CAPTURE_AUDIO_OUTPUT` | scrcpy shell-uid `voice-call-downlink` / `voice-call-uplink` / `voice-call`, call active | `adb root` |
|---|---|---|---|---|---|
| API 36 `google_apis` (userdebug, dev-keys) | `ILLEGAL_STATE` | `ILLEGAL_STATE` (`AudioFlinger could not create record track, status: -1`; `AudioRecord` init error -20) | `SecurityException: Permission android.permission.CAPTURE_AUDIO_OUTPUT is managed by role` | server started, file written; mean -9.0 / -8.9 / -9.0 dB, max 0.0 dB (loud) | not run (userdebug) |
| API 36 `google_apis_playstore` (user, release-keys) | `ILLEGAL_STATE` | `ILLEGAL_STATE` (same two log lines) | same `SecurityException ... managed by role` | same: file written; mean -9.0 / -8.9 / -9.0 dB, max 0.0 dB | `adbd cannot run as root in production builds` |

Not run: `NO_RECORD_AUDIO` (only with the grant withheld), `SECURITY_EXCEPTION` (Android reported the refusal as a failed `AudioRecord` init, not a thrown exception, on this image), `SILENCED_BY_POLICY`, `SILENT`, `AUDIO_PRESENT` from the app uid. Not run: API 34, a root-uid probe, the `app_process` helper, any uplink injection (AudioPolicy/AudioMix, `REMOTE_SUBMIX`; scrcpy PRs #6439/#7004, issue #3880 were not read or tried).

What these rows do establish:

1. **A normal app cannot read `VOICE_DOWNLINK`**, call or no call, on both a userdebug and a production-signed API 36 image: the framework refuses to create the record track. The probe reports this as `ILLEGAL_STATE`. [measured, emulator]
2. **`pm grant` of `CAPTURE_AUDIO_OUTPUT` fails** and Android's own words are "is managed by role", i.e. it is a role-held permission on API 36, not something `adb` can hand to an app uid. [measured; supersedes the earlier "signature permission" recall]
3. **The shell uid is accepted by audioserver for the voice-call sources**: scrcpy's server captured on both images without error, on the production image too (no root needed). [measured]
4. **That shell-uid capture is not evidence of call audio.** A control run with *no call* gave the same loud signal (`voice-call-downlink`, no call: mean -8.9 dB, max 0.0 dB), while the plain `mic` source gave about -77 dB. So the emulator HAL feeds a synthetic signal into the voice-call sources whether or not a call exists. Do not treat "non-zero audio" as success without a no-call control and listening to the recording. [measured]

Consequence for the probe: `AUDIO_PRESENT` means only "non-zero samples". On a real phone, always run the same probe once with no call and compare before believing it (the script's no-call step should be added to the owner's procedure).

Cleanup: emulators stopped, the three scratch AVDs deleted, the unused API 34 playstore image uninstalled. The API 36 playstore system image stays in the shared SDK for the next run. Raw logs and recordings are in `/home/ambsd/.cache/gb-wifi-out/` (not committed).

## 7. Open questions, cheapest first

1. Run the probe script on the Fold5 during an iPhone-to-phone call (owner, real device). Records the app-uid result and the scrcpy shell-uid result. This is the single measurement that turns most **[GUESS]** items into facts.
2. Does the scrcpy recording contain far-party speech on One UI 8 / Android 16-17, over VoLTE and over Wi-Fi calling?
3. Is there any uplink route at all for the shell uid (e.g. `MODIFY_AUDIO_ROUTING` plus an audio policy mix)? Not found; not investigated beyond desk search.
4. Does a Samsung phone route call audio to a USB UAC headset? Only relevant if a gadget device were ever in scope.

## Sources

- [MediaRecorder.AudioSource](https://developer.android.com/reference/android/media/MediaRecorder.AudioSource)
- [Sharing audio input](https://developer.android.com/media/platform/sharing-audio-input)
- [InCallService](https://developer.android.com/reference/android/telecom/InCallService)
- [AOSP Shell manifest](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/packages/Shell/AndroidManifest.xml)
- [AOSP USB digital audio](https://source.android.com/docs/core/audio/usb)
- [scrcpy audio.md](https://github.com/Genymobile/scrcpy/blob/master/doc/audio.md)
- [ShizuCallRecorder](https://github.com/kitsumed/ShizuCallRecorder)
- [BCR call recorder](https://github.com/c4pt00/BCR-call-recorder)
- [Huawei Multi-Screen Collaboration calls](https://consumer.huawei.com/en/support/content/en-us15894561)
- Phone Link, Continuity: search-result summaries only (elevenforum.com threads, macworld/imore articles), low authority
- In-repo: `docs/research/windows-calling.md`, `docs/research/macos-calling.md`, `docs/call-control.md`
