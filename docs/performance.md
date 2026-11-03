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
| `geekbridge-service` size | 10 722 896 B | 5 502 392 B (2026-11-02, with the tray; 5 172 248 B without) | 5 800 000 B (raised from 5.6 MB after the first real CI run: the GitHub runner built 5 577 488 B (older commit) and 5 657 360 B (release candidate), a dev box 5 663 032 B) |
| `geekbridge-ctl` size | 1 555 944 B | 1 117 360 B (2026-11-02) | 1 350 000 B (raised from 1.3 MB on 2026-11-03: CI runner 1 211 496 B and 1 247 056 B, then 1 310 112 B at d669c2109 after 8e9c976fa added the plain-words "why the phone is not connected" reason codes and sentences to `status`/`ctl`/waybar; +63 kB over the earlier build is consistent with that added text and formatting (no new dependency; not bisected); 40 kB headroom kept) |
| `geekbridge-messages` (UI) size | 11 870 224 B | 5 744 760 B | not budgeted |
| UI idle RSS (`--ui`, warm GL shader cache) | 67-68 MB | 65.1 MB (2026-11-02, was 68.5 MB on the same machine) | 75 MB |
| GUI binaries with AccessKit (2026-11-02, `accesskit` feature, default) | n/a | messages 6.16 -> 8.05 MB (+1.9 MB), idle RSS +1.5-2.5 MB, +3 threads, ~+2 wakeups/s | not budgeted. On the 2026-11-02 machine the UI idle RSS is 105-124 MB with or without it (budget 90 MB exceeded by the GL stack, not AccessKit); see `docs/accessibility.md` |
| UI wakeups (`--ui`) | ~4-6/s | ~2/s (window unfocused, poll back-off) | 12/s |

On CI (`GB_CI=1`, set by the performance job) the load-dependent rows get slack because shared runners differ in CPU count,
neighbours and kernel: RSS 12 500 kB, threads 10, fds 24, wakeups 6/s, CPU 6 ticks, startup 1000 ms. The CI runs measured
8060-8188 kB, 5-6 threads, 20 fds, 1-4 wakeups/s (the 3/s budget was exceeded once), 0 ticks, 12 ms. Binary sizes stay strict on CI. Any `BUDGET_*` variable still overrides.

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

## GUI memory pass (2026-11-02)

Tooling: `scripts/ui-mem.sh [--apps "home calls ..."] [--wayland]` prints Rss/Pss/Anonymous and the file-backed
split from `/proc/PID/smaps_rollup` per app and state (settled, focused, window unmapped, remapped) under a private
Xvfb or headless sway. `cargo build --features memstats` adds a counting global allocator; with
`GEEKBRIDGE_MEMSTATS=1` the app prints live/peak Rust heap every 3 s and at marks (GL ready, fonts installed, frames
1/3/90 around a trim). `heaptrack`, `valgrind` and `weston`/`cage` are not installed; sway is, but headless sway has no
GPU, so it measures Mesa llvmpipe, not a real driver.

**Where the 65 MB goes** (NVIDIA proprietary driver, X11, this machine, `geekbridge-calls`): Rust's own live heap is
only ~6 MB (1.1 MB at GL-ready, 2.5 MB with fonts, ~6 MB after the first frame, peak 10 MB). The rest is the GL driver
and libraries: `libnvidia-glcore` 14 MB, `libnvidia-gpucomp` (shader compiler) 26 MB mapped, `/dev/nvidiactl` 5 MB,
~11 MB driver heap. Per app the numbers are the same (home, calls, messages, calendar, contacts within +-0.5 MB):
every app is the same shell, so no screen holds a big buffer (no photo or calendar caches exist; contact photos are
not loaded). Windows that are hidden or unfocused do not change RSS (no repaint when minimised; nothing to drop).

| Before -> after (RSS kB, warm cache, real display, 7 s settled) | before | after |
| --- | --- | --- |
| geekbridge (home) | 67 700 (anon 21 192) | 62 728 (anon 17 292) |
| geekbridge-calls | 68 396 | 64 608 |
| geekbridge-messages | 68 520 | 65 008 |
| geekbridge-agenda (calendar) | 68 660 | 62 864 |
| geekbridge-contacts | 68 032 | 64 928 |

Wins implemented:

* **Glyph atlas width** (`memtrim::cap_atlas`, via `raw_input_hook` in all three apps): egui sizes its font atlas as
  wide as the GPU allows (16384 px on NVIDIA, an f32 per pixel plus an RGBA copy for upload). Capping at 2048 cut the
  Rust live heap after the first frame 5.9 -> 4.4 MB and the start-up peak 10.3 -> 5.7 MB, and the driver-side texture
  with it. No visual change (the atlas just grows taller; egui rebuilds it if it ever fills).
* **`malloc_trim(0)`** (`memtrim::on_frame`): at frame 3, frame 90 and when the window loses focus / is minimised,
  never per frame. glibc only; a no-op elsewhere. Anonymous memory 21.0 -> 17.4 MB warm. Together with the atlas cap
  that is the 4-6 MB above.

Findings that are not fixable in the app:

* **Cold GL shader cache costs +40-45 MB.** With the NVIDIA disk cache disabled (`__GL_SHADER_DISK_CACHE=0`) or
  evicted, the driver compiles the shader in-process and keeps ~47 MB of live heap and `libnvidia-gpucomp`: RSS 111-123
  MB, anonymous 55-68 MB. `malloc_trim` does not return it (it is live driver state, not freed memory). It happened
  unprompted once or twice during measuring (cache entries written by concurrent processes), and is once per driver
  update in normal use. `scripts/perf-check.sh --ui` therefore does a throw-away warm-up launch first. This is
  also why `docs/accessibility.md` once saw 105-124 MB.
* **Software GL (Xvfb, headless sway) is 105 MB / 177 MB**: llvmpipe maps a 45 MB `libLLVM`, starts ~24 render
  threads and keeps 33-52 MB anonymous. Do not use those numbers for the budget, and do not force
  `LIBGL_ALWAYS_SOFTWARE` (strictly worse, as measured).
* **AccessKit** (2026-11-02, same build flags, `--no-default-features` vs default): RSS +3 MB (66.0 vs 69.0 MB in an
  opt-level 1 build; anonymous +0.4 MB, the rest is zbus/AT-SPI code pages), +3 threads. Kept: accessibility wins.
* **Fonts**: one system regular (+ bold) file is read, 1.25 MB; egui's built-in fallbacks stay for glyph coverage.
  No CJK or colour-emoji font is ever loaded (NotoColorEmoji is 10 MB and skipped); dropping the built-in
  fallbacks would save <1 MB and risks missing-glyph boxes, so it was left.
* **Several GUI processes at once.** Home launches each app as its own process on demand (`launch`), so nothing is
  paid until opened. Concurrent cost measured with five apps open: each adds ~18-21 MB private anonymous plus ~9 MB
  private file pages (Pss ~38 MB each, the other ~36 MB of RSS is shared library pages counted once system-wide), so
  the true marginal cost of an extra window is ~30 MB, not 65. A single multi-window process (egui viewports) would
  save roughly 25 MB per extra app, but it merges crash domains, app ids and the launch contract and needs a
  rewrite of `shell::run`; not done. Recommendation: keep separate processes, and revisit if users routinely keep 4+
  open.
* **wgpu vs glow** (analysis only): wgpu on Vulkan typically maps more driver code than GL (Vulkan ICD + loader,
  and egui-wgpu pulls naga/wgpu-core, +3-4 MB binary) and has a higher idle RSS in published egui comparisons; its win
  would be Windows/macOS via Metal/D3D. No measurement here, so no change.

## Remaining findings

* UI RSS is 65 MB, of which ~47 MB is file-backed (GL driver and Wayland/X libraries mapped by
  `glow`); anonymous memory is ~17 MB (6 MB of it Rust's).
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
* `scripts/soak-e2e.sh`: a multi-hour soak of the release service and release APK on an emulator (clipboard, files, link flaps,
  calls, phone data, restarts, screen-off gaps) with leak charts; results in `docs/reports/soak-report.md`.

The phone side in these tests is a real `bridge-kde` endpoint. `bridge-phone-sim` simulates a
Bluetooth telephony stack, not the KDE Connect transport, and `bridge-android` has its own tests
(`tests/transport*.rs`); no test here drives `bridge-android` against the desktop service.
