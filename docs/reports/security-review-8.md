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
