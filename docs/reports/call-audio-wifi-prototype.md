# Call audio over Wi-Fi (ADB helper): prototype and emulator results

Date: 2026-11-04. **EXPERIMENTAL, off by default, not in the product.** Bluetooth hands-free remains the primary,
supported call-audio path. Design, protocol and threat model: [design/call-audio-over-wifi.md](../design/call-audio-over-wifi.md).
Related: [call-audio-over-wifi-feasibility.md](call-audio-over-wifi-feasibility.md) (desk spike and the other agent's
`ShellProbe` emulator tables), [oss-call-audio-over-wifi.md](oss-call-audio-over-wifi.md) (open-source survey).

Marks: **[CERTAIN]** measured here (command and evidence below); **[EMU-ONLY]** measured, but only on an emulator whose
voice path is synthetic, so it says nothing about a real call's audio; **[GUESS]** not measured.

## Verdict

1. **The transport half works end to end [CERTAIN].** A shell-uid helper on an emulator streamed PCM through
   `adb forward` into the lab, which carried it as Opus over the real GeekBridge TLS link code (pinned certificates,
   TLS 1.3), through the jitter buffer, out to a sink; the computer's tone went back the other way into the helper's
   injection route. Latency (capture stamp to jitter-buffer output): **35-80 ms p50**, 40-120 ms p95 on a clean link;
   ~140 ms p50 / 160 ms p95 with 40 ms added delay and 3 % TCP-retransmit stalls. Under the 150 ms budget except the
   worst chaos tail.
2. **The computer half works against PipeWire [CERTAIN].** In a private PipeWire daemon (never the user's session) the
   far party's 523 Hz tone came out of the "headset" null sink at 523 Hz, and a 660 Hz tone played into the "headset
   mic" null sink reached the phone side at 660 Hz (p50 56 ms, p95 108 ms).
3. **The shell uid can open every call source on all emulator images, including the user (production-like) builds
   [EMU-ONLY].** `VOICE_DOWNLINK`, `VOICE_UPLINK`, `VOICE_CALL` give a loud synthetic signal (~620 Hz, RMS ~15 000)
   **even with no call**: the emulator's audio HAL generates it. So these numbers prove *permission*, not content.
   A no-call control is mandatory on a real phone too.
4. **New on-device route: Android 13+'s call-audio-interception API is open to the shell uid on these images
   [EMU-ONLY].** `shell` holds `CALL_AUDIO_INTERCEPTION`; `isPstnCallAudioInterceptable()` is true; during a call (audio
   mode `IN_CALL`) `getCallDownlinkExtractionAudioRecord` returned audio and `getCallUplinkInjectionAudioTrack`
   returned a track that accepted the computer's frames (253 of 254 written). Outside a call both refuse with
   "not available in mode MODE_NORMAL". This is the platform's own, documented-for-system-apps way to put audio *into*
   the cellular uplink (it is what call-screening features use), and it is the most promising uplink route found. Whether
   the far party hears it on a phone is **unknown [GUESS]**: the emulator has no far party, and the uplink loop test
   (inject 1 kHz, record `VOICE_UPLINK`) did not see the tone on the emulator, whose uplink tap is synthetic.
5. **The AudioPolicy VoIP mix (scrcpy #3880 technique) works from the shell uid [EMU-ONLY]:** a 1 kHz tone injected
   through a loop-back INJECTOR mix reached a `VOICE_COMMUNICATION` recorder (RMS 6950 vs. 3 without). That feeds VoIP
   apps (WhatsApp, Signal, Teams), not a carrier call, matching the other agent's `ShellProbe` result.
6. **Wireless debugging cannot be switched on from the shell on the emulator** (`settings put global adb_wifi_enabled 1`
   is reset to 0; no TLS port). The self-ADB bootstrap therefore needs the user to turn it on in Settings and pair
   with a code, as designed. No pairing client was built (see "Not done").

## What was built

| Piece | Where | Tested by |
| --- | --- | --- |
| `geekbridge.callaudio.*` wire format, strict parser | `experiments/callaudio-wifi/src/wire.rs` | unit tests; mutation fuzzing (30 000 inputs, round-trip and no-consent oracles) in `tests/hostile.rs` |
| Three-consent gate (computer per phone, phone switch, per-call start), indicator, rate cap, idle timeout | `src/gate.rs` | unit tests |
| Opus 16 kHz mono 20 ms, VoIP/SILK 24 kbit/s, FEC, pure Rust (`opus-pure` 0.2.2, BSD-3-Clause, no C, no deps) | `src/codec.rs` | tone round trip, garbage/loss always yields one frame, 5 000 random packets |
| Adaptive jitter buffer (RFC 3550 jitter, 2..6 frames, FEC on gaps, shed, rebuffer, restart, wrap) | `src/jitter.rs` | 8 unit tests + 50 000-step random arrivals |
| Helper framing (Rust twin of the Kotlin one, shared byte vectors) | `src/helper.rs` | unit tests, mutation fuzzing |
| PipeWire I/O (`pw-cat --raw`, role Communication, nodes "GeekBridge Phone Call") | `src/pcm.rs` | `scripts/e2e/call-audio-pipewire.sh` |
| Lab: phone + computer in one process over `bridge-kde` TLS endpoints, optional `bridge-chaos` proxy | `src/lab.rs`, `src/bin/lab.rs` (`geekbridge-callaudio-lab`) | 2 lab tests (clean, chaos), both e2e scripts |
| Shell-uid helper (`app_process` from the installed APK; abstract socket, per-launch token, peer-uid check, fixed start/stop/probe) | `android/app/src/debug/java/dev/geekbridge/android/callaudio/` | `HelperProtocolTest` (vectors, token refusal, relay both ways with fakes); emulator e2e |
| Emulator e2e | `scripts/e2e/call-audio-emulator.sh` (+ experimental CI suite `call-audio-wifi`) | itself |

The crate is deliberately **outside the workspace** (own `[workspace]`, own `Cargo.lock`): nothing in the default
build, release, notices or `cargo deny` sees it. The helper is in the Android **debug** source set only;
`scripts/check-release-isolation.sh` now also rejects the helper classes in a release APK (checked: an existing
release APK passes).

**Opus choice.** `opus-pure` 0.2.2: BSD-3-Clause (GPL-compatible, on the `deny.toml` allow list), zero dependencies,
no build script, so it cross-compiles to Android like the rest of the Rust core. Measured on this machine: ~49-61
bytes/frame at 24 kbit/s, encode 0.2-0.36 ms and decode 0.03-0.08 ms per 20 ms frame (release), ~550 KB added to an
x86_64 `opt-level=z`, LTO, stripped binary (ARM size not measured [GUESS: similar]). It has 164 `unsafe` sites
(SIMD/DSP) and is young; the alternative is the `opus` crate (MIT/Apache) over system/bundled libopus (BSD-3), which
needs a C build for Android. Decoder robustness is covered by our hostile tests, not by an audit.

**libadb-android** (for the in-app self-ADB later): dual GPL-3.0-or-later / Apache-2.0 per its README (fine for us),
pulls Conscrypt (`conscrypt-android` 2.5.3 AAR is 3.75 MB with all ABIs), BouncyCastle and `spake2-android`; the
README warns it is unaudited and has an LGPL dependency. Not added.

## Emulator results

Command: `scripts/e2e/call-audio-emulator.sh` (own AVD per image, own console/adb ports, `-no-audio`, emulated call
with `adb emu gsm call`/`accept`; evidence under `dist/android/logs/call-audio-emulator/<stamp>/`, JSON and logs only).

Images: **36g** = `android-36;google_apis` (userdebug, `adb root` works: upper bound); **36p** =
`android-36;google_apis_playstore` (user build, production-like: lower bound). **34p** (`android-34` playstore) was
in the plan but its system image had been removed from the shared SDK before my run; the other agent's `ShellProbe`
tables in the feasibility report cover API 34 production for capture and the mic mix.

| Check (shell uid via `app_process` unless noted) | 36g userdebug | 36p user | Mark |
| --- | --- | --- | --- |
| `shell` holds `CAPTURE_AUDIO_OUTPUT`, `CAPTURE_VOICE_COMMUNICATION_OUTPUT`, `MODIFY_AUDIO_ROUTING`, `MODIFY_PHONE_STATE`, `CALL_AUDIO_INTERCEPTION`, `RECORD_AUDIO` | all granted | all granted | [CERTAIN] (`dumpsys package com.android.shell`) |
| `TELEPHONY_TX` output in the audio policy / as an `AudioDeviceInfo` | present / yes | present / yes | [CERTAIN] |
| No call: `VOICE_DOWNLINK`, `VOICE_UPLINK`, `VOICE_CALL` | audio, RMS ~15 000, ~620 Hz (synthetic) | same | [EMU-ONLY] |
| No call: `MIC`, `VOICE_COMMUNICATION` | RMS 3 (silence) | RMS 3 | [CERTAIN] |
| No call: `REMOTE_SUBMIX` | silent | silent | [CERTAIN] |
| No call: interception API (downlink record, uplink track) | refused: "not available in mode MODE_NORMAL" | same | [CERTAIN] |
| `isPstnCallAudioInterceptable()` | true | true | [CERTAIN] |
| Emulated call off hook (`mCallState=2`) | yes | yes | [CERTAIN] |
| In call, mode `IN_CALL`: `getCallDownlinkExtractionAudioRecord` | audio (run 1) | audio | [EMU-ONLY] |
| In call, mode `IN_CALL`: `getCallUplinkInjectionAudioTrack` | track (run 1) | track | [CERTAIN that the platform hands it out; EMU-ONLY otherwise] |
| In call, mode still `NORMAL` (run 2, 36g): interception API | refused (mode race, see notes) | n/a | [CERTAIN] |
| Uplink loop: 1 kHz into the injection track, seen in `VOICE_UPLINK`? | not seen (ratio 0.0) | not seen | [EMU-ONLY]: the emulator's uplink tap is synthetic; inconclusive |
| AudioPolicy INJECTOR loop-back mix → `VOICE_COMMUNICATION` recorder | 1 kHz received (RMS 6950) | 1 kHz received | [CERTAIN] for VoIP-style recorders |
| Root probe (`adb root`) | same as shell | n/a (user build) | [CERTAIN] |
| `pm grant <app> CAPTURE_AUDIO_OUTPUT` | message not captured in these runs (fixed in the script); the other agent saw "managed by role" | | |
| Chain: helper tone → adb forward → Opus/TLS lab → jitter → sink | 440 Hz, 289 frames, p50 80 / p95 119 ms | 440 Hz, p50 80 / p95 120 ms | [CERTAIN] |
| Chain: computer 1 kHz → helper injection, `voip_mix` | 247 of 251 frames written | 157 of 251 | [CERTAIN] (written to the track, not heard by anyone) |
| Chain: `VOICE_DOWNLINK` + inject `telephony_tx` | played 279, p50 55 ms; injected 257/269 via the preferred-device track | played 284, p50 80 ms; injected 245/259 via the interception track | [EMU-ONLY] |
| Chain: `DOWNLINK_EXTRACTION` + inject `telephony_tx` | run 1: played 293, p50 35 ms, injected 253/254 (interception); run 2: refused (mode NORMAL) | played 270, p50 39 ms, injected 253/257 (interception) | [EMU-ONLY] |
| Chain under chaos (+40 ms, 3 % 120 ms stalls) | 440 Hz, p50 140 / p95 161 ms, 25 lost of 396, 2 rebuffers | 440 Hz, p50 140 / p95 160 ms, 3 rebuffers | [CERTAIN] |
| `settings put global adb_wifi_enabled 1` from the shell | reset to 0, no TLS port | stays 1, but no TLS port (no adbd TLS listener) | [CERTAIN] |

Notes:
* `mode` is `AudioManager.getMode()` at probe time. The call was off hook (`mCallState=2`) in every run, but the audio
  mode was `IN_CALL` in one 36g run and `NORMAL` in another: Telecom moves the mode a moment after the call goes
  active. The interception API only answers in `IN_CALL`; the script now waits up to 30 s for it (added after
  these runs, not yet exercised).
* Root (`adb root`, userdebug only) changed nothing: the shell uid already gets everything root gets here.
* Synthetic signal: the phone side measured ~620 Hz; after Opus at 24 kbit/s the computer side's dominant bin was
  480 Hz. The HAL's test signal is harmonic-rich (peak 32734, RMS ~15000, nearly a square wave) and SILK at 16 kHz
  reshapes its spectrum; with a pure tone the frequency is preserved exactly (440 → 440, 523 → 523).

## Latency, CPU, battery

| Measurement | Value | Mark |
| --- | --- | --- |
| Lab loopback, clean (capture stamp → jitter-buffer out) | p50 40 ms, p95 41-42 ms | [CERTAIN] |
| Emulator helper → adb forward → lab, tone | p50 80 ms, p95 119-120 ms | [CERTAIN] (adb forward adds a hop the product will not have) |
| Emulator, call sources | p50 35-55 ms, p95 40-60 ms | [EMU-ONLY] |
| Chaos: +40 ms, 3 % stalls of 120 ms | p50 139-140 ms, p95 160-161 ms; ~6 % concealed, 2 rebuffers in 8 s | [CERTAIN] |
| Chaos: +30 ms, 5 % stalls of 200 ms | p50 146 ms, p95 246 ms, 14 rebuffers in 10 s | [CERTAIN]: TCP head-of-line blocking; the case for a datagram channel |
| Not included | Android capture buffer (scrcpy reports 40-80 ms per read on phones), PipeWire output (20 ms node latency), Wi-Fi | [GUESS] adds 60-120 ms on a phone |
| CPU (both ends + TLS in one process, x86_64) | 4.6-6 % of one core for both directions | [CERTAIN] |
| Codec per 20 ms frame | encode 0.2-0.36 ms, decode 0.03-0.08 ms (x86_64) | [CERTAIN]; on a phone big core [GUESS] 2-4× |
| Battery | phone: helper + one encode + one decode per 20 ms, ~8 KB/s each way over an already-open socket, Wi-Fi kept awake for the call. [GUESS] 3-6 % per hour of call on top of the call itself; not measured | [GUESS] |

## Real-phone test plan (owner, Fold5)

Everything below is opt-in and leaves nothing behind. Do it on a test call with someone who agrees to it.

1. **Install a debug build** of this commit (or later) on the phone: `scripts/build-android.sh` then
   `adb install -r dist/android/geekbridge-debug.apk`.
2. **Enable Wireless debugging** on the phone (Settings → Developer options → Wireless debugging), pair the computer
   once (`adb pair <ip>:<pairing-port>` with the code shown on the phone), then `adb connect <ip>:<port>`.
3. **No-call control** (expected: on a real phone `VOICE_*` should be silent or refused without a call):
   ```
   adb shell 'CLASSPATH=$(pm path dev.geekbridge.android | sed s/^package://) app_process /system/bin dev.geekbridge.android.callaudio.CallAudioHelper --probe'
   ```
   One JSON line. Note each source's `outcome`/`rms`, `pstnInterceptable`, `telephonyTxDevice`, `voipMix`.
4. **During a call** (answer, wait until the call timer runs, keep the phone at the ear or on speaker silently), run
   the same command. Decisive fields:
   * `sources[VOICE_DOWNLINK]` and `sources[DOWNLINK_EXTRACTION]`: `audio` with an RMS that moves when the other
     person talks and stays near 0 when they are silent = downlink capture works. `silent`/`refused` = this phone
     blocks it (Samsung reports in the survey say so for some models).
   * `uplinkInjectionTrack.outcome`: `track` = the platform lets the shell inject into the call uplink.
   * `uplinkLoop`: `tone_in_uplink` would be a strong sign; `tone_not_seen` is inconclusive.
5. **Hear it and speak it.** On the computer:
   ```
   NAME=gbca$RANDOM; TOKEN=t$(od -An -N8 -tx8 /dev/urandom | tr -d ' ')
   adb shell "CLASSPATH=\$(pm path dev.geekbridge.android | sed s/^package://) app_process /system/bin dev.geekbridge.android.callaudio.CallAudioHelper --listen $NAME --token $TOKEN" &
   PORT=$(adb forward tcp:0 localabstract:$NAME)
   cd experiments/callaudio-wifi && cargo build --release
   target/release/geekbridge-callaudio-lab --seconds 60 --helper 127.0.0.1:$PORT --source DOWNLINK_EXTRACTION \
       --token $TOKEN --inject telephony_tx --down-pw-target <your headset sink name> --up-pw-target <your mic source name>
   ```
   (`pw-cli ls Node | grep node.name` lists sink/source names. With your normal session's WirePlumber the nodes
   link themselves to the targets.) Expected if everything works: you hear the other person in the headset within
   ~0.2 s, and **they hear you**. Ask them. Repeat with `--source VOICE_DOWNLINK`, and with `--inject voip_mix`
   (expected: they do not hear you on a carrier call). Try once over VoLTE and once over Wi-Fi calling.
   Stop with Ctrl-C, then `adb forward --remove-all` and turn Wireless debugging off.
6. **Send back**: the JSON lines from steps 3 and 4, the lab's final JSON, and one sentence: "heard them yes/no,
   they heard me yes/no".

What each answer means: downlink audio + they hear you → two-way over Wi-Fi is possible on this phone; build the
product path. Downlink only → listen-only feature at most. Neither → Bluetooth only (as today).

## Not done (and why)

* **In-app self-ADB pairing client** (Wireless debugging pairing code, TLS + SPAKE2, connect to localhost, start the
  helper): not built; evaluated libadb-android above. Until then the helper is started from a computer with `adb`.
* **Wiring into the product**: Settings → Experimental switch and opt-in screen, `bridge-android` (phone) and
  `geekbridge-service` (computer) handling of the packets, the "GeekBridge Phone Call" virtual node routed through
  `crates/bridge-calls` (being reworked by another agent; not forked). Deliberately left until a real phone proves
  step 4/5 above.
* **Datagram channel** for media: only if real Wi-Fi shows TCP stalls like the 5 %/200 ms chaos case.
* **Emulator far-end audio**: the emulator's modem carries no voice and its HAL synthesises the voice taps, so a real
  far-end tone could not be injected; host-audio routing was not used (it would touch the owner's real PipeWire).
