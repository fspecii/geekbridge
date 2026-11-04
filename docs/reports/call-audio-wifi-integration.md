# Call audio over Wi-Fi: integration status

Date: 2026-11-04. EXPERIMENTAL, off by default, listen-first, debug phone builds only. Bluetooth stays primary.
Background: [design](../design/call-audio-over-wifi.md), [prototype results](call-audio-wifi-prototype.md).

## Done in this change (tested on the host only)

* New workspace crate `crates/bridge-callwifi`: wire format, three-consent gate, Opus codec, jitter buffer, helper
  framing, self-test verdict (moved out of `experiments/callaudio-wifi`, which now re-exports it).
* Gate now listens only; "speak back" is a separate `geekbridge.callaudio.speak` step that needs the phone's own opt-in
  and a phone-reported uplink route. The phone can switch it off alone. New `ready` packet (helper up, self-test verdict).
* Self-test verdicts in plain words: works / silent / refused / unsure (loud with no call, so it proves nothing).
* Hostile tests: no packet sequence opens a gate whose consents are missing; mutation fuzzing covers `ready` and `speak`.
* Control API types (`Request::CallWifi`, `CallWifiView`), consent `call_audio_wifi`, phone grant `callaudio`,
  `docs/privacy.md` tables. The daemon answers `CallWifi` with an empty view (placeholder).
* `scripts/callaudio-enable.sh`: PC-assisted start of the helper over adb (not run against a phone yet).

## Not done (do not rely on it tomorrow)

* Phone: Rust media hub, JNI, Settings > Experimental screen, in-app self-test runner, notification, broadcast receiver
  `CallWifiReceiver` that the enable script talks to. Until it exists the script reports the app did not take the helper.
* Computer: daemon hub, PipeWire playback through the shared `bridge-calls` device choice, call-card button,
  `geekbridge-ctl callaudio-wifi`, doctor lines, i18n, goldens.
* Emulator e2e for the integrated path, the real-phone checklist section, release-isolation additions, in-app pairing
  (libadb-android is GPL-3.0-or-later / Apache-2.0 dual and fits the licence, but adds Conscrypt, several MB).

## Certain vs unproven

Certain: the gate, wire and codec behaviour above (unit and fuzz tests). Unproven: everything on a real Fold5, in
particular whether Samsung silences `VOICE_DOWNLINK` for the shell user and whether any uplink route reaches the far party.
