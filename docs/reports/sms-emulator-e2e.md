# Messages (SMS) on the emulator: was the dead app a crash?

Follow-up to [messages-review-2.md](messages-review-2.md), whose emulator run ended with the phone declaring `sms=false` and
the app process gone, so no SMS stage ran. That report suspected `scripts/e2e-phonedata.sh` was out of date but did not
confirm it, and a dead app process could have been a real crash.

## Verdict: not a crash; the old harness was wrong

* **No crash.** In every run on a clean AVD clone (full run below, plus five earlier partial runs on the same build) the app
  process stayed alive through pairing, link restarts, 50+ sends, an MMS row, a permission revoke and the limiter notice.
  `logcat -b all` has no `FATAL EXCEPTION`, no `ANR in dev.geekbridge.android`, no `am_crash`/`am_anr` for the package.
  (The only `am_anr` in the log is `com.android.systemui` while the image boots.)
* **`sms=false` was the harness.** The phone offers Messages only with the master switch (`geekbridge_phonedata.xml`) **and**
  a per-computer grant (`geekbridge_grants.xml`, added when grants became per computer). `e2e-phonedata.sh` and
  `e2e-calendar.sh` wrote only the switches. On the emulator the phone says exactly
  `sms: not offered: per-computer Messages switch is OFF (no paired computer is allowed)`; the new stage `sms_not_declared`
  records that line.
* **"App process gone" was `pm grant`/`force-stop`.** Granting READ_SMS/SEND_SMS/RECEIVE_SMS (or `force-stop` before the prefs
  are rewritten) ends the app's process, and `stage_messages` granted the permissions and then read the desktop without
  starting the app again. `sms.sh` grants first, force-stops, writes the files, starts the app, waits for the old link to be
  seen going down, then waits for the new one.

Fixed in the harness: `e2e-phonedata.sh` and `e2e-calendar.sh` now write the grants file; `stage_messages` restarts the app
and waits for the link after granting. Guard: `scripts/tests/e2e-grants-test.sh` fails when an e2e script switches a feature
on without writing the grants file (it failed on both scripts before the fix). The two fixed scripts were not re-run live
(their own AVD `gb36d`/calendar setup was not available here); `sms.sh` exercises the same paths and passes.

## The new suite: `scripts/e2e/sms.sh`

Own AVD clone `gbsms` (from `gb36`, deleted on exit), emulator console 5688, adb server 16188, desktop control 17931 /
api 17930 / discovery 17886 / listener 1787, Xvfb :84, private session bus, state under `~/.cache/gb-tmp/sms-e2e`. Nothing
touches the live service or ports 1716, 17381, 8000. Wired into `.github/workflows/emulator-e2e.yml` as
`sms` with `experimental: true` (never turns the run red), documented in `docs/emulator-ci.md`.

Fresh clone, build of origin/main plus the changes below, 23:40-23:53 local, one run, all 14 rows PASS:

| Row | Result |
|---|---|
| `sms_env`, `sms_pair` | AVD clone boots, QR pairing through the real UI (`tap_to`) |
| `sms_not_declared` | switch on, SMS permissions granted, **no per-computer grant**: desktop `sms=waiting`; phone `sms: not offered: per-computer Messages switch is OFF` |
| `sms_declared` | with the grant: desktop `sms=ready`, `kdeconnect.sms.messages` negotiated, app pid alive, no FATAL EXCEPTION/ANR; `phone-state`: `sms: offered` |
| `sms_incoming`, `sms_thread` | `adb emu sms send 5551234 ...` appears in `geekbridge-ctl messages` with unread 1 and in its thread |
| `sms_short_code` | desktop text to `5554`: `failed (blocked_number)` (shorter than 7 digits is refused while the opt-in is off) |
| `sms_send` | desktop text to `5551234567`: `sent` (earlier runs: `delivered`) from the phone's own status packets |
| `sms_limiter`, `sms_limiter_notice` | 10 different numbers: 7 accepted, 3 held back by the phone with reason `recipients`; the phone shows the "Texts held back" notification |
| `sms_quick_eleven` | 11 quick texts to one number: 10 queued, the 11th refused by the computer (`TooMany`) |
| `sms_mms` | an MMS row inserted into `content://mms` (as root, plus an `addr` row) is listed as a picture message (no body, attachment marker) |
| `sms_sim` | READ_PHONE_STATE granted: 1 SIM listed; revoked: 0 listed (default SIM, chooser hidden) |
| `sms_no_crash` | no crash/ANR for the package in the whole log |

Notes on what the limiter can and cannot show from the desktop. The computer already limits itself to 10 texts a minute, the
same number the phone allows, so "11 quick sends" is stopped by the computer (`sms_quick_eleven`) before the phone's
per-minute rule can fire. The phone's own limiter is reached through its other rule, 8 different numbers per 10 minutes
(`sms_limiter`). The per-minute and per-hour rules of the phone stay covered by `SmsGuardTest` only.

## Bugs the live run found, fixed test first

1. **A picture message with no readable address hid its whole thread.** The phone sent the MMS row with an empty address;
   the desktop refuses a message without one (`EmptyAddress`), so the thread kept showing the previous text and the MMS never
   appeared, with no log line. Now `MmsRows.fillAddress` gives such a row the address of a text in the same thread and leaves
   it out when there is none (`SmsMmsTest.aPictureMessageWithoutAnAddress...`). Checked live: the thread shows "MMS message -
   open on phone" and the MMS row. A provider failure when reading pictures now leaves one log line with the exception class
   only (never a row), instead of nothing.
2. **A stale SIM chooser after SIM access was withdrawn.** The phone left `sims` out of a packet when it had none, and the
   desktop keeps what it had when the field is absent, so after revoking READ_PHONE_STATE the computer kept offering SIMs that
   the phone then refused as `unknown_sim`. The phone now always sends the list, empty when it has none
   (`PhoneDataTest.aPhoneWithoutSimAccess...`). The first live run failed `sms_sim` for this reason (1 listed after the revoke).

## SIM chooser gap (review 2, limitation 1)

The SIM list needs READ_PHONE_STATE, which belonged only to the Calls step. The Messages step now explains and requests it
itself, only on a phone with two SIM slots (`TelephonyManager` slot count, no permission needed) and only once Messages works
(switch on, three SMS permissions granted):

* not asked or denied: a note ("This phone has two SIM slots ... It is optional: without it every text is sent from your
  phone's default SIM.") and an **Allow SIM choice** button;
* permanently denied: the same fallback sentence plus "open app settings > Permissions > Phone", and an app-settings button;
* granted, one SIM, or Messages not ready: nothing shown.

Decision table `SmsSimChoice` is pure and tested (`SmsSimChoiceTest`, including the wording); strings `sms_sim_ask`,
`sms_sim_blocked`, `sms_sim_allow` are in `gb_phonedata.xml` (the i18n test keeps placeholders and keys aligned). The emulator
has one SIM, so the dual-SIM screen itself was **not** seen live; only the list/empty-list behaviour on the wire was.

## Checks

* Gradle `testDebugUnitTest lintDebug`: BUILD SUCCESSFUL (run after the last Kotlin change).
* `cargo test --workspace`: exit code 0 (no Rust source changed).
* `scripts/tests/e2e-grants-test.sh`: ok.

## Not covered

* MMS from a real carrier (the emulator cannot receive one); the inserted row only proves the placeholder path.
* Multipart/long texts, delivery reports from a carrier, dual SIM, Samsung/Xiaomi restrictions: see `real-device-sms.md`.
* The first pairing attempt of an earlier partial run timed out at the phone's "Pair" prompt (`pairing_timeout`, the `Pair`
  tap did not land on a loaded host) and passed on the next attempt; the other four runs and the final run paired first try.
  This is the existing `tap_to` path, not changed here, and `sms.sh` records its tap log in `dist/android/logs/e2e-sms-tap.log`.
