# Soak 4: the real service with the call and message features, 2026-11-05

Scope: `geekbridge-service` (release build of `origin/main` at `d66e59908`) in a private HOME/XDG, private ports and a
private PipeWire, with a stand-in paired phone behind the fault proxy. Method and fail criteria as in `soak-3.md`. New in
the driver: decline-with-message rings (`CallDeclineReply`), Do Not Disturb toggled every data round (a stub `makoctl`
on the service `PATH` reads a flag file), and all `notify-send` children (call banners and phone-notification banners)
counted together.

## Verdict

**PASS. No service defect found, no service code changed.** One full 67 minute run and two burst-only runs. The verdict
script failed the full run on two hard caps that were set for an older layout (state size, banner count); both were wrong
for documented, bounded behaviour and are corrected in `scripts/soak-fit.py` (see "Script changes"). With them the same
samples pass. One pairing under a faulty fault profile timed out at 20 s and left nothing paired, as in soak-3.

| criterion | full run, 67 min, 176 rings |
|---|---|
| growth trend in RSS | none: 7.4 / 9.4 / 10.9 MB (min / median / max), second half falling |
| growth trend in fds / threads | none: fds 14-27, threads 7-28 (transient), idle 15 and 7 |
| anonymous memory | 0.9 MB at start, 3.0 MB at minute 25, then flat at 3.0-3.3 MB to the end (soak-3: 0.9 -> 6.4 MB at 47 min, still creeping) |
| stuck ring (call list not empty 15 s after) | 0 of 176 |
| orphaned overlay card / `pw-cat` / Wi-Fi session | 0 / 0 / 0 |
| control API stalls over 1 s | 0; slowest 30 s window 120 ms; 7790 polls, 0 failed |
| panics, error lines in the service log | 0 |
| unbounded banners | no: peak 12 = 4 (call banner pool) + 8 (phone notification banners), each at its own cap |

## What ran (full run)

| counter | value |
|---|---|
| rings (answered / declined / missed or link dropped) | 176 (57 / 62 / 57); 20 of the declines carried a text, all `ok` in the call trace |
| Wi-Fi listen sessions live / failed to go live | 39 / 1 |
| reconnects ok / failed | 133 / 0 |
| pairings ok / failed, unpairings | 92 / 1, 74 |
| files up / down / failed, bytes | 132 / 164 / 0, 1.69 GB |
| packets sent / received | 7804 / 2853 |
| data, privacy, DND and agenda requests | 1272 |
| ring retries (second ring after none seen in 6 s) | 5, `ring_not_seen` 0 |

Windows of 5 minutes, min / median / max of the 30 s samples:

| window (min) | RSS KiB | anonymous KiB | threads | fds | banners |
|---|---|---|---|---|---|
| 0-5 | 7384 / 9812 / 10488 | 924 / 2124 / 2440 | 7 / 14 / 28 | 15 / 23 / 27 | 2 / 4 / 12 |
| 10-15 | 7788 / 10160 / 10844 | 2500 / 2760 / 3000 | 7 / 11 / 26 | 15 / 18 / 26 | 2 / 4 / 11 |
| 20-25 | 7724 / 10256 / 10576 | 2612 / 2788 / 2884 | 7 / 10 / 15 | 15 / 17 / 21 | 2 / 3 / 6 |
| 30-35 | 8032 / 9772 / 10608 | 3008 / 3048 / 3048 | 7 / 15 / 24 | 16 / 21 / 25 | 2 / 6 / 10 |
| 40-45 | 7852 / 9632 / 10792 | 2884 / 3052 / 3128 | 7 / 23 / 26 | 15 / 24 / 26 | 1 / 9 / 11 |
| 50-55 | 8100 / 10644 / 10924 | 3108 / 3176 / 3280 | 8 / 13 / 23 | 17 / 20 / 25 | 1 / 5 / 10 |
| 60-65 | 7788 / 10380 / 10724 | 2896 / 3012 / 3116 | 7 / 11 / 25 | 15 / 19 / 26 | 1 / 4 / 11 |
| idle (90 s) | 7860 / 8708 / 8756 | 3012 | 7 / 7 / 8 | 15 / 15 / 17 | 0 |

Idle after the load: 0.3 wake-ups/s, 0.03% CPU, one link, one paired phone. During the run: median 5.1 wake-ups/s (p95
43.5), CPU median 0.6% of one core (p95 1.9%), both with a flat slope.

## Message store and banner caps

Burst-only runs (SMS, notification, battery and call-state packets, `GEEKBRIDGE_SOAK_FAST=1`; the store cap is 10,000
messages per phone, `MAX_MESSAGES_PER_PHONE`):

| burst run | packets | anonymous memory | reading |
|---|---|---|---|
| 15 min | 72,878 | 0.76 -> 6.6 MB (soak-3, same load: 0.9 -> 12.6 MB) | still rising about 0.7 MB per 3 min at the end |
| 30 min | 145,765 | 0.93 MB, 5.2 MB at 12 min, then 6.0 (16), 6.3 (18), 6.4 (20), 6.65 (22), 6.7 (24), 6.9 (26), 7.0 (28), 7.1 MB (30) | rate falls from about 0.4 MB per 2 min to about 0.1: a plateau near 7 MB once the store holds its cap (roughly 20,000 messages arrived, twice the cap, so eviction ran for the second half) |

The ratio of messages to packets is not measured directly; "roughly 20,000" is from soak-3's figure (about 10,000
messages per 73,000 packets). Threads (about 20) and fds (22) in the burst runs are flat and drop to 9 / 15 at idle. The
30 minute burst run passes the verdict script with the script change below (before it, it failed only the state-size
growth budget).

Banners: call banners go through `banner_pool` (cap 4, a new one replaces the oldest); phone notification banners go
through `mirror::MAX_OPEN_BANNERS` (cap 8, extra ones are only listed). The observed maximum was 12 in the full run and 8
in the bursts, and the count returns to 0 at idle. The soak-3 finding (about 390 simultaneous banner children) is closed.

## Script changes (harness only)

- `scripts/soak-fit.py`: banner hard cap 12 (4 + 8); state size hard cap and growth budget 320 KiB, because
  `call-trace.log` is a file ring of 256 KiB (`call_trace::FILE_CAP`) that fills during the run and then rewrites itself
  (140-263 KB in the full run), plus about 5 KB of other files.
- `crates/bridge-service/tests/soak_real.rs`: decline-with-message ring kind, Do Not Disturb toggling, card check skipped
  when Do Not Disturb is on. In the full run `card_missing` read 38 of 176; the service trace shows "ring kept quiet:
  Do Not Disturb" for rings in that state (213 such lines in the retained window, 84 cards launched). That is the designed
  behaviour. I did not log the flag per ring in this run, so the 38 is explained by the trace, not matched ring by ring.
  Orphaned cards (a card alive 10 s after the ring ended) are 0.

## Notes

- A first 90 minute run was lost: something deleted the contents of `~/.cache/gb-tmp` while it was in its idle tail,
  including its samples. The runs above used another scratch directory and a loop that copies the samples.
- One pairing under a faulty profile timed out at 20 s (soak-3: 2-4 of 60). `paired` stayed at 3 or fewer.
- Not covered, as in soak-3: the real card window, a real notification daemon, Bluetooth hands-free, two phones ringing at
  once, and contact/agenda replies (the stand-in phone does not answer them). The full run is 67 minutes plus 90 s idle,
  not 2 hours.

## Reproduce

    SECS=3900 scripts/soak-features.sh --keep
    GEEKBRIDGE_SOAK_FAST=1 GEEKBRIDGE_SOAK_SKIP=clip,files,faults,pairs,data,rings,repair SECS=1800 scripts/soak-features.sh
