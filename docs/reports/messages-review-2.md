# Messages (SMS) review 2

Scope: send path, receive path, permission states, parser tests. Done in a clean worktree of origin/main, in the sandbox
only (own cargo target, own AVD clone `gbmsg2`, console 5842, adb server 16142, control 17891, listen 19731, display :95).
The live service, the real config and ports 1716, 17381 and 8000 were not touched. Everything started was stopped afterwards.

## Defects found and fixed (test first where a test could fail first)

| # | Defect | Fix | Tests |
|---|---|---|---|
| 1 | The phone's text limit (10/min, 60/hour) was **one counter for the whole phone** and was **cleared whenever the link went down**. A paired computer that dropped and reopened its link got a fresh allowance each time, and one computer could use up another's. There was no limit on how many *different* numbers a computer could text, which is the real premium-number attack. | New `SmsGuard`: allowance per computer, kept for the life of the app process (not per link), plus at most 8 different numbers per 10 minutes (numbers compared by their last 9 digits, so `+49 151 ...` and `0151 ...` are one). A refused text is not counted. Memory bounded (16 computers, 32 numbers each). The owner gets a toast plus a heads-up notification ("Texts held back", own channel) naming the computer, at most once per 10 minutes per computer. | `SmsGuardTest` |
| 2 | The phone could not tell *which* computer asked: the relayed event carried only a connection id. | The native core now puts `device` and `computer` beside the event body (from the pairing, never from the packet; a body that claims otherwise stays inside the body). Kotlin reads them with length and character checks. | `events_say_which_computer_asked_and_a_body_cannot_forge_it` (Rust, real TLS link), `SmsGuardTest` |
| 3 | A repeated request id (computer resends after a reconnect) was correctly not sent twice, but it was answered with **silence**, so the send stayed "unknown" on the computer. | The phone remembers the last status per request id (bounded, memory) and replays it. After an app restart nothing is remembered: still silent, still never sent twice. | `SmsGuardTest` |
| 4 | `sent` could overtake the phone's own `accepted` (separate threads) and then `accepted` was emitted after it. The computer tolerates it, but the order was wrong on the wire. | Status never goes backwards. | `SmsGuardTest` |
| 5 | Only 5 Android result codes were mapped; every other one (short code blocked by the carrier, SIM absent, network reject, FDN, emergency, call in progress, ...) became "The phone could not send it." `unknown_sim`, `undelivered` and the new codes had no computer-side text either. | `failureReason` maps the SmsManager.RESULT_* codes (constants checked against android.jar API 36) to 17 short reasons; the computer has one distinct sentence for each. Unknown/future codes fall back to `generic`. | `SmsGuardTest`, `every_reason_the_phone_can_report_has_its_own_plain_sentence` |
| 6 | **MMS was dropped silently on the phone**: only the SMS table was read, so a picture message never reached the computer and a thread with only pictures did not exist. (The computer side already had a placeholder, but nothing ever fed it.) | The phone also lists MMS rows (id, thread, date, box, read mark, SIM, sender/recipient address; never a caption or a part), merged with SMS by date, with unread counts, and re-sends the list a few seconds after the MMS provider changes (receiving MMS needs RECEIVE_MMS, which is not requested). The computer shows "MMS message — open on phone" in the list and in the thread. | `SmsMmsTest`, `a_picture_message_is_shown_as_a_placeholder_not_as_an_empty_line`, `picture_messages_say_to_open_them_on_the_phone` |
| 7 | With 300 conversations held, a text from a **new** person was silently ignored. | The stalest conversation makes room if the new one is newer; older ones are still left out. | `a_text_from_a_new_person_is_not_lost_when_the_conversation_list_is_full` |

## Reviewed, found correct (no change)

* **Segmentation** (`sms_segments`): GSM 160/153 septets with escape pairs never split, UCS-2 70/67 units, surrogate pairs never split, an emoji turns the whole text to UCS-2. Existing tests pin the boundaries; the fuzz target keeps `units <= segments * per_segment`. The phone sends with `divideMessage` + `sendMultipartTextMessage`, so the carrier-facing split is Android's own.
* **Sent/delivered tracking**: one PendingIntent pair per message, `FLAG_IMMUTABLE`, explicit package, receivers not exported (API 33+) with an unguessable action token below that, `SmsPartTracker` reports `sent` after all parts, `failed` at the first part error, `delivered` after all reports. Receivers are removed after 15 minutes if a carrier never reports delivery. Permission lost mid-send maps to `no_permission` (SecurityException); airplane mode to `radio_off` (code 2/9/100); no signal to `no_service`.
* **Computer-side state machine** (`SendRecord`): a dispatched send is never auto-resent; connection loss makes it `Uncertain`; later phone evidence resolves it; delivered then failed is a conflict that leaves the state alone. `SendLedger` is idempotent per submission id and refuses a different payload under the same id. The ledger is memory only on the computer, the request-id ledger on the phone is persisted (200 ids).
* **Dual SIM**: the computer offers "Phone default / SIM n" only from the list the phone reported; the phone refuses an unlisted subscription id (`unknown_sim`) instead of falling back. `getSmsManagerForSubscriptionId` is used when a SIM is chosen. **Limitation (not fixed):** the SIM list needs READ_PHONE_STATE, which belongs to the Telephony step, not the Messages step. Without it the phone reports no SIMs, the chooser is hidden and every text uses the phone's default SMS SIM. Not verifiable on the emulator (one SIM).
* **Receive**: messages are keyed by (thread, SMS or MMS table, row id), so SMS and MMS rows with the same id do not collide; a reconnect re-lists without duplicates or notifications (the first list after a link comes up never notifies); unread comes from the phone's count for GeekBridge phones, read marks are local and memory-only (documented earlier); desktop notifications show content only when the user enabled it, one per sender per 2 s, at most 3 senders per packet. Contact names are matched by the last digits of the number.
* **Scale**: 10,000 to 30,000 messages were already covered (`docs/reports/scale-report.md`: 20,000 SMS over 800 threads on the phone logic, 30,000-message flood ingest 137 ms on the computer, caps 2,000 per conversation, 20,000 per phone, 300 conversations, 100 shown, 150 KiB per reply).

## Fuzz and parser tests

* Rust fuzz target `sms` now also checks `parse_sims` (count and label alphabet), `parse_sms_status` (request id and reason alphabets, lengths) and that any text becomes either a refusal or a one-recipient send request with unchanged text. Existing corpora replay in `cargo test`.
* Kotlin `SmsParserFuzzTest`: 6,000 seeded hostile send requests (wrong types, nulls, control characters, huge strings, nested values), 3,000 hostile event lists, 1,500 hostile rows, SIM and unread caps; nothing throws and everything accepted stays inside the limits; emoji length is counted in characters.
* `SmsMmsTest`, `SmsGuardTest` cover merge order, date units, address choice, limits, notices, replay.

## Permissions and the default-SMS-app requirement

What a sideloaded, non-default app can do (GeekBridge never becomes the default app):

| Capability | Needs | Non-default app | Evidence |
|---|---|---|---|
| Send text/multipart with `SmsManager` | `SEND_SMS` | works | Android 4.4+ rule (only the *default app* gets to write the provider, sending is open to any app with the permission); AVD API 36 earlier report `real-device-sms.md`: send accepted |
| Read the SMS provider (`content://sms`) | `READ_SMS` | works; rows are written by the default app | AVD API 36 (`scale-report.md`, `real-device-sms.md`) |
| Read the MMS provider (`content://mms`) | `READ_SMS` | works in principle; a provider that lacks a column or refuses is treated as "no pictures", texts keep working | **not verified**: the emulator cannot receive MMS and the scripted run below did not reach the SMS path |
| Be told of a new text | `RECEIVE_SMS` (`SMS_RECEIVED` broadcast, manifest receiver) | works; the app then waits for the default app's provider row (1.5/3/4.5 s) | AVD API 36 |
| Write or delete messages, mark read on the phone | default SMS app | **not possible** (so read marks stay on the computer) | design |
| Receive MMS | `RECEIVE_MMS` + default app for the full flow | not requested; handled by watching the MMS provider | design |
| Grant the three permissions | one runtime dialog (SMS group) | on Android 13+ a sideloaded app may first need "Allow restricted settings"; `adb install` is exempt so it cannot be reproduced with adb | `sms-not-offered-analysis.md`, `SmsReadinessTest` |

Android 13, 14 and 16: only an API 36 (Android 16) image exists in this sandbox. Android 13 and 14 behaviour above is from
the platform documentation and the app's tests, **not checked on a device or emulator**. Per version: Android 13 adds the
restricted-settings gate for sideloaded installs (hard-restricted SMS permissions) and the notification permission; Android
14 changes nothing for SMS (the `SmsManager` system-service lookup is guarded, `smsManagerIsSystemService`); Android 16 the
same, plus the emulator's default SMS app is Google Messages. OEM variations (Samsung Auto Blocker, auto-revoke of unused
apps) show up as a missing permission and are reported by name.

## Emulator run: what happened

The scripted run (own AVD clone, desktop service built earlier, APK built from this branch) installed and **paired** (both
verification codes matched, trusted, ping ok), but the SMS stages could not be completed: after the harness wrote the
per-computer grant and the Messages switch the phone declared `sms=false` at link start and the app process was gone when
the probe ran, so no text, multipart, burst or notification result was obtained. This looks like drift in the old
`e2e-phonedata.sh` harness (it predates per-computer grants; it needed a hand-written grants file and still did not
connect) rather than an app defect, but it was **not** diagnosed further. So the following are covered by unit/integration
tests only, not by a live emulator run in this review: the limit notice banner, MMS rows from a real provider, multipart
status propagation, SIM choice. The earlier AVD evidence for permission states (switch, grants, restricted setting) is in
`real-device-sms.md`. The emulator, desktop service, Xvfb and adb server were stopped; the AVD clone and scratch were removed.

## Verification

* `cargo fmt --all` clean; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --workspace --locked --no-fail-fast` exit 0 (110 test binaries ok) on the branch before the final rebase onto origin/main.
* Gradle `:app:testDebugUnitTest :app:lintDebug` exit 0 (SmsGuardTest 15, SmsMmsTest 10, SmsParserFuzzTest 5, PhoneDataTest 30, ResourcesI18nTest 8, rest unchanged).
* `assembleDebug` with native libraries built by `scripts/build-android.sh` (debug APK 7.7 MB, arm64-v8a and x86_64).

## Left for the owner / follow-up

1. Real phone: send to a number you control from the computer 11 times in a minute and confirm the "Texts held back" notification and the `limit` message on the computer; text 9 different numbers and confirm `recipients`.
2. Real phone: receive a picture message and confirm "MMS message — open on phone" appears within a few seconds; confirm on a phone where the MMS provider is restricted that texts still list.
3. SIM chooser on a dual-SIM phone needs READ_PHONE_STATE; consider requesting it from the Messages step (separate dialog) or reporting the default SIM id in `accepted`.
4. The old `scripts/e2e-phonedata.sh` needs a per-computer grant step to run again.
