# Messages, contacts and calendar: code review 2

Read-only review of the main checkout working tree (`/home/ambsd/.t3/projects/geekbridge`). No source was edited.

## Coverage and limits

Bash was unusable for the whole session. Every call failed with EDQUOT on `/tmp/claude-1000`. I had no `ls`, `grep` or test runs, only the Read tool on paths I guessed or inferred from `lib.rs`. Every finding below comes from reading code; nothing was run.

Files read in full or in the relevant part:
- `crates/bridge-service/src/phone_data.rs` (to line 2200)
- `crates/bridge-core/src/messages.rs` (to line 760)
- `crates/bridge-core/src/collate.rs`
- `crates/bridge-kde/src/sms.rs` (to line 260)
- `crates/bridge-apps/src/recipient_pick.rs`
- `crates/bridge-apps/src/prefill.rs`
- `crates/bridge-apps/src/phone_ui.rs`
- `crates/bridge-apps/src/apps/messages.rs` (to line 1850)
- `crates/bridge-service/src/tray/mod.rs` (to line 300)
- `android/.../PhoneDataAndroid.kt` (all)

Not reviewed because I could not find or list them:
- `tray/model.rs` and `tray/sni.rs`
- `notify_text.rs` and `desktop_notify.rs`
- `bridge-core::phone_view` constants and `agenda::Agenda::people`
- `input.rs` (`sanitize_dial`, `Ime`)
- Kotlin `PhoneDataEngine`, `SmsMerge`, `NotificationMirror`, `PhoneDataGate`
- `AndroidManifest.xml`
- the privacy-switch store in `bridge-core`

Items that depend on those files are marked VERIFY.

Checked and found fine:
- No UTF-8 slicing panic found. `suffix` slices an ASCII-only digit string. `truncate`, `snippet`, `short` and `take_chars` all work on `chars()`. `snippet`'s `lower[..b]` uses an offset from `find`, so it is always a char boundary.
- No message text goes to any log in the files read. `sms.rs` states bodies are never logged. The Kotlin `GbLog` calls print only exception class names and feature flags. The tray `collect()` reads counts only.
- With "Show message text" off, `IncomingText.body` is `None`, so no body reaches `notify-send`. The tray tooltip only gets `unread_total()`, a number.
- SMS send consent, the connected check, the capability check and the SIM allow-list are all enforced in `check_can_send` and `send_text`, before the ledger or the link is touched. A duplicate click returns `Submit::Existing` and never resends.
- Cursors on the Android side use `use {}`. Provider reads honour a `CancellationSignal`. `SecurityException` is caught everywhere.

---

## Ranked findings

### High

**H1. A reply in a group conversation goes privately to one participant.**
- Location: `crates/bridge-apps/src/apps/messages.rs:698` (`to = conv.addresses.first()`) and `:774-783`.
- Scenario: a thread has several addresses (a group MMS, or an SMS thread the phone lists with several addresses). The title shows "Ann, Bob". The user types in the reply box and presses Enter. `SMS_MAX_RECIPIENTS` is 1, so only Ann gets the text. The UI gives no warning.
- Fix: when `conv.addresses.len() > 1`, set `blocker = Some("Group conversations can't be answered from the computer")`. This is a one-line blocker next to the existing `to.is_none()` check.

**H2. Unread count and tray badge go stale after reading on the phone.** This is three causes in one chain.

(a) `crates/bridge-kde/src/sms.rs:106-118` with `phone_data.rs:678-684`.
- The phone's `unread` map lists only threads with unread above 0 (`Telephony.Sms.READ = 0` query, `PhoneDataAndroid.kt:541-559`).
- `on_messages` only updates threads that appear in the map. A thread read on the phone drops out of the map, so its `phone_unread` keeps the old value and `unread()` stays above 0.
- VERIFY the engine to confirm it never writes zeros.
- Fix: make `parse_unread` return `Option<Vec<_>>`, `Some` when the key is present even if the object is empty. When `Some`, set `phone_unread = Some(0)` for every held thread not in the list.

(b) `phone_data.rs:207-222`.
- `unread()` returns `loaded.max(phone)`.
- The conversation list refreshes only each thread's newest message. If three messages were loaded unread and the phone then marks them read, only the newest flips to `read`. The other two stay `read == false` forever, so `loaded` is 2 even when the phone says 0.
- Fix: when `phone_unread` is `Some`, use `phone_unread.saturating_sub(unread_at_mark)` alone. Use the loaded count only when it is `None` (stock phones).

(c) `PhoneDataAndroid.kt:954-970`.
- The phone observes `Telephony.Mms.CONTENT_URI` and the calendar. Nothing observes `Telephony.Sms`, so a read-state change on the phone pushes nothing.
- The PC refreshes only when `messages_view` runs (window open and more than 5 minutes since the last ask, `phone_data.rs:1618`).
- With the window closed, the tray badge shows a stale count until the next incoming text.
- Fix: register a debounced observer on `Telephony.Sms.CONTENT_URI` (reuse `mmsRefresh`), or have the service re-ask every few minutes while `unread_total() > 0` and a tray exists.

**H3. "Load older" can get permanently stuck at "exhausted".**
- Location: `phone_data.rs:664-677`.
- Scenario: the user presses "Load older" on thread X, which sets `asked_older = true`. Before X's page arrives, an unrelated `kdeconnect.sms.messages` packet comes in (a push for another thread, or a list refresh). X is not in `touched`, so the loop at 672-677 marks X `exhausted = true`. When X's real page arrives, `refresh_messages` has already returned `Ok(())` early on `th.exhausted` (line 1160), and "Load older" stays dead until the thread is dropped.
- Fix: tag the request with its thread id and only run the "empty page means exhausted" rule for the packet that answers that thread. For example, set `exhausted` only when the packet's messages are all in thread X and none was inserted. Alternatively, clear `asked_older` on any packet and never infer exhausted from unrelated traffic.

### Medium

**M1. A conversation is marked read while the window is in the background.**
- Location: `messages.rs:541-551`.
- Scenario: the Messages window is open on a conversation but minimised or on another workspace. A new text arrives. `c.unread > 0` and `marked != message_count`, so `MessagesMarkRead` fires immediately. The unread badge in the tray and sidebar is cleared, and the PC banner already suppressed the message (the 2 s per-sender gap or `sms_listed`).
- Fix: gate on `ctx.input(|i| i.focused)`, and optionally on the thread being scrolled to the bottom.

**M2. A capped thread is never re-marked read.**
- Location: `messages.rs:541-543`, using `marked[..] == c.message_count`.
- Scenario: once a thread holds `MAX_MESSAGES_PER_THREAD`, each new message inserts one and trims one, so `message_count` is constant. `marked == message_count` is true and the new unread stays shown while the conversation is open.
- Fix: key `marked` on the newest message's `(date, uid)` or on `unread`. A `last_date_ms` check is enough: mark again when `last_date_ms` differs from the date stored at the last mark.

**M3. Two different people can be merged by name or by number tail.**
- Compose picker, `recipient_pick.rs:60-67`: `out.iter_mut().find(|c| c.name == p.name)` merges numbers of two different contacts with the same name, such as two "John Smith" entries. One row then offers both people's numbers, and a text can go to the wrong John. Fix: key on `(device_id, p.id)`. Collapse across phones only when the number sets overlap.
- Display name and banner key, `phone_data.rs:1877-1900`, `suffix` = last 7 digits:
  - Numbers from different countries or areas that share a 7-digit tail show the wrong contact name.
  - Banners from those senders suppress each other (`pick_notifications` dedupes by `sender_key`, and the synchronous hint tag is the same).
  - An alphanumeric sender such as `Bank12345` yields the key `12345`.
  - Fix: prefer an exact digit match, including the country code when both sides have one. Use the 7-digit tail only as a fallback when exactly one contact matches, and never for `sender_key` when both sides carry a `+`.

**M4. The compose picker may not show the best match.**
- Location: `messages.rs:899-908` asks for `people_limit: 12`. `agenda_view_range` returns `d.agenda.people(&query)` alphabetically, cut at 12 (`phone_data.rs:1543-1546`). `recipient_pick::candidates` then ranks prefix-first, but only inside those 12.
- Scenario: with 2,000 contacts the query "li" matches Alice, Alicia, Eli, Elias, Felix, Julia and more. "Liam" sorts after the first 12 and is not offered, even though it starts with the query.
- Fix: rank prefix-first in the service before truncating (use `collate::fold(name).starts_with`), or raise the limit to 60 for this request.
- VERIFY `Agenda::people` ordering.

**M5. The send limiter counts texts, not SMS parts.**
- Location: `phone_data.rs:135` (`SENDS_PER_MINUTE = 10`) and `:1787`, with `MAX_TEXT_CHARS = 2000` in `messages.rs`.
- Scenario: a 2,000-character emoji text is 30 UCS-2 parts (67 units each). Ten per minute is 300 parts per minute, billed per part. The 15-minute receiver lifetime on Android is not a limit either. The phone-side `SmsLimitNotice` exists but is the second line of defence.
- Fix: charge the limiter `sms_segments(text).segments` units against a budget (for example 40 parts per minute). Optionally cap one outgoing text at 10 parts.

**M6. SMS data stays after the phone stops offering SMS.**
- Location: `phone_data.rs:939-944`. `link_up` clears contacts and calendar when the capability is gone but has no matching branch for SMS (`!caps.sms_read`).
- Scenario: the user revokes the SMS permission on the phone. On reconnect `state_of` reports `NotOffered`, but `messages_view` (consent only, line 1630) still lists the old conversations and `unread_total` still counts them.
- Fix: add `if !caps.sms_read { d.clear(SMS) }`.

**M7. Two identical incoming bodies in quick succession can be missed.**
- Location: `PhoneDataAndroid.kt:611-612` (`findIncoming`: newest inbox row from `address` with equal `body`, `firstOrNull`).
- Scenario: "OK" arrives and is already in the provider. A second "OK" arrives and the broadcast fires before the default app writes its row. The query returns the first row and the second is pushed as a duplicate (the PC drops it as `Duplicate`). The second message appears only at the next list refresh, and no banner is shown for it.
- Fix: only accept rows with `_id` above the highest id seen, or `date >= broadcastTime - 5s`.

**M8. A message with an empty or malformed address is dropped silently.**
- Location: `bridge-core/src/messages.rs:331-337` and `bridge-kde/src/sms.rs:90-97`.
- Scenario: `Address::new("")` returns `EmptyAddress`, so `from_wire` rejects the whole message. Examples: an MMS whose `addr` query failed (`PhoneDataAndroid.kt:536-538` returns `address = ""`), and some operator or system messages. The message is counted only in `rejected`, which nothing displays. The thread then has no row, but the phone's unread count includes it.
- Fix: map an empty address to a placeholder such as "Unknown sender" (the view already has that fallback at `phone_data.rs:1670`). Drop only the address, not the message.

**M9. vCards are truncated at 512 KiB before the photo is stripped.**
- Location: `PhoneDataAndroid.kt:111-124` and `:134`.
- Scenario: a contact with a large photo exceeds the cap, the read stops mid-photo, and the card has no `END:VCARD`. Fields after `PHOTO` are lost, and `contact_from_vcard` may reject it, so the contact is skipped for the whole round.
- Fix: request the card without the photo. `CONTENT_VCARD_URI` accepts `ContactsContract.QUERY_PARAMETER_VCARD_NO_PHOTO` ("no_photo", added in a later API level than minSdk 26; VERIFY the level). Otherwise drop `PHOTO` lines while streaming.

**M10. The number sent to the phone can contain a trunk zero, and the notification Reply can prefill the wrong number.**
- Compose, `messages.rs:1079`: a contact stored as `+44 (0)20 7946 0001` goes through `sanitize_dial`, which keeps the digits in the parentheses, so the phone is asked to text `+4402079460001`, a wrong number. VERIFY `sanitize_dial`.
  - Fix: after a `+CC`, remove a parenthesised single `0`.
- Reply from a banner, `prefill.rs:58-66`: `clean_number` keeps only digits and a few symbols. An e-mail gateway sender `91234@txt.example.com` becomes `91234`, a plausible but wrong recipient. A sender such as `AMAZON` gives nothing.
  - Fix: if `raw` contains `@` or a letter, return `None`.

### Low

- **L1. Instance duplicates around the pivot.** `PhoneDataAndroid.kt:176-181`. The Instances query returns every instance overlapping the window. An event that spans `pivot` appears in both the forward and the backward query, so `ahead + behind` can hold it twice. Dedupe on `(eventId, begin)` before `withDetails`. VERIFY whether the sync encoder already dedupes.
- **L2. Eviction at the 10,000-message cap is not oldest-first.** `phone_data.rs:744-754`. It trims the oldest 100 of the largest thread, not the globally oldest messages. It also keeps only 1 message when the largest thread holds 100 or fewer. And it fires for an `Updated` message too, because the `room == 0` check at line 627 comes before the merge outcome is known. Thread-level eviction (`drop_stalest_thread_before`) also loses the thread's `phone_unread` and its local read mark. Fix: choose the thread whose newest message is oldest. Check `room` only for messages that would insert.
- **L3. Ghost thread.** `phone_data.rs:620-626`. An empty `Thread` is inserted before the room and merge checks. If `evict_for_room` returns 0 and the loop breaks, or the merge errors, an empty thread stays. It uses one of the 300 thread slots. `messages_view` hides it, and it is evicted first because its date is 0.
- **L4. No notification for the first listing after a connect.** `phone_data.rs:650`. `self.sms_listed` is false on the first packet after `link_up`, so a text that arrived while the link was down is never announced, even if less than 10 minutes old.
- **L5. Notification age uses two clocks.** `phone_data.rs:639`. `now_ms()` is the PC clock and `date_ms` is the phone clock. A skew of more than 10 minutes silences or floods banners. Compare against the time the packet arrived, or use the difference to the previous packet.
- **L6. Sidebar unread badge differs from the tray.** `messages.rs:350`. It is the sum over `view.conversations`, which is search-filtered and cut to `MAX_CONVERSATIONS_SHOWN`. The tray uses `unread_total`. Fix: add an `unread_total` field to `MessagesView`.
- **L7. Bubble height cache collides.** `messages.rs:1652` and `:1685`. The key is `(m.uid, width_bucket)`. The uid is not unique across SMS and MMS (the core key has the transport) or across threads, so two messages share a cached height and the virtual scroll jumps until repainted. Include `(thread, mms, uid)`.
- **L8. Conversation search is not accent- or case-folded like the picker.** `phone_data.rs:1608-1611`, `:1650-1659`. It uses `to_lowercase`, so "jose" does not find "José". Contact search in the picker uses `collate::fold`. Use `collate::fold` for the needle, the title and the bodies. `snippet` then needs matching offsets. `fold` only maps Latin letters. Cyrillic `ё` and `й`, and Arabic or Hebrew marks (harakat, niqqud), are not folded.
- **L9. RTL and complex scripts.** egui does no bidi reordering or Arabic or Indic shaping. `script_fonts::ensure` supplies glyphs only. Arabic and Hebrew bodies, titles and contact names render in logical order, and Arabic letters stay disconnected. This is a documented-limitation item, not a quick fix. Do not claim RTL support in the user guide.
- **L10. Notification text goes on `notify-send`'s command line when "Show message text" is on.** `phone_data.rs:2041-2045`. The number is correctly kept in the environment, but the sender name and body appear in `/proc/<pid>/cmdline` for the lifetime of `--wait`, up to 8 s or more. This is only visible to other local users with a permissive `hidepid`. The notification code has no lock-screen awareness, so with the text switch on a banner is shown over whatever the compositor displays. Hyprland's session lock covers layer-shell surfaces, so I rate this low. No change needed beyond the doc. Gate it on the lock state only if you want belt and braces.
- **L11. Android: undelivered `onSent` receivers live 15 minutes.** `PhoneDataAndroid.kt:651-666`. The sent receiver unregisters only on failure. On success it is removed by the 15-minute `postDelayed`. If `sendMultipartTextMessage` throws (permission revoked, invalid address), both receivers stay registered for 15 minutes and `onStatus("accepted")` is not sent. Wrap the call in try/catch and release both on exception.
- **L12. Android: `smsUnread`/`mmsUnread` are capped (5,000 and 2,000) and the MMS query catches only `RuntimeException` while the SMS query catches only `SecurityException`.** `PhoneDataAndroid.kt:471-484`, `541-559`. Undownloaded MMS notification rows (`m-notification-ind`) are unread forever, so the count can be stuck above zero. An `IllegalArgumentException` from an OEM provider in `smsUnread` escapes. Catch `RuntimeException` in both.
- **L13. Android: `goAsync` window.** `PhoneDataAndroid.kt:1101-1124`. Attempts are scheduled at 1.5 s, 3 s and 4.5 s, but each runs on the shared single worker. A long provider read or send ahead of it can push the third attempt past the roughly 10 s `goAsync` limit and the receiver is killed. Run the lookup on its own small executor, or call `done()` before the final retry.
- **L14. Senders' read state across devices.** `mark_read` only sets `read_through_ms` to the newest date. A message that arrives with a phone date at or below the mark (a clock jump, or an older message delivered late) never counts as unread.

### To verify (could not be read)

1. `AndroidManifest.xml`: `SmsReceivedReceiver` must be declared `android:exported="true"` with `android:permission="android.permission.BROADCAST_SMS"`. Without it any app can forge `SMS_RECEIVED`. The `findIncoming` check means a forged text is not pushed as a message, but the 4.5 s fallback `refreshConversations()` and the work per intent are still triggered.
2. `NotificationMirror` (mirror setting): mirrored notifications from the user's SMS app include the message text. If "Show message text in notifications" is off on the PC, the mirror still sends it. The PC may then show it through the mirror path. Check that the PC-side mirror honours the same switch for SMS apps, or document that it does not.
3. `Agenda::people` ordering and whether it matches digits.
4. `sanitize_dial` (`input.rs`) and `notify_text::plain` handling of bidi controls (U+202A to U+202E, U+2066 to U+2069). `parse_sims` strips them from SIM labels; bodies, titles and contact names may not be.
5. `phone_view` constants: `MAX_MESSAGES_PER_PHONE` and `MAX_MESSAGES_PER_THREAD`. Memory estimate for the 5,000-message and 2,000-contact case: roughly 300 bytes per message plus body, so 5,000 messages is about 2 to 3 MB. Contacts drop the raw vCard (`phone_data.rs:552`), so 2,000 contacts is about 1 MB. Neither is a concern. The worst case is the per-body byte cap: `MAX_INCOMING_BODY_BYTES` is 256 KiB, so 10,000 maximum-size bodies would be 2.5 GB. A paired phone is trusted, but a byte budget in `on_messages` would make the cap hold in bytes too.

### Performance notes (no change required at 5,000 messages)

- `Conversation::newest`, `newest_date_ms` and `unread()` are O(n) per thread, run per tray poke and per view. For 10,000 messages that is about 10,000 comparisons per call.
- `messages_view` holds the global service lock while it scans every thread, lowercases message bodies for a search and builds `name_index` (all contacts) for each device.
- `display_name` rebuilds `name_index` for every banner. Cache the index per `Dev`, invalidated on contact changes, if profiling shows it matters.
