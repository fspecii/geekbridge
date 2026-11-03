# Windows persistence test stability

## Failure (check.yml, windows-latest, run 41650103995, real-device-kit commit)

Three tests in `crates/bridge-kde/tests/persistence.rs` failed in a 43 s run, all with the same text:

    Store("timed out after 10s waiting for another process to release ...\counter.lock")   (thread test, 5 threads)
    Store("timed out after 10s waiting for another process to release ...\trusted_peers.json.lock")   (separate instances; separate processes, in a child)
    Io(Os { code: 5, kind: PermissionDenied, message: "Access is denied." })   (one writer, `MoveFileExW` retries exhausted)

check.yml runs without `--nocapture`, so that run carries no per-cycle numbers.

## Numbers from the debug workflow (Windows runner, bridge-kde only)

Uncontended: `per cycle: lock ~0.2-0.6 ms, read ~0.05-0.1 ms, atomic write 8-16 ms`. The disk is not slow: 80 locked cycles are
about 1 s of work in total, an order of magnitude below the 10 s lock timeout. So a healthy runner does not explain a 10 s wait.

## Diagnosis: the lock was unfair

The thread test now records each thread's worst wait for the lock. With the old `LOCKFILE_FAIL_IMMEDIATELY` polling:

    threads: total 1.15 s, max wait 894 ms, per-thread max wait [7.6, 466, 894, 752, 480, 609, 142, 562] ms

One thread waited 78 % of the whole run, another 0.7 %: a thread that unlocks and re-locks within microseconds almost always
beats pollers that sleep 1-20 ms, so a waiter sits through other threads' cycles in an arbitrary order. Its wait is bounded by
the total work of everyone else, not by the number of other waiters. Whenever the runner stalls (a write-through taking seconds,
Defender, a noisy neighbour), the total work passes 10 s and the unlucky waiter times out even though the lock is never stuck.
That matches the failure: all three tests hit the product timeout together. A real fairness defect, not only disk speed.

## Fix

Product (`fsutil/windows.rs`):
- The lock file is opened `FILE_FLAG_OVERLAPPED` and a *blocking* `LockFileEx` is queued with an event; the wait is
  `WaitForSingleObject(timeout)`, on timeout `CancelIoEx` + `GetOverlappedResult(wait)` withdraws the request (if it was granted
  in the race, it is taken). The kernel hands the lock to queued requests as the holder unlocks, in arrival order. The product
  timeout (`LOCK_TIMEOUT`, 10 s) and the error text are unchanged.
- `MoveFileExW` retry is bounded by time (5 s, backoff 5 ms to 100 ms) instead of 20 x 25 ms, for the `ACCESS_DENIED` seen above.

After the change (three debug runs, same runner type):

    threads: total 1.16 s, per-thread max wait [126, 123, 121, 123, 122, 122, 123, 125] ms
    threads: total 0.89 s, per-thread max wait [128, 122, 120, 82, 118, 122, 127, 125] ms
    threads: total 0.71 s, per-thread max wait [69, 65, 68, 68, 68, 70, 70, 65] ms

Flat waits, about (threads - 1) x cycle, instead of a long tail.

Tests:
- The contended tests calibrate on the machine first (`cycle_cost`: worst of five locked cycles, doubled for a trust-store
  insert) and run as many cycles as fit in a 3 s budget, never fewer than a small minimum (so they still contend), never more than
  before (80 / 20 / 30). At 500 ms per write they run 8 / 4 / 4 cycles, a few seconds. They still assert no lost updates and no
  deadlock. The thread test uses a 300 s lock timeout; the trust-store tests cannot change the product timeout and rely on the budget.
- Each test prints `cycle cost ...`; the thread test prints total, max wait, max hold and per-thread max wait.
- `windows-debug.yml` has a `test_threads` input (default 1; empty = default parallelism like check.yml).

## Verification

See the section appended to `windows-ci.md` for the run numbers.
