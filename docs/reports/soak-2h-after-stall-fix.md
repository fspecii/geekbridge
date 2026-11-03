# Two-hour soak after the control-stall fix, 2026-11-03

`SECS=7200 scripts/soak-real.sh` on a clean clone of origin/main at `ed4070a1f` (contains `5e54be057` control-stall
fix and `cecd99c7d` pairing durability), release build, sandboxed service and stand-in phone (private state under
`~/.cache`, random ports, never the live service). 122 min of load plus a 90 s idle phase. `/home` before 826 G
used / 93 G free, after 839 G / 80 G (other agents write there too; scratch deleted). The driver now samples
`smaps_rollup` itself and, when a poll is pending for 2 s or fails, writes load average, PSI, the service's and the
driver's process state, wchan and per-thread state/wchan/CPU to `poll-diag.log`.

Verdict from the script: **FAIL on control-API polls (20 of 14087, 0.14%)**. Everything else passed.

## Did the stalls vanish? No, but they fell, and the new diagnostics found a real cause

| | old run (`f2a29e178`, 4 h) | this run (2 h) |
|---|---|---|
| failed polls | 151 / 26669 (0.57%) | 20 / 14087 (0.14%) |
| after minute 10 | 119 | 10 |
| clusters | 24, 140 in the first 90 min | 3 (at 3 min, 33 min, 51 min), none in the last 70 min |

The 20 failures sit in three windows in which the host was in a disk storm from other agents' builds and tests:
loadavg 127 to 187 at 3 min and 33 min, 15 to 28 at 51 min; I/O pressure `full avg10` about 60%; btrfs in
`wait_for_commit`, `btrfs_sync_log`, `btrfs_commit_transaction`, `handle_reserve_ticket` (38 snapshots in
`poll-diag.log`). CPU pressure stayed under 2%: the machine was not short of CPU, it was short of journal commits.

In 14 of the 38 snapshots one of the service's **two async worker threads** was itself in D state, in
`wait_for_commit` / `wait_log_commit` / `btrfs_sync_log` / `btrfs_commit_transaction`: an fsync running on an async
worker. (The other snapshots show a blocking-pool thread in D with no worker in D, or no D thread at all; those are
the part I cannot attribute, see below.) The remaining fsync-on-worker sites were not the ones fixed earlier.

## Finding the sites: an fsync backtrace shim

No `strace`, and `ptrace_scope` 1 blocks gdb on a sibling process, so I built a symbolised service
(`CARGO_PROFILE_RELEASE_STRIP=none`, line tables) and ran the soak driver for 8 min with an `LD_PRELOAD` shim that
wraps `fsync`/`fdatasync`, logs fd path, thread, duration and a `backtrace()` (resolved with `addr2line`). Of 230
fsyncs, 42 (18%) ran on the async worker threads (`worker::Context::run_task` on the stack): 20 + 20 from the first site, 2 from the second:

- `Daemon::forget_consent` called inline from `Daemon::unpair` (control API) and from `trusted_loop` on
  `PeerUnpaired`: `ConsentStore::persist` (flock + fsync of `consent.json` and `.bak`) and `ShareManager::forget_device`;
- `Daemon::share_settings` -> `share::Settings::persist` (`share.json`), run inline for `ShareSettings`.

Also found by reading: `ShareManager::set_settings` / `forget_device` held the `Settings` mutex across the flock and
fsync, and `ShareManager::status` (called by the control API's share status and by `Daemon::status` paths) takes the
same mutex, so one slow write froze the readers too. `Daemon::unpair`'s fallback `store.remove` was inline as well.
The soak's driver calls `Unpair` and `ShareSettings` in every cycle, which is why the soak hit this and the
earlier fix (trust insert, debug consent) did not cover it. With two workers, two such writes on a stalled disk park
both and the control listener cannot answer (the client times out at 4 s, the service at 3 s).

## Fix (test first)

Test `share_api::slow_settings_and_consent_writes_do_not_stall_status` pairs two phones, holds the `share.json` and
`consent.json` file locks from outside, fires two `ShareSettings` and then two `Unpair` requests, and requires
`status` to answer within 2 s while they wait. It failed before (status timed out, `WouldBlock`) and passes now.

- `ShareSettings`, the `Unpair` consent/share cleanup, the `Unpair` fallback `store.remove`, and the
  `PeerUnpaired` cleanup now run in `spawn_blocking` (`forget_consent_off_worker`).
- `ShareManager`: writers serialise on a new `settings_writer` mutex and take the `Settings` mutex only to copy and to
  publish; the file lock and fsync happen with no reader-visible lock held (`SettingsWriter`).

Checks: share_api 11/11, `--lib` 296, control_api 24, clipboard_api, lifecycle, state_compat all pass; clippy clean.
Re-running the shim on the fixed binary: 212 fsyncs, 206 on the blocking pool, 6 at startup, **0 on async workers**
(was 42 of 230), and no poll errors on a calm disk.

## What is still not shown

- A 2 h storm-free-or-not soak of the fixed binary was not run; the storms come from other agents and cannot be
  ordered up. The fix is proven by the failing-first test and the fsync trace, not by a field soak with the host
  stormed. In several snapshots (for example at 178 to 193 s) no thread of the service was in D state while polls
  still timed out; at load 130+ with the disk saturated the service can also stall on page faults of dropped code
  pages (`memtrim`'s `codepages::drop_idle`, seen as `filemap_fault` in D at 248 s) and on writes to `service.log`. Those
  are not removable by code on a saturated shared disk, and the 4 s client timeout is the other half. A follow-up
  could skip `drop_idle` when the PSI I/O `full` figure is high, but I did not change it without evidence.
- The failure rate is not a pass for the verdict script: it fails any poll error after minute 10. In a storm of
  load 187 this is host noise as much as service behaviour; I did not loosen the script.

## Anonymous memory (heap and stacks)

`anon_kib` (= Private_Dirty), mean per 20 min over the run: 2526, 3676, 3330, 3668, 4157, 4329, 4152. Trimmed to
t >= 10 min: OLS +669 KiB/h over the window, but +141 KiB/h (Theil-Sen +379) over the last 30 min, and the last
four readings sit between 4.15 and 4.45 MB. A fit of `a + b ln t` gives b = 552 KiB per e-fold, so about 276 KiB/h at
2 h and about 46 KiB/h at 12 h. The old 4 h run reached 3.5 to 4.1 MB in hours 3 and 4 (mean 3731, 3547, 4011,
4136). Two independent runs therefore plateau at the same 4.1 to 4.4 MB with load, dips included (40 to 60 min here,
the trim at 140 min there): **the Anonymous creep is a warm-up to a working-set ceiling (glibc arenas and caches
under 187 pairings and 800 MB moved), not a leak**, and I consider it closed. It is the fit's early steep part that
makes the 2 h OLS look like 670 KiB/h; the late part does not. Series with budgets: RSS drift 1.7 MB over the window
(budget 3.0), threads, fds, state files and bytes, log, events (cap 128/256) all flat. Wake-ups median 15.8/s (p95
45.5) with slope -0.24/s per hour; CPU median 0.8% (p95 2.1%), slope -0.04%/h. Idle: 0.3 wake-ups/s, 0.02% CPU,
fds 14, threads 5. Counters: 335 reconnects (0 failed), 187 pairings / 186 unpairings (5 pairing failures under the
faulty profile), 603 files moved, 794 MB, 9923 packets sent / 2727 received.

## Reproduce

    SECS=7200 scripts/soak-real.sh     # now also writes poll-diag.log and pss/priv_dirty/anon columns
    # samples.csv, poll-errors.log, poll-diag.log, verdict.md are copied to dist/soak/real-<ts>/
