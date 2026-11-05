# Answering, declining and ending phone calls from the computer

What works, how it is wired, what it needs, and what has **not** been tried on a real phone.

## What the user gets

- **Incoming call.** The Calls window (`geekbridge-calls`) shows a popup with the caller (name from the phone's
  contacts if allowed, else the number, else "Unknown caller"), **Decline** and **Answer** buttons and keyboard
  shortcuts: `Enter` / `A` answers, `Esc` / `D` declines. The window asks for attention and focus when a call starts
  to ring. Independently of any window, the service raises a desktop notification with the same two actions on
  Linux (`notify-send`, libnotify 0.8+ for actions; without it no notification is shown).
- **Ongoing call.** A panel under the toolbar shows the caller, a timer, **Hang up** (`H`), **Mute** (`M`, only where a
  backend can really mute) and the **audio route**: "Audio on this computer (mSBC)", "Moving the audio to this
  computer…", or "Audio on the phone" with the reason. **Use computer audio** appears only when the Bluetooth
  backend declares audio-link control.
- **Keypad (phone menus).** While a call is live and answered, the panel has a **Keypad** button (the in-call card has one
  too; it opens the Calls window on the keypad, because the card is a small window that never takes the keyboard). The 12
  keys (`0-9 * #`) each play one tone into the call, and digits typed on the keyboard do the same while the keypad is
  open. It exists only when the service offers it (see "Keypad tones" below), and `geekbridge-ctl call tone KEYS` does the
  same from a script.
- **Honest controls.** A button exists only if the service offered a target for it (`CallRow.answer/reject/hangup`).
  If a ringing call cannot be answered from here the popup says why ("Answer on the phone. To answer from here, allow
  GeekBridge to answer calls in the phone app's setup.") instead of showing a dead button.
- **Scripts.** `geekbridge-ctl calls [--reveal]` (`calls --trace [--json] [--clear]` shows the redacted call-event trace), `call answer|reject|hangup`, `call mute on|off`, `call audio`,
  `call dial NUMBER [--yes]` (numbers masked unless `--reveal`).
- **Dialing.** The keypad's Call button opens a confirmation sheet ("Call +49... with Pixel?", saying where the voice
  will be); only its Call button sends the request. `geekbridge-ctl call dial NUMBER` asks `[y/N]` on a terminal and
  refuses without `--yes` when there is none. Digits with an optional leading `+` only: `*` and `#` (USSD/MMI codes)
  are refused on this path. It is offered only while no call is going on, Telephony is allowed for the phone and the
  phone reported `canDial` (Android `CALL_PHONE`). When it is not, the Call button and the CLI say which of these is
  missing.
- **No Bluetooth controller.** The window shows (and `geekbridge-ctl doctor` / `diagnose` say) that call control works
  over the phone link but the audio stays on the phone, and that a USB Bluetooth adapter is the way to hear calls
  here. This is reported as information, not as a fault.

## Two control paths, merged

| | Bluetooth hands-free (HFP) | GeekBridge link (`geekbridge.calls.*`) |
| --- | --- | --- |
| What it is | This computer is the phone's hands-free unit | The paired phone app reports call state and runs requests |
| Carries call audio | **Yes** (the OS owns it) | **No, never** |
| Answer / decline / hang up | Linux: `bridge-calls` over PipeWire `org.pipewire.Telephony`. Windows: `bridge-calls-windows`. macOS: `geekbridge-macos-calls` helper | Android `TelecomManager.acceptRingingCall()` / `endCall()` |
| Mute microphone | Windows only (`MuteAsync`); not Linux (no telephony API), not macOS | No |
| Needs | Phone paired over Bluetooth with the computer as hands-free device | Phone paired over GeekBridge **and** the "Calls" permission enabled for it on the computer **and** the opt-in switch plus `ANSWER_PHONE_CALLS` on the phone |

`bridge_core::calls_view::build` merges them per phone: a call seen by both becomes **one row**. For each control the
first source whose own pre-flight (`CallState::check_command`, the same check execution uses) accepts it wins, HFP
first because that is the path that brings the audio here. Name and number are merged (the link usually knows the
contact name, HFP only the number).

**Answering through the link without HFP audio leaves the voice on the phone.** The UI says so ("Answered on the
phone. Call audio stays there until this computer is paired with the phone as its Bluetooth hands-free device") and,
when HFP is present but its audio link is closed, offers **Use computer audio**.

## `bridge-core` additions (all additive)

- `CallCommand::Reject { call }`: decline a ringing call. It is checked with the `Hangup` capability (every backend
  declines through its hang-up operation) and `check_command` accepts it only for a ringing call, so it can never end a
  call in progress. Linux maps it to `Action::Reject`, Windows to `RejectIncomingAsync`, macOS to `endCall`. This is the
  "reject vs hang up" gap the Windows report named, closed without adding a `CallFeature`.
- `CallBackend::GeekBridgeLink`.
- `calls_link`: packet names, strict bounded parsing of `STATE`/`REQUEST`/`RESULT`, `LinkCallReport::capabilities()`
  and `::apply()` (folds a report into `CallState`).
- `calls_view`: the merged, UI-ready view (`CallsView`, `CallRow`, `AudioRouteView`, `ActionTarget`).
- IPC (`ipc.rs`): `Request::Calls`, `CallAction {target, action}`, `CallMute {source, muted}`, `CallAudio {source}`;
  `Response::Calls(CallsStatus {view, notice, sources})`. Numbers and names only travel on this authenticated local
  channel; `Debug` output of every call type is redacted.

## Link protocol (`geekbridge.calls.*`)

| packet | direction | body |
| --- | --- | --- |
| `geekbridge.calls.state` | phone → computer | `state` (`idle`/`ringing`/`offhook`), optional `number`, `name` (≤ 128 bytes, no control characters), `canAnswer`, `canEnd`, `canDial` (absent = false) |
| `geekbridge.calls.request` | computer → phone | `requestId` (`[A-Za-z0-9_-]{1,64}`), `action` (`answer`/`reject`/`hangup`/`dial`/`report`); `dial` also carries `number` (`+`? then 1-20 digits); `report` asks the phone to send its state again (sent when Telephony is switched on here; answered with a `state` packet and no `result`, never reaches the app) |
| `geekbridge.calls.request` with `action: "tone"` | computer → phone | adds `tone`: exactly one of `0-9 * #` (one character per request; the phone counts and limits them) |
| `geekbridge.calls.result` | phone → computer | `requestId`, `ok`, optional `reason` (`permission_missing`/`no_such_call`/`failed`) |

Rules (service, `gate.rs` + `call_control.rs`):

- The packets belong to `Feature::Telephony`, whose consent is **off by default per device** (`geekbridge-ctl consent
  DEVICE telephony on`, or the device permissions in the apps). Without it the phone's reports are ignored and nothing
  is ever sent to the phone.
- Only negotiated types are routed. A malformed packet or a result nobody asked for counts against the link's
  misbehaviour budget.
- Every decision is re-checked against the **current** view (`CallHub::act`): a stale or forged target is refused
  ("that call changed; nothing was sent") before anything leaves the computer. At most 4 requests are pending per phone.
- The phone refuses on its own too: a request is executed only if the call state fits (`answer`/`reject` while ringing,
  `hangup` while off-hook), the permission is held, and the user's opt-in is on. The refusal reason is shown to the user.
- Phone side (`crates/bridge-android/src/transport/engine/calls.rs`): requests become events collected with the blocking
  `transport_calls_wait` (no polling); reports go out with `transport_calls_report`; results with
  `transport_calls_result`. The packet types are declared in the identity only when `transport_start` carries
  `calls: {enabled}`; the app always passes it (enabled = the user's switch, default off) so turning the switch on needs no
  reconnect. While it is off nothing is reported and requests are refused. A computer that connects while a call is ringing receives the current report at once. When the
  app's switch is off, requests are refused by the core itself and never reach Kotlin.

### Keypad tones (DTMF)

Two paths, merged like the other controls (`calls_view::CallsView.tones`: Bluetooth first, the link as the fallback):

- **Bluetooth hands-free (Linux, Windows).** `CallCommand::SendTones` (`AudioGateway1.SendTones` on Linux; the Windows
  backend's tone call) was already there; the view now offers it for a live call and the UI and `call tone` use it. Not run
  against a real phone. macOS plans it (`sendDTMF:`) but has no keypad path in the helper protocol yet.
- **Phone link.** No new packet type: `tone` is one more `action` of `geekbridge.calls.request` (a `geekbridge.*` name, never
  in the plaintext identity, so a stock KDE Connect peer neither sees nor sends it). The phone says it can play tones with
  `canTones` in its state report (absent = false, so an older app is simply not offered a keypad), and the computer sends
  nothing otherwise.

Checks, in every layer (each also exists on the phone, so a compromised computer is not trusted):

- The character is exactly one of `0-9 * #`. `A-D`, look-alike Unicode digits, several characters, none and wrong types are
  refused (the wire parser, the service, the phone core and Kotlin each check).
- Only while a call is live and answered: the view offers tones only for an active call, the service re-checks that the
  target is still offered, the phone core only forwards, and Kotlin requires the phone to be off hook. Ringing, held, idle
  or unknown state answers `no_such_call`.
- Rate limit: 20 tones per 5 s on the computer (`MAX_TONES`, "too many keypad tones" when over) and 24 per 5 s per computer on
  the phone (`rate_limited`, answered by the Rust core without waking Kotlin). A person typing a PIN stays far under it.
  Tones have their own pending-request allowance so quick taps neither fill nor are refused by the few slots Answer, Hang up
  and Dial use.
- **Nothing is logged but the count.** The tone may be a PIN or a menu choice. `Request::CallTone`, `LinkRequest`,
  `CallRequest` and `LinkCallReport` never print it; the call trace gets `keypad tones sent=N` and the outcome, nothing more
  (`geekbridge-ctl calls --trace`).

**What the phone can really do.** Android has no `TelecomManager` call for DTMF. `Call.playDtmfTone` / `stopDtmfTone` exist
only on the `Call` objects Android hands to a bound `InCallService`, and Android binds one only for the app the user chose
as the phone (dialer) app. GeekBridge is not that and does not ask to be. `GeekBridgeInCallService` (no screen, reads nothing,
declared with `BIND_INCALL_SERVICE` and `IN_CALL_SERVICE_UI=false`) is therefore only the plug: on a stock phone it is never
bound, `canTones` stays false and the computer shows no keypad on the link path. The keypad works there when Bluetooth
hands-free carries the call. Everything on the link path is built and tested against the stand-in phone and Kotlin fakes;
it has not played a tone on any real phone.

### Phone-side dial protection

A paired computer with call access could otherwise dial any plain number, so the phone adds three layers of its own:
a rate limit of 3 dial requests per computer per 10 minutes (the Rust core answers `rate_limited`, counted on arrival so
failed attempts cannot probe); a visible notice ("Dialing <number> requested by <computer>") whenever a call is really
about to be placed; and an optional on-phone confirmation (a "Place this call?" notification with Call / Deny, 45 s to
answer, **off by default**, switch in the Answer calls step; unanswerable counts as declined). The computer shows the
refusals `rate_limited` and `declined` in its usual notice line. Details: [reports/followups-security-5.md](reports/followups-security-5.md).

## Recents and missed calls

`call_history.rs` folds the merged view into a list of finished calls (30, newest first, memory only, gone when the service
restarts). A call whose source vanished (link flap, phone out of range) is dropped, not recorded. `CallsView.recent` carries
it to the Calls window's Recents tab and `geekbridge-ctl calls`. A ring that ends unanswered and was not declined here
raises a banner ("Call back", "Message", body click opens the Calls window); the number reaches the new window through its
environment (`GEEKBRIDGE_DIAL`, `GEEKBRIDGE_MESSAGE_TO`) and Call back only fills the keypad, so the confirmation sheet still
comes first. Missed and declined on the phone cannot be told apart. See [reports/call-ux-gap-analysis.md](reports/call-ux-gap-analysis.md).

## Android side (`CallControl.kt`, `CallControlAndroid.kt`)

| Permission | Why | Needed for |
| --- | --- | --- |
| `READ_PHONE_STATE` (already optional) | know that the phone is ringing / in a call | everything here |
| `ANSWER_PHONE_CALLS` (runtime, API 26+) | `TelecomManager.acceptRingingCall()`, `endCall()` | answer / decline / hang up |
| `READ_CALL_LOG` (runtime, optional) | the only way Android gives an app the **caller's number** (`ACTION_PHONE_STATE_CHANGED` extra); the call log itself is never read | caller number |
| `READ_CONTACTS` (already optional) | look the number up on the phone | caller name |
| `CALL_PHONE` (runtime, optional) | place a call when the computer asks to dial | dialing from the computer (`canDial`) |

- Each is its own wizard step ("Answer calls", "Caller number") with a plain explanation; calls are shared with a computer
  only after the user turns on **"Share call status with my computer"** in the Answer calls step (off by default,
  persisted, negotiated at `transport_start`).
- Honest capabilities from the phone: `canAnswer = ANSWER_PHONE_CALLS ∧ READ_PHONE_STATE ∧ API ≥ 26`;
  `canEnd = … ∧ API ≥ 28` (`TelecomManager.endCall()` does not exist on API 26-27, so the phone does not claim it and
  the computer shows no Decline/Hang up there).
- No numbers are logged. The idle report carries no party. `CallReport.toString()` is redacted.
- `READ_CALL_LOG` is a Google-Play-restricted permission. GeekBridge is distributed outside Play (GitHub releases); a Play
  build would have to drop the caller-number step.
- `acceptRingingCall()` is marked deprecated by recent SDKs but still works for apps holding `ANSWER_PHONE_CALLS`; it is
  the one public way for a non-dialer app to answer. A future Android release may remove it.

## Per-OS Bluetooth hosts (`bridge-service`)

| OS | Adapter | Notes |
| --- | --- | --- |
| Linux | `call_control::linux::LinuxHfp` (a `bridge_calls::Session` on its own thread) | Mute is overridden to *unsupported* (the telephony API has none); `Reject` uses `Action::Reject` |
| Windows | `windows_host::WindowsHost` over `bridge_calls_windows::CallsService` | Observes only (`watch`) and selects the single paired phone (read-only). It never registers, connects or requests access: until the user did those explicit steps the view shows the first blocker in words instead of controls |
| macOS | `macos_host::MacHelperHost` speaking the helper's JSON-lines protocol | Spawns `geekbridge-macos-calls serve` next to the service; connects **only** to the phone in `GEEKBRIDGE_MAC_PHONE` (explicit choice; there is no picker yet), otherwise reports that no phone is chosen |

Hosts are started only when the service's calling probe is enabled. Their capabilities are never above *declared*.

## Tests

| Layer | Where | What |
| --- | --- | --- |
| Model | `bridge-core` `calls`, `calls_link`, `calls_view` | Reject rules, strict parsing, ring→answer→idle reduction, merge rules (HFP wins, link fallback, no Answer without permission, mute only if supported), JSON round trip, redaction |
| Service | `bridge-service` `call_control` unit tests; `tests/calls_api.rs` | hub flow with a scripted host; a real daemon with a real TLS stand-in phone: consent gating, ring → decline → ring → answer → hang up, forged/stale targets, permission refusal shown, malformed packets ignored |
| Hosts | `windows_host` (in-memory Windows port), `macos_host` (stand-in helper script) | answerable/rejectable ringing call, no connect without a chosen phone, dead helper reported |
| Phone transport | `bridge-android/tests/calls_transport.rs` | declared only when asked, reports validated, mid-ring greeting, requests become events, results leave, disabled app refuses by itself, blocked wait ends on stop |
| Keypad tones | `calls_link` (wire), `calls_view` (offer), `tests/call_chain.rs` (link and Bluetooth, in order, ended call, older phone, hostile digits, no call, flood), `calls_transport.rs` (phone core: one character, hostile bodies, limit, app off), `CallTonesTest.kt`, `call_ui`/`calls_tests`/`call_overlay` (UI) | one tone per request, only live and answered, validated and limited at every hop, never logged |
| Kotlin (JVM) | `CallControlTest.kt` | capabilities by API level and permission, number/name rules, answer/reject/hang-up preconditions, failure vocabulary, protocol builders and strict wait parsing, engine dedupe/retry/stale requests, wizard steps |
| Desktop UI | `call_ui`, `call_feed` unit tests | a button exists only if offered, mute toggling, route text, worker wakes only on change and shows refusals |
| End to end (emulated) | scenario **S8** in `bridge-phone-sim` (`scripts/emulate-call.sh fake\|emulator`) | the service hub over the real `bridge-calls` stack against the simulated phone: ring → offers → Decline → Idle; ring → Answer → Active with the audio route → Hang up → Idle; a stale target sends nothing. Also S1/S2 exercise `Session` answer/reject/hang-up directly |

## Not verified on real hardware

- **No real cellular call was answered, declined or ended with GeekBridge, and no call audio was heard.** The emulated runs
  (S1, S2, S8) exercise signalling against a simulated modem through a simulated `org.pipewire.Telephony`; there is no
  Bluetooth, HFP AT exchange or SCO audio in them.
- Android: `acceptRingingCall()` / `endCall()` were **not** run on a device or emulator (the JVM tests use a fake
  `TelecomPort`); the `ACTION_PHONE_STATE_CHANGED` number delivery, `READ_CALL_LOG` behaviour across OEMs, the wizard UI and
  the API 26-30 listener paths are untested on devices.
- Linux: whether a real PipeWire/WirePlumber/BlueZ stack exposes the same shapes as the simulator, and whether a phone
  brings SCO audio to the computer after an HFP answer, are open (see the calling implementation notes).
- Windows: no Windows host ran this code; the `cfg(windows)` startup block in the service is not compiled on Linux.
- macOS: the helper and `MacHelperHost` have never run against a Mac and a phone; the `cfg(target_os = "macos")` startup
  block is not compiled on Linux.
- The `notify-send` Answer/Decline notification depends on the desktop's notification daemon; it was only tested as
  argument construction, not on a live desktop.
- Dialing over the link (`dial`, `CALL_PHONE`) is covered by the stand-in phone tests, Rust/Kotlin unit tests and the
  emulator script `scripts/e2e/features.sh` ([reports/emulator-features-e2e.md](reports/emulator-features-e2e.md); a
  simulated modem was dialed, no network). It has not run on a real phone, and no real call was placed anywhere.
- Keypad tones: Bluetooth path not run against a real phone; link path inert on stock Android (see "Keypad tones" above)
  and untested on a device; the in-call card's Keypad button only opens the Calls window.
- Not implemented: waiting/second calls over the link (Android reports one current call), hold/swap from the UI, dialing `*`/`#` codes, a Windows/macOS phone picker, microphone mute on Linux/macOS, an OS notification that
  launches the Calls window when none is open (the service notification covers Linux only).
