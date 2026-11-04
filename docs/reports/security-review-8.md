# Security review 8: experimental Wi-Fi call audio

Scope: `crates/bridge-callwifi`, `crates/bridge-service/src/callwifi.rs`, the phone hub
(`crates/bridge-android/src/transport/engine/callwifi/`), the debug-only phone code
(`android/app/src/debug/.../callaudio/`), `scripts/callaudio-enable.sh` and the `geekbridge.callaudio.*`
packets in the KDE/TLS link. Commits 7592f37cc..bbced385f.

## Fixed

1. **Offer could pre-grant speak back (computer gate).** An `offer` with `dirs` containing `up` made the computer
   accept with `up: true` and record the session as speaking, skipping the separate Speak step. The microphone
   thread still only starts from an explicit speak state, so no audio left, but the gate state and the answer were
   wrong. The computer now always accepts listen-only. Test: `an_offer_claiming_up_does_not_turn_the_microphone_on`.
2. **Speak off answered with on.** The computer matched a speak answer only on the session id, so a phone replying
   `on: true` to an off request turned the microphone on. An answer may now turn speaking on only if on was asked.
   Test: `a_phone_cannot_answer_speak_off_with_on`.
3. **Helper listener window.** The 90 s timeout was per `accept`, so a local app that kept connecting (and failing
   the token) held the listener open indefinitely; the pre-token read now has 2 s instead of 5 s. There is one
   overall 90 s deadline. Switching the experiment off or the STOP broadcast now also closes a listener still
   waiting, which makes its token unusable.
4. **Hidden indicator.** If the user blocked GeekBridge notifications (or the channel), `notify` does not throw, so
   the "being shared" indicator would silently not show. Sharing now ends with a message in that case.
5. **Runtime loop could die.** An exception in the event loop ended it, and the loop is what notices a call ending
   and drives the idle timeout. Each iteration is now guarded.

## Checked, no defect found

- Consent: the phone gate needs the phone switch, a live call and a request or press; the hub additionally needs
  the per-computer grant and the self-test result. The computer gate needs its per-phone consent for every packet
  except an offer, which is declined. Frames need a live session, the right id and direction, and the rate cap
  (75/s). The existing property test (`no_packet_sequence_opens_a_gate_without_its_consents`) covers arbitrary orders.
- Session end: link loss, grant revocation, consent off, call end, user stop, 5 s idle and an unanswered offer
  (10 s) all end it; the computer side drops the session on `link_down`.
- Listener: 127.0.0.1, random port, 128-bit one-time token from `SecureRandom`, constant-time compare, one helper,
  token not reused (a new listener makes a new token), HELLO frames limited to 4 KiB.
- `CallWifiReceiver` and `DebugControlReceiver` require `DUMP` (shell only); `CallWifiStopReceiver` is not exported.
  The STATUS hook returns numbers and words only.
- Leakage: no `Log`, file, or `tracing` use in the call audio code; audio only passes frame by frame.
- Resource use: Opus packets are capped at 400 bytes, helper frames at 4096, jitter buffer and uplink queue are
  bounded, the decoder is fuzzed.
- Release isolation: `scripts/check-release-isolation.sh` passes on the existing release APK (built 2026-11-03;
  re-run after the next release build). The `callwifi` cargo feature is only enabled by the debug build path.

## Residual risk

- The helper runs as the shell user and the token is visible in its command line to the shell and to the user
  with adb; that is inherent to the design and debug-only.
- Another local app can still connect to the loopback port during the window and cause a failed helper start (a
  denial of service for the setup, not access). Re-running the script fixes it.
- While an offer is pending (up to 10 s) the helper may already be capturing on the phone without the indicator;
  no frame leaves until the computer accepts.
- The new Kotlin changes are covered by compile only (the runtime needs an Android device); on-device check
  belongs in the real-phone checklist.

## Speak addendum (hear and speak, commits up to 3ca3e5768)

Scope: the `speak` packet and microphone frames (`gate.rs`, `crates/bridge-service/src/callwifi.rs` mic thread and
echo guard, `real.rs` uplink queue), the debug-only uplink code (`UplinkTest.kt`, `HelperSession` INJECT,
`CallAudioHelper.openRoute`, `CallWifiRuntime.startSpeaking`) and the release isolation check.

### Fixed

6. **"Off" could be refused while an "on" was in flight (computer gate).** `ask_speak` allowed one request at a
   time, so pressing Speak off before the phone answered an earlier "on" returned an error. The phone's late "on"
   answer then opened the microphone after the user had asked for it to be closed. "Off" now always goes through
   and replaces the pending request; the late "on" is dropped as unasked. Tests:
   `off_pressed_before_the_phone_answers_on_wins` (gate), `off_pressed_before_the_phone_answers_keeps_the_microphone_closed`
   (hub).
7. **The release isolation check could pass by accident.** `scripts/check-release-isolation.sh` ran
   `strings | grep -q` under `pipefail`. `grep -q` exits at the first match, `strings` then dies of SIGPIPE, the
   pipeline reports failure and the `if` takes the "clean" branch: a debug dex could pass. Found by running the
   script on the debug APK as a negative control (the dex rows did not fail). All rows now use `grep -c` into
   `/dev/null`; the control now fails on the debug APK for the dex rows, the manifest and the native library. The
   dex pattern also names the new uplink code (`UplinkTest`, `UplinkRoutes`, `PcmInjector`, `TrackInjector`, the
   route names, `getCallUplinkInjectionAudioTrack`).

### Checked, no defect found

- **Consent for speak.** Three independent steps, none implied by another: the computer's per-phone consent and a
  live listening session, the computer user's Speak press, and on the phone its separate "Speak (experimental)"
  opt-in plus an uplink route that the self-test or the "Test speaking" answer produced. The phone grants only if
  all hold (`s.up = on && speak_allowed && uplink != none`) and answers with what it granted. An offer that claims
  `up`, an unasked `speak on`, and a `speak on` answer to an off request are all refused (tests existed for the last
  two; a new fuzz test, `no_packet_inside_a_live_listen_only_session_turns_speaking_on`, throws arbitrary packets at
  a live listen-only pair and asserts no `SpeakState(true)`, no accepted microphone frame, no microphone frame
  sent). Hub tests: `a_phone_cannot_switch_the_microphone_on_by_itself`, `speaking_is_a_separate_step_only_with_an_uplink_and_only_while_on`.
- **Phone side, defence in depth.** `CallWifiRuntime.startSpeaking` checks the opt-in and a valid route again on
  its own, before any audio reaches the helper, so a Rust-side mistake alone cannot inject. Turning the opt-in off
  stops injecting and sends `speak off` at once (`speak_stops_when_the_phone_opt_in_goes_off`).
- **Session end.** Link loss, call end, consent off, user stop and the peer's stop each clear speaking on both
  gates and close the microphone thread: new `speaking_never_survives_an_ending` (gate, with replayed
  `speak on`/microphone frame afterwards) and `every_ending_closes_the_microphone_at_once` (hub: link down,
  consent off, phone stop, user stop; no microphone frame is sent afterwards). On the phone the uplink queue is
  cleared on speak off and on stop, the inject thread leaves when `speaking` goes false, the helper closes the
  injector on `stop` and on disconnect.
- **Echo guard.** `auto` fails closed: if the output or microphone cannot be identified the guard is on; only a
  headset-like output or an echo-cancelling microphone turns it off, and the user can force it off or on. While on,
  frames that overlap the far party (plus a 300 ms tail) are zeroed, not skipped, so timing holds. Test:
  `the_echo_guard_silences_the_microphone_while_the_far_party_talks_unless_off`. The user forcing it off is the
  user's explicit choice and is shown on the card.
- **Hostile microphone frames (computer to phone).** Frames are accepted only for the live session id, direction
  `up`, and only while the phone's gate has speaking granted; Opus payload at most 400 bytes (base64 length checked
  first), 75 frames per second (new `microphone_frames_are_rate_capped_on_the_phone`), the decoder returns exactly
  one 20 ms frame for any input (fuzzed), the phone's uplink queue holds 5 frames and drops the oldest, the
  receiver report is numbers only. Helper INJECT frames must be exactly 648 bytes (8 header + 320 samples) with flags 0 or 1 and an
  authenticated helper; a failing route is counted, not fatal.
- **"Test speaking".** Runs only from the screen's button (or the shell-only `CallWifiReceiver`, which needs `DUMP`).
  No packet from the computer can start it. It needs the helper up, no session or test running, and the helper
  itself refuses unless the audio mode is in-call (`MODE_IN_CALL`), so no beeps can play outside a call. It plays
  nothing on its own: `uplink_check` opens and releases a route without writing. Only route names and counts go
  into the status JSON (`clean` strips quotes and backslashes).
- **Helper token.** Unchanged by the speak work: 128-bit one-time token, loopback, one overall 90 s window,
  constant-time compare, INJECT and every command require the authenticated state.
- **Release isolation.** On a fresh `assembleRelease` build at 3ca3e5768 (native library built without the
  `callwifi` feature) the fixed script passes, and a direct `strings` search of both dex and both native libraries
  finds none of: the uplink classes, route names, `uplink_test`, `callwifi`, `speakOptIn`, `geekbridge.callaudio`,
  the Opus codec or jitter code. The debug APK fails the same script on every row, as it should.

### Residual risk

- The Speak request has no timeout: if the phone never answers an "on", the computer shows no speaking and the user
  can only retry after pressing off (which now works) or stopping. Fails closed, a usability matter only.
- "Test speaking" plays audible beeps into a real call on a tap; the screen says so before, and the other person
  has to agree. It cannot be started remotely.
- The echo guard set to off, or `auto` choosing a headset by name, is a heuristic: a name containing "headset"
  that is really speakers would let the far party hear themselves. The call card shows the guard state.
