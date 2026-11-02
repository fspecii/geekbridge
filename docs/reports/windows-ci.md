# Windows CI: first real run

Run 41506237970, job 125486159969 (windows-latest). `crates/bridge-update` was the only crate with failures; the job then hit
its 30-minute limit and was cancelled by two hanging tests. Fixes were validated on Linux and type-checked with
`scripts/xcheck.sh windows`; their effect on the runner is confirmed only by the next Windows run
(`.github/workflows/windows-debug.yml` runs only this crate, 12-minute cap).

## 1. `a_touched_download_directory_survives_the_sweep` — real bug (production + test)
- `Os { code: 5, PermissionDenied }`: `File::open` on a directory needs `FILE_FLAG_BACKUP_SEMANTICS` on Windows, and setting
  times needs `FILE_WRITE_ATTRIBUTES`.
- Production `touch_dir` had the same pattern and silently ignored the error, so on Windows an installer directory in use by
  another instance was never refreshed and could be swept after the age limit. Fixed through `open_dir_for_times`
  (backup semantics, write-attributes access, full share mode so it never blocks another instance); the test uses it too.
- `sweep_stale_downloads` itself only does `read_dir`, `symlink_metadata`, `remove_dir_all` (best effort, errors ignored).
  A directory held open by a running installer cannot be deleted on Windows; the sweep skips it and retries next time. Fine.

## 2. `tests/chaos_http.rs` hangs (> 60 s each) — hardened, root cause not proven
No Windows machine was available, so the exact cause was not reproduced. The deadline logic relied on per-read socket
timeouts set on a `try_clone` of the socket (a second `WSADuplicateSocket` handle on Windows), with no independent bound.
Changes:
- `http::Guard` now starts a watchdog thread per connection that `shutdown`s the socket at the overall deadline, so a read or
  write that ignores its timeout is still unblocked; `Client::get` reports the resulting error as `Timeout`.
- Socket timeouts are also set on the original handle before cloning, and are never zero.
- Test server `Silent` mode looped forever on read errors (Windows reports a peer's close as an error, not `Ok(0)`).
- Both chaos tests now run in a thread under `bounded(120 s)` and print a progress line per behaviour, so a hang names the
  case and fails the test instead of wedging the job.
- `check.yml`: the Windows/macOS test step has `timeout-minutes: 10` (and keeps `--no-fail-fast`).
If it still hangs, the progress line in the log names the behaviour.

## 3. Audit of other crates (not changed; risks to watch)
Most `set_permissions(from_mode)`, `symlink` and rename-over-script tests are `cfg(unix)`-gated. Remaining risks:
- `bridge-core/tests/ipc_client_abuse.rs:29` `listener.accept().unwrap()` with no timeout: hangs if the client never connects.
- `bridge-android/tests/transport.rs:1331` `silent.accept()` blocks without a timeout (read side is bounded to 5 s).
- `bridge-kde/tests/persistence.rs` `lock_exclusive*` (file-lock semantics differ: Windows locks are mandatory, so a second
  handle in the same process can fail differently) and the parallel-lock test at ~124.
- Tests using `tokio::time::timeout` around `read_to_end` (`control_wire.rs:112`, `control_abuse.rs:244`) are bounded.
- `bridge-android/tests/clipboard_transport.rs:578,614,626`: files opened (not directories) then deleted later; on Windows,
  deleting or renaming an open file fails. Check ordering if these fail.
- Any `remove_dir_all`/`TempDir` drop while a child process or server thread still holds a file open fails on Windows.
Recommendation: wrap new socket tests in a bounded thread helper like `bounded()` in `chaos_http.rs`.

# Second Windows run (run 41511686867, job 125486159969)

The first complete run: no hang, 14 failing tests in 8 binaries (Linux and macOS green). Nothing here was run on Windows
by the author; the fixes are checked only with `scripts/xcheck.sh windows` and on Linux. Re-run with
`gh workflow run windows-debug.yml --ref <branch> -f packages="bridge-kde bridge-service bridge-apps bridge-calls"`.

| Test | Class | Cause | Fix |
|---|---|---|---|
| `home_app::tests::sibling_falls_back_to_bare_name` | test assumption | `left: "geekbridge-nonexistent.exe"`, `right: "geekbridge-nonexistent"`: `sibling()` appends `.exe` on Windows, also for the fallback | Test expects `.exe` on Windows |
| `readiness::tests::only_hci_controllers_count_not_connections` | test assumption | `Os { code: 267, kind: NotADirectory }`: the fixture creates a directory `hci0:256`; `:` is illegal in Windows names (the code under test is Linux-only) | `#[cfg(not(windows))]` with a comment |
| `config::tests::parses_flags_and_rejects_bad_ones`, `default_tests::{phones_can_find…, listening_and_announcing…}` | test assumption | `"--state-dir must be an absolute path"`: `/tmp/gb` has no drive letter, so it is relative on Windows (the check is right) | Tests use `C:\tmp\gb` on Windows |
| `clipboard::tests::{corrupt_settings…, settings_are_validated…}` | test assumption | `InsecurePermissions … owned by S-1-5-32-544, not by the current user`: `tempfile::tempdir()` on an elevated runner is owned by Administrators and inherits its ACL; the (correct) check refuses it and settings silently fall back to defaults | Tests keep files in a `state` subdirectory that `fsutil` creates private (owner = user SID, protected DACL) |
| `lifecycle::renaming_the_computer_…` | test assumption | `the name could not be saved`: same temp dir ownership refusal in `atomic_write_private` | Same `state` subdirectory |
| `discovery_api::malformed_and_oversized_datagrams_are_dropped_without_killing_the_listener` | **real bug** | `timed out waiting for: boundary-size identity cached`. On Windows `recv_from` of a datagram larger than the buffer fails with WSAEMSGSIZE (10040) instead of truncating; `listen_discovery` used `?`, so one oversized UDP packet from anyone on the LAN ended discovery for good (also WSAECONNRESET 10054) | `listen_discovery` skips per-datagram errors (`is_per_datagram_error`) and keeps listening |
| `chaos_transfer::sender_vanishing_mid_transfer_…` | test harness | `took 15.87s` against a 10 s bound for a 3 s idle timeout. `bridge-chaos` blackhole only stopped *new* reads; chunks already queued on the bandwidth-shaped link kept being delivered (and Windows' ~15 ms timer granularity makes each shaped chunk slower), so the receiver was not idle | Blackhole now also discards queued chunks in the writer. Not proven: only the next run shows whether this was the whole delay |
| `persistence::lock_serialises_…`, `concurrent_writers_with_separate_instances…`, `concurrent_writers_in_separate_processes…` (+ `child_process_inserter`) | **undetermined**, see below | `Store("timed out after 10s waiting for another process to release …\\counter.lock")` | see below |

## The lock timeouts (`bridge-kde` persistence)

Read of `fsutil/windows.rs`: the lock file is opened `FILE_SHARE_READ|WRITE`, `LockFileEx(EXCLUSIVE|FAIL_IMMEDIATELY)` on byte
0 of a handle nobody reads, `UnlockFileEx` on drop, polling for `ERROR_LOCK_VIOLATION`. Data files are read with
`FILE_SHARE_READ|WRITE|DELETE` and replaced by `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)` with a bounded retry (20 × 25 ms)
on `ACCESS_DENIED`/`SHARING_VIOLATION`/`LOCK_VIOLATION`. I found no share-mode or ordering mistake and no path where
the lock is held forever, and `lock_times_out_while_held_and_is_released_on_drop` passes, so the lock itself works.
The most likely cause is throughput: every locked cycle does `sync_all` plus a write-through rename, 80 cycles in the
thread test (30 across processes) must all fit in the 10 s timeout of the last waiter, and the waiters polled with a flat
10 ms sleep (about 15.6 ms on Windows) while the releasing thread re-took the lock at once. That would be a slow CI disk,
not a data-loss bug, but it is not proven.

Changes: the poll now starts at 1 ms and backs off to 20 ms (`lock_private_file` on Windows); new test
`uncontended_locked_cycle_is_fast_enough` prints the per-cycle lock/read/write time (run with `--nocapture`, it is shown in
the debug workflow) and fails above 300 ms per cycle. The contended tests are unchanged on purpose.
**Next run:** if the diagnostic shows ≥ ~100 ms per cycle the timeouts are disk speed, and the contended tests should get a
lighter load or longer timeout on Windows; if the cycle is fast and the contended tests still time out, there is a real
lock or replace bug and the failing `Store(...)`/OS error text in that run is the lead.

## Only the next run can confirm
Everything above that is not marked as "test assumption with an exact quote" is unverified on Windows: the datagram fix,
the blackhole change, the lock polling and the diagnostic numbers, and whether the `state` subdirectory is accepted by the
ACL check on the runner.

## Outcome for 236a9abd4

- Windows debug run 41517054444: success.
- check.yml run 41517055060: success (all jobs, including macOS).
