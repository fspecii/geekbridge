# Messages follow-ups

Closes the "not fixed" items of [messages-desktop-review.md](messages-desktop-review.md). Everything was built and tested in a
sandbox (private temp state, loopback ports picked at run time); the live service, the real config and ports
1716/17381/8000 were not touched.

## 1. Privacy switches (live, from the UI)

* New control requests `PrivacyStatus` and `PrivacySet {key, on}`; keys `message_text` (default off), `caller_number`
  (default on, as today) and `phone_notification_text` (default off). Pure rules in `bridge_core::privacy`, service side in
  `bridge-service/src/privacy.rs`; choices live in `<state dir>/privacy` (`name=on|off`, atomic, owner-only).
* Live: `Daemon::privacy_set` pushes the values into `PhoneData` (message banners) and `Mirror` (phone-notification banners)
  at once; the call card reads the choice each time it is started, so it also needs no restart. Previously only Linux set the
  text flag at start; the state folder now feeds all platforms.
* Settings -> Privacy (`privacy_ui.rs`): three checkboxes with a one-line effect text each, named for screen readers with
  their current state; a switch an older mechanism forces is disabled and says which file or variable to remove; a refusal
  is shown and the switch snaps back.
* `geekbridge-ctl privacy [show|set KEY on|off]` prints the three values and any forcing source.
* Backward compatible: `sms-notify-content` / `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` force both text keys on;
  `GEEKBRIDGE_CALL_OVERLAY=on|name` or the `call-overlay` file force the caller number (an empty file or `off` forces nothing).
* docs/privacy.md has a new "Display privacy" section and updated SMS, notification and call text;
  `privacy_doc` guard test is green. Also state-formats, user-guide, service-integration, CHANGELOG.
* Tests: `bridge_core::privacy` (names, resolution, file format), `privacy.rs` (persist, marker/env forcing, overlay word),
  `tests/privacy_api.rs` (over the control port: default, change, stored file, marker forcing), `call_overlay::mode`,
  `privacy_ui` (rows, click -> one request, refusal), Settings a11y test (names, no unnamed control).

## 2. Unread badge

* `PhoneData::unread_total` sums the per-conversation unread (Messages consent on). The tray collects it; the tooltip body
  gains "N unread messages" (`tray.unread`, plural-aware) on Linux (StatusNotifierItem `NewToolTip` signal), Windows and
  macOS (tooltip only; the icon is unchanged). The count is poked when messages arrive and when a conversation is marked read, so
  it updates live and falls when read on this computer. The waybar JSON has a numeric `unread` field (the `unread` class and
  tooltip line already existed).
* The tooltip carries a count only, never a name or text (noted in docs/privacy.md).
* Tests: model tooltip text and Windows buffer, waybar field, and an SNI test on a private bus (count appears, signal fires,
  clears).

## 3. Compose by contact name

* The To field takes a name or a number. A letter makes it a name: the shared contacts are searched (`AgendaView` query, then
  filtered locally by every word, case and accent folded), up to 6 contacts are listed. One number: Enter or a click picks it.
  Several numbers: a second list asks which. Up/Down wrap, Enter picks, Escape goes back a step then closes the list, a second
  Escape leaves the page. Rows have spoken names ("Ann Lee, +44 ...", "Joanna Smith, 2 numbers. Press Enter to choose
  one."); the field is named "Recipient: a contact name or a phone number". Picked contact shown as "Sending to NAME".
* A plain number behaves as before and starts no contact search. A name that was not resolved cannot be sent (the page says
  to pick a contact or type a number). With Contacts off for the phone the page says no contact matched.
* Logic in `recipient_pick.rs` (pure, tested); window tests drive typing and keys.

## 4. Refused reply and IME

* `Feed::run` now returns a ticket and the feed state records which tickets were refused. A reply is cleared on send, then
  put back in front of anything typed since (or into its own conversation's draft) if the service refuses, with "Not sent:
  REASON Your text is back in the box." Tests: refused reply restored, accepted one stays gone, merge with newer text.
* IME: `input::Ime` tracks preedit state from `Event::Ime` and removes an Enter press that happens during a composition or in
  the same frame as the commit that ends it, in either order, before any key handling (so it is neither a send nor a new line).
  Unit tests cover both orders, a normal Enter, and Disabled; a window test sends Enter during a preedit and after the commit.

## Not done / limits

* An Enter that confirms a composition but arrives in a *later* frame than the commit cannot be told from a real Enter and
  would send. Not tested with a real IME (no input method in the sandbox): only synthetic `Event::Ime` sequences.
* The tray badge is text only; no overlay on the icon image.
* The contact list covers contacts the phone shared (Contacts consent), at most 12 requested per keystroke.
