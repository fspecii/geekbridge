# Call experience: gap analysis

Date: 2026-11-05. Scope: what a person does and sees around a phone call on the computer. Behavioural only: it lists what
people reasonably expect from answering, making and managing a call from a computer, and where GeekBridge stands. It does not
compare against, quote or reuse anything from another product. GeekBridge itself has still not placed or answered a real
cellular call (see `docs/call-control.md`, "Not verified on real hardware"), so treat this as design input, not as a
measurement.

## The flow today

| Moment | GeekBridge today | Expected by people |
| --- | --- | --- |
| Incoming call | Top-right card with caller, phone, Answer, Reject, X (`docs/reports/call-overlay.md`), plus a `notify-send` banner with Answer/Decline. Raised by the service, so it works with no window open. Decline with a message is offered (see below). | Answer, decline, and decline with a short text, even with no app window open. |
| During the call | In-call card: caller, timer, Hang up, Mute (where a backend can), Keypad, audio route with reason, Use computer audio, Wi-Fi listen/stop. No hold. | Timer, mute, keypad, end, a clear audio route. |
| Audio device | "Use computer audio" and the route line. No switch back to the phone from the card on every path; device choice lives in Settings. | Switch between the phone and the computer from the call. |
| Dialling | Dial pad (typing, paste, long-press 0 for +), confirmation sheet naming the phone and where the voice goes. Contacts is a separate app that can open the dialer with a number; the dialer suggests contact names. | Dial pad plus contact search. |
| Recent calls | Recents tab: calls since the service started, with Call back and Message per row. The phone's own history is not read. | A list with call-back per row. |
| Missed call | A banner with Call back, Message and Open. No badge. | A notification and a count. |
| Do not disturb | The card is its own window, so it is not hidden by notification settings; an optional switch in Settings > Privacy respects Do Not Disturb (Linux notification daemons only). The banner follows the desktop daemon's rules. | A way to stay quiet in a meeting, without silently losing a call. |
| Bluetooth off or phone out of range | The Calls window says what is missing in words (`call audio` checklist, sources list, "no Bluetooth controller" as information). Control over the phone link keeps working without Bluetooth; the audio stays on the phone and says so. | Plain statements of what is missing. |
| First-run pairing | Link pairing (QR or code) is a guided window. Call audio: Settings -> Calls checklist (adapter, unblocked, powered, paired, hands-free, headset) with a one-line fix and Copy command per step, and `geekbridge-ctl call setup` for the same with the BlueZ agent handling the confirmation (`--confirm yes|no`). | A guided flow with a code comparison. |
| Wi-Fi audio | Experimental listen/speak over the existing link, behind consent. Helps on computers with no Bluetooth. | Not expected; a bonus. |

## Gaps, ranked by value over effort

Value is how often a person meets the gap and how much it breaks the "it just works" feeling. Effort is rough, for this code
base.

| # | Gap | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | No missed-call notification | High | Low | A call you missed while away from the phone leaves no trace on the computer. Needs only what the view already knows. **Done here.** |
| 2 | Recents were empty | High | Low-medium | Every other platform has a list with call-back. Phone's real call log needs READ_CALL_LOG over the link (new packets, consent, Kotlin); the in-memory list needs none. **Done here for calls seen by this service.** |
| 3 | No Message from a missed call or a recent call | Medium-high | Low | Messages already opens with a prefilled recipient. **Done here.** |
| 4 | Decline with a message (from the card) | High | Medium | **Done here** (see call-control.md). Needed a place for canned texts, a settings switch, the Messages send path with its consent, and room on a 360x96 card. Next best after the three above. |
| 5 | Keypad during a call (DTMF) | Medium-high | Medium | The Bluetooth backends already accept tones (`SendTones`) on Linux and Windows. Missing: the link has no tone packet, and the in-call card has no keypad. Useful for phone menus. Start with the Bluetooth path only and say so. **Done here, see below.** |
| 6 | Do Not Disturb awareness | Medium | Medium | Respect the desktop's DND (freedesktop inhibit or the compositor's mute) for the card and banner, still show the tray state. Needs a setting; silently hiding a call is a risk, so it must be opt-in. |
| 7 | Hold and swap, call waiting | Medium | Medium-high | The Windows backend has hold; the link reports one call only. Needs a model change for a second call. |
| 8 | Contacts autocomplete in the dialer | Medium | Medium | The Contacts feed exists; the dialer reads it (consent is already per phone). **Name suggestions done**; arrow-key selection and announcements remain. |
| 9 | Persistent, phone-side call history | Medium | High | New link packets, READ_CALL_LOG on the phone, a size budget for the list, retention rules. The in-memory list is the stop-gap. |
| 10 | Switch audio back to the phone from the card | Medium | Low-medium | `DisconnectAudio` exists in the model; the card only offers the opposite direction. |
| 11 | Automatic Bluetooth pairing from the computer | Medium | High | Fully scripted pairing needs real Bluetooth to test. Keep the checklist and `call setup` until a real phone has run them. |
| 12 | Badge for missed calls in the tray | Low-medium | Low | The tray already pokes on ring changes; add a count that clears when Recents is opened. |

## What was implemented (1 to 3)

All three share one source of truth, a small call history kept in the service's memory.

* **Call history in the hub** (`bridge-service/src/call_history.rs`). Built from the merged view, so a call seen over Bluetooth
  and over the phone link is one entry. A call whose source disappears (link flap, phone out of range) is dropped rather than
  recorded as missed. Outcomes: Missed, Declined (pressed here), Incoming (answered), Outgoing, Not answered, and Call when
  the phone reported no direction. Thirty entries, newest first, never written to disk, numbers only on the existing
  authenticated local channel (debug output is redacted like the rest of the call types).
* **Missed-call banner.** A ring that ends unanswered and not declined here raises a banner once, with Call back, Message and a
  body click that opens the Calls window. The number goes to the new window in its environment, never on a command line.
  Call back only fills the keypad; the confirmation sheet still names the phone and where the voice goes before anything is
  dialled. Offered only for a plain number (digits, optional leading +).
* **Recents tab.** Rows with name or number, how it went, talk time and how long ago; missed calls in the error colour;
  Call back (opens the confirmation sheet, or the keypad with the reason when dialling is not offered) and Message per row;
  a note that the phone's own history is not read. `geekbridge-ctl calls` lists the same entries with masked numbers.

## Gap 5: keypad tones (done)

The 12 keys sit in the Calls window's live-call panel (a Keypad toggle; keyboard digits work while it is open) and the in-call
card gets a Keypad button that opens that panel. `geekbridge-ctl call tone KEYS` is the script form. The merged view offers it
(`CallsView.tones`) for a live, answered call: Bluetooth hands-free first (`SendTones`), else the phone link
(`geekbridge.calls.request` with `action: "tone"`, gated by the phone's `canTones`). Every layer validates the character
(`0-9 * #` only), requires a live call, rate-limits (20 per 5 s on the computer, 24 on the phone) and logs only a count.

The honest limit is the phone: Android lets only the bound in-call service (the default phone app) play a DTMF tone, so a stock
phone reports `canTones = false` and the link path stays off; Bluetooth is what works. Details and tests: [../call-control.md](../call-control.md).

## Known limits

* The history starts empty every time the service starts. That is a deliberate privacy choice, and it is why gap 9 stays open.
* A ring the phone stops reporting for long enough (the watchdog's "never confirmed" path) is recorded as missed. It cannot be
  told apart from a real miss with the data available.
* "Missed" and "declined on the phone" are indistinguishable (the same limit the KDE Connect protocol has).
* Phones that report only "off hook" (the link, no direction) show as "Call" without incoming or outgoing.
* The banner relies on the desktop's notification daemon honouring actions (libnotify 0.8+). Daemons that draw no buttons still
  get the body click. Not run on a live desktop here.
* Nothing here was run against a real phone.
