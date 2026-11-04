# Desktop Messages review (end to end)

Scope: `crates/bridge-apps/src/apps/messages*.rs`, the Messages half of `crates/bridge-service/src/phone_data.rs`
(notifications, limits, read marks), `os_notify.rs`, `geekbridge-ctl messages*`, fonts. Everything was checked in the
sandbox (headless egui rig, hub unit tests, scripted phone); no live service, real config or fixed port was touched.

## Defects found and fixed (each with a test written first)

| # | Defect | Fix | Test |
|---|---|---|---|
| 1 | Reply box: Enter inserted a newline, so a chat could only be sent with the mouse. | Enter sends, Shift+Enter adds a line, Ctrl/Cmd+Enter sends too; hint text says so. Enter on an empty box does nothing. | `enter_sends_a_reply_and_shift_enter_adds_a_line` |
| 2 | One shared `reply` string: text typed for Ann stayed in the box after clicking Bob and could be sent to Bob. | An unsent reply is kept per conversation (memory only, at most 50), restored when you come back, dropped when sent. | `every_conversation_keeps_its_own_unsent_reply` |
| 3 | No focus management: opening a conversation or leaving compose left no cursor anywhere. | Cursor goes to the reply box on opening a conversation, after sending, and after Escape. Enter in "To" moves to the text. | same tests, `ctrl_n_...` |
| 4 | No keyboard shortcuts: Ctrl+N, Escape, Ctrl+F did nothing. | Ctrl/Cmd+N new message (cursor in To), Escape leaves compose (draft kept), Ctrl/Cmd+F focuses search, Ctrl/Cmd+Enter sends from compose. A second click on Send while the first is awaited no longer sends twice. | `ctrl_n_opens_a_new_message...`, `ctrl_f_puts_the_cursor_in_the_search_field` |
| 5 | The conversation scroll area was shared by all threads: a newly opened conversation inherited the previous scroll offset instead of showing its newest message. | Scroll state keyed by (phone, thread). | by construction (egui id), exercised by the open/switch tests |
| 6 | `same_number` matched on the last 3 digits for short numbers: a short code "345" or "22000" was treated as the tail of any long number (wrong thread opened from a notification or Contacts, wrong pending-send line). | Under 7 digits the numbers must be equal; 7+ digits compare the last 7 (so +49 151 2345678 and 0151 2345678 are one person). | `numbers_group_across_country_code_and_trunk_prefix_but_short_codes_stay_exact` |
| 7 | Notification limiter keyed by the raw sender string: "+49 151 ..." and "0151 ..." (same person) bypassed the 2 s gap, and the banner replace-slot differed. Alphanumeric senders had no usable key. | Key = last 7 digits when the sender is a number, else the text as written; the banner slot uses the same key. | `one_person_written_two_ways_is_one_banner_and_text_senders_stay_apart` |
| 8 | macOS and Windows banners ignored the "show message text" choice (only `received` was implemented, so the text was never shown even when allowed). | `incoming` implemented; the body is the text only when the user opted in, else the phone's name. | `a_text_banner_shows_the_message_only_when_allowed` |
| 9 | Missing glyphs: the bundled fonts cover Latin, Greek, Cyrillic and monochrome emoji only, so Chinese, Japanese, Korean, Arabic, Hebrew, Indic and Thai messages and contact names were empty boxes (the fixture even contains Hebrew and Arabic). | New `script_fonts` module: text about to be drawn is scanned, and for each script not covered the system font is found (`fc-match :charset=` on Linux, well-known paths on macOS and Windows) and installed as a last-resort fallback. Nothing is bundled (no licence or size cost), nothing loads until needed (the CJK face is 20 MB), a miss is remembered, the golden harness (`GEEKBRIDGE_BUNDLED_FONTS_ONLY`) is unaffected. Used by Messages and Contacts. | `script_fonts::tests` incl. a real font load |
| 10 | `geekbridge-ctl` could not resend a failed text or mark a conversation read. | `messages-resend DEVICE SEND_ID` (new message, may arrive twice) and `messages-read DEVICE THREAD` (this computer only). | builds; used by the same service calls the window uses |
| 11 | "Read marks stay on this computer" was easy to miss. | Header of every conversation now says "Marked read on this computer only. The phone keeps its own unread state." | golden `messages-*` |

Golden screens added (light and dark, standard and minimum size): `messages-search`, `messages-compose`,
`messages-mms` (picture-message placeholder), `messages-emoji`. The four existing `messages-*` goldens changed
only by the new header line.

## Checked and found correct

- Hub: reconnect dedupe (a text the phone lists again after a reconnect is not announced twice; the first listing
  after a link comes up never notifies), 300-conversation window with "Showing the newest N of M" notes, per-thread
  and per-phone message caps with eviction, read marks memory-only and lowered when the phone reads, 10 sends per
  minute, one banner per sender per 2 s and at most 3 senders per packet, bodies never reach the notifier unless
  `sms-notify-content` / `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` is set (default: sender and phone name only), banner text
  stripped of markup, control and direction characters.
- Send status wording never claims "delivered" without evidence; failed and unknown sends offer "Send again as new"
  with the duplicate warning; MMS shows "MMS message - open on phone" (with the attachment count); SIM chooser only
  with 2+ SIMs and only for SIMs the phone listed; local times use the zone offset at that instant (DST safe) and
  day headings are relative.
- Unread: Home tile sums conversation unread; waybar module carries the count. Clicking a Linux banner (or Reply)
  starts Messages with the number, which opens that conversation or a new message.

## Known limits (not fixed, stated plainly)

Follow-up: the notification-text switch, the unread badge, compose by name, restoring a refused reply and IME Enter
are done; see [messages-followups.md](messages-followups.md). The matching bullets below are kept as history.


- egui does not shape or reorder text: Arabic is drawn in isolated forms and Hebrew/Arabic run left to right in
  logical order. Glyphs now appear, but right-to-left messages are not typeset correctly. Fixing it needs a shaping
  engine (rustybuzz + bidi), a size and licence review of its own.
- Emoji are monochrome (egui cannot draw colour bitmaps); modern emoji sequences may show as separate symbols.
- Chinese text uses the system's first CJK face (often the Japanese cut of Noto Sans CJK on Linux): same characters,
  some glyph shapes differ.
- "Show message text in notifications" has no switch in the UI: it is the `sms-notify-content` marker file or an
  environment variable read when the service starts (default off). A Settings toggle would need a service restart
  or a live setting; left for a decision.
- The tray icon has no unread badge on Linux/macOS/Windows (only the Home tile and the waybar module).
- Compose takes a phone number only, not a contact name; search covers loaded messages only (the window says so).
- A reply is cleared when Send is pressed; if the local service refuses the request the text is not restored
  (compose keeps its draft until accepted).
- Enter during IME composition (CJK input methods) relies on the platform delivering the confirm key to the IME.
