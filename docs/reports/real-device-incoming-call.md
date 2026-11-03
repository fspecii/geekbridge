# Real device: incoming call never shown on the PC (Fold5, Android 17)

Date: 2026-11-04. Device: Samsung SM-F946B, Android 17 / API 37, debug APK code 10099 (`v010-rc1-125-gb9b67bdae`).
The ranked analysis is in `incoming-call-rca.md`. This report covers what was found while looking at the code and the
live (read-only) state, what was changed, and what to check on the phone next.

## Certain

- The live phone log is NOT stuck. It streams past 01:18 (display and GC lines). "No call-state lines" proves nothing,
  because the call path had no logging at all.
- The process was alive and the link was `Running` during both calls (foreground service, EXEMPTED bucket, not dozing).
  The "frozen by Samsung's freezer" idea is not supported.
- Phone and PC gates all pass: READ_PHONE_STATE, ANSWER_PHONE_CALLS, Calls=ON, calls grant for the computer, PC
  Telephony consent ON, `calls: offered`. READ_CALL_LOG is not needed to learn RINGING (it only adds the number).
- The old code observed call state through ONE `TelephonyCallback` on the default `TelephonyManager`. It did not use
  `createForSubscriptionId`, had no per-SIM callbacks and used the phone-state broadcast only for the number, and only
  with READ_CALL_LOG.
- A failed registration (`MonitorStart.Failed`) was swallowed silently and never retried until the next link restart.
- Reproduced on an API 36 emulator clone (single SIM): with the new build the log shows, per call, the source that
  reported (`sub1/slot0`, `default`, `broadcast`), the merged state, the hand-over to the engine and the report
  outcome (`ringing -> sent queued=1`), and the PC shows the ringing call. No API 37 image is available offline.

## New evidence (owner)

The calls rang on eSIM 2, and eSIM 2 is the only connected SIM. So the default `TelephonyManager` (default
subscription) can sit on an inactive slot. This is the most likely cause. It is still not proven on the Fold5.

## Guessed (not verified)

- That Samsung/Android 17 deliver the default-subscription callback only for the default SIM.
- Any Android 17 specific restriction on call-state callbacks (none was found; none is claimed).

## What changed

1. Call state now comes from three kinds of source, merged: the default callback, one `TelephonyCallback` per active
   subscription (re-made on `OnSubscriptionsChangedListener`), and the `ACTION_PHONE_STATE_CHANGED` broadcast (all SIMs;
   active whenever READ_PHONE_STATE is granted). Ringing wins over off-hook, which wins over idle; idle only when every
   source is idle (`CallSourceMerger`, tests in `CallStateTest.kt`).
2. A failed registration is retried every 30 s while calls are on and the link is up (`ListenerRetry`).
3. The executor thread is shut down on stop (it leaked one thread per link restart).
4. INFO logging, tag `GB`, area `calls` (codes and counts only, no numbers, no ICCID): listener registered or not
   registered with a reason code, each source report, each merged state, the state handed to the engine, and each
   report outcome (`sent queued=N`, `refused CODE`, `unchanged`, `skipped_calls_off`).
5. The debug phone-state dump has a `== Calls ==` section: process importance and service state, listener
   registered/not (why), active subscriptions (count, slot, eSIM flag), default voice subscription id, last state per
   source with time, merged changes, hand-overs, and the last report outcome.

## Next steps on the phone

Install the new debug APK, keep Calls on, call the Fold5 from the iPhone, then:

- `geekbridge-ctl phone-log <id> -n 200 | grep ' calls:'`
  - `listener registered via TelephonyCallback` and `active subscriptions: 1 [subN slot S eSIM]; default voice subscription: ...`
    on link start.
  - On a call: `source subN/slotS reported ringing`, `callback #K merged=ringing`, `state ringing handed to the call
    engine`, `report to core: ringing -> sent queued=1`.
- `geekbridge-ctl phone-state <id>`, section `== Calls ==`.
- Reading it: only `broadcast` or only `subN` reported ringing and `default` stayed idle: the dual-SIM cause is
  confirmed. No source reported anything: Android did not call the app (look at `process importance`). `queued=0`:
  the computer is not connected or not permitted. `refused CODE`: the core rejected the report.
