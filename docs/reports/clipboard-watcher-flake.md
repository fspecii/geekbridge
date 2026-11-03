# Clipboard watcher flake: `watcher_stops_and_forgets_text_when_last_device_is_disabled`

## Verdict

Both. The test slept a fixed 120 ms, which is a race on a loaded runner, but the failure also exposed a
real race in the service, with a privacy impact.

## Real race

`ClipboardService::observe` reads the clipboard, then (a) takes the lock to decide, (b) drops the lock to
await the blocking sensitivity probe, and (c) re-takes the lock and stores `last_text`. If the last device
was disabled during (a)-(c), `set_enabled(false)` had already cleared `last_text` through
`forget_text_if_idle`, and the watcher then wrote the clipboard text back. The text stayed in memory until
the next enable, and could be used as a baseline or announced to the next device.

## Fix

`observe` now returns without storing when no peer has sync enabled, both when it first takes the lock and
after the sensitivity await (crates/bridge-service/src/clipboard.rs).

## Tests

- New deterministic test `disable_while_watcher_is_mid_read_does_not_restore_the_text`. A gated backend
  parks the watcher inside the sensitivity probe, the device is disabled, then the watcher is released.
  It fails without the fix and passes with it.
- The original test now polls for `watching == false` with a 5 s deadline instead of one fixed sleep.

## Verification

200 looped runs of both tests, pinned to one CPU with `taskset -c 0` while all cores were saturated with busy
loops: 0 failures. fmt, `clippy --all-targets -D warnings` and the 30 clipboard tests all exit 0.
