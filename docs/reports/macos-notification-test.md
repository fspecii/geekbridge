# macOS failure: notifications_go_only_to_allowed_computers_and_requests_surface_as_events

Verdict: test race, not a transport bug.

Cause: the phone's data hub releases events once a wait has handed them out (`delivered` floor in
`transport/engine/data.rs`). The test sends a `.request` and a `.action` back to back, then
called `events(0, 1)` and `events(first.seq, 1)`. On a fast machine (macOS runner) both packets
were already queued, so the first wait returned both; the second wait found nothing (already
delivered) and timed out with `seq: 2, events: []`. On Linux the first wait usually returned
before the second packet arrived.

Reproduced on Linux with `taskset -c 0` plus six busy loops (about 9 of 25 failures after a first
naive fix). Link/allow-list/tie-break path was not involved: one link, gate behaved correctly.

Fix (tests only):
- data_transport.rs: the test now waits for both events at once and checks their order.
- data_transport.rs and calls_transport.rs `events()` helpers accumulate events across polls,
  because earlier polls release what they return (a later poll would otherwise lose them).
- clipboard_transport.rs left unchanged: its hub replays events by `since`, so accumulating
  would duplicate them.

Verification under the same load: 30 runs of data_transport and 20 runs of data+calls transport,
0 failures. fmt, clippy -D warnings and bridge-android tests pass.
