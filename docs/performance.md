# Performance, budget and soak testing

Measured 2026-11-01 on x86_64 Linux (24 cores, rustc 1.99.1), loopback only, no phone linked. All
numbers come from `/proc` (`scripts/perf-check.sh`); `perf`, `strace`, `valgrind` and `heaptrack` are
not installed here, so there is no allocation profile. The machine was under heavy load from other
builds (load average 23-32), which affects timings but not RSS, thread or voluntary-wakeup counts.

## Perf budget (enforced by `scripts/perf-check.sh`)

| Metric (service, idle) | Before | After | Budget |
| --- | --- | --- | --- |
| Idle RSS | 6824 kB | 5808 kB | 10000 kB |
| Threads | 25 | 3 | 8 |
| Open fds | 12 | 12 | 20 |
| Wakeups (voluntary context switches, all threads) | ~1/s | ~0.3-2/s | 3/s |
| CPU over a 20 s idle window | 0 ticks | 0 ticks | 2 ticks (20 ms) |
| Startup to "discovery API" line | 8-20 ms warm (614 ms first run, cold cache) | 7-14 ms | 300 ms |
| `geekbridge-service` size | 10 722 896 B | 4 321 752 B | 5 000 000 B |
| `geekbridge-ctl` size | 1 555 944 B | 719 192 B | 800 000 B |
| `geekbridge-messages` (UI) size | 11 870 224 B | 5 744 760 B | not budgeted |
| UI idle RSS (`--ui`) | 67-68 MB | 65.7 MB | 90 MB |
| UI wakeups (`--ui`) | ~4-6/s | ~4/s | 12/s |

Run `scripts/perf-check.sh --build [--ui] [--window N]`; it exits 1 when any row is over. Override a
budget with e.g. `BUDGET_RSS_KIB=9000`. The UI rows need a display.

## What changed

* **Runtime workers** (`bridge-service/src/main.rs`): `#[tokio::main]` started one worker per core
  (25 threads on this host). The service is I/O bound, so it now runs 2 workers. Threads 25 -> 3.
  RSS dropped only a little because idle thread stacks are untouched pages, but the thread count no
  longer scales with the machine.
* **Release profile** (`Cargo.toml`): fat LTO, `codegen-units = 1`, `strip = "symbols"`,
  `opt-level = "s"`, `panic = "unwind"` (kept: tokio contains a panic in one task; abort would let a
  single parser bug take the whole service down). Dev profile untouched.

| Variant (bridge-service + bridge-apps) | service | ctl | messages UI | build time* |
| --- | --- | --- | --- | --- |
| default release (before) | 10 722 896 | 1 555 944 | 11 870 224 | ~2 min cold |
| + `strip = "symbols"` | 7 843 936 | 1 097 152 | 8 578 280 | 3.6 min |
| + fat LTO, 1 codegen unit, opt-level 3 | 5 787 864 | 848 408 | 7 026 432 | 5.2 min |
| + opt-level "s" (**chosen**) | 4 321 368 | 719 192 | 5 744 760 | 3.6-4.9 min |

\*wall clock on a loaded 24-core machine, only a rough guide. Release CI builds are slower than before.
Idle RSS: 6.2 MB (opt 3 + LTO) vs 5.6 MB (opt "s" + LTO).

## Findings in code owned by others (not edited)

* `bridge-service/src/clipboard.rs` `watch()`: polls the OS clipboard every 500 ms via
  `spawn_blocking` for as long as any peer has sync enabled. Fine while idle (it stops when no peer
  syncs), but it is the largest steady wakeup source when sync is on. Prefer a compositor/X event
  source (wl-clipboard `--watch`, XFixes) with polling as fallback.
* `bridge-apps` UI workers poll the service: control worker every 1 s, discovery every 2 s, update
  status every 4 s. They only repaint on change, so idle CPU is ~0, but this is ~4 wakeups/s per open
  app window; lengthen the intervals when the window is unfocused/minimised.
* UI RSS is 65 MB, of which ~46 MB is file-backed (GL/Mesa and Wayland/X libraries mapped by
  `glow`); anonymous memory is ~20 MB.
* `bridge-update` scheduler uses a dedicated OS thread that sleeps; harmless, costs one thread.
* Dead-peer detection relies on TCP keepalive (30 s idle, 10 s interval, 3 retries, `bridge-kde`
  `link.rs`). There is no application-level ping timeout, so a half-open link can stay "connected" for
  up to ~60 s. `lifecycle.rs` asserts the keepalive timer is armed; it cannot black-hole packets
  without privileges.

## Fuzz finding

`Packet::from_line` -> `to_line` -> `from_line` is not bit-exact for floats beyond f64's shortest
round-trip (serde_json without the `float_roundtrip` feature moved `-9.099271347182199e79` by one ulp).
Harmless for the protocol (bodies are treated as opaque JSON), so the property test checks id, type and
payload size, not full equality, for mutated input.

## Tests added

* `crates/bridge-service/tests/lifecycle.rs`: pair / reconnect / disconnect / unpair both ways,
  silent peer during handshake, keepalive armed, hostile packets on an established link (oversize
  type, over-deep JSON, over-long line, negative or unsolicited payload, unknown type), junk and silent
  connections on the inbound listener.
* `crates/bridge-service/tests/leaks.rs` (serialised, process-wide fd and RSS counters): reconnect
  cycles, pair/unpair cycles with fresh identities, junk connection churn. Asserts fds return to the
  baseline (+4) and RSS grows less than 6 MiB after warm-up.
* `crates/bridge-kde/tests/packet_fuzz.rs`, `crates/bridge-service/tests/parse_fuzz.rs`: fixed-seed
  xorshift property tests for packet parsing, line framing with random chunking, discovery datagrams,
  clipboard wire bodies and the control hello. `GEEKBRIDGE_FUZZ_ITERS` scales them.
* `scripts/soak.sh`: the above at large counts in release mode, then the real binary under junk
  TCP/UDP traffic with fd and RSS growth checks.

The phone side in these tests is a real `bridge-kde` endpoint. `bridge-phone-sim` simulates a
Bluetooth telephony stack, not the KDE Connect transport, and `bridge-android` has its own tests
(`tests/transport*.rs`); no test here drives `bridge-android` against the desktop service.
