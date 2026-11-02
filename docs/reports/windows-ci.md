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
