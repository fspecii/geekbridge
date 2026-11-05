# Call experience: gap analysis against Phone Link and Continuity

Date: 2026-11-05. Scope: what a person does and sees around a phone call. Behavioural only; no code, assets or strings from any
other product. The Phone Link column comes from the static package and APK notes under `docs/research/` (kept out of git by
design) and from how the product is publicly described. **Nothing in the Phone Link column was run or observed on a live
system here**, and GeekBridge itself has still not placed or answered a real cellular call (see `docs/call-control.md`, "Not
verified on real hardware"). Treat both columns as design input, not as measurements.

## The two flows side by side

| Moment | Phone Link (Windows) | GeekBridge today |
| --- | --- | --- |
| Incoming call | A toast with Answer, Decline and Message (decline and send a canned text). Works with the app closed (background trigger). | Top-right card with caller, phone, Answer, Reject, X (`docs/reports/call-overlay.md`), plus a `notify-send` banner with Answer/Decline. Raised by the service, so it works with no window open. No Message. |
| During the call | A call window: caller, timer, mute, keypad (DTMF), hold, speaker/audio device switch, end. | In-call card: caller, timer, Hang up, Mute (where a backend can), audio route with reason, Use computer audio, Wi-Fi listen/stop. No keypad, no hold. |
| Audio device | Switch between the phone and the PC from the call window. | "Use computer audio" and the route line. No switch back to the phone from the card on every path; device choice lives in Settings. |
| Dialling | Dial pad plus contact search with autocomplete. | Dial pad (typing, paste, long-press 0 for +), confirmation sheet naming the phone and where the voice goes. Contacts is a separate app that can open the dialer with a number; no autocomplete inside the dialer. |
| Recent calls | A list with call-back per row, read from the phone's call log. | Recents tab showed only a placeholder before this change. Now lists calls since the service started, with Call back and Message (below). The phone's own history is not read. |
| Missed call | A notification, and a badge in the app. | None before this change. Now a banner with Call back, Message and Open. No badge. |
| Do not disturb / Focus | Windows Focus assist can hide the toast; the app probes for this when a connection fails and tells the person. | The card is its own window, so it is not hidden by notification settings, but it ignores Do Not Disturb altogether (it will show in a meeting). The banner follows the desktop daemon's rules. No setting for "quiet hours", no detection that a banner was suppressed. |
| Bluetooth off or phone out of range | Setup state machine reports radio off, no adapter, not paired, connection failed; calls controls disappear. | The Calls window says what is missing in words (`call audio` checklist, sources list, "no Bluetooth controller" as information). Control over the phone link keeps working without Bluetooth; the audio stays on the phone and says so. |
| First-run pairing | In-app flow: pairing started from the PC with a PIN comparison, manual fallback steps, then registration and connect. | Two parts. Link pairing (QR or code) is a guided window. Call audio: Settings → Calls checklist (adapter, unblocked, powered, paired, hands-free, headset) with a one-line fix and Copy command per step, and `geekbridge-ctl call setup` for the same with the BlueZ agent handling the confirmation (`--confirm yes|no`). Comparable in shape; ours is more explicit about commands and less automatic about pairing. |
| Wi-Fi audio | Not part of the product (it uses Bluetooth hands-free). | Experimental listen/speak over the existing link, behind consent. A GeekBridge advantage on computers with no Bluetooth. |

## Gaps, ranked by value over effort

Value is how often a person meets the gap and how much it breaks the "it just works like the other platforms" feeling.
Effort is rough, for this code base.

| # | Gap | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | No missed-call notification | High | Low | A call you missed while away from the phone leaves no trace on the computer. Needs only what the view already knows. **Done here.** |
| 2 | Recents were empty | High | Low-medium | Every other platform has a list with call-back. Phone's real call log needs READ_CALL_LOG over the link (new packets, consent, Kotlin); the in-memory list needs none. **Done here for calls seen by this service.** |
| 3 | No Message from a missed call or a recent call | Medium-high | Low | Messages already opens with a prefilled recipient. **Done here.** |
| 4 | Decline with a message (from the card) | High | Medium | **Done here** (see call-control.md). Needed a place for canned texts, a settings switch, the Messages send path with its consent, and room on a 360x96 card. Next best after the three above. |
| 5 | Keypad during a call (DTMF) | Medium-high | Medium | The Bluetooth backends already accept tones (`SendTones`) on Linux and Windows. Missing: the link has no tone packet, and the in-call card has no keypad. Useful for phone menus. Start with the Bluetooth path only and say so. **Done here, see below.** |
| 6 | Do Not Disturb awareness | Medium | Medium | Respect the desktop's DND (freedesktop inhibit or the compositor's mute) for the card and banner, still show the tray state. Needs a setting; silently hiding a call is a risk, so it must be opt-in. |
| 7 | Hold and swap, call waiting | Medium | Medium-high | The Windows backend has hold; the link reports one call only. Needs a model change for a second call. |
| 8 | Contacts autocomplete in the dialer | Medium | Medium | The Contacts feed exists; the dialer would read it (consent is already per phone). Arrow keys plus accessible announcements add most of the cost. |
| 9 | Persistent, phone-side call history | Medium | High | New link packets, READ_CALL_LOG on the phone, a size budget for the list, retention rules. The in-memory list is the stop-gap. |
| 10 | Switch audio back to the phone from the card | Medium | Low-medium | `DisconnectAudio` exists in the model; the card only offers the opposite direction. |
| 11 | Automatic Bluetooth pairing from the computer | Medium | High | Fully scripted pairing needs real Bluetooth to test; this machine has none. Keep the checklist and `call setup` until a real phone has run them. |
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

## Known limits of the change

* The history starts empty every time the service starts. That is a deliberate privacy choice, and it is why gap 9 stays open.
* A ring the phone stops reporting for long enough (the watchdog's "never confirmed" path) is recorded as missed. It cannot be
  told apart from a real miss with the data available.
* "Missed" and "declined on the phone" are indistinguishable (the same limit the KDE Connect protocol has).
* Phones that report only "off hook" (the link, no direction) show as "Call" without incoming or outgoing.
* The banner relies on the desktop's notification daemon honouring actions (libnotify 0.8+). Daemons that draw no buttons still
  get the body click. Not run on a live desktop here.
* Nothing here was run against a real phone.
