# Flake sweep 2

Goal: find the tests that fail intermittently, fix their causes (not their symptoms), and show the suite is
quiet. Method: `cargo test --workspace --locked --no-fail-fast` in a clean worktree, 8 runs before and 8 runs
after, alternating idle and loaded (`taskset -c 0-1` with 6 busy loops sharing those two cores). The
machine was also shared with other builds (load average 8 to 90 at times), which made it a harsher
environment than CI.

## Before / after

Baseline is origin/main 961977c99 (8 runs). After is origin/main 7ace58a12 plus this change (8 runs,
4 idle and 4 loaded, all exit 0). Counts are failing runs out of 8.

| Test | Before | After | Cause |
|---|---|---|---|
| `bridge-apps` calendar `stepping_and_jumping_move_the_shown_day_in_every_mode` | 8/8 | 0/8 | Used today's date, so it failed from the 4th of every month. Fixed upstream in 1308b99c9 (fixed clock); not duplicated here. |
| `bridge-calls` dbus_mock `service_loss_and_restart_void_old_handles` | 2/8 (loaded) | 0/8 | `drop(mock)` is asynchronous: the next call could reach the dying connection and come back `NoReply`. |
| `bridge-service` photos_api `a_full_picture_is_taken_only_because_it_was_asked_for_...` | 1/8 (loaded) | 0/8 | `try_recv` on the fake phone's "seen" channel raced the phone task, which reports an ask after serving it; thumbnail asks can interleave. |
| `bridge-fuzz` deterministic `every_target_survives_2000_deterministic_inputs` | 2/8 (idle and loaded) | 0/8 | The 1.5 s wall-clock "slow input" finding fired when the process was descheduled (vcard, hub_clipboard, cert). |
| `bridge-kde` cert `concurrent_initialization_returns_one_persisted_identity` | 0/8, seen in extra loaded runs | 0/8 | Eight threads serialised on the identity lock (key generation plus fsyncs) hit the production 10 s lock wait. |
| `bridge-android` chaos `a_nudge_does_not_undo_the_phone_users_disconnect` (any chaos pairing) | 0/8, seen in extra loaded runs | 0/8 | The desk endpoint used the production 25 s pairing response timeout; a starved test thread can exceed it. |
| `bridge-android` transport sharing tests (`peer task timed out`) | 0/8, seen in extra loaded runs | 0/8 | 10 and 15 s harness deadlines for positive waits. |
| `bridge-service` clipboard_api `paste_from_phone_pulls_a_text_item_and_reports_the_outcome` | 0/8, seen in extra loaded runs | 0/8 | "delivered" means accepted; the clipboard write is a background task, and the test read the clipboard at once. |
| `bridge-service` control API tests (`expected ack ... service closed the connection`): battery, control_api unpair/debug logs, photos_api consent, leaks `pair_unpair_cycles` | 0/8, seen in extra runs (idle and loaded) | 0/8 | The control exchange was cancelled at 3 s (client 4 s). Handlers fsync several state files; on a slow disk the request was cut off half done. |

"Seen in extra runs" are failures from the interim after-runs while the fixes were being developed; they
did not occur in the 8 baseline runs, which shows the baseline sample is small and the tail is long.

## Changes

* `crates/bridge-calls/tests/common/mod.rs`, `dbus_mock.rs`: `MockService::stop(&bus)` drops the service and
  waits until the bus lists no owner for the name.
* `crates/bridge-service/tests/photos_api.rs`: wait (30 s, returns early) for the `Get p2` ask.
* `crates/bridge-fuzz/src/harness.rs`: a slow run is repeated up to three more times and counts only if the
  fastest attempt is still over the limit; panics and allocation findings are unchanged.
* `crates/bridge-kde/src/cert.rs`: `load_or_create_waiting(dir, lock_timeout)`; production keeps 10 s, the
  concurrency test uses 300 s.
* `crates/bridge-android/tests/chaos.rs`: the desk endpoint's pairing timeouts are 300 s (the chaos under test
  is in the network, not in the pairing clock).
* `crates/bridge-android/tests/transport.rs`: positive waits raised to 60 s and the join to 120 s; all of
  them return as soon as the event happens.
* `crates/bridge-service/tests/clipboard_api.rs`: poll (30 s, 10 ms steps) for the clipboard text after "delivered".
* Control API deadlines: `EXCHANGE_DEADLINE` 3 s to 8 s (`crates/bridge-service/src/control.rs`) and
  `CLIENT_DEADLINE` 4 s to 10 s (`crates/bridge-core/src/ipc.rs`). The client stays longer than the service so a slow
  request is answered or closed by the service first. This is a behaviour change: a hung service now makes
  `geekbridge-ctl` wait up to 10 s instead of 4 s. Tests that waited for the cut-off scale with the constants
  (`control_wire.rs`, `control_api.rs`, the slow-peer test in `ipc.rs`).

## Scans

* `thread::sleep(` / `tokio::time::sleep(` in tests: roughly 300 uses. The ones inspected are poll steps
  (10 to 30 ms) inside loops with a deadline, or "nothing must happen" pauses (200 to 400 ms) that can only pass
  falsely, never fail falsely, so they were left.
* Sub-200 ms deadlines in assertions: `chaos.rs:918`, `dbus_mock.rs:665`, `control_api.rs:679`,
  `audio_route.rs:41`. All are negative checks or a configured mock timeout, not deadlines for something that must happen.
* Shared ports: the rigs bind `127.0.0.1:0`. Env vars: the only `set_var` in the workspace tests is the `TZ` test in
  `localtime.rs`, which no other test depends on.

## Not reproduced here

macOS phone_feed, clipboard watcher, Windows persistence and qr_pairing under load did not fail on Linux in any
run; the phone_feed and clipboard-watcher ones were already fixed earlier today (3db630ea3, dcc1f41b0). The
deadline changes above also cover the slow-disk shape of the Windows persistence failures, but that was not
verified on Windows.

## Measurement notes

* An interim set of after-runs was discarded because a background `dd oflag=dsync` fsync load of mine was
  still running; its failures were excluded from the table except as "seen in extra runs".
* The final 8 runs ran on a quiet tree with no edits in between. fmt and `clippy --workspace --all-targets
  --locked -- -D warnings` both exit 0.
