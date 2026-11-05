# New features review: Do Not Disturb, decline with message, dialer autocomplete

Reviewed at origin/main 1bb9f747d. Fixes are in the same commit series; each has a test.

## Do Not Disturb (crates/bridge-service/src/dnd.rs, daemon.rs, call_control.rs)

1. MEDIUM, fixed. `Detector::active` held its mutex across a probe chain of up to 5 tools x 500 ms and
   was called from `announce_new_calls` (under the hub's `announcing` lock) on every ring pass.
   Now: `active()` returns the last known answer at once (false until one is learned) and starts a
   single short-lived `gb-dnd-probe` thread when the answer is older than 3 s. No lock is held while a
   tool runs. The chain has a 600 ms total deadline (each tool still gets at most 500 ms of what is left).
   Because a stale answer could be hours old on the first ring after idle, the daemon runs a `gb-dnd-warm`
   thread that refreshes every 2 s only while "Respect Do Not Disturb" is on; it holds a `Weak` and ends
   when the hub drops the closure. Tests: slow probe never blocks the caller, stale answer returned at
   once with the lock free, one refresh at a time, chain stops at its deadline.
   Known trade-off: the first ring in the first ~2 s after switching the setting on is treated as
   "not in DND" (fails open: the ring is shown).
2. OK. No user text reaches a command line; no shell; stdin null; stdout capped at 4096 bytes.
3. OK. The ring is still recorded in Recents and the trace when held quiet (`kept quiet: Do Not Disturb`);
   the decision is made once per ring, so a ring stays quiet if DND ends while it rings (covered by tests).

## Decline with message (call_replies.rs, call_control.rs, daemon.rs, ipc.rs, bridge-apps)

Verified OK:
- Consent, both sides: `replies_for` requires the ring to be a phone-link row with a decline target,
  a plain number, and `PhoneData::can_send_text` (Messages allowed here, phone connected, phone takes
  texts, limiter has room), checked before the call is touched. The send itself goes through
  `PhoneData::send_text`, the same function and the same per-minute limiter as normal sends, which
  re-checks everything. The UI hides the action when not offered; the service does not trust the UI.
- Number comes from the hub's in-memory row, never from the request; the request carries only the reject
  target and an index (u8). Index is validated against the current list (`no such reply`, BadRequest).
- Storage: owner-only file via `atomic_write_private`, at most 3 texts, 70 chars, no control characters,
  enforced on save and on load (a hand-edited file falls back to the defaults).
- Injection: notify-send gets fixed action keys (`reply0`..), labels pass through `notify_text::plain`,
  caller goes after `--`; the number is never in argv.
- Old banner on a new ring: `still_ringing(ring)` by ring number gates it; tested (a banner from an ended
  ring sends nothing).
- Double click: `replied` set keyed by ring number; a failed decline releases it; tested (one decline,
  one text).
- Duplicates in the list are accepted and harmless. Bidi/format characters (not "control") are accepted;
  they are the person's own text.

Defects fixed:
- MEDIUM. A banner button was resolved by index against the list as it is now. Editing the replies while
  a banner was up made the button send different words than its label. Banner presses now go through
  `decline_with_reply_expecting`, which refuses (nothing declined, nothing sent) when the text at that
  index is not what the button was labelled with. Test:
  `a_banner_button_never_sends_other_words_than_it_was_labelled_with`.
- LOW. When a ring had no ring number (not yet announced) the double-click guard was skipped and two
  requests could send two texts. It is now refused ("that call changed; nothing was sent").
- LOW. `Replies::set` held the `saved` mutex across the fsync, so `texts()` (ring path, status poll)
  could wait on the disk. The write now happens under a separate `writing` mutex; `saved` is taken only
  to swap the list.

Left, noted:
- LOW. The overlay card and `CallDeclineReply` over IPC still resolve by index only (the request has no
  room for the label). The race needs an edit during the few seconds a call rings.
- LOW. `replied` is never pruned (8 bytes per decline-with-text per daemon run).
- LOW. Settings editor: if a save is refused by the service, the drafts keep showing the unsaved text
  until the person edits again (the service's old list equals `seen`, so no resync).

## Dialer autocomplete (apps/calls.rs, recipient_pick.rs)

All as intended; no defects found.
- Lookup only for a name (any letter) or a number of at least 3 digits made of digits, `+` first,
  spaces, `-`, `.`, `(`, `)`. `*` and `#` codes are never looked up (tested).
- Digits only suggest a contact whose stored number contains them; a number nobody stored finds nobody
  and shows no list.
- Never dials: picking fills the field only (`Step::Picked` sets `self.number` and `picked`), Enter is
  consumed by the list only while it is open and has rows, and Call still opens the confirmation sheet;
  the service re-checks the number is plain digits.
- The typed text goes to the local service as an `AgendaView` query (capped at 64 chars there); the
  service also matches digits only from 3 digits.
- LOW, not changed. The service returns at most 12 people alphabetically, so for a 3-digit query with
  many matches the "starts with" ranking only sees those 12.
