# Call audio over Wi-Fi (ADB helper): design, protocol and threat model

**Status: EXPERIMENTAL. Off by default. Bluetooth hands-free stays the primary, supported path for call audio.**
Nothing here is wired into the service or the release UI yet. The code lives in
`experiments/callaudio-wifi/` (a standalone crate outside the main workspace) and in the Android **debug** source set
(`android/app/src/debug/java/dev/geekbridge/android/callaudio/`). Measured results: `docs/reports/call-audio-wifi-prototype.md`.

Tags: **[CERTAIN]** tested here or read in the platform source/docs; **[MEASURED-EMU]** measured on an emulator only;
**[GUESS]** reasoned, must be proven on a real phone.

## 1. The idea in one paragraph

Make the computer behave like a Wi-Fi "headset" for the phone call. On the phone, a small helper runs as the `shell`
user (uid 2000), started through Wireless debugging. `shell` holds call-audio permissions an app can never get
(`CAPTURE_AUDIO_OUTPUT`, `MODIFY_AUDIO_ROUTING`, `MODIFY_PHONE_STATE`, …). The helper reads the far party's voice
(`VOICE_DOWNLINK`) and, where the device allows it, plays the computer's microphone into the call. The GeekBridge app
carries both directions as Opus over the existing paired TLS link. On the computer the stream appears as a virtual
headset, **"GeekBridge Phone Call"** (a PipeWire sink and source), routed to the user's chosen headphones and microphone
by the same routing code as the Bluetooth path.

## 2. What Android allows, honestly

| Piece | Who can do it | Status |
| --- | --- | --- |
| Read the far party (`VOICE_DOWNLINK`, `VOICE_CALL`) | privileged/system apps, the `shell` uid; never a normal app (`pm grant` refuses) | Works on some phones (scrcpy, ShizuCallRecorder) **[CERTAIN for the permission model]**; silent on several Samsung builds **[SECONDARY]**; on a given Fold5 unknown **[GUESS]** |
| `AudioManager.getCallDownlinkExtractionAudioRecord` / `getCallUplinkInjectionAudioTrack` (SystemApi, API 33) | holders of `CALL_AUDIO_INTERCEPTION` while a call is up | Only if `shell` holds that permission and the HAL supports "call audio interception" (`isPstnCallAudioInterceptable`). Probed on the emulator; see the report |
| Play into the cellular uplink via the `TELEPHONY_TX` output ("in-call music") | the audio policy routes a track to `TELEPHONY_TX` with the `INCALL_MUSIC` flag only for permitted callers, during a call, on devices whose audio policy declares that output (common on Qualcomm, e.g. the Fold5's Snapdragon) **[GUESS from AOSP recall + root projects using the same `incall_music` path]** | The only plausible no-root uplink for a carrier call. Probe-only so far |
| AudioPolicy dynamic mix, role INJECTOR, loop-back (scrcpy #3880/#6439/#7004 technique) | `MODIFY_AUDIO_ROUTING` (shell has it) | Feeds **apps that record** (VoIP: WhatsApp, Signal, Teams) **[CERTAIN for the mechanism]**. A carrier call's uplink does not go through an `AudioRecord`, so it is not expected to reach the far party; one scrcpy tester confirmed "reached the phone, not the other caller" **[SECONDARY]** |
| `REMOTE_SUBMIX` capture | `CAPTURE_AUDIO_OUTPUT` | Captures a remote-submix *output*, not the call. Useful only as the loop-back half of the mix above |
| `setMode(MODE_IN_COMMUNICATION)` | anyone | Changes routing for VoIP; during a carrier call it would fight Telecom. Never used |
| Hidden `AudioSystem.setParameters` vendor keys, `tinymix` | root / vendor | Out of scope: root, per-SoC, per-ROM |

**Effective result to promise: listen-only (far party on the computer) where the phone allows capture; two-way only
on devices where a `TELEPHONY_TX`/interception route is proven, otherwise the user speaks into the phone.** The UI
self-test says which applies.

## 3. Components

```
 PHONE                                                              COMPUTER
 ┌──────────────────────────────┐   abstract unix socket   ┌───────────────────────┐   TLS (paired, pinned)   ┌──────────────────────────┐
 │ CallAudioHelper (uid 2000,   │ ◄──────────────────────► │ GeekBridge app        │ ◄──────────────────────► │ geekbridge-service       │
 │ app_process, from the APK)   │  token + peer-uid check  │ (Opus enc/dec, gate,  │  geekbridge.callaudio.*  │ (gate, jitter buffer,    │
 │ AudioRecord VOICE_DOWNLINK   │  PCM 16 kHz mono 20 ms   │  jitter buffer)       │                          │  Opus, PipeWire virtual  │
 │ AudioTrack → TELEPHONY_TX /  │                          └───────────────────────┘                          │  headset → chosen        │
 │ VoIP mix (where allowed)     │                                                                             │  headphones + mic)       │
 └──────────────────────────────┘                                                                             └──────────────────────────┘
```

* **Bootstrap.** Two ways, both explicit: (a) the user's computer runs `adb` once (`scripts/e2e/call-audio-emulator.sh`
  shows the exact commands); (b) self-ADB: the app pairs with its own adbd through Wireless debugging (pairing code
  typed on the phone), connects to `localhost`, and starts the helper with `app_process`. (b) needs an ADB client with
  TLS pairing (SPAKE2): `libadb-android` (dual GPL-3.0-or-later / Apache-2.0 per its README; ships Conscrypt, a
  native library of several MB per ABI **[GUESS on the exact size]**) or our own Rust implementation in the existing
  native library (rustls already in it; SPAKE2 over Ed25519 would be new crypto code). Not built yet.
* **Helper.** `CallAudioHelper.kt` (debug source set). Runs from the installed APK as classpath, no extra dex asset.
  Fixed command set (`start`, `stop`, `probe`), no shell passthrough, no files, no network socket (abstract unix socket
  only), first command must carry a per-launch random token, the peer must be uid 0, 2000 (adb forward) or the app's
  uid. Uses a `ContextWrapper` that presents the shell's own `AttributionSource` (`com.android.shell`, uid 2000);
  hidden APIs via reflection where needed (app_process is not subject to the hidden-API filter **[CERTAIN for
  app_process processes, as scrcpy relies on]**).
* **Transport.** `geekbridge.callaudio.*` packets on the existing link (section 4).
* **Computer side.** Jitter buffer + Opus decoder; PipeWire playback/capture (`pw-cat --raw`, media role
  `Communication`) into a virtual node "GeekBridge Phone Call". Routing to the chosen headset must reuse
  `crates/bridge-calls` (the Bluetooth path's routing), not a fork **[not done: that crate is being reworked by another
  agent]**.

## 4. Protocol: `geekbridge.callaudio.*`

GeekBridge-only, inside TLS only, declared as incoming capabilities only while the user's switch is on (kept out of the
plaintext identity like every `geekbridge.*` name). Phone incoming: `request, answer, frame, stats, stop`. Computer
incoming: `offer, frame, stats, stop`. A stock KDE Connect peer never sees any of it.

| packet | direction | body |
| --- | --- | --- |
| `geekbridge.callaudio.request` | computer → phone | `session` (computer user pressed "call audio here") |
| `geekbridge.callaudio.offer` | phone → computer | `session`, `dirs` (`["down"]` or `["down","up"]`), `codec:"opus"`, `rate:16000`, `frameMs:20`, `uplink` (`telephony_tx`/`voip_mix`/`none`) |
| `geekbridge.callaudio.answer` | computer → phone | `session`, `accept`, `dirs` (when accepted), optional `reason` (`no_consent`, `busy`, …) |
| `geekbridge.callaudio.frame` | both | `session`, `dir` (`down`/`up`), `seq` (u32, +1 per frame, wraps), `ts` (u32 sample index), `cap` (u32 sender ms, wraps), `opus` (base64, 1 to 400 bytes) |
| `geekbridge.callaudio.stats` | both | `session`, `dir`, `got`, `lost`, `late`, `depthMs` (receiver report, ~1/s) |
| `geekbridge.callaudio.stop` | both | `session`, optional `reason` (`user`, `call_ended`, `consent_off`, `timeout`, `link_lost`) |

Rules: `session` 1..2^53-1; strict on every field acted on, extra keys ignored; reasons `[a-z0-9_]{1,32}` or dropped;
frames for another session, the wrong direction, an unaccepted direction, or above 75 frames/s are dropped and counted.
Parser: `experiments/callaudio-wifi/src/wire.rs`; hostile-input tests in `tests/hostile.rs` (mutation fuzzing,
round-trip and "no consent, no action" oracles). Shared byte vectors pin the helper framing between Rust and Kotlin.

**Media.** Opus (RFC 6716), 16 kHz mono, 20 ms, VoIP application (SILK), 24 kbit/s, in-band FEC on (10 % expected
loss), DTX off. About 60 bytes/frame, ~150 bytes of JSON line per frame, 50 frames/s each way: ~8 KB/s per direction.

**Why the JSON link and not a separate channel (yet).** It reuses the authenticated link with no new port or key, and
the measured loopback latency is ~40 ms. Its weakness is TCP head-of-line blocking: a lost segment stalls every later
frame by one retransmission timeout. The jitter buffer sheds the backlog afterwards (measured in the report). If real
Wi-Fi shows long stalls, phase 2 is a dedicated datagram channel keyed from the TLS session (DTLS or an AEAD over UDP
with keys exported from TLS 1.3 `export_keying_material`), with the same frame body in binary.

**Latency budget (target < 150 ms mouth-to-ear, one way).** Phone capture buffer 20-40 ms (+ HAL; scrcpy saw 40-80 ms
per 960-sample read **[SECONDARY]**) + encode <1 ms + Wi-Fi 5-30 ms + jitter buffer 40-120 ms (adaptive: 2.5× the
RFC 3550 jitter estimate, 2..6 frames) + decode <1 ms + PipeWire output 20 ms (node latency). Echo cancellation is the
headset's / PipeWire's job on the computer (the phone's own AEC still runs on its own mic path).

**Jitter buffer.** `experiments/callaudio-wifi/src/jitter.rs`: plays in sequence order, conceals a gap (using the next
frame's FEC when it is already there), drops late frames, never holds more than `max_frames`, sheds one frame after
0.5 s above target+2, rebuffers after 5 empty pops, treats a sequence jump of 8× the ceiling as a new stream.

## 5. Consent and privacy rules

Three independent consents, all required, none implied by another (`experiments/callaudio-wifi/src/gate.rs`):

1. **Computer consent, per phone**: on the computer, for this paired phone only, off by default.
2. **Phone switch, per computer**: Settings → Experimental → "Call audio over Wi-Fi (ADB helper)", off by default. The
   opt-in screen says, in plain words: *"This lets GeekBridge capture the audio of your phone calls through Wireless
   debugging and send it to <computer>. Wireless debugging gives the helper powers far beyond call audio (it can do what
   `adb shell` can do). Turn it off when you are done. Recording calls may be illegal where you live without the other
   party's consent."*
3. **Explicit per-call start**: a press on either device during this call. Never automatic, never carried to the next
   call.

While audio flows: a persistent indicator on both devices (phone notification "Call audio on <computer>", computer
card "Phone call audio here") with a Stop button. Audio is never written to disk, never logged (logs carry counts and
levels only), never sent anywhere but the paired computer. Helper STATUS lines carry numbers only.

## 6. Threat model

| Asset / threat | Risk | Mitigation |
| --- | --- | --- |
| The call's content (both parties) | Eavesdropping by a LAN attacker | Only inside the paired, pinned TLS link; no new port; frames dropped outside a live session |
| | A malicious app on the phone talks to the helper | Abstract socket name is random per launch; first command must carry a random token; peer uid must be 0/2000/app uid |
| | A malicious paired computer pulls call audio | Needs phone switch + per-call press on the phone **or** a computer request answered only while the phone switch is on and a call is active; indicator always visible; stop on either side |
| | Audio left behind | Nothing stored; no logs of audio; the helper keeps no buffers beyond one frame |
| Shell-uid power | The helper is a powerful process | Fixed commands, no passthrough, no file or network access, exits with the session or the link; it is started only by an explicit user action |
| | Wireless debugging left on | adbd listens on the LAN while it is on; any paired adb key can use it. The UI tells the user to switch it off; Android 17 turns it off on untrusted networks on its own **[SECONDARY]** |
| | Pairing code theft | Pairing happens on the phone only; the code is never shown on the computer |
| Integrity of the call | Injection of unexpected audio into the call | Uplink frames accepted only for the live session and the accepted direction; the computer's mic is the only source; rate-capped |
| Denial of service | Frame floods, huge packets | 75 frames/s cap, 400-byte Opus cap checked before base64 decode, jitter buffer ceiling, idle timeout 5 s |
| Legal | Call recording/interception laws | Plain warning in the opt-in screen; no recording feature at all |

## 7. Open items (in order)

1. Real phone (Fold5, Android 17): does `VOICE_DOWNLINK` as shell carry the far party? Does the `TELEPHONY_TX` or
   interception route exist and reach the far party? (`--probe`, see the report's real-phone plan.)
2. Self-ADB pairing client (libadb-android vs. Rust), then the Settings → Experimental switch and the opt-in screen.
3. Wire the gate/codec/jitter into `bridge-android` (phone) and `geekbridge-service` (computer); present the virtual
   headset through `bridge-calls` routing.
4. Dedicated datagram channel if TCP stalls hurt on real Wi-Fi.
