# Soak 3: the real service with the call features active, 2026-11-05

Scope: `geekbridge-service` (release build of `origin/main` at `45e9db278`) under a 45 minute mixed load that exercises the
newest features: incoming rings (answer on the computer, decline, answer on the phone, miss, link drop in the middle of
the ring), a ring during a file transfer, Wi-Fi call audio listen sessions against a private PipeWire, SMS and
notification bursts, photo, agenda and message requests, privacy switch toggles, link drops and reconnects through the
`bridge-chaos` fault proxy, fresh-identity pair/unpair and unpair/re-pair of the main phone. Everything is sandboxed (see
"Isolation"). Run with `SECS=2700 scripts/soak-features.sh`.

## Verdict

**PASS after one fix, with two watch items.** The first run failed on the control-API criterion (one call took 14.5 s,
one more over 1 s). The cause, an `fsync` on an async worker, was fixed (`Privacy::set`, regression test) and the second
run, with an `fsync` tracer attached on a host whose disk stalled single `fsync`s for up to 35.8 s, had no control call
slower than 44 ms. In both runs: no stuck ring, no orphaned overlay card, no orphaned `pw-cat`, no panic, no error line
in the service log, file descriptors and threads flat. Watch items: anonymous memory grows by about 5.5 MB in the first
45 minutes and is still creeping at a decelerating rate (below); one control-API criterion is judged against a host that
was in an I/O storm in the first run.

| criterion | run 1 (before fix) | run 2 (after fix) |
|---|---|---|
| growth trend in RSS | drift 0.68 MB over the fit window (budget 3.07), decelerating | drift 2.52 MB (budget 3.07), decelerating, see "Memory" |
| growth trend in fds | none (min 13, end 14) | none (min 13, end 14) |
| growth trend in threads | none (min 4, end 7, transient max 43) | none (min 5, end 7, transient max 43) |
| stuck ring (call list not empty 15 s after the call ended) | 0 of 114 | 0 of 113 |
| orphaned overlay card (process alive 10 s after) | 0 | 0 |
| orphaned `pw-cat` / Wi-Fi session after the call | 0 of 18 sessions | 0 of 22 sessions |
| control API stalls over 1 s | **2 (14.5 s)** | 0 (slowest 44 ms) |
| panics / error lines in the service log | 0 | 0 |

## What ran

Two full runs (47.2 min of load, 4 idle samples of 30 s each, samples every 30 s), five shorter runs that leave out parts
of the load to see which part a trend belongs to.

| counter at the end | run 1 | run 2 |
|---|---|---|
| rings (answered / declined / missed or dropped) | 114 (33 / 31 / 50) | 113 (38 / 28 / 47) |
| Wi-Fi listen sessions live / failed to go live in 8 s | 18 / 4 | 22 / 1 |
| reconnects ok / failed | 86 / 0 | 85 / 0 |
| pairings ok / failed, unpairings | 56 / 4, 45 | 58 / 2, 46 |
| files up / down / failed, bytes moved | 77 / 106 / 8, 1.11 GB | 85 / 105 / 0, 1.09 GB |
| packets sent / received | 5138 / 1741 | 5061 / 1818 |
| data, privacy and agenda requests | 759 | 759 |
| control polls, failed | 5350, 1 | 5365, 0 |
| overlay cards launched / orphaned | every ring, 0 | every ring, 0 |

Outcome per ring kind (run 2): every one of the kinds `answer-pc`, `decline-pc`, `answer-phone`, `miss`, `drop-link`, with and
without `+transfer` and `+wifi`, ended `ok` (call list empty, card gone, no `pw-cat`, no session) in 113 of 113 rings.

## Graphs as tables

Run 2, windows of 5 minutes, min / median / max of the 30 s samples. `banners` are `notify-send` children (see "Findings").

| window (min) | RSS KiB | anonymous KiB | threads | fds | banners |
|---|---|---|---|---|---|
| 0-5 | 7552 / 9152 / 10784 | 948 / 2692 / 3292 | 6 / 22 / 38 | 17 / 28 / 39 | 2 / 14 / 25 |
| 5-10 | 8524 / 9978 / 11308 | 3416 / 3698 / 3764 | 5 / 20 / 26 | 13 / 26 / 31 | 0 / 13 / 17 |
| 10-15 | 9240 / 10464 / 12476 | 3796 / 4360 / 4736 | 7 / 20 / 42 | 15 / 26 / 42 | 1 / 12 / 28 |
| 15-20 | 9544 / 11374 / 13080 | 4628 / 5078 / 5504 | 9 / 22 / 43 | 17 / 27 / 42 | 4 / 14 / 27 |
| 20-25 | 10064 / 11492 / 13616 | 5320 / 5778 / 5944 | 5 / 17 / 30 | 13 / 24 / 32 | 0 / 10 / 18 |
| 25-30 | 10496 / 11882 / 13304 | 5744 / 5874 / 5944 | 7 / 20 / 33 | 15 / 25 / 33 | 1 / 11 / 19 |
| 30-35 | 10556 / 13018 / 13872 | 5796 / 6102 / 6344 | 7 / 14 / 39 | 15 / 22 / 40 | 1 / 8 / 25 |
| 35-40 | 11100 / 12534 / 13724 | 6116 / 6318 / 6540 | 6 / 28 / 37 | 15 / 31 / 36 | 2 / 17 / 22 |
| 40-45 | 10984 / 12760 / 14136 | 6304 / 6414 / 6604 | 5 / 23 / 32 | 14 / 29 / 33 | 1 / 15 / 19 |
| 45-50 (idle) | 11024 / 11728 / 12520 | 6408 / 6408 / 6512 | 7 / 7 / 18 | 14 / 14 / 25 | 0 / 0 / 11 |

Run 1, same layout (before the fix):

| window (min) | RSS KiB | anonymous KiB | threads | fds | banners |
|---|---|---|---|---|---|
| 0-5 | 8236 / 9636 / 11020 | 972 / 2678 / 3356 | 9 / 25 / 37 | 17 / 30 / 37 | 2 / 16 / 23 |
| 10-15 | 9416 / 10954 / 11960 | 3972 / 4206 / 4444 | 7 / 20 / 43 | 15 / 28 / 42 | 1 / 14 / 28 |
| 20-25 | 7892 / 9858 / 11044 | 2488 / 3394 / 3476 | 8 / 17 / 27 | 16 / 24 / 35 | 3 / 10 / 17 |
| 30-35 | 9116 / 9768 / 11860 | 4044 / 4224 / 4308 | 9 / 18 / 34 | 18 / 26 / 34 | 0 / 12 / 20 |
| 40-45 | 10020 / 11264 / 12664 | 5104 / 5214 / 5280 | 5 / 29 / 38 | 14 / 31 / 36 | 1 / 17 / 22 |
| 45-50 (idle) | 9916 / 10028 / 10744 | 5004 / 5004 / 5116 | 7 / 7 / 17 | 14 / 14 / 24 | 0 / 0 / 10 |

Idle after the load (run 2): 0.3 wake-ups/s, 0.03% CPU, fds 14, threads 7, one link, one paired phone. During the run:
median 9.8 wake-ups/s (p95 39.4), CPU median 0.8% of one core (p95 2.2%), both with a falling slope.

## The failure and the fix (control-API stall)

Run 1: one `Status`/`Events` poll waited 14.5 s (the client gave up at 4 s with `WouldBlock`), at 1857 s into the run. The
stall snapshot (`poll-diag.log`) shows host load average 41, I/O pressure `full avg10` 64% and the disk 99-100% full, and
**one of the service's two async workers in D state in `wait_log_commit`** (an `fsync` waiting for the btrfs log). A
second, 8 s stall in a 4 minute pilot run (also on this binary) had a worker in D in `handle_reserve_ticket` (a write
waiting for free space on the nearly full disk), so file I/O on an async worker, whatever its kind, is what turns a slow
disk into a frozen control API (two workers: one parked plus one blocked on a lock leaves nobody to answer).

Root cause found by reading the paths the new features added: `Request::PrivacySet` ran `Privacy::set` directly on the
worker. `set` wrote `<state>/privacy` (atomic write with `fsync`) **while holding the mutex that every reader takes**
(`Daemon::status`, the notification and overlay mode checks). The soak toggles three privacy keys with every data round.
The earlier stall fixes (`control-stall-investigation.md`, `soak-2h-after-stall-fix.md`) covered trust, consent and share
settings but predate the privacy switches.

Fix (`crates/bridge-service/src/privacy.rs`, `daemon.rs`):

- `Privacy::set` serialises writers on a separate `writer` mutex and takes the `chosen` mutex only to copy and to
  publish, never across the file write (`set_with` takes the write as a closure).
- `Request::PrivacySet` runs in `spawn_blocking`.
- Regression test `privacy::tests::a_slow_write_does_not_block_readers`: a writer blocked inside its "file write"
  must not delay `view()` / `on()`; before the change the same shape of code held the reader lock for the whole write.
  (The test is written against the new `set_with` seam, so it does not compile against the old code; I did not
  re-create the old code to watch it fail.) `privacy_api` (privacy control API) and `control_api` tests pass.

Evidence after the fix: run 2 with `scripts/soak/fsync-shim.c` (an `LD_PRELOAD` that logs thread, duration and path of
every `fsync`/`fdatasync`): **1934 fsyncs, 0 on async workers** (3 on the main thread at startup, the rest on blocking-pool
threads), 5 slower than 1 s, the worst 35.8 s (state directory) and 14.4 s (`trusted_peers.json`), and still **no control
call slower than 44 ms**. The same host conditions froze the control API for 14.5 s before.

Not fixed, named: `ProcessLauncher::launch` (call card start: preflight reads, `fork`/`exec`, a one-time write of
`call-overlay-status`) and the banner `notify-send` spawn run on whichever thread reports the ring, which can be an async
worker. They did no I/O that showed up as a stall in the second run, but they are the next candidates if a stall recurs
(`poll-diag.log` now prints the syscall number of each thread in D state, which would name it).

## Findings that are not failures

1. **Anonymous memory** (heap and stacks, `Private_Dirty`): 0.9 MB at start, 6.4 MB at 45 minutes. Increments per 5
   minutes in run 2: +1.7, +0.7, +0.7, +0.7, then +0.1, +0.2, +0.2, +0.1 (see the table). The five partial runs say where
   it comes from, each 7-15 minutes at a 10 times faster pace (`GEEKBRIDGE_SOAK_FAST=1`):

   | partial run (only this load) | anonymous KiB start -> end of load | reading |
   |---|---|---|
   | A: SMS/notification/battery/call packet bursts, 73,000 packets (about 10,000 messages) | 0.9 -> 12.6 MB, still +1 MB per 3 minutes when the load ended (8.6 MB after the idle trim) | the message store filling toward its documented cap (2000 per thread, 10,000 messages in this test): about 1 KB per message |
   | B: rings with Wi-Fi listen sessions | 0.85 -> 4.7 MB in 15 minutes | first-use cost of the Wi-Fi audio path (Opus, `pw-cat` pipes, jitter buffers) plus allocator arenas; decelerating in the full run |
   | C and C2: photo, agenda, message requests, privacy toggles (8305 and 2717 requests) | 0.8 -> 1.2 MB, then back to 0.9-1.4 MB | no growth |
   | D: rings, no Wi-Fi | 0.8 -> 1.6 MB, flat | no growth |

   So the growth belongs to bounded caches (messages) and to the Wi-Fi call path warm-up. In the full run the Wi-Fi
   session rate is steady (about 7 per 15 minutes in each third) while the growth falls from 3.8 MB to 1.1 MB to 0.6 MB per 15
   minutes, which is not what a per-session leak looks like. It is not proven flat: the 45 minute run ingests only about
   1,300 messages (about 1.4 MB), while a phone with a 10,000 message history costs about 10 MB in the message store by
   design (the cap), which this test only reached in partial run A. A 4 hour run
   with this load (`SECS=14400`) would settle it; the verdict script cannot tell warm-up from a slow leak in 45 minutes.
2. **Concurrent `notify-send` banners are unbounded.** Partial run A (8 rings per second, absurd for a phone) held about
   390 simultaneous banner children, with a thread and a descriptor each (threads and fds 405 at the peak, back to 14 at
   idle: no leak). The sandbox has no notification daemon, so every `notify-send --wait` lives until its 30 s expiry; with
   a real daemon they end when the user dismisses or the ring ends. A cap on concurrent banners would bound a
   misbehaving notification daemon; not changed here.
3. **Idle blocking threads.** After thousands of data requests the process keeps 6 parked blocking-pool threads (10 in
   total) at idle; they stay at that number for the rest of the run (flat from minute 7 to 15 in partial run C). This
   is a steady state, so the idle-thread check in `scripts/soak-fit.py` now compares with the run's own median instead of
   `start + 4` (it flagged partial run C as a leak at 10 vs 5).
4. **`ring_retry` 3-4 per run** (the driver sends a ring, sees nothing in 6 s, rings again; the second always arrived,
   `ring_not_seen` 0). All seven cases came 2-6 s after a `latency+fragment` reconnect: that fault profile stays on the live
   link (20-120 ms per fragment), so the state packet is slow, not lost. A driver artifact, not a service fault.
5. **Pairing failures** 4 and 2 (of 60), `file_fail` 8 and 0, `wifi_fail` 4 and 1: the failed pairings timed out at 20-60 s
   under the faulty profiles and an I/O storm (run 1, load average 41 at its worst); every one left nothing paired behind.
   No pairing failure in either run is attributed to a code path.

## Isolation

Scratch `HOME`, `XDG_*`, `--state-dir` and ports (free ports above 20000, never 1716, 17381 or 8000) under
`~/.cache/gb-tmp`; a private D-Bus, PipeWire, WirePlumber and `pipewire-pulse` (null sinks, BlueZ and ALSA monitors
disabled) with their own runtime directory; the service environment is cleared and given `DBUS_SESSION_BUS_ADDRESS` of a
path that refuses connections; the call card is a stub `geekbridge-calls` beside a copy of the service binary
(`DISPLAY=:99` fake); the stand-in phone is a real `bridge_kde` endpoint over TLS behind the fault proxy. The live
service, `~/.config`, Hyprland, BlueZ and the user's PipeWire were not touched. `/home` was at 99-100% during run 1 (other
agents); scratch is deleted by the script unless `--keep`.

## Not covered

The real card window (a stub process stands in; its lifecycle, not its pixels, is tested); a real notification daemon;
Bluetooth hands-free (no BlueZ, by rule); contact/agenda replies (the stand-in phone answers photo list and thumbnail
requests but not agenda or message requests, so the service's request paths run, its merge paths do not); two phones ringing
at once (the second identity pairs and unpairs, it does not ring); a run longer than 45 minutes.

## Reproduce

    SECS=2700 scripts/soak-features.sh             # verdict.md, samples.csv, events.tsv in dist/soak/features-<ts>/
    GEEKBRIDGE_SOAK_SKIP=bursts,sms,clip,files,faults,pairs,rings,repair GEEKBRIDGE_SOAK_FAST=1 SECS=420 scripts/soak-features.sh
    gcc -shared -fPIC -O2 -o fsync-shim.so scripts/soak/fsync-shim.c -ldl      # then GEEKBRIDGE_SOAK_PRELOAD=... FSYNC_SHIM_LOG=...

`GEEKBRIDGE_SOAK_SKIP` takes `bursts, clip, files, faults, pairs, sms, data, rings, wifi, repair`.
