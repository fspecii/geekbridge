# CI red fix 6

Causes and fixes:

1. Calendar test depended on the real date. `stepping_and_jumping_move_the_shown_day_in_every_mode`
   expected a month step to move 28 to 31 days, but a month step deliberately lands on the 1st of the
   next month, so from the 4th of a month (from 2026-11-04) the gap is 27 days. The Month grid logic
   itself is correct (always six Monday-first rows; 1 Nov 2026 is a Sunday). The test now runs over a
   table of fixed dates (month starts and ends, Sunday the 1st, leap day, year ends, DST change weeks)
   and asserts the real rule (lands on the 1st of the next month).
   New tests cover 4, 5 and 6 week months and leap years.
2. Determinism: `localtime` gained a thread-local `FixedClock` and `unix_now_ms()`; `localtime::now()`
   and the apps' `now_ms()` helpers (calendar, contacts, messages, home) use it. The calendar and
   messages render tests pin Wed 4 Nov 2026 noon UTC. Remaining `SystemTime` uses are id generators.
3. Dead-code helper in the android transport test was fixed concurrently upstream (cfg(unix)); no change here.
   `#[cfg_attr(not(unix), allow(dead_code))]`, fixing the Windows/macOS dead-code error.
4. `bridge-calls` `dbus_mock::signals_wake_the_watcher_and_quiet_times_out` drained startup signals
   with one fixed 200 ms wait; it now drains until the bus is quiet (bounded), before and after the burst.

Local: cargo fmt, clippy --workspace --all-targets -D warnings (exit 0), bridge-apps, dbus_mock and
android transport tests pass.
