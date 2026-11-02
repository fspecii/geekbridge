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
| `geekbridge-service` size | 10 722 896 B | 5 502 392 B (2026-11-02, with the tray; 5 172 248 B without) | 5 600 000 B |
| `geekbridge-ctl` size | 1 555 944 B | 1 117 360 B (2026-11-02) | 1 200 000 B |
| `geekbridge-messages` (UI) size | 11 870 224 B | 5 744 760 B | not budgeted |
| UI idle RSS (`--ui`) | 67-68 MB | 65.7 MB | 90 MB |
| GUI binaries with AccessKit (2026-11-02, `accesskit` feature, default) | n/a | messages 6.16 -> 8.05 MB (+1.9 MB), idle RSS +1.5-2.5 MB, +3 threads, ~+2 wakeups/s | not budgeted. On the 2026-11-02 machine the UI idle RSS is 105-124 MB with or without it (budget 90 MB exceeded by the GL stack, not AccessKit); see `docs/accessibility.md` |
| UI wakeups (`--ui`) | ~4-6/s | ~2/s (window unfocused, poll back-off) | 12/s |

Size budgets were raised on 2026-11-02 (service 5.0 -> 5.3 MB, ctl 0.8 -> 1.2 MB) after trimming was
investigated and judged not worthwhile. An unstripped `geekbridge-ctl` (`cargo rustc --release -p bridge-service
--bin geekbridge-ctl -- -C strip=none`, `nm --size-sort`) shows the Hyprland/Omarchy modules (`hypr_config`,
`hotkey_cli`, `waybar`, ...) are only ~46 kB of the 1.1 MB: ~416 kB is serde_json instantiations for the grown IPC
protocol (`bridge_core::ipc::Response`/`Request`/`EventKind` now carry calls, contacts, calendar, SMS), ~147 kB is
std's backtrace/symbolizer (gimli, miniz_oxide), the rest std/core. Feature-gating the Hyprland code could not reach
the old 800 kB, and `geekbridge-ctl` must keep `hotkey` and `status --waybar`. Under fat LTO the service already drops
lib code it does not reference, so its growth is feature growth, not those modules. Real fixes would need a smaller
wire protocol (e.g. a ctl-only slim response type) or nightly `build-std`; neither is justified for ~0.3 MB.

**The tray (2026-11-02).** The tray icon runs inside the service (`bridge-service/src/tray/`), so `perf-check.sh` now
starts the service with `--tray` on a private session bus with a stand-in watcher (`PERF_NO_TRAY=1` measures it
without, for A/B). Measured, idle, watcher present, same machine:

| | without tray | with tray |
| --- | --- | --- |
| Idle RSS | 6.6 MB | 8.0 MB |
| Threads | 3 | 5 (zbus executor + async-io reactor) |
| Open fds | 14 | 18 (bus socket, epoll, eventfds) |
| Wakeups | 1/s | 1/s |
| `geekbridge-service` size | 5 172 248 B | 5 502 392 B (+330 KB: zbus' object server, the two interfaces, the drawing code) |

The size budget went 5.3 -> 5.6 MB for this; everything else stays inside its budget. The tray never polls (the rest of
the service pokes a `Notify`; the task sleeps in between), and it connects the bus socket itself: letting zbus connect
sends the connect to the `blocking` crate's pool, whose first thread then wakes every 500 ms for the life of the
process (measured: +2 wakeups/s, which put the service exactly on its 3/s budget). With no bus or no watcher the cost is
the same connection and nothing visible.

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

## Findings in code owned by others (addressed 2026-11-01)

* **Clipboard watcher** (`bridge-service/src/clipboard.rs`): was a 500 ms `spawn_blocking` tool spawn
  (2 process spawns/s) while any peer had sync enabled. Now `ClipboardBackend::change_events()` starts
  `wl-paste --watch echo` on Wayland, or a `clipnotify` loop on X11 when that tool is installed; the
  watcher then reads only on a change, with a 30 s safety poll. Without an event source it polls every
  1.5 s (was 0.5 s): reads/s 2.0 -> 0.67 (X11 without `clipnotify`, this host) or ~0.03 plus one read per
  real copy (event source). The watcher and its helper process stop immediately when the last device
  disables sync (`Notify` wake), and a dead helper falls back to polling. Idle-lock detection was not
  added: no portable signal exists without a D-Bus dependency; the existing "no peer syncs" stop covers
  the unpaired case. No XFixes crate is linked, hence `clipnotify` rather than an in-process listener.
  Covered by a `ChangeStream` unit test; the service-level perf-check has no phone, so its numbers
  are unaffected (3 threads, 12 fds, 0 CPU ticks, RSS 5.7 MB).
* **UI workers** (`bridge-apps/src/pace.rs`): the service IPC is request/response with no pushed events,
  so polling stays. Control (1 s), clipboard link (1 s), discovery (2 s), update status (4 s, not while
  busy), share (500 ms) and the periodic repaints now stretch x4 when the window is unfocused or
  minimised and x2 when the service is reachable but no phone is paired (x8 both). User commands still
  wake a worker at once. `--ui` wakeups 4/s -> 2/s with the unfocused test window.
* **Dead-link ping** (`bridge-kde`): not added. TCP keepalive (30 s + 3 x 10 s) plus
  `TCP_USER_TIMEOUT` 60 s already bounds a half-open link to ~60 s; an app-level ping would force a
  radio wake-up on the phone every interval for little gain and changes protocol traffic.
* **Android**: `ClipboardAndroid.kt` has no timers or polling (clipboard is read on demand from a
  focused activity); the only `postDelayed` pollers (`LinkSection`, `PairPanel`, `ShareCoordinator`)
  are UI-only and stopped in `onStop`/`onPause`. Nothing to remove.

## Remaining findings

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
