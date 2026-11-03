# phone_feed test flake (macOS CI)

Test: `phone_feed::tests::follows_params_shows_action_results_and_reports_a_down_service`, "timed out: notice".

Cause: a test sampling problem, not an ordering bug in `Feed`. After an action the worker publishes
the notice and clears it one poll cycle later (20 ms in the test). The test sampled the state every
5 ms, so a runner stall longer than about 20 ms hid the notice completely. A longer deadline would not
have helped, because the notice is gone for good once it expires. The wake-up path is fine: state is
published under the mutex before `wake()`.

Fix:
- The test now records every notice the worker publishes (from the wake callback) and waits for
  "pinged" in that record. The wait is event-driven.
- `call_feed` "a_refused_decision_is_shown_and_then_cleared" had the same transient-window shape
  (40 ms); its poll interval is now 1 s so the refusal stays observable.
- `wait_until` deadlines raised to 30 s in `phone_feed`, `call_feed` (were 3 s) and `test_support`
  (was 5 s). They return as soon as the condition holds, so passing runs take the same time.

Checks: fmt, `clippy -D warnings` and `cargo test -p bridge-apps` (205 passed) all exit 0. 200 runs of
the phone_feed and call_feed tests pinned to one CPU beside 8 busy loops: 0 failures. The failure was
not reproduced on the old code, so this is a fix from reading the code, not a measured before/after.
