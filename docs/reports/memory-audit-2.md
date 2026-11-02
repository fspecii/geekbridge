# Memory and CPU audit 2

> Status: snapshot from 2026-11-02, see git history for later changes.

Scope: the desktop service (`bridge-service`) and the Android Rust core (`bridge-android`). Measured with the existing
tests (`rss_kib` / `fd_count` helpers), release profile, `GEEKBRIDGE_SOAK_CYCLES=1000`.

## Measurements

| Test | Result |
| --- | --- |
| Service, 1000 reconnect cycles (alternating phone-vanishes / service-hangs-up) | fds 12 -> 12, RSS 7352 -> 7636 KiB after a 100-cycle warm-up (+284 KiB) |
| Service, 255 pair/unpair cycles with fresh identities | fds 12 -> 12, RSS 7356 -> 7564 KiB (+208 KiB) |
| Service, 4000 junk connections | fds 13 -> 13, RSS 6132 -> 6236 KiB (+104 KiB) |
| Android core, 1000-cycle reconnect storm (loss, fragmentation, resets) | fds 12 -> 17 (budget +6), threads 6 -> 6, RSS 6632 -> 6840 KiB |
| Service idle RSS | about 15 MB for the real daemon (docs/reports/scale-report.md); the test-process figures above include the test harness and stand-in phones. The real daemon was not started here: it would bind the live discovery and link ports of the running session. |

Growth is a few hundred bytes per cycle at most, which is allocator noise rather than a leak (it does not scale with
cycle count between 80 and 1000).

## Structures reviewed

Bounded, no change needed: event log (`EVENT_CAP` = 256 ring, `PingReceived` rate-limited), pending pairing challenges
(`MAX_CHALLENGES` = 8), reconnect book (entries dropped by `keep_only` / `settled`), discovery `SourceLimiter`
(256 sources, 4 ids each), `phone_debug` links/waiters (removed on link down; log files rotate at 5 MiB, 3 kept),
clipboard peers/pulls (removed on link down, pulls expire), share transfers (`MAX_TRANSFERS` = 64, offered/active caps),
call-control `unnamed_since` (retained to the ringing set), calendar `alerted` (retained to live keys), the Android
event log (asserted <= 512).

## Defect found and fixed

SMS send book grew without bound on a link that stays up. `Dev::requests`, `Dev::send_reasons` and the `SendLedger`
only emptied when the phone's SMS data was cleared. Every sent text stayed forever, each status snapshot
(`messages_view().sends`) listed all of them, and at 10,000 records the ledger refused every further send until the link
was reset. Fix: `SendLedger::prune_settled(keep)` forgets the oldest settled sends (sent, delivered, failed) beyond
`SEND_HISTORY` = 200 per phone; in-flight and uncertain sends are never dropped. `Dev::dispatch` prunes and drops the
matching `requests` / `send_reasons` entries.

## Regression tests and budgets

- `bridge_core::messages::tests::prune_settled_keeps_in_flight_and_the_newest`
- `bridge_service::phone_data::tests::send_book_stays_bounded_over_a_long_lived_link` (600 sends: ledger, requests,
  reasons and snapshot all <= 200)
- `crates/bridge-service/tests/leaks.rs`: RSS growth budget tightened from 6 MiB to 1.5 MiB in release builds
  (6 MiB kept for debug builds); measured growth is under 300 KiB.

## Repeating this

    GEEKBRIDGE_SOAK_CYCLES=1000 cargo test --release -p bridge-service --test leaks -- --nocapture --test-threads=1
    GEEKBRIDGE_SOAK=1 cargo test --release -p bridge-android --test chaos reconnect_storm -- --nocapture
