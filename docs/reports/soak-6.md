# Soak 6: the real service after the slow-disk fixes, 2026-11-06

Scope: `geekbridge-service` built from `origin/main` (824f69d14; service code identical to 4ff72908d, the only newer
commit is documentation) in a private HOME/XDG, ports 30000-30999 (`--control-port` set), a private PipeWire and D-Bus, a
stand-in paired phone behind the fault proxy, and a loopback mock release server on 30990. Method and fail criteria as
in `soak-5.md`; scratch and `CARGO_TARGET_DIR` under `/home/ambsd/.cache/gb-soak6-*`. Nothing touched the live service,
ports 1716 / 17381 / 8000, or any process I did not start. Soak-5 predates: no fsync on the async workers (7d6a63cd2),
the unpair task and the pairing write retry, `bridge-service` at opt-level z (82d1e02c0), message search, the drop_idle
race fix.

Runs (all `scripts/soak-features.sh`, `SOAK_UPDATE_MOCK=1 SOAK_IO=1`):

| run | length | purpose |
|---|---|---|
| main | 72.2 min (4200 s + quiesce), 114 rings | the soak; I/O windows of 240 s at 30 / 55 / 80% of the run |
| targeted | 27.4 min | driver diagnostics for the pairing stalls (late completion, busy answers), same windows |
| final | 12 min, 120 s windows | driver after the fixes below: exit 0, verdict PASS, history 3 of 3 checks complete |

Machine: 24 cores, shared and busy. Load average (1 min) during the main run: median 9.3, mean 11.6, min 0.2, max 82
(sampled every 30 s). Targeted run: median 18.7, max 42. Final: 28-45.

## New in the driver (`crates/bridge-service/tests/soak_real.rs`, `scripts/soak-features.sh`)

- **Per-conversation history completeness.** The stand-in phone has 100 conversations: 8 with 2,000 messages (the
  service's per-conversation cap, 100 pages of 20) and 92 with 40 (16,000 + 3,680 held, under the 20,000 per-phone cap).
  Every fourth data round the driver turns the phone's SMS consent off and on (forgets the store), asks for the list, and
  pages every conversation to its end, one request per answer (a second request is sent only when the held count moved
  or 16 s passed). Then for each conversation it asserts: held count equals what the phone has (2,000 / 40), every
  message is of this link's history, no gap (k falls by one from the oldest to the newest), and the newest is k=0.
  A history is void (counted as skipped, logged) when the phone is not paired, the consent call did not finish in time,
  nothing is held for any conversation at the end, or two links' histories are mixed. `history.log`, `extras.txt`.
  Dates and ids of a link's history no longer overlap another link's.
- **I/O-stress windows** (`GEEKBRIDGE_SOAK_IO=1`, `SOAK_IO=1`): two writers (`dd`, 256 MiB of incompressible data
  four times a pass, `conv=fsync`, in a loop) on the same filesystem as the service's state, while the control API is
  pinged every 200 ms (Status / ShareStatus / ClipboardStatus). The test fails if the slowest ping exceeds 5 s.
  `io-stress.log`.
- Step names and status dumps for every failed pairing (`pair-fail-<s>.json`) and a late-completion wait (90 s) after a
  pairing missed its 20 s, so a slow pairing is told from a lost one.
- Fixed from soak-5: the re-pair failure dump went to a directory the driver deleted (now the run dir, copied by the
  script; this is the dump soak-5 lacked). `repair_p0` waits up to 60 s for the unpair and retries a `Busy` connect for a
  minute (it was giving up after 6 s and leaving the stand-in phone unpaired for ~25 minutes, see below).

## Verdict

**No service leak, no deadlock, no wakeup regression, no lost data. No service code changed.**
The control API stayed responsive through 38 GiB of fsynced writes: 3,143 pings at 200 ms, 0 failed, 0 over 1 s,
slowest 8 ms (soak-5: one stall of 35 s). The pairing and unpair paths still take tens of seconds under that load
because the disk does, not because of the service (below). The script verdict FAILs on pairing and reconnect counts
that all trace to that.

| criterion | soak-5 final (92 min) | soak-6 main (72 min) |
|---|---|---|
| RSS slope on the floor (budget 3072 KiB / window) | -249 KiB/h, drift -331 KiB | -1241 KiB/h Theil-Sen, drift -1250 KiB, tau -0.41: ok (falls after the first half) |
| RSS min / median / max | 6.6 / 8.7 / 11.0 MB | 7.8 / 10.3 / 13.8 MB |
| anonymous memory | 1.1 -> 3.2 MB | 1.1 -> 4.2 median, max 6.3, end 5.2 MB (the deep histories: up to 19,680 messages held, about 194 B each) |
| fds / threads | fds 14-26 (idle 14), threads 6-28 (idle 6), drift 0 | fds 14-26 (idle 15), threads 6-27 (idle 8), drift 0 |
| children / banners / notifications / events | within caps | within caps (children <= 2, banners 11 of 12, events 128 of 256) |
| state size | ring-capped 263 KB | 204 KB (cap 320 KiB) |
| wake-ups/s during the run | median 16.3, p95 48.4, +4.7/h | median 26.6, p95 1116, +5.7/h (tau 0.06) |
| CPU | median 0.8%, p95 2.2%, slope 0 | median 1.0%, p95 8.4%, slope -0.2 %/h |
| idle after the load | 0.3 wake-ups/s, 0.04% CPU | 0.3 wake-ups/s, 0.07% CPU, RSS 11.8 MB, fds 15, threads 8 |
| errors in service log / panics | 0 / 0 | 0 / 0 |
| control calls slower than 1 s | 2 (one 35 s) | 0 (slowest 29 ms in any 30 s window; 3,143 stress pings, slowest 8 ms) |
| orphan card / pw-cat / Wi-Fi session | 0 / 0 / 0 | 0 / 0 / 0, ring_stuck 0, card_missing 0 |

Wake-up and CPU p95 are higher because the load is: a deep history burst moves 16,000-20,000 messages in 18 s about
every 200 s (a burst is 9% of the run time; it lands in the p95 and not in the median, 26.6 vs 16.3 wake-ups/s). Idle
after the load is the same 0.3/s. RSS is higher because the store now really holds 16,000 messages for most of the run.
The RSS and anon series end below their peak with a negative slope: no leak.

Counters (main): 114 rings (37 answered / 33 declined / 37 missed), 23 Wi-Fi listen sessions live of 25, 78 reconnects
ok (8 failed), 53 pairings ok (7 failed), 43 unpairs, files 70 up / 96 down (985 MB, 20 failed), 5,149 packets sent,
21,672 received, 8,190 control polls (0 failed), 24,708 feature requests.

### Message history (the new check)

15 of 17 checks complete: 1,500 conversations, each with exactly 2,000 (8 of 100) or 40 messages, no gap, newest k=0,
every page arrived. A check takes 17.9 s for 20,000 messages (100 paged conversations, 16 requests in flight, 60
packets/s service rate limit). The other two (2864 s, 3582 s) found no conversation at all: the stand-in phone was
unpaired at the time (see 2 below). The first version of the check counted that as incomplete; the final driver calls it
void and a 12 minute run with it has 3 of 3 complete (300 conversations). The same driver found no service bug in the
paging: the queue of 16, the stale-request timer and the per-conversation trim all held.

### Update checks, DND, clipboard, files, rings

Update checks (two per data round against the mock, plus the scheduler): no failures, no slow call; the mock log has 60
answered release lookups (`GET /repos/.../releases/latest` 200). It also logs 207 `GET /` 404 and 416 unparsable
(TLS-looking) requests on that port from clients I did not identify; the service log has no error line, so I did not
chase them. DND toggled every data round, clipboard churn, 1.0 GB of file transfers in both directions and 114
rings with transfers and Wi-Fi listen sessions: no resource effect. CPU per MB of file: total CPU divided by MB moved is
35 ms/MB in soak-5 and 91 ms/MB here, but that is the whole process, and the history bursts are the difference. Taking
the intervals without a history burst and subtracting the median CPU of intervals with no transfer (0.93% soak-5,
0.95% soak-6) the excess per MB moved is -0.5 ms/MB (soak-5) and -3.2 ms/MB (soak-6): below the noise, no measurable
CPU per MB in either. (The sampling interval is 30 s on a busy host; this is a bound, not a measurement.)

## What the verdict script flagged, and why it is the disk

**Pairings that missed their 20 s: 7 of 60.** Five fresh-identity pairings (all five inside I/O windows) and two
stand-in re-pairs. Every status dump (`pair-fail-*.json`, `repair-fail-3702.json`) shows the link in phase `pairing`
with the challenge already approved and nothing else wrong: the service is writing the trust record (file lock, write,
fsync, rename, fsync of the directory) before it sends the accept. Outside the I/O windows the host's own disk load made
four more pairings take 9.8-17.5 s (the usual one is 0.7 s). In the targeted run, pairings that missed 20 s completed
`late-ok after 13 s more` and `after 23 s more`: late, not lost.

The disk, measured on its own with the same two writers and a probe doing what the pairing does (write 2 KiB, fsync,
rename, fsync the directory, every 200 ms, 90 s): without writers p50 3 ms, max 36 ms; with them **p50 107 ms, p90
24 s, max 32 s** per durable write. Pairing needs one, plus the consent files; unpair needs several. That is the cost of
durable-before-accept on a saturated btrfs, as in soak-5 (stack traces in `btrfs_sync_log` and `wait_for_commit`); the
fix in 7d6a63cd2 keeps it off the async workers, and that works: while a pairing waits 60 s, status, ping, clipboard and
rings keep answering in milliseconds. I did not weaken durability.

**The stand-in phone stayed unpaired for ~25 minutes (2568 s to 4049 s of the main run).** `repair-p0` unpaired it, the
unpair needed more than the 15 s the driver waited, and the connect that followed got the service's deliberate answer
`Busy: the phone is still being unpaired; try again in a moment` (7d6a63cd2 keeps a fresh pairing from being overwritten
by the unpair still deleting the record). The driver gave up after 6 s, and the next re-pair is 30 rounds later. All 8
reconnect failures, all 20 `ring_not_seen`, and (not separated out) probably most of the 20 failed file transfers fall in that window; they fail by
construction. This was soak-5's "open" item (re-pair failing, dump lost): it is the same mechanism, now with the dump. The
targeted run reproduced the busy answer (`busy answers 1` in `events.tsv`) and the driver, now waiting, re-paired.
Not a service defect: the service answers truthfully and the work finishes. If it should be nicer for a user, the
settings page could show "still unpairing" rather than an error; I did not change it.

## Not covered

The real card window, a real notification daemon, Bluetooth hands-free, two phones ringing at once, contact/agenda replies
(the stand-in does not answer them). The stand-in sends 20 SMS per page and ignores the requested count; a real phone
sends 50 and has MMS, long bodies and attachments (the message store clips bodies at 4 KiB, not exercised here). The
stand-in's history is one link's and static; a phone receiving new messages while paging is covered by the unit tests, not
here. The disk numbers are for this btrfs volume on this busy host, with two writers; another filesystem, an SSD with
a different cache or a lighter load will stall less (and a USB stick much more); the 5 s ping bound is a service
check, not a statement about pairing time. Pairing, unpair and consent changes are not covered by any latency budget
here, only by "completes". The run is 72 minutes.

## Reproduce

    GB_TMP=$HOME/.cache/gb-soak6-tmp CARGO_TARGET_DIR=$HOME/.cache/gb-soak6-target GEEKBRIDGE_SOAK_PORT_BASE=30000 \
      SOAK_UPDATE_MOCK=1 SOAK_UPDATE_PORT=30990 SOAK_IO=1 SECS=4200 scripts/soak-features.sh --keep

(`GEEKBRIDGE_SOAK_IO_WINDOW` seconds per window, default 240; `GEEKBRIDGE_SOAK_IO_MAX_MS`, default 5000.)

Checks: `cargo fmt --check`, `cargo clippy -p bridge-service --tests -- -D warnings`, `scripts/sim-nonlinux.sh`,
`scripts/perf-check.sh` (service 7,305,880 B of 7,560,000; ctl 1,712,488 B of 1,745,000; idle 2.6 MB, 7 threads, 19 fds,
2 wake-ups/s): all within budget. No service code changed, so no regression test was added; the driver's own history
and I/O assertions are the new checks.
