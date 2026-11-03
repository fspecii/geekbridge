# Contacts and calendar: the emulator `cases` stage, run

The `cases` stage of `scripts/scale-e2e.sh` (written in the contacts/calendar review, [contacts-calendar-review.md](contacts-calendar-review.md))
was never run because the disk ran low. This is the run. Real Calendar and Contacts providers on an API 36 emulator, the real
debug APK, the real desktop service and `geekbridge-ctl`, a clean worktree of `origin/main`, made-up data.

## Setup

Own AVD clone `gb36cc` (console 5780, adb server 16270), control port 17851, Xvfb :97, own desktop cargo dir under
`~/.cache`; nothing on 1716 / 17381 / 8000 was touched. 88 GB free before, about 75 GB during. Everything stopped afterwards
(`scripts/scale-e2e.sh --stop`, adb server killed, scratch removed).

## Result

Final run (`env,battery,pair,cases`, fresh emulator, script exit code 0): all 25 checks PASS.

| Area | Check | Result |
|---|---|---|
| Recurrence | weekly series across the 28 Mar 2027 DST change shows 15 Mar and 5 Apr at 09:00 local (08:00Z / 07:00Z) | PASS |
| Exceptions | moved occurrence shown once at its new time; cancelled occurrence absent; EXDATE removed; series marked recurring | PASS |
| All-day | one day and three days across a month end keep 86 400 000 ms multiples (snap to UTC midnights) | PASS |
| Zones, accounts | Tokyo event 08:00 JST with its zone; same title in two accounts kept apart; read-only calendar flagged | PASS |
| Contacts | unicode / RTL names arrive intact; the U+202E override is stripped from the name; duplicate numbers collapsed; 100 kB note does not drop the contact | PASS |
| Revision recompute | edit based on a stale version: the phone answers `changed` and keeps its title; the same edit from the current version applies | PASS |
| Invitation | update of an event the user does not organise: `not_organizer` | PASS |
| Read-only calendar | update refused on the desktop before anything is sent | PASS |
| Deletion | deleted on the phone leaves the desktop; deleted from the desktop leaves the phone | PASS |
| Consent | desktop withdraws contacts / calendar: people, events and counts all zero at once | PASS |
| Phone withdraws | phone stops offering contacts and calendar (prefs wiped, link redialled): the desktop copy is gone, state `not_offered` | PASS |
| Logs | no event title, name, e-mail or number in logcat or the service log | PASS |

## Defects found, and what they were

1. **A calendar change could be lost on the phone (real, fixed, Kotlin).** The provider observer pushes at most every 30 s; a change
   inside that spacing scheduled one retry, and the retry called `onCalendarChanged()` once and gave up if another push (an applied
   edit, the answer to a refresh) had moved the spacing in the meantime. The change then stayed unsent until some unrelated change.
   Reproduced by hand: edit, stale edit (`changed`), then delete an event on the phone: the desktop still showed it 100 s later.
   Fix: the engine remembers that a change is owed (`calOwed`, cleared by any push, which always reads the provider afresh),
   exposes `calendarRetryInMs()`, and the retry in `PhoneDataRuntime` keeps rescheduling until nothing is owed. Tests:
   `PhoneDataTest.aChangeSeenTooSoonIsRememberedUntilSomePushCoversIt`, `anyPushClearsTheOwedChange`; the end-to-end check
   `case_phone_delete_reaches_desktop` failed before and passes after.
2. **The seeded series was not realistic (test data, fixed).** Exceptions linked to the series by `ORIGINAL_ID` alone made the
   stock provider drop the series and the moved occurrence from Instances (the cancelled one stayed). Real sync adapters name the
   series with `_SYNC_ID` and the exceptions with `ORIGINAL_SYNC_ID`; the seeder now does, and the provider then returns exactly
   the expected instances. No change to the product: the phone reads Instances, which the provider keeps correct for real data.
3. **The run measured a stale desktop (script, fixed).** `scripts/e2e-android.sh` builds the desktop service only if the binary is
   missing, and `target/*` is redirected to a cargo directory shared by all e2e jobs; the service there predated the review, so the
   first run showed bogus failures (no bidi stripping, no `changed`, ...). `scale-e2e.sh` now runs the incremental cargo build on every
   `env` (`SCALE_PREBUILT=1` skips it in CI), and its `--stop` pattern now matches the binary it started.
4. **Check expressions and timing (script, fixed).** After consent is off the device list is empty, so `devices[0]` raised; the
   checks now accept an empty list. The invitation reply is polled for 10 s instead of read after a fixed 4 s. The invitation call's
   output is kept in the log.

No Rust change was needed: the desktop-side behaviour (baseRev check, `changed` refresh, bidi stripping, phone-withdrawn data) was
correct once the stale binary was out of the way.

## Not defects, but noted

- `pair` failed once in four runs ("no pairing dialog with a code on the phone") on a freshly started AVD, and `battery`
  once on the very first boot of a new clone ("never reached the Battery step"); both passed on the rerun. The CI job is
  non-blocking for that reason.
- The cases stage must start from a freshly started emulator; re-seeding a provider that already holds a seeded series gave
  odd Instances. The script header says so.
- The emulator and the service outlive a script invocation (a transient systemd unit); the old header claimed otherwise.

## Wiring

- `scripts/scale-e2e.sh`: `SCALE_TARGET`, `SCALE_SCRATCH`, `SCALE_PREBUILT`, exit status non-zero when a result line of the run is FAIL.
- `.github/workflows/emulator-e2e.yml`: suite `contacts-calendar-cases` (non-blocking, `experimental: true`), see docs/emulator-ci.md.
- docs/phone-data-link.md lists the stage under Emulator tests.

## Reproduce

```
scripts/scale-e2e.sh --stop
SCALE_AVD=gb36cc SCALE_EMU_PORT=5780 SCALE_ADB_PORT=16270 SCALE_CTRL_PORT=17851 SCALE_API_PORT=17850 \
  SCALE_DISC_PORT=17856 SCALE_LISTEN_PORT=1747 SCALE_REDIR_PORT=17861 SCALE_XDISP=:97 SCALE_TAG=-cc \
  scripts/scale-e2e.sh env,battery,pair,cases; echo $?
scripts/scale-e2e.sh --stop   # same variables
```

Results land in `dist/android/logs/scale-results-cc.tsv`, per-query JSON in `dist/android/logs/cases-cc/`.
