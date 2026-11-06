# Soak 5: the real service after the idle-wakeup and queue changes, 2026-11-06

Scope: `geekbridge-service` built from `origin/main` (6a1614bf5 plus the driver changes below) in a private HOME/XDG,
ports 27000-27999 (`--control-port` set), a private PipeWire and D-Bus, a stand-in paired phone behind the fault proxy,
and a loopback mock release server on 27990. Method and fail criteria as in `soak-4.md`; scratch and `CARGO_TARGET_DIR`
under `/home/ambsd/.cache/gb-soak5-*`. Nothing touched the live service, ports 1716 / 17381 / 8000, or any other process.

New in the driver (`crates/bridge-service/tests/soak_real.rs`, `scripts/soak-features.sh`):

- Message history bursts: the stand-in phone answers `request_conversations` with 100 conversations and
  `request_conversation` with 20-message pages from 200 messages per conversation (a new history on every link, so
  reconnects bring new messages and the store evicts). Every data round asks for the list and then older pages of all 100
  conversations (exercises `pump_older`, the 16-in-flight queue).
- Update checks: `UpdateCheck` + `UpdateStatus` twice per data round against `scripts/e2e/mock_release_server.py`
  (`SOAK_UPDATE_MOCK=1`, `GEEKBRIDGE_UPDATE_BASE_URL`); the background scheduler runs too. `update-state.json` showed the
  mock release (v99.0.0) found.
- `GEEKBRIDGE_SOAK_PORT_BASE` picks ports from a range; call event storms (8 ringing/idle flips, then the ring) are now
  sent in order with each ring instead of from the unordered burst generator (see "Harness findings").
- Failure evidence: the driver dumps the call list and call trace tail for a stuck ring and the status for a failed
  re-pair; each repair failure records the step that stopped it.

Already in the driver from soak-4: link drops/redials with fault profiles, pair/unpair, DND toggled every data round,
clipboard churn, file transfers in both directions, rings (answer / decline / decline with text / miss / link drop,
some during transfers and Wi-Fi listen sessions).

## Verdict

**No service leak, no deadlock, no wakeup regression. No service code changed.** The final 92 minute run fails the
script verdict only on items that come from the shared machine (see below), not on any resource series.

Machine: 24 cores, shared and busy. Load average (1 min) during the final run: median 9.7, max 172 (other jobs; I/O
pressure `some avg60` up to 78% during the worst stall). The first run: 2-75, mean 14.

| criterion | first run (92 min, 214 rings) | final run (92 min, 185 rings) |
|---|---|---|
| RSS slope on the floor (budget 3072 KiB over the window) | +449 KiB/h Theil-Sen, drift 591 KiB, tau 0.13: ok | -249 KiB/h, drift -331 KiB, tau -0.16: ok (second half falling) |
| RSS min / median / max | 8.1 / 10.9 / 11.6 MB | 6.6 / 8.7 / 11.0 MB |
| anonymous memory | 1.1 -> 3.6 MB, then flat 3.4-3.9 | 1.1 -> 3.2 MB, flat after minute 25, 3.1 at end |
| fds / threads | flat: fds 14-29 (idle 15), threads 6-30 (idle 9), drift 0 | flat: fds 14-26 (idle 14), threads 6-28 (idle 6), drift 0 |
| children / banners / notifications / events | within caps (children <= 3, banners 11 of 12, events 128 of 256) | same (banners 11) |
| state size | ring-capped at 265 KB (cap 320 KiB) | ring-capped at 263 KB |
| wake-ups/s during the run | median 13.2, p95 47.7, slope +0.39/h | median 16.3, p95 48.4, slope +4.7/h (tau 0.12) |
| CPU | median 1.0%, p95 2.4%, slope +0.07 %/h | median 0.8%, p95 2.2%, slope 0 |
| idle wake-ups after the load | 0.3/s, 0.02% CPU | 0.3/s, 0.04% CPU |
| error lines in the service log / panics | 0 / 0 | 0 / 0 |
| control calls slower than 1 s | 0 (slowest 232 ms) | 2 (one 35 s, see below) |
| orphan card / `pw-cat` / Wi-Fi session | 0 / 0 / 0 (1 ring stuck, see "Harness findings") | 0 / 0 / 0 |

Idle wake-ups 0.3/s match the 0.2-0.3/s the idle-wakeup work reports (soak-4: 0.3/s); the redial pass waiting 30 s and
the DND keep-warm thread sleeping while the setting is off cost nothing visible. Wake-ups during the load are higher than
soak-4's median of 5.1/s because this load adds 100-conversation history bursts, update checks and more traffic;
the slope has tau 0.0-0.12, no trend.

Final run counters: 185 rings (62 answered / 65 declined / 52 missed), 39 Wi-Fi listen sessions live of 39, 128 reconnects
ok (11 failed), 92 pairings ok (6 failed), 75 unpairs, files 126 up / 157 down (1.63 GB, 11 failed), 7857 packets sent,
5251 received, 10571 control polls (2 failed), 12432 feature requests, ring_stuck 0, card_missing 0.

## What the verdict script flagged, and why it is not the service

1. **A 35 s control stall at 4858 s (final run).** `poll-diag.log` shows the service's tokio worker in state `D`
   (`btrfs_sync_log`), another in `btrfs_btree_wait_writeback_range`, another in `wait_for_commit`: three threads inside
   fsync on a btrfs volume under 78% I/O pressure while the machine's load average was 150-170 (another job). It
   recovered by itself; no lock was held across it that other work needs (other calls finished), and the service was
   never in a futex deadlock. The first run had no such stall (slowest 232 ms). This is the known cost of synchronous
   state writes on a saturated disk, as in `soak-2h-after-stall-fix.md`, not a regression; I did not change it.
2. **Re-pair of the stand-in phone failing at "connected" (2 of ~90 in the final run; 1 in the first run; 3 in
   a 25 minute targeted run).** After such a failure the stand-in phone stays unpaired until the next re-pair round
   (every 30 rounds), so the reconnects, rings and pairings in between fail by construction (final run: 11 reconnect, 16
   `ring_not_seen`). Both failures in the final run sit in load spikes of 77 and 126-172. I could not reproduce it
   with the service alone: 200 consecutive re-pairs (rings and bursts off) all succeeded in under 0.3 s, and two further
   mixed 15 minute runs had none. The status dump that would show the link phase at the failure was lost because the
   driver deleted its scratch before the script copied it (fixed now: dumps go to the run directory). **Open**: if it
   shows again at low load, the dump will say what the service thought of the link.
3. **Ring retries (18) and `ring_not_seen`** (16, all in the windows after a failed re-pair, see 2).

## Harness findings (fixed in the driver, no service change)

The first run and the targeted runs showed rings still on the call list 15 s after they ended (1 of 214, then 4 of
160 and 1 of 30 in the targeted runs) and an orphaned card. These disappeared (0 of 102 rings in a run without bursts,
0 of 185 in the final run) once the driver stopped mixing two sources of call-state packets: the unordered burst generator
sent raw ringing/idle pairs that the driver's own ring logic could take for its ring, and when the ring under test was
then not answerable the driver never ended it (the phone stand-in stayed in `ringing`, so the service correctly kept the
row). With call storms ordered inside the ring round, the same service converges every time. The trace of a stuck ring
(service side) was consistent with this: the last state the phone sent was `ringing`.

## Not covered

The real card window, a real notification daemon, Bluetooth hands-free, two phones ringing at once, contact/agenda
replies (the stand-in does not answer them). I did not verify per conversation that all 20-message pages arrived;
`MessagesRefresh` calls were issued (about 100 per data round) and the service log is clean, nothing more. The run is
92 minutes, not hours; the machine was loaded.

## Reproduce

    GB_TMP=$HOME/.cache/gb-soak5-tmp GEEKBRIDGE_SOAK_PORT_BASE=27000 SOAK_UPDATE_MOCK=1 SOAK_UPDATE_PORT=27990 \
      SECS=5400 scripts/soak-features.sh --keep

Checks on the driver change: `cargo fmt --check`, `cargo clippy -p bridge-service --tests -- -D warnings`,
`scripts/sim-nonlinux.sh`: all clean. No service code changed, so the perf budgets are unchanged.
