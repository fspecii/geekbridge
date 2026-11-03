# Phone data over the paired link: contacts, calendar (read and edit), SMS

Status: wire contract shared by the desktop service (`bridge-service::phone_data`), the Rust wire
helpers (`bridge_core::phone_link`, `bridge_kde::sms`), the Android native relay
(`bridge-android` `transport/engine/data.rs`) and the Kotlin sources (`PhoneData*.kt`).

Desktop side: the **Contacts** app (phonebook) and the **Calendar** app (month / week / day / agenda list). The
executable names are `geekbridge-contacts` and `geekbridge-agenda` (the Calendar app; the old name is kept so installs,
launchers and scripts keep working).

Everything here is **opt-in on both sides**: the phone declares a capability only while the user
has enabled that feature in the Android app *and* Android has granted the permission; the desktop
sends a request or reads a packet only while the per-device consent (`Feature::Contacts`,
`Feature::Calendar`, `Feature::Sms`, default off, `gate.rs`) is on. Nothing is persisted on the
desktop by default (memory only, dropped on unpair, consent-off, restart).

## Packets

| Kind | Dir | Body | Notes |
| --- | --- | --- | --- |
| `kdeconnect.contacts.request_all_uids_timestamps` | desktop -> phone | `{}` | stock KDE Connect |
| `kdeconnect.contacts.response_uids_timestamps` | phone -> desktop | `{uids:[uid..], "<uid>": "<timestamp>"..}` | stock; timestamp = `CONTACT_LAST_UPDATED_TIMESTAMP`; <= 5000 uids |
| `kdeconnect.contacts.request_vcards_by_uid` | desktop -> phone | `{uids:[uid..]}` | stock; desktop asks <= 25 uids at a time |
| `kdeconnect.contacts.response_vcards` | phone -> desktop | `{uids:[uid..], "<uid>": "<vcard text>"..}` | stock; phone strips `PHOTO` lines, each vCard <= 64 KiB, whole packet <= 900 KiB |
| `geekbridge.calendar.request` | desktop -> phone | `{requestId, since?: token}` | GeekBridge extension. `since` = token of the last sync the desktop applied; absent = full sync wanted |
| `geekbridge.calendar.sync` | phone -> desktop | see below | GeekBridge extension |
| `geekbridge.calendar.write` | desktop -> phone | see "Calendar edits" | GeekBridge extension; only with the `calendar_edit` consent and a phone that declares it |
| `geekbridge.calendar.write_result` | phone -> desktop | `{opId, state, reason?, eventId?, duplicate?}` | GeekBridge extension |
| `kdeconnect.sms.request_conversations` | desktop -> phone | `{}` | stock |
| `kdeconnect.sms.request_conversation` | desktop -> phone | `{threadID, rangeStartTimestamp?, numberToRequest?}` | stock; desktop asks <= 50 |
| `kdeconnect.sms.request` | desktop -> phone | `{version:2, sendSms:true, addresses:[{address}], messageBody, subID?, requestId}` | stock + `requestId` (stock phones ignore it). One address, no attachments |
| `kdeconnect.sms.messages` | phone -> desktop | `{version:2, messages:[msg..]}` | stock message shape. A GeekBridge phone may put messages from several threads in one packet (<= 100 messages, <= 900 KiB) |
| `kdeconnect.sms.messages` extra `sims` | phone -> desktop | `"sims": [{"id", "slot", "label"}]` on any messages packet | GeekBridge extension, <= 4 SIMs. `id` = Android subscription id (the `subID` of a send), `slot` 1-based (0 unknown), `label` <= 24 printable chars. The desktop offers a SIM choice only from this list and refuses any other `subID`; the phone refuses an unlisted `subID` as `unknown_sim` (never a silent fallback to another SIM). Stock phones send none, so only "phone default" is offered |
| `geekbridge.sms.status` | phone -> desktop | `{requestId, state, reason?}` | GeekBridge extension. `state` = `accepted` (phone handed it to the radio stack), `sent` (Android reports the message left the phone), `delivered` (carrier delivery report), `failed` (`reason` = short code: `no_permission`, `no_service`, `radio_off`, `no_sim`, `short_code`, `network`, `fdn`, `emergency`, `cancelled`, `call_active`, `invalid`, `undelivered`, `generic`, `limit`, `recipients`, `blocked_number`, `unknown_sim`). A repeated `requestId` is never sent twice; the phone answers it with the last status it had for that id. Stock phones never send this |

| `geekbridge.photos.request` | desktop -> phone | `{op:"list", limit:1..24}`, `{op:"thumb", id}` or `{op:"get", id}` | GeekBridge extension (recent photos, [design/recent-photos.md](design/recent-photos.md)). `get` makes the phone send that one picture as an ordinary file share |
| `geekbridge.photos.list` | phone -> desktop | `{items:[{id, name, taken, size, w, h}], partial}` | GeekBridge extension; <= 24 items, unique opaque ids, `partial` = Android shares only the photos the user picked |
| `geekbridge.photos.thumb` | phone -> desktop | `{id, w, h, jpeg: base64}` or `{id, gone: true}` | GeekBridge extension; JPEG <= 24 KiB, <= 256 px |

Capabilities (identity `incomingCapabilities` / `outgoingCapabilities`) of the Android app, per enabled feature:

* contacts: incoming `kdeconnect.contacts.request_all_uids_timestamps`, `kdeconnect.contacts.request_vcards_by_uid`; outgoing `kdeconnect.contacts.response_uids_timestamps`, `kdeconnect.contacts.response_vcards`
* calendar: incoming `geekbridge.calendar.request`; outgoing `geekbridge.calendar.sync`
* calendar editing (needs calendar too): incoming `geekbridge.calendar.write`; outgoing `geekbridge.calendar.write_result`
* sms: incoming `kdeconnect.sms.request_conversations`, `kdeconnect.sms.request_conversation`, `kdeconnect.sms.request`; outgoing `kdeconnect.sms.messages`, `geekbridge.sms.status`
* photos (needs photo access): incoming `geekbridge.photos.request`; outgoing `geekbridge.photos.list`, `geekbridge.photos.thumb`

The desktop declares the mirror image (it needs `kdeconnect.sms.messages` etc. incoming).

### `geekbridge.calendar.sync` body

```json
{
  "version": 1,
  "requestId": "<echo, optional>",
  "full": true,
  "base": null,
  "token": "g7f3a:12",
  "windowStartMs": 1730000000000,
  "windowEndMs": 1740000000000,
  "truncated": false,
  "calendars": [{"id": "3", "name": "Personal", "account": "me@gmail.com", "accountType": "com.google",
                  "color": -14575885, "writable": true}],
  "events": [{
    "id": "41@1730500000000", "rev": "9c1f2a0b",
    "title": "Dentist", "allDay": false,
    "startMs": 1730500000000, "endMs": 1730503600000, "tz": "Europe/Berlin",
    "location": "Main St 4", "description": "bring card", "calendarId": "3",
    "eventId": "41", "recurring": false, "reminders": [30, 10],
    "attendees": {"count": 3, "names": ["Ann", "Bob"]}
  }],
  "deleted": [{"id": "40@1730400000000"}]
}
```

* The phone reads `CalendarContract.Instances` (recurrences already expanded, so a weekly series arrives as one
  instance per date, each flagged `recurring: true`) for the window `[now - 12 months, now + 24 months]` (nearest to now kept when more than the cap; see docs/reports/scale-report.md), only for
  calendars the user ticked in the app (default: none). **Reading is the default; writing needs the separate
  switch below.** Every calendar the Calendar Provider exposes can be ticked, which includes the Google accounts
  synced on the phone: that is how the desktop calendar is shared with Google, with no Google API key, OAuth or
  network access of GeekBridge's own. `id` = `<Events._ID>@<begin ms>`; all-day instances use UTC-midnight `startMs`/`endMs`
  (exclusive end) exactly as Android stores them (`bridge_core::agenda::When::from_android`).
* Calendar rows have no modified timestamp, so `rev` is a stable hash of the event's content fields (including
  calendar, reminders and attendees, so a changed reminder travels in the next delta).
* Calendar extras are all optional (an older phone sends none): `account` / `accountType` (`com.google` for Google),
  `color` (Android ARGB int), `writable` (the phone's access level is contributor or better); per event `eventId`
  (`Events._ID`, the handle for edits), `recurring` (RRULE / RDATE / exception), `reminders` (minutes before, <= 5),
  `attendees` (`count` + first <= 6 names). Reminders and attendees are read with one bounded query per 200 events.
* `token` is opaque (`<phone boot id>:<counter>`), compared for equality only. The phone remembers the id->rev
  map it last sent for the current link. If the request's `since` equals the phone's last token the answer is a
  delta (`full:false`, `base` = that token, `events` = new/changed only, `deleted` = ids that vanished). Otherwise
  (no `since`, unknown token, new link, calendar selection changed) it is a full snapshot (`full:true`, `base:null`):
  the desktop replaces everything it held for that phone, tombstoning ids not listed.
* Bounds: <= 1000 events per packet (more -> `truncated:true`, nearest first), <= 32 calendars, <= 500 `deleted`,
  title <= 512, location <= 512, description <= 2000 chars (phone trims), packet <= 900 KiB. Empty `calendars`
  with `full:true` means "nothing selected / no permission" and the desktop clears.
* The phone pushes an unsolicited delta when the calendar provider changes, at most once per 30 s.

### Calendar edits (`geekbridge.calendar.write`)

Two-way calendar, **off by default, per phone, on both sides**:

* Desktop: the per-device consent `calendar_edit` (`Feature::CalendarEdit`, `gate.rs`) must be on, plus `calendar`
  (reading). Without it the service refuses the request locally and nothing leaves the computer
  (`geekbridge-ctl consent DEVICE calendar_edit on`, or "Edit the calendar" in the Calendar sidebar).
* Phone: the app switch "Let the computer edit my calendar" *and* Android's runtime permission `WRITE_CALENDAR`.
  The permission is requested **only when that switch is turned on**; reading never needs it. The phone declares the
  two packet kinds only while both hold, so a phone without them is never even sent a write.

```json
{"version": 1, "opId": "gb123-4-1730500000000", "action": "create",
 "calendarId": "3",
 "event": {"title": "Lunch", "allDay": false, "startMs": 1730500000000, "endMs": 1730503600000,
           "tz": "Europe/Berlin", "location": "Cafe", "description": "...", "reminderMin": 10}}
```

`action` is `create` (needs an explicit `calendarId`; there is no default calendar), `update` or `delete` (both name
`eventId`; `update` carries the whole `event`). All-day events use UTC midnights with an exclusive end and `tz: "UTC"`
(Android's write rule); timed events carry the zone they were entered in. Limits: title <= 512, location <= 512, notes
<= 2000, reminder 0..40320 minutes. The desktop validates before sending; the phone validates again.

The phone writes through the Calendar Provider **as an ordinary app** (not a sync adapter). The provider marks the row
dirty and the phone's own sync adapter (Google's, for a Google calendar) uploads it. GeekBridge therefore never talks
to Google, and "saved" means "written to the phone's calendar", not "already on Google".

Answer, always sent: `{opId, state, reason?, eventId?, duplicate?}` with `state`

| state | meaning |
| --- | --- |
| `applied` | the provider accepted the write (`eventId` = `Events._ID`) |
| `failed` | nothing was written; `reason` is one of `invalid`, `calendar_missing` (not shared / unknown), `not_writable` (read-only calendar), `not_found`, `recurring`, `no_permission` (Android revoked it), `disabled`, `generic` |
| `unknown` | an earlier attempt of this `opId` was interrupted mid-write (app killed); the phone cannot say whether it took effect |

Rules (all enforced on the phone, whatever the desktop sends):

* Only calendars the user ticked for sharing **and** Android reports as writable are touched.
* **Repeating events** (RRULE, RDATE or an exception of a series) are never changed or deleted from the desktop; they
  are shown, each occurrence on its date, with "edit on the phone".
* **Idempotency.** Each `opId` is applied at most once. The phone records it as *started* before touching the provider
  and *finished* after, in a bounded (200) on-device ledger of ids and result codes only (no event text). A repeated
  id gets the first answer with `duplicate: true`; an id found in the *started* state after a restart is answered
  `unknown`, never written again.
* **Honest status on the desktop** (`EditPhase`): `HandedToPhone` until the phone answers, `Applied`, `Failed` (with a
  plain-language reason), or `Uncertain` (the link dropped, 45 s of silence, or the phone said `unknown`). Nothing is
  resent automatically; "Ask again" re-sends the same packet with the same `opId`, which is safe because of the
  idempotency above. After `Applied` the desktop asks for a fresh calendar delta.
* Rate: <= 20 edits per minute per phone on the desktop; the relay allows 20 packets per 10 s.

Desktop notifications before events are a third, local consent (`calendar_alerts`, off by default): the service announces
a timed event once, at its largest reminder (10 minutes if it has none, at most 60), through `notify-send`; nothing is sent
anywhere.

## Android native relay (`bridge-android`, JSON actions, `docs/android-contract.md` envelope)

The Rust engine does not interpret phone data; it relays an allow-listed set of packet kinds between the
trusted link and Kotlin, bounded and rate limited (the pattern of `transport_calls_*`).

* Start argument `data`: `{"contacts":bool,"calendar":bool,"calendar_edit":bool,"sms":bool}` (optional object, absent = all off; `calendar_edit` only declares anything together with `calendar`).
  Decides the capabilities declared in the identity packet. Changing it needs a transport restart (like `calls`).
* `transport_data_config` `{contacts?, calendar?, calendar_edit?, sms?}`: runtime enable flags. A disabled feature drops its
  packets (neither queued nor sent) and `transport_data_send` for it fails `data_disabled`.
* `transport_data_wait` `{since: u64, timeout_ms: u64 <= 25000}` -> `{seq, events:[{seq, connection_id, kind, body}]}`
  of desktop requests received on the trusted link (incoming kinds of the enabled features, body = packet body
  object). Queue bounded (64); the oldest is dropped on overflow. Packets with a payload are rejected.
* `transport_data_send` `{kind, body: object}` -> `{}`; `kind` must be an outgoing kind of an enabled feature, the
  link must be connected (`not_connected`), serialized body <= 1 MiB (`too_large`), sustained rate <= 8 packets
  per second with burst 20 (`rate_limited`; the app retries a refused answer for about 3 s). The request itself is exempt from the usual 16 KiB request cap up to 1 MiB.

Kotlin owns everything Android specific: permissions, content providers, hashing, diffing, SMS sending and
status, notifications of what is shared. Sensitive content (contacts, event text, message bodies) is never logged.

## Desktop behaviour (summary)

* Per paired device and feature, consent default off. Turning it off clears that device's in-memory data.
* On link-up with consent: contacts `request_all_uids_timestamps` (then `Store::plan_sync`, vCards in batches of 25,
  vCard `REV:` or the timestamp as revision, absent uids tombstoned, empty first listing ignored when the store has
  data), calendar `request` (with `since`), SMS `request_conversations`. Refresh: IPC `agenda_refresh` /
  `messages_refresh`, and every 5 minutes while the app windows poll.
* SMS send state shown to the user: queued (nothing left the computer), **handed to phone** (dispatched, no
  confirmation), **accepted by phone**, **sent** (Android handed it on; not a receipt), **delivered** (carrier report
  relayed by a GeekBridge phone only), failed (with a plain-language reason), **uncertain** (connection lost after
  dispatch). Queued and handed-over sends are never called sent or delivered. Never auto-resent: "Send again as new"
  on a failed or uncertain send creates a **new** record (new request id) and warns that a possibly sent text may
  go out twice and be billed twice.
* Messages window: conversation list with unread badges (sum on the sidebar), contact names resolved from the
  synced phonebook by the last 7 digits (number if unknown or contacts not shared), search (name, number and any
  held message; matching snippet shown; it covers only messages already loaded in memory, so older history of a
  conversation is searchable after opening it), date separators (Today / Yesterday / weekday / date in the computer's
  zone), local sends shown as lines under the thread until the phone's own copy appears, MMS rows as an
  "Attachment" placeholder with "Picture message: open it on the phone" (no attachment bytes are requested),
  SIM choice when the phone listed 2+ SIMs, and a segment counter (GSM-7 160/153, UCS-2 70/67 per part; an emoji
  or any non-GSM character turns the whole text to UCS-2; an estimate, the phone/carrier decide).
* **Read state is local.** Opening a conversation marks it read on this computer only (`messages_mark_read`); the
  stock KDE protocol has no way to mark a message read on the phone, so the phone's unread state is untouched.
  Messages arriving after the mark are unread again. Forgotten with the other in-memory data.
* **Memory bounds** (no persistence by default; an encrypted cache is out of scope): <= 300 conversations, <= 2000
  messages per conversation and <= 20000 per phone. Past a cap the oldest messages are dropped (the thread then shows
  "Showing the latest N messages; older ones stay on the phone" and stops offering "Load older"); on a full phone the
  oldest history of the biggest thread makes room for what just arrived. A view reply carries <= 100 messages.
* Incoming SMS (new inbox message after the first sync) -> desktop notification "New message from <name or number>"
  with a **Reply** action (and a click on the body does the same) that opens the Messages window on that number.
  The message text is **hidden by default**: it reaches the notifier only if the user chose to show message content
  (`touch <state dir>/sms-notify-content` or `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` for the service).
* Honest states: *not allowed* (consent off), *offline*, *waiting*, *phone does not offer Messages* (exact steps:
  GeekBridge, Share with computer, Messages on, allow SMS; on a sideloaded Android 13+ first Settings, Apps, GeekBridge,
  ⋮ menu, Allow restricted settings, then Permissions, SMS, Allow; GeekBridge never needs to be the default SMS app),
  and *read-only phone* (reads but does not accept sends). Failure codes map to sentences (`no_permission`,
  `no_service`, `radio_off`, `limit`, `blocked_number`, `unknown_sim`, ...).

## Permission limits and store policy (honest summary)

* **Contacts / calendar:** `READ_CONTACTS` and `READ_CALENDAR` are ordinary runtime permissions. Contacts are only
  read, never written. The calendar is read from the calendars the user ticks; it is written only after the user turns
  on "Let the computer edit my calendar", which is when Android's `WRITE_CALENDAR` is requested (and revocable in
  Android settings at any time). Photos are not sent in this version (the phone strips `PHOTO` from vCards).
* **SMS:** reading and sending need `READ_SMS`, `SEND_SMS` (and `RECEIVE_SMS` for new-message pushes). Google Play
  restricts these to the *default SMS handler* (with narrow exceptions), so a Play build of this feature would have
  to become the default messaging app, which GeekBridge deliberately does not (that would take over the user's
  messaging, MMS and the provider). It is therefore a sideload / F-Droid feature. On Android 13+ a sideloaded app
  may first need "Allow restricted settings" (App info, overflow menu) before the SMS permission can be granted.
* **Not supported:** MMS attachments (listed without content as "MMS message — open on phone"; the phone sends only an `attachments` marker, never a caption or part), group messages (one
  recipient per send), RCS (the system messaging app keeps RCS outside the SMS provider), scheduled sends, marking
  read on the phone. Dual SIM works only against a GeekBridge phone that lists its SIMs (needs `READ_PHONE_STATE`). On a phone with two SIM slots the Messages step offers that permission itself ("Allow SIM choice", optional); without it every text uses the default SIM and the chooser stays hidden.
* **Delivery receipts** exist only if the carrier sends one and the phone app relays it; "sent" means Android
  handed the message on, not that the recipient has it.
* **Incoming SMS push** needs the SMS provider to have stored the message: a non-default app receives
  `SMS_RECEIVED` but the provider row is written by the default messaging app, so GeekBridge waits briefly for it
  and otherwise does not push that text. The next `request_conversations` always shows the stored state.

## Tests

* Rust: `bridge_core::phone_link` (wire parsers, vCard 2.1/3.0, calendar delta/tombstones), `bridge_kde::sms`,
  `bridge_service::phone_data` (consent gating, bounds, send state, erasure), `bridge-phone-sim` `tests/data_link.rs`
  (real pinned TLS link: scripted phone <-> service <-> control API), `bridge-android` `tests/data_transport.rs`.
* Kotlin JVM: `PhoneDataTest` (gating, builders, diff/token logic, SMS validation and status) and `CalendarWriteTest`
  (write gating, parsing, validation, one-write-per-opId, restart/unknown, read-only/unshared/recurring refusals, Android
  denial, bounded ledger, account/colour/reminder/attendee JSON).
* Rust, calendar: `bridge_core::phone_link` (extras, draft validation, write packet), `bridge_service::calendar_edit` +
  `phone_data` (consent/capability/calendar gating, single send, uncertain after a drop, same-id retry, alerts),
  `bridge-phone-sim` `tests/data_link.rs` (real TLS link: refused without consent, created/edited/deleted, read-only and
  recurring refused), `bridge_apps::calendar_model` (time zones, DST edges, month grid, editor conversion).
* Emulator, Contacts and Calendar: `scripts/e2e-calendar.sh` (injects Google-typed calendars, events with a reminder, a
  weekly series and contacts with `content insert`, reads them on the desktop, **creates an event from the desktop and
  finds it with `content query` in the emulator's Calendar Provider**, checks a read-only calendar is refused, deletes it).
  Desktop screenshots without an emulator (scripted phone, `examples/data_sim.rs`): `scripts/screenshots-desktop-calendar.sh`
  -> `dist/screenshots/desktop/`.
* Messages: `bridge_core::messages` (segment counting GSM/UCS-2/extension characters, bounded `trim_oldest`,
  out-of-order and duplicate delivery), `bridge_service::phone_data` (local read marks, search, per-thread cap, SIM
  validation, notification content/reply action), `bridge_kde::sms::parse_sims`, `bridge_apps::apps::messages`
  (state labels never say "delivered" early, day labels, pending sends, blockers, SIM labels), Kotlin `PhoneDataTest`
  (SIM list in packets, unlisted SIM refused). Screenshots without an emulator:
  `scripts/screenshots-desktop-messages.sh` -> `dist/screenshots/desktop/messages-{list,search,compose,mms,emoji}-{light,dark}.png`.
  Emulator: stage `messages` of `scripts/e2e-phonedata.sh` (`adb emu sms send` -> conversation list with unread ->
  thread -> desktop send reaches the phone's own status). Its phone-side switch and per-computer grant are written by
  the script itself (`geekbridge_grants.xml`, guarded by `scripts/tests/e2e-grants-test.sh`). The self-contained SMS suite
  is `scripts/e2e/sms.sh` (report: [reports/sms-emulator-e2e.md](reports/sms-emulator-e2e.md)).
* Emulator, correctness on small named data: stage `cases` of `scripts/scale-e2e.sh` (`scripts/scale-e2e.sh env,battery,pair,cases`,
  about 10 minutes, own AVD / ports, `--stop` afterwards; report docs/reports/contacts-calendar-emulator.md). It seeds a weekly series across a
  DST change with a moved and a cancelled occurrence, an EXDATE series, all-day events, a Tokyo event, an invitation, a read-only
  calendar and unicode / right-to-left / override-character contacts, and checks the instances on the desktop, the edit-conflict reply
  (`changed`), `not_organizer`, deletes both ways, consent withdrawn on either side and logs free of content. Nightly as the
  non-blocking `contacts-calendar-cases` suite (docs/emulator-ci.md).
* Emulator: `scripts/e2e-phonedata.sh` (injects contacts, calendar events and SMS with `adb`, drives the real
  Agenda and Messages windows under a private Xvfb, screenshots in `dist/screenshots/phonedata/`).

## Per-computer limits for texts (phone side)

Counted per paired computer (device id from the native core, delivered beside the request as `device`/`computer` in
the relayed event, never read from the packet body) and kept for the life of the app process, so reconnecting gives no
new allowance: 10 texts per minute, 60 per hour, and at most 8 different numbers per 10 minutes. Over the limit the
text is refused (`limit` or `recipients`), not counted, and the owner gets one notice per computer per 10 minutes
(toast plus a notification on the "Texts held back" channel). Short codes and premium ranges stay refused unless the
owner allows them.
