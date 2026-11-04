# Call audio over Wi-Fi: integration status

Date: 2026-11-04. EXPERIMENTAL, off by default, listen-first, debug phone builds only. Bluetooth stays primary.
Background: [design](../design/call-audio-over-wifi.md), [prototype results](call-audio-wifi-prototype.md).

**Update 2026-11-05:** the phone side, Speak end to end (uplink routes, "Test speaking" beeps, echo guard, uplink
numbers), the in-call card's Speak button and `geekbridge-ctl callwifi setup` are built; the "Not done" list below is
partly outdated. Current state, the honest route table and the real-phone procedure:
[call-audio-wifi-realphone.md](call-audio-wifi-realphone.md).

## Done in this change (tested on the host only)

* New workspace crate `crates/bridge-callwifi`: wire format, three-consent gate, Opus codec, jitter buffer, helper
  framing, self-test verdict (moved out of `experiments/callaudio-wifi`, which now re-exports it).
* Gate now listens only; "speak back" is a separate `geekbridge.callaudio.speak` step that needs the phone's own opt-in
  and a phone-reported uplink route. The phone can switch it off alone. New `ready` packet (helper up, self-test verdict).
* Self-test verdicts in plain words: works / silent / refused / unsure. `works` needs a valid no-call control (the capture opened and delivered samples) that was quiet.
  `unsure` = the control was loud, refused, empty or missing (e.g. the android-36p emulator refuses `DOWNLINK_EXTRACTION` outside a
  call, so a loud in-call signal cannot be told from a test signal); treated as not working, never offers Listen.
* Hostile tests: no packet sequence opens a gate whose consents are missing; mutation fuzzing covers `ready` and `speak`.
* Control API types (`Request::CallWifi`, `CallWifiView`), consent `call_audio_wifi`, phone grant `callaudio`,
  `docs/privacy.md` tables. The daemon answers `CallWifi` with an empty view (placeholder).
* `scripts/callaudio-enable.sh`: PC-assisted start of the helper over adb (not run against a phone yet).

## Not done (do not rely on it tomorrow)

* Phone: Rust media hub, JNI, Settings > Experimental screen, in-app self-test runner, notification, broadcast receiver
  `CallWifiReceiver` that the enable script talks to. Until it exists the script reports the app did not take the helper.
* Computer, still open: a golden screen for the call card (the fixtures `GEEKBRIDGE_CALLS_FIXTURE=active-wifi |
  listening-wifi | speaking-wifi` exist; the screen list lives in `scripts/golden-desktop.sh`), PipeWire playback
  proven against a real linked sound server (see "Computer side" below).
* Emulator e2e for the integrated path, the real-phone checklist section, release-isolation additions, in-app pairing
  (libadb-android is GPL-3.0-or-later / Apache-2.0 dual and fits the licence, but adds Conscrypt, several MB).

## Certain vs unproven

Certain: the gate, wire and codec behaviour above (unit and fuzz tests). Unproven: everything on a real Fold5, in
particular whether Samsung silences `VOICE_DOWNLINK` for the shell user and whether any uplink route reaches the far party.

## Computer side (added 2026-11-04, tested on the host only)

* `bridge-service::callwifi`: the hub. One `Gate` per phone (computer side), at most one live session in the service,
  a jitter buffer and Opus decoder per session, a 20 ms playback worker that exists only while a session does (no idle
  thread or wake-up), the receiver report once a second, the five-second idle stop, the call-ended stop (from the
  phone link's call state), the consent-off stop and the link-down stop. A second phone's offer is declined `busy`;
  an offer that crosses the user's stop is declined `stopped`. Without the `call_audio_wifi` consent every packet is
  dropped unread, except an `offer`, which the gate declines with `no_consent` so the phone can say why.
* Packets: `geekbridge.callaudio.*` are in the service's capabilities (`gate.rs`), classified `Incoming::CallWifi`
  and mapped to `Feature::CallAudioWifi`; a stock peer never sees or sends them.
* Device choice: `bridge_calls::audio::endpoint` resolves the very pick the Bluetooth path uses (`call-audio.json`,
  `route::resolve`, so an unplugged pick falls back to the default). Playback is `pw-cat --target <node>` and the
  microphone for Speak is the same through `pw-cat -r`; Linux with PipeWire only, everything else says why not.
* Speak is its own step: it exists only when the phone's `ready` said an uplink route and its own opt-in; the
  microphone is opened only after the phone agrees and closed at once on off, stop, or the phone's own off.
* UI: the live-call card in the Calls window (`call_wifi_ui.rs`): "Listen here (Wi-Fi, experimental)", a red
  listening box with Stop, "Speak (experimental)" and a microphone warning. The box also shows with no call row on
  screen. The small ringing card (`--overlay`) is unchanged: it closes when the call is answered, before there is
  anything to listen to.
* `geekbridge-ctl callwifi [status | listen [ID] | stop | speak on|off] [--json]`, `doctor` lines (silent when no
  phone offers the feature), `Request::Calls` carries the same view for the card (`CallsStatus.wifi`).
* Tests: hub unit tests with a stand-in sound output (tone in, tone out, every stop path), and
  `tests/callwifi_api.rs`: a real daemon and a real TLS stand-in phone (negotiation, consent, press, offer, frames,
  stop paths).

Unproven on the computer: `pw-cat` argument handling was checked against a private PipeWire without a session manager
(the arguments are accepted); that the stream is linked to the chosen sink, the latency and the glitch behaviour on a
real WirePlumber setup are not measured. Nothing here has run against the phone engine or a real phone.
