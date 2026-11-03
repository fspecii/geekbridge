# Contacts and calendar review

> Status: snapshot from 2026-11-03, see git history for later changes.

Scope: the phone's Contacts and Calendar providers, the link, the desktop hub and views, two-way calendar edit.
Sandbox only (no live service, config or ports). Rust and JVM tests, the deterministic fuzzer and the existing scale
tests ran in full; the new emulator `cases` stage was written but **not run** (see "Not done").

## 1. Large data

Re-run of `GEEKBRIDGE_SCALE=1 cargo test -p bridge-service --test scale --release` (5,000 contacts, 1,000 events per sync
from a 20,000-row provider): contacts first batch 13 ms, all 5,000 in 49 ms (200 round trips, 1.9 MB on the wire),
view 5.5 ms, search 3.6 ms, digit search 8 ms, relist 13 ms, RSS +23 MB; calendar full sync 499 KB applies in 4.8 ms,
month view 0.2 ms, one-event delta 1.5 KB. Emulator numbers (22.7 s for 5,000 contacts, PSS 58-71 MB) are from
`scale-report.md` and were not repeated.

Code review of the phone side: both listings are streaming cursors read to a cap (`listUids` stops at 5,000 pairs,
the Instances cursor at cap+1 per side, 25 vCards per request, reminders/attendees in batches of 200 events), every
read has a `CancellationSignal`. No full list is held except the 5,000 (key, timestamp) pairs.
Packets: 900 KiB cap with halving; 64 KiB per vCard.

Defect: the desktop kept the raw vCard of every contact although nothing reads it (up to 64 KiB each, 320 MB worst
case for 5,000). Now only the parsed fields are kept.
Virtualised lists / frame cost: unchanged from `scale-report.md` (0.23 ms for 1,000 rows); not re-measured.

## 2. Correctness: defects found and fixed (test first where a failing case could be written)

| # | Defect | Fix |
| --- | --- | --- |
| 1 | **Edit conflict**: a PC edit made from a stale view silently overwrote an edit made on the phone meanwhile | update/delete carry `baseRev` (the revision the desktop holds); the phone recomputes the event's current revision with the same code that builds the sync and answers `changed`, writing nothing; the desktop shows why and asks for a delta. Older desktops (no `baseRev`) behave as before |
| 2 | Editing an **invitation you do not organise** "succeeded" and Google's copy came back at the next sync | phone refuses updates with `not_organizer` (deleting stays allowed) |
| 3 | **All-day events off UTC midnight** (some sync adapters), or end == start, were dropped silently | snapped outwards to whole days |
| 4 | vCard **Latin-1/Windows-1252 quoted-printable** decoded as UTF-8 (U+FFFD) | charset honoured |
| 5 | vCard `\;` `\,` and QP `=3B` split names/organisations wrongly; name suffix dropped | split on raw text first, then decode; suffix kept |
| 6 | `tel:` / `mailto:` values; the same number or address in two spellings listed twice | prefixes stripped; numbers deduped on digits, addresses case-insensitively (compared as stored: found by the fuzzer) |
| 7 | `item1.PHOTO` not stripped by the desktop parser (the phone did) | group prefix handled |
| 8 | Bidi embedding / override characters (U+202A-E) in contact names, event text | dropped on the desktop |
| 9 | A contact with a huge note (> 64 KiB card) vanished completely | properties over 2 KB are dropped, identity kept |

Checked and fine: recurrence is expanded by the provider (one instance per row, series/exception/EXDATE/cancelled all
come out of the Instances table), the desktop never expands RRULEs; multi-account calendars carry account and colour;
read-only calendars are flagged on the phone and refused on both ends; one-off timed events keep their zone, all-day
ones are UTC dates (no floating times exist in the Calendar Provider); deletions propagate by `deleted` tombstones.
Known gap: a property name folded across lines (`item1` + ` .PHOTO`) is not recognised as PHOTO by the stripper on
either side; bounded by the 64 KiB card cap.

## 3. Privacy

Both ends gate on consent (existing tests: nothing requested or answered without it). Findings:

* A phone that **stopped offering** contacts or calendar (switched off or permission revoked in its app) left its
  data on the desktop until restart. `link_up` now clears the held data of a feature the phone no longer offers.
* The phone's per-session contact listing cache (lookup keys) survived the session; now dropped on reset.
* The phone's persisted write ledger (operation ids, event ids) survived withdrawing editing/calendar; now cleared.
* Desktop data is memory only (no file writes in the hub); consent off and unpair erase it (existing tests).
* Logs: no content is logged on either side (only feature flags); the emulator `cases` stage greps for it but was not run.

Not touched: SMS data is retained on the desktop in the same phone-withdrawal case (messages hold send state); follow-up.

## 4. Fuzzing

`vcard` and `phone_wire` (calendar sync parser) already existed. There is no iCalendar text parser (the phone
expands series). Added: target `agenda` (stored contact/event/recurrence JSON, RRULE/RDATE/EXDATE text, dates,
`When::from_android`), oracles in `vcard` (no control or bidi override characters, numbers unique), a Kotlin seeded
mutation test (`ContactsCalendarAbuseTest`). Deterministic runs: vcard 350k, agenda 1.06M, phone_wire 519k inputs,
0 findings after fix 6 (the first vcard run found the dedupe-after-truncation bug in my own change).

## Tooling

`geekbridge-ctl calendar-update`; `scripts/scale-e2e.sh` accepts name/port overrides and has `cases` and `hold`
stages; `ScaleSeedReceiver` seeds named scenarios (`what=cases`).

## Not done

The emulator `cases` run (DST series, exception, EXDATE, conflict, invitation, deletion both ways, phone-side
withdrawal, log grep) could not complete: the host fell below the 10 GB free-disk limit (other jobs were filling it),
the AVD clone was removed and everything stopped. The stage is in the script, un-run. The Android-side revision
recomputation (`currentRevision`) and guest detection are therefore verified by JVM tests with fakes only.

Update: the `cases` stage has since been run on an emulator; see [contacts-calendar-emulator.md](contacts-calendar-emulator.md).
