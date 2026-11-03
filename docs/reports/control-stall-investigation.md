# Control-API stall while pairing: investigation

Trigger: `soak-2026-11-03.md` item 4 (13 of 7,675 polls timed out in two ~30 s windows, each during the pairing of a
fresh identity; the driver's `Unpair` fell into the stall).

## Verdict

A real, user-visible hang mechanism, found by code reading and proven by two failing-first tests. Not reproduced
end to end on a healthy disk (see "What was not reproduced").

## Cause

The service runs two async workers. On a paired link, two stores were written with a `std::sync::Mutex` held across
file I/O, and the same mutex is what the control API's `status` (and every handshake `store.get`) takes:

1. `FileTrustStore::modify` (`crates/bridge-kde/src/store.rs`) took the in-memory `state` mutex, then waited for the
   cross-process `flock` (up to `LOCK_TIMEOUT` = 10 s, polling with `thread::sleep`), then wrote with fsync, all
   under that mutex. `get` and `list` need the same mutex, so one slow insert froze every reader.
2. `ConsentStore` (`crates/bridge-service/src/private.rs`) was behind `Mutex<ConsentStore>` in the daemon, and
   `default_debug_on` / `set` / `forget` did flock + fsync while holding it. `Daemon::status` takes that mutex.
   `default_debug_on` runs on the async worker right after a pairing completes, for any phone that declares the debug
   packets (the soak's stand-in phone declares every family), which matches "the stall starts when the trust record is
   stamped".
3. Both writers were called directly on async workers (`PairingLink::complete` insert, `TrustedLink` remove,
   `debug_link`), so a slow fsync also parked a worker. With two workers, one parked writer plus one `status` call
   blocked on the mutex leaves no worker for accept, so even a refused/timeout hello follows.

Why a 250 ms fsync shim did not show it: a stall needs the write to take longer than the poll timeout (4 s). A disk
shared with heavy builds can do that; 250 ms cannot. TLS handshake CPU, `StateFile` itself, `control.rs` and the ipc
module were read and are not involved (no lock held across `.await`, no blocking I/O in the exchange).

## Fix

- `FileTrustStore`: readers take only the short `state` mutex; writers serialise on a separate `writer` mutex and
  re-take `state` only to apply and publish. A writer stuck on the lock or an fsync no longer blocks `get`/`list`.
- `ConsentStore`: same split (`view` mutex for memory, `writer` mutex for read-modify-write); methods take `&self`;
  the daemon no longer wraps it in a mutex.
- Trust insert on pairing, trust remove on unpair / peer-unpair, the debug-consent default and `SetConsent` run in
  `spawn_blocking`, so they do not park the two async workers.
- Not moved: `Daemon::unpair`'s fallback `store.remove` and `forget_consent` (user-initiated, rare, and no longer
  able to block readers). A follow-up could move them too.

## Tests (written first, failed before the fix)

- `store::tests::readers_are_not_blocked_by_a_writer_waiting_for_the_file_lock`: holds the lock file from another
  descriptor, starts an insert, asserts `list`/`get` return in under 200 ms. Before: the reader waited the full 10 s.
- `private::tests::consent_reads_are_not_blocked_by_a_writer_waiting_for_the_file_lock`: same for `is_enabled`
  (did not compile before: reads needed `&mut` through the daemon mutex).

## What was not reproduced

A 150 s `scripts/soak-real.sh` run (private state under `~/.cache/gb-tmp`, random ports) on the unfixed binary had
no poll errors; the original stall needed a disk that stalled fsync for seconds. The mechanism is therefore shown by
the deterministic tests above, not by a field repro. If a stall recurs, `poll-errors.log` in the soak result
directory timestamps it; correlate with `iostat`/`dmesg` for blocked I/O.
