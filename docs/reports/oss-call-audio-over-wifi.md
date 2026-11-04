# Open-source phone-call audio on a computer over Wi-Fi: survey and recommendation

Date: 2026-11-04. Question: the owner believes some open-source project already carries phone-call audio to a computer
over Wi-Fi (no PC Bluetooth). What exists, how does it work, and what can GeekBridge build?

Tags: **[SOURCE]** read directly on the cited page or API; **[SECONDARY]** reported by a search summary or a third
party; **[GUESS]** my inference. Repository dates come from the GitHub API on 2026-11-04. No code was copied; only
methods were read. GeekBridge is GPL-3.0-or-later: MIT/Apache/GPL-3.0 sources are compatible for *reading and
re-implementing*; GPL-2.0-only (KDE Connect) and GPL-3.0-only (BCR) would be a problem for copying, and proprietary
sources must never be copied.

## Short answer

1. **Nobody has shown full-duplex cellular-call audio on a PC over Wi-Fi on a stock, unrooted phone.** The things that
   come closest are two different halves, in two different projects.
2. **Hearing the call (downlink) on a PC over Wi-Fi, no root, is real.** scrcpy 3.2+ (`--audio-source=voice-call`,
   `voice-call-downlink`) and ShizuCallRecorder do it by running as the ADB **shell** user, which holds
   `CAPTURE_AUDIO_OUTPUT`. This overturns the verdict in `docs/call-audio-options.md` ("impossible without root"): that
   document considered only installable apps, not a process launched through wireless debugging. It works on carrier
   calls; it is unreliable for Wi-Fi calling/VoIP and on some Samsung builds (see table).
3. **Speaking (uplink) is the hard half.** The only no-root uplink technique found is an `AudioPolicy` mix that injects
   PC audio into apps that *record from the mic* (scrcpy PRs #6439, #7004, issue #3880). A tester reported it reached the
   phone but **not the remote party** of a call [SOURCE]. It plausibly works for VoIP apps; for a cellular call, whose
   mic path runs through the modem/codec and not an `AudioRecord`, I expect it not to work [GUESS].
4. **Full duplex has only been demonstrated with root and Qualcomm mixer control**: `pulpoff/gsm2sip` and
   `s-xander/Android-sip-gateway` bridge GSM calls to SIP over Wi-Fi by driving ALSA (`incall_music`, `VOC_REC_*`).
   This is probably what the owner heard about. It needs Magisk or a custom ROM and per-device profiles.
5. Every vendor "call on PC" product (Phone Link, Intel Unison, Sefirah, Samsung Flow, Honor/Huawei) uses Bluetooth HFP
   or a privileged OEM stack. Apple's Continuity is the only mainstream feature documented as Wi-Fi-based and it is
   closed.

## Survey table

| # | Project (URL) | Licence | Last push | Mechanism / privilege | Downlink | Uplink | Android / devices | Samsung A14-17 | Reusable (method only) |
|---|---|---|---|---|---|---|---|---|---|
| 1 | **scrcpy** <https://github.com/Genymobile/scrcpy> (v5.0.1, 2026-10-08) | Apache-2.0 | 2026-11-02 | `app_process` as ADB shell uid 2000; `MediaRecorder.AudioSource` VOICE_CALL / VOICE_UPLINK / VOICE_DOWNLINK, shell holds `CAPTURE_AUDIO_OUTPUT` (maintainer-side explanation, issue #5412). Wireless via `adb connect` | Yes: `voice-call-downlink` (remote side), `voice-call` (both, mixed) [SOURCE doc/audio.md, PR #5870] | `voice-call-uplink` exists but testers report it is **not separated** from downlink (Pixel 8 Pro A15, others) [SOURCE PR #5870]. Phone still plays call audio; scrcpy cannot mute it [SOURCE #6359, Pixel 9 A16] | Sources need Android 12+ (A11 needs unlocked screen); voice-call sources added in 3.2. Works: Redmi K70 Pro A15 (samples flow, even with no call), Pixel 8/9. Not VoIP/Wi-Fi calling (#5412, non-rooted A13 failed) | Galaxy S22 Ultra A14, scrcpy 3.2: call audio only on phone, stream was silent [SOURCE PR #5870]. No S23-S25 report found. [GUESS] Samsung dialer likely sets a capture policy | Apache-2.0 compatible with GPL-3.0-or-later; method documented here |
| 2 | **scrcpy client-audio injection** PR #6439 / #7004, issue #3880 (open, not merged; maintainer: "not soon", 2026-08-31) | Apache-2.0 | n/a | `AudioPolicy` mix (hidden API via reflection) on shell identity (`MODIFY_AUDIO_ROUTING`), Opus over a 4th socket, matches capture presets MIC/VOICE_COMMUNICATION/etc. into a REMOTE_SUBMIX loopback [SOURCE] | n/a | PC mic reaches *apps recording from the mic*. In a real call: "PC mic reached only the phone, not the other caller" [SOURCE PR #6439 comment]; Snapdragon phone kept using its internal mic [SOURCE] | Needs Android 13+; Android 16 added a package-name check (fix: fake app context). Reported working: realme GT 6T/ColorOS 16, POCO X7 Pro/HyperOS, OnePlus Nord 6/OxygenOS 16, Pixel-class [SOURCE PR #7004] | No Samsung report | Method only. Latency "high" per testers |
| 3 | **ShizuCallRecorder** <https://github.com/kitsumed/ShizuCallRecorder> | GPL-3.0 + s.7 extra terms (name/package reserved) | 2026-11-03 | Shizuku (wireless-debugging start or root) gives shell permissions; v2.0.0 (PR #132, merged 2026-10-20) dropped scrcpy for native `AudioRecord` via reflection; sources: VOICE_CALL, up+down mix, mic-voice-communication + downlink, playback capture [SOURCE] | Yes, carrier/VoLTE calls; README matrix: A12-13 "Basic", A14-17 "Yes" | Not applicable (records only), stereo split uplink/downlink | Android 12+ (11 limited) | Note 20 Ultra A13 and S23 Ultra: mic-only [SOURCE issue #13]; Galaxy A53 A16: WhatsApp fixed in debug build; **S25 Ultra, Android 17 / One UI 9 (AT&T)**: voice-call and up+down **silent**, only mic+downlink gave the user's own voice; maintainer suspects the dialer sets capture policy NONE; new issue requested [SOURCE]. Wi-Fi calling preferred: nothing recorded | GPL-3.0 with extra s.7 terms: do not copy; learn only |
| 4 | **pulpoff/gsm2sip** <https://github.com/pulpoff/gsm2sip> (v1.4.14) | MIT | 2026-10-24 | Magisk module installs a privileged system app (`CAPTURE_AUDIO_OUTPUT`, `MODIFY_PHONE_STATE`, `READ_PRECISE_PHONE_STATE`), `tinymix`/`tinycap` ALSA control; call audio out via `incall_music` into `Telephony Tx`, in via `VOICE_DOWNLINK`/`VOC_REC_*`; SIP over Wi-Fi to Asterisk [SOURCE README] | Yes | **Yes** (full duplex) | Poco X3 NFC (A12, MIUI 14 and LineageOS) and Galaxy S4 Mini (LOS 16/A9) verified; Pixel 4a contributed, unverified; **Galaxy S10e (Exynos): no audio path in either direction** | Not usable on stock Samsung; requires root + per-SoC mixer profile | MIT: compatible; method only |
| 5 | **s-xander/Android-sip-gateway** <https://github.com/s-xander/Android-sip-gateway> | README says MIT, GitHub shows no licence file [SOURCE] | 2026-01-29 | Root + SELinux permissive, tinyalsa via JNI: `VOC_REC_DL` capture, `Incall_Music` injection, `InCallService` call hook; privapp permissions XML [SOURCE] | Yes | Yes | One tested device: Redmi Note 7, LineageOS 17.1; Qualcomm only | No | Method only; licence ambiguous, do not copy |
| 6 | **BCR** <https://github.com/chenxiaolong/BCR> (v3.11) | GPL-3.0-only | 2026-10-15 | System app via Magisk/KernelSU/custom ROM; `VOICE_CALL` + `CONTROL_INCALL_EXPERIENCE`; stereo uplink/downlink split "only known on newer Pixels" [SOURCE] | Record only | n/a | A9+ | No Samsung statement | Confirms permission model. Shizuku not mentioned |
| 7 | **Sefirah** (Windows) <https://github.com/shrimqy/Sefirah>, Android <https://github.com/shrimqy/Sefirah-Android> | GPL-3.0 | 2026-10-18 / 2026-10-15 | View/answer calls, Windows Bluetooth calling since 3.0.0; README needs a Bluetooth device paired to the PC [SOURCE README, SECONDARY release notes]. No call audio over the network documented | via Bluetooth | via Bluetooth | Calls `NotSupported` on Linux (repo `docs/research/competitors.md`) | n/a | GPL-3.0: ok to learn; not an answer to the question |
| 8 | **open-android-dex** <https://github.com/dotnetdreamer/open-android-dex> (v0.3.32) | GPL-3.0-or-later | 2026-11-02 | Notification-listener "Answer/Decline" via the dialer's notification buttons; app has no call permission [SOURCE README]. Audio routing described only for app windows | No | No | A8+ | n/a | Control only, no audio |
| 9 | **KDE Connect** <https://github.com/KDE/kdeconnect-android> / **GSConnect** <https://github.com/GSConnect/gnome-shell-extension-gsconnect> | GPL-2.0 (both, per API) | 2026-11-03 / 2026-10-18 | Telephony plugin: ring/missed notifications, mute ringer. No call control, no call audio; users ask for HFP [SECONDARY: UBports and KDE Discuss threads] | No | No | n/a | n/a | Nothing to learn for audio. GPL-2.0-only code must not be copied |
| 10 | **ShizuCallRecorder-like others**: Cube ACR, "Call Recorder" apps | Closed | n/a | Accessibility + mic/speakerphone fallback; Samsung blocks third-party recorders since S9 per a Samsung staffer [SECONDARY] | Acoustic only | Mic | n/a | Blocked | Acoustic hack; not viable |
| 11 | **Gammu, asterisk chan_mobile / chan_quectel, USB modems** | GPL | n/a | Need a modem/Bluetooth HFP or a separate GSM dongle with its own SIM; do not use the phone's SIM | n/a | n/a | n/a | n/a | Out of scope (different SIM) [GUESS: not re-verified in this pass] |
| 12 | **Waydroid / Anbox** | GPL/Apache | n/a | Containerised Android on the PC has no modem/telephony | n/a | n/a | n/a | n/a | Not applicable [GUESS, not researched deeply] |
| 13 | Closed vendor stacks: **Microsoft Phone Link** (Bluetooth HFP, PC must have Bluetooth; mono HFP audio) [SECONDARY, Microsoft Q&A]; **Intel Unison** (Bluetooth pairing mandatory, no calls while a BT headset is on the PC) [SOURCE Intel guide via search]; **Samsung Flow** (audio reported to stay on phone) [SECONDARY]; **Honor Connect / Huawei Share** (Bluetooth and same account for pairing; call audio path undocumented) [SOURCE vendor pages, via search]; **Samsung "Call & text on other devices"** (same Wi-Fi network required for calls, account based, tablets/watches not PCs) [SOURCE Samsung support]; **Apple "Calls on Other Devices"** (same Wi-Fi, same Apple Account, Bluetooth on; audio transport undocumented) [SOURCE Apple support]; **AirDroid** (dial pad; audio via phone) [SECONDARY]; **Pushbullet** (SMS/notifications only) [SECONDARY]; Android Auto wireless uses Bluetooth HFP for calls [GUESS, not re-verified]; Chromebook Phone Hub: no call audio [GUESS] | Closed | n/a | Do not copy; HFP or privileged OEM paths | | | | | |

Also searched with nothing useful: XDA/Reddit/HN threads (answers are always "use Bluetooth", "forward to Google
Voice", or "speakerphone"), F-Droid (ShizuCallRecorder is the only call-audio-adjacent entry), GitLab (axet
android-call-recorder issue #134 documents one-sided capture on Android 8+).

## What the evidence says about the mechanism

- Android documents `VOICE_CALL`, `VOICE_UPLINK`, `VOICE_DOWNLINK` as requiring `CAPTURE_AUDIO_OUTPUT`, "reserved for
  system components" [SOURCE developer.android.com `MediaRecorder.AudioSource`, via search]. The sharing-audio-input
  guide lists privileged apps and accessibility services as the only capturers of a voice call [SOURCE].
- The ADB shell package holds that permission. scrcpy's server runs as uid 2000, so it records the voice-call sources
  (yume-chan, issue #5412, 2025-11-04) [SOURCE]. One tester confirmed with `dumpsys` that the permission is held, yet
  still failed on a non-rooted Android 13 phone, so the permission is necessary, not sufficient [SOURCE].
- Cellular vs VoIP: Wi-Fi calling and VoIP streams are treated as more protected and are not captured by the voice-call
  sources (ShizuCallRecorder maintainer, issue #13) [SOURCE]. VoIP is captured, when at all, through playback capture
  plus mic, a different path.
- Samsung is the weak point: Note 20 Ultra A13 mic-only; S22 Ultra A14 silent; S25 Ultra on Android 17 silent. No
  evidence either way for a Fold5 on Android 17 specifically; it must be tested [UNKNOWN].
- Latency/quality: voice-call sources emit no packets in silence and may be narrowband (rom1v suspected 8 kHz,
  unconfirmed); a 960-sample read took 40 to 80 ms on tested devices [SOURCE PR #5870]. Fine for a call; PTS handling
  needs care.
- Uplink to the *remote caller* needs the modem to take audio from somewhere other than the codec mic. In the root
  projects this is done with ALSA mixer controls (`incall_music` to `Telephony Tx`) which are Qualcomm- and
  HAL-specific [SOURCE]. There is no shell-level equivalent found in any project [GUESS: may exist in Samsung's HAL, not
  researched].

## Ranked recommendation for GeekBridge

Honest framing first: we can credibly build **"hear the call on the PC over Wi-Fi"** (downlink), probably not
**"talk into the call from the PC"** over cellular. Do not advertise two-way audio until a Fold5 test proves it.

1. **Self-ADB helper for downlink (build, behind an explicit opt-in).** The Android app pairs with its own adbd through
   Wireless debugging (the Shizuku/LADB pairing-code flow, notification reply, TLS pairing then a `connect` to
   localhost or the Wi-Fi IP), starts a small `app_process` helper as uid 2000, and the helper opens `AudioRecord` on
   `VOICE_DOWNLINK` (fallback `VOICE_CALL`), sends Opus frames over the existing GeekBridge link to the PC player.
   Cost: moderate. Value: remote side of cellular calls on the PC with no PC Bluetooth. Risk: Samsung silence (probe at
   runtime and say so), VoIP/Wi-Fi calling unsupported, helper dies on reboot or when wireless debugging toggles.
   Gate it on a self-test: capture 2 s during a call, check RMS, otherwise report "this phone blocks call capture".
2. **Uplink stays "needs Bluetooth" in the UI** until item 3 proves otherwise. The acoustic fallback (phone on speaker, PC mic nowhere near) has no PC-to-phone path, so it is not an option [GUESS].
3. **Uplink injection experiment (research spike only).** Reimplement the `AudioPolicy` mix described in scrcpy #3880
   as a time-boxed test on the Fold5 during a real cellular call. Expected result: works for VoIP apps, not for the
   carrier call [GUESS]. If it works, it is a major finding; if not, record the negative result.
4. **Recommend a USB Bluetooth adapter** for full duplex (already in `doctor` text). It is the only path any product
   documents for two-way call audio to a PC.
5. **Do not pursue**: root/Magisk gateways (gsm2sip style; Knox trip, per-SoC mixer profiles, S10e had no path), SIM
   over IP / srsRAN, Waydroid, chan_mobile.

## Security analysis of the self-ADB route

What the user must grant: Developer options, Wireless debugging, a pairing code typed into our notification, and
(for the helper) a one-time "allow this computer/app" ADB authorisation. The helper then holds **shell uid** powers.

| Risk | Detail | Mitigation |
| --- | --- | --- |
| Over-privilege | A shell-uid process can read other apps' data via `run-as`-like surfaces, install packages, change settings, record the screen/audio, and read call audio | Helper is a single small `dex` with a fixed command set (start/stop capture), no shell passthrough, no network listener; talks to the app over a local-only socket with a random per-launch token |
| Eavesdropping of calls | Capturing call audio is sensitive and legally regulated; the stream leaves the phone | Only after per-device telephony consent (already off by default), only during a call the user toggled, TLS/Noise link already used by GeekBridge, visible persistent notification while capturing, no recording to disk, no logs of audio |
| Persistent ADB trust | Pairing leaves a long-lived adb key on the phone; while Wireless debugging is on, adbd listens on the LAN and any paired key can use it | Pair only the app's own key; keep Wireless debugging off except during start; Android 17 "ADB Wi-Fi 2.0" turns it off on untrusted networks [SOURCE 9to5google], which also means it can stop on its own; surface that state honestly |
| Stolen pairing code / LAN attacker | Pairing window is open for as long as the dialog is up | Do pairing on the phone only, time-limit the window, never display the code on the PC |
| Fragility | Hidden-API reflection (ShizuCallRecorder README warns of breakage); reboot kills the helper (Shizuku docs: restart after every reboot) [SOURCE] | Version-gated capability probe, graceful "unsupported" state, no retry storms |
| Android policy | Some OEMs auto-disable Developer options (MIUI Security scan [SOURCE Shizuku docs]); Android 17 changes to Wireless debugging are only partly documented and I found nothing on app self-connect [UNKNOWN] | Test on the Fold5 first; keep it feature-flagged |
| Play policy | Irrelevant: we sideload (F-Droid/APK), so no Play review of `READ_CALL_LOG`, accessibility or hidden-API use. It does not remove the user-facing risks above | Say so plainly in the docs |
| Licensing hygiene | scrcpy (Apache-2.0) and gsm2sip (MIT) are compatible with GPL-3.0-or-later for learning; ShizuCallRecorder is GPL-3.0 with extra terms and BCR is GPL-3.0-only: read only; do not copy | Write the helper from the Android API docs |

## Unknowns to resolve on the real Fold5 (Android 17)

1. Does `VOICE_DOWNLINK`/`VOICE_CALL` as shell return non-silent audio on a Fold5 during a VoLTE call? (The S25 Ultra on
   Android 17 reported silence; the S22 Ultra on Android 14 too.)
2. Is self-connect to Wireless debugging allowed on Android 17 with ADB Wi-Fi 2.0 (trusted-network gating), and does it
   survive screen-off and Wi-Fi roaming?
3. Does the `AudioPolicy` injection reach a cellular call? Expected no.
4. Does Samsung's dialer set a capture policy that blocks shell capture? (ShizuCallRecorder maintainer's theory only.)
5. The claim that Apple and Samsung carry call audio over Wi-Fi is from vendor support pages only; the transport is not
   documented in anything I could read.
