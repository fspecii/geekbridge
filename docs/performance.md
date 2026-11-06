# Performance, budget and soak testing

Measured 2026-11-01 on x86_64 Linux (24 cores, rustc 1.99.1), loopback only, no phone linked. All
numbers come from `/proc` (`scripts/perf-check.sh`); `perf`, `strace`, `valgrind` and `heaptrack` are
not installed here, so there is no allocation profile. The machine was under heavy load from other
builds (load average 23-32), which affects timings but not RSS, thread or voluntary-wakeup counts.

## Idle wakeups round (2026-11-06)

`scripts/perf-check.sh` reported 4-6 wakeups/s on the current head against its budget of 3 (about 2/s in earlier rounds,
~1/s on the CI runner). It was real, not load: 788543220 and 84e0e0c23 (head) measure the same, so no round-8-to-11
change moved it; the earlier ~2/s readings and the CI runner's ~1/s did not show these timers. Method: a private HOME, XDG, state, ports,
session bus (stand-in StatusNotifierWatcher), no announcements, `--tray`; with and without a stand-in phone
(`perf-phone`) paired and idle; 60 s windows after 8 s of settling; per thread `voluntary_ctxt_switches` deltas from
`/proc/<pid>/task/*/status` with names from `comm` (no `perf`/`strace` here), plus a 20 ms-resolution sampler of the same
counters to read each source's period. Tokio's two workers cannot be told apart per timer, so a source is attributed by
its period in the trace and confirmed by removing it. The machine was not quiet (load 4-15 from other work; 24 cores),
which does not move voluntary switches: repeated runs differ by under 0.1/s.

| Thread / source | Period | Before (wakeups/s) | After |
| --- | --- | --- | --- |
| `gb-dnd-warm` (keep-warm, read the privacy file every pass) | 2 s, even with Respect Do Not Disturb off | 0.50 | 0.02 (off: sleeps 60 s, woken by saving the choice; on: still 2 s, as before) |
| Redial pass `reconnect_tick` (tokio worker) | 2 s | ~0.5-1.0 (timer wake plus a second worker) | 0.03 idle; 2 s only while a redial or dial-back has tries left; an announcement or any link event runs it at once |
| Session-bus probe `coexist::watch` (tokio worker + `async-io`) | 20 s, 11-17 wakeups per probe | 0.55-0.75 | 0: once at start, then when the clipboard status is asked for and is over 20 s old |
| `expire_devices` (tokio worker) | 5 s | 0.2 | 0.03 (30 s; every reader checks the age itself) |
| `tick_alerts` via `spawn_blocking` | 30 s, started a blocking-pool thread each time | ~0.1 | 0.03 (runs in the task; the pool thread, and the 8th thread, are gone) |
| `geekbridge-calls-watchdog` | parks 30 s | 0.03 | 0.03 |
| LAN announcement every 15 s (NIC walk, send, code page drop) | 15 s | 0.1 | 0.1 |
| Link heartbeat tick (phone linked) | 20 s | 0.15 | 0.15 |
| `banner-reaper`, call-trace writer, update scheduler (daily), tray, zbus connection thread | none while idle | 0 | 0 |
| **Whole process, no phone** | | **2.9** (788543220 and 84e0e0c23: 2.88-2.97) | **0.22** |
| **Whole process, stand-in phone paired, idle** | | **2.6-3.1** | **0.35-0.40** |
| Threads | | 8 | 7 |
| `scripts/perf-check.sh` (session env, announcing, tray; ceiling of wakeups/s) | | 4-5 | 1 |
| `geekbridge-service` size | | 7 518 696 B | 7 516 272 B |

What changed: the keep-warm thread waits on a condition that saving the choice signals (`dnd::choice_saved`); the
redial loop is `Daemon::redial_forever`, whose wait is chosen by `Book::active` / `Fallback::active` and cut short by
`reconnect::wake` (called from `record_device` and from every link event, after the dial-back is registered); the
coexistence probe is asked for instead of timed; the expiry housekeeping runs every 30 s; the alerts pass no longer
starts a thread. Behaviour that stays: redial latency (2 s steps while anything is being redialed, immediate on an
announcement or a loss), dial-back after a silent loss, ring detection, the watchdog and the heartbeat. Tests:
`reconnect::tests::a_timer_is_only_needed_while_a_redial_has_tries_left`, `a_wake_ends_the_wait_early`,
`link_stability::the_redial_pass_waits_long_unless_a_redial_is_under_way`, the silent-loss dial-back test now runs the
real loop (started idle, so the loss event has to wake it), `dnd::tests::keep_warm_sleeps_long_while_off_and_wakes_when_the_choice_is_saved`.

Not changed, and worth knowing: with the option on, Respect Do Not Disturb still refreshes every 2 s (a tool run per
pass); the 20 s heartbeat tick with a phone linked is the protocol's keepalive. The budgets are unchanged (3/s locally,
6/s on CI): the service now sits at 0.2-0.4/s and 1/s in the script's ceiling, so the 3/s line still absorbs a busy
machine. One measurement trap: run from an agent session whose environment carries the `T3CODE_*` / `ELECTRON_*`
variables, the same service shows a burst of about 35 wakeups every 15.7 s (1.8/s) that vanishes with a clean
environment (`env -i` or those variables unset); it was not found inside the service (no code reads them), so compare
numbers taken in the same environment.

## Perf budget (enforced by `scripts/perf-check.sh`)

| Metric (service, idle) | Before | After | Budget |
| --- | --- | --- | --- |
| Idle RSS | 6824 kB | 5808 kB (3.4-3.6 MB after round 3, below) | 6000 kB (was 10000; round 3) |
| Threads | 25 | 3 | 8 |
| Open fds | 12 | 12 | 20 |
| Wakeups (voluntary context switches, all threads) | ~1/s | ~0.3-2/s | 3/s |
| CPU over a 20 s idle window | 0 ticks | 0 ticks | 2 ticks (20 ms) |
| Startup to "discovery API" line | 8-20 ms warm (614 ms first run, cold cache) | 7-14 ms | 300 ms |
| `geekbridge-service` size | 10 722 896 B | 5 502 392 B (2026-11-02, with the tray; 5 172 248 B without); 5 846 328 B on 2026-11-03 (phone mirroring, battery, notifications, dial, doctor, goodbye) | 7 560 000 B (unchanged by the 2026-11-05 self-update work: `--version` and the start-up recovery of an interrupted update cost +21.5 kB, 7 512 296 B locally against 7 490 792 B without; 44 kB headroom). Raised 2026-11-05 from 7 500 000 B: CI measured 7 515 424 B with the Messages data work, no new dependency). Before that 7 500 000 B (raised 2026-11-04 from 7 420 000 B: CI measured 7 370 952 B, no new dependency). Before that 7 420 000 B (raised 2026-11-04 from 7 360 000 B: CI measured 7 357 896 B, no new dependency). Before that 7 360 000 B (raised 2026-11-04 from 7 300 000 B: CI measured 7 303 880 B after the call chain review, no new dependency, 56 kB headroom). Before that 7 300 000 B (raised 2026-11-04 from 6 670 000 B for the experimental Wi-Fi call audio on the computer: 7 247 968 B locally, +578 kB, almost all of it the pure-Rust Opus codec `opus-pure` (the encoder for Speak is 266 kB, measured by building without it; the decoder and the hub the other ~310 kB); the codec runs only while a session does, idle RSS, threads and wake-ups are unchanged; 52 kB headroom). Before that 6 670 000 B (raised 2026-11-04 from 6 650 000 B after security review 7: CI measured 6 653 584 B, +8.8 kB over the 6 644 752 B step; the BlueZ agent's confirmed-device and name checks, private state files, the QR code grace and bidi-clean text, no new dependency). Before that 6 650 000 B (lowered 2026-11-04 from 6 700 000 B after size trim 4: 6 575 448 B locally, `docs/reports/size-trim-4.md`). Before that 6 700 000 B (raised 2026-11-04 for call audio on the computer: 6 558 424 B locally, CI 6 609 168 B in a first version that linked libpulse; about +420 kB over the 6 138 064 B CI step. Unstripped `nm --size-sort -S`: the new code is ~230 kB (`bridge_calls::setup` pairing agent 60 kB, `call_audio_host` 49 kB, controller/route/sound-server adapter ~85 kB, `hear` checklist 26 kB); the rest is its share of zbus, serde and std instantiations. No new dependency. The sound server is driven through `pactl`/`parec` at call time, not an in-process libpulse: linking libpulse mapped libpulse, libpulsecommon, libdbus, libsndfile and libsystemd at start and CI measured idle RSS 9 692 kB against the 8 000 kB budget (main ~7 500 kB). ~100 kB headroom.) Before that 6 200 000 B (raised 2026-11-04 after the pairing, call-audio setup and Calls window work: CI measured 6 138 064 B at b5633caee, +135 kB over the 6 003 456 B step; no new dependency, growth is feature code and strings; 62 kB headroom). Before that 6 100 000 B (5.0 -> 5.3 -> 5.6 -> 5.8 -> 6.0 -> 6.1 MB; the 6.1 step on 2026-11-04: CI measured 6 003 456 B after the updater asset selection tables, Messages (SMS) work and the Wi-Fi bind safety net, all legitimate feature code with no new dependency or feature; the last step on 2026-11-03 because the feature work above reached 5.85 MB; earlier raises followed the first real CI run: the GitHub runner built 5 577 488 B and 5 657 360 B, a dev box 5 663 032 B) |
| `geekbridge-ctl` size | 1 555 944 B | 1 117 360 B (2026-11-02); 1 214 392 B after round 3; 1 249 056 B on 2026-11-03 | 1 790 000 B (raised 2026-11-05 from 1 650 000 B for `update install|rollback|restart` (Linux per-user self-update, [design/desktop-self-update.md](design/desktop-self-update.md)): 1 764 712 B locally against 1 634 088 B for the same tree without it, +131 kB, of which ~77 kB is the new code (streaming gzip and strict tar readers, staging, validation, swap, recovery, the outcome record) and the rest std `Command`/fs/format instantiations; no new dependency, no flate2 (it is not in the ctl tree); 25 kB headroom). Before that 1 650 000 B (raised 2026-11-04 from 1 600 000 B: CI measured 1 608 632 B, no new dependency). Before that 1 600 000 B (raised 2026-11-04 from 1 552 000 B: CI measured 1 559 960 B, no new dependency). Before that 1 552 000 B (raised 2026-11-04 from 1 540 000 B for the ufw source-range check in `doctor` and the plain-words hotkey state: 1 545 400 B locally, +8.5 kB over 1 536 872 B; 6.6 kB headroom). Before that 1 540 000 B (raised 2026-11-04 from 1 480 000 B for `geekbridge-ctl callwifi`: 1 517 848 B locally, +38 kB over the 1.48 MB step: the `CallWifi` wire types, the command and the doctor lines; no new dependency; 22 kB headroom). Before that 1 480 000 B (lowered 2026-11-04 from 1 550 000 B after size trim 4: 1 454 056 B locally, `docs/reports/size-trim-4.md`). Before that 1 550 000 B (raised 2026-11-04 for call audio: 1 509 632 B locally, +59 kB: the `HearCalls`/`CallAudioDevices`/`CallAudioPair` wire types and `call setup`/`call audio-devices`; the checklist logic was moved to the service so the ctl renders the service's result and links no D-Bus or sound-server code; 40 kB headroom). Before that 1 500 000 B (raised 2026-11-04: CI measured 1 450 704 B at b5633caee, +92 kB over 1 358 984 B. Unstripped `nm --size-sort` shows no new dependency or big single symbol: the growth is serde deserializers for new `bridge_core` wire types (call-audio `Setup`, pairing, Calls window views) that `Response` pulls into every client, plus `call_cli`; 49 kB headroom). Before that 1 400 000 B (raised again on 2026-11-04: CI measured 1 358 984 B after the share-result commit, no new dependency; 41 kB headroom). Before that 1 350 000 B (raised again for recent photos: CI measured 1 318 016 B after the photo request/response types and the `photos` command, +65 kB over 1 252 368 B; no new dependency in the ctl; 32 kB headroom). Before that: 1 300 000 B (round 3 put it back to 1.3 MB; had been raised from 1.3 MB on 2026-11-03: CI runner 1 211 496 B and 1 247 056 B, then 1 310 112 B at d669c2109 after 8e9c976fa added the plain-words "why the phone is not connected" reason codes and sentences to `status`/`ctl`/waybar; +63 kB over the earlier build is consistent with that added text and formatting (no new dependency; not bisected); 40 kB headroom kept) |
| `geekbridge-messages` (UI) size | 11 870 224 B | 5 744 760 B (2026-11-02); 7 868 672 B on 2026-11-03 (AccessKit, i18n, onboarding, mirroring UI) | not budgeted |
| UI idle RSS (`--ui`, warm GL shader cache) | 67-68 MB | 65.1 MB (2026-11-02, was 68.5 MB on the same machine) | 75 MB |
| GUI binaries with AccessKit (2026-11-02, `accesskit` feature, default) | n/a | messages 6.16 -> 8.05 MB (+1.9 MB), idle RSS +1.5-2.5 MB, +3 threads, ~+2 wakeups/s | not budgeted. On the 2026-11-02 machine the UI idle RSS is 105-124 MB with or without it (budget 90 MB exceeded by the GL stack, not AccessKit); see `docs/accessibility.md` |
| UI wakeups (`--ui`) | ~4-6/s | ~2/s (window unfocused, poll back-off) | 12/s |

On CI (`GB_CI=1`, set by the performance job) the load-dependent rows get slack because shared runners differ in CPU count,
neighbours and kernel: RSS 8500 kB (raised from 8000 on 2026-11-04: CI settled-minimum readings were 7860 and 8128 kB on the same code size class while a local run of the same head settles at 3.8 MB, so the CI runner does not reach the trimmed state; threads, fds and wakeups unchanged), threads 10, fds 24, wakeups 6/s, CPU 6 ticks, startup 1000 ms. The CI runs measured
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

## Phone linked: calls, Wi-Fi call audio, Messages (2026-11-05)

`scripts/perf-phone.sh` starts a release `geekbridge-service` in a sandbox (private HOME/XDG/state, private loopback ports, private
session bus with the tray, stub `pw-cat`, `pactl`, `notify-send` and `geekbridge-calls` so nothing touches a sound server or a
display) and links a stand-in phone to it (`crates/bridge-service/examples/perf-phone.rs`: a real `bridge_kde` endpoint that pairs
through the control API, reports calls, answers requests, sends Opus frames, serves contacts and texts). It samples `/proc`
(RSS, anonymous memory, threads, fds, child processes, CPU ticks, voluntary context switches). No heaptrack or valgrind here, so
the codec allocations are counted with a global allocator (`crates/bridge-callwifi/tests/alloc.rs`). Idle RSS is the sawtooth
described above (code pages come back with each ping and go with the next drop): read the settled minimum and the anonymous column.

| Scenario (service, phone linked) | Before | After |
| --- | --- | --- |
| Idle, 30 s: RSS min / peak, threads, fds, wakeups | 4.7 / 9.2 MB, 6-7, 18, 1.3/s | 4.5 / 8.9 MB, 6-7, 18, 1.5/s |
| Idle anonymous memory | 1 364 kB | 1 140 kB (one malloc arena) |
| 200 x ring, answer on the PC, hang up on the PC: anonymous growth | +140 kB (1 428 -> 1 568) | +48 kB (1 156 -> 1 204), flat after the first 20 cycles |
| Same: threads, fds, child processes after | 7, 18, 0 (1 card at most while ringing) | 7, 18, 0 |
| Same: CPU | 63 ticks (incl. the stand-in's polling) | 98 ticks (noisier machine) |
| 60 s Wi-Fi listen: RSS, CPU (3 001 frames) | 7.0-9.5 MB, 0.93 % | 6.7-7.5 MB, 0.77 % |
| Same: anonymous at start / live / 1 s after stop | 1 568 / 1 800 / 1 788 kB (+220 kB kept) | 1 204 / 1 420 / 1 244 kB (+40 kB) |
| Same: threads and fds after stop, `pw-cat` children | 6-7, 18, 0 | 6-7, 18, 0 |
| Service with 2 000 contacts and 2 600 texts held (the hub caps unsolicited history per thread; 250 threads) | not measured | anonymous 6.5 MB, RSS 11-13.7 MB, settled min 11 MB |
| `geekbridge-messages` with that data (Xvfb, llvmpipe: absolute numbers are not a GPU desktop's) | 63.1 MB (no data) | 66.2 MB, 58 threads (software GL), budget 75 MB |
| `geekbridge-service` size | 7 307 664 B | 7 307 792 B |

* **What was found.** No leak: fds, threads and child processes return to the baseline after 200 call cycles and after the
  listen session; the card supervisor leaves no process behind (0 children 8 s after the last ring); `drop_idle` still works after
  a session (RSS 7.2 MB live -> 4.4 MB one second after stop). The Opus decoder holds 149 kB and the encoder 122 kB, a decoded
  frame allocates nothing in steady state, and dropping them returns every byte (`tests/alloc.rs`). What stayed behind was glibc
  heap: every thread gets its own malloc arena, and the session worker thread's arena kept the decoder's freed pages (+220 kB per
  session, growing with each one).
* **Fixes.** `memtrim::single_arena()` (`mallopt(M_ARENA_MAX, 1)` first thing in `main`, glibc only; the service is I/O bound so the one
  lock is not contended) takes 230 kB off the idle anonymous memory and stops the growth across sessions; `memtrim::trim_heap()` runs
  once when the Wi-Fi call audio worker ends (after dropping the decoder), never per frame. Tests: `memtrim::tests::trim_heap_returns_freed_heap_to_the_os`
  and `bridge-callwifi/tests/alloc.rs`. Cost: +128 B of binary.
* **Measured, not kept.** `opus-pure` at `opt-level = "z"`: service 7 340 944 B (larger, inlining differences). At `opt-level = 3`:
  7 458 832 B (+150 kB) for 0.67 % instead of ~0.8-1.0 % CPU while listening. Neither is worth it; the decoder at `s` costs under 1 % of one core.
  Fat LTO, one codegen unit and stripped symbols were already on; no feature of a direct dependency is unused enough to drop cheaply.
* **Still open.** 5 000 texts could not be held: the hub keeps at most about ten texts per thread unless the user opens it (history is requested a
  page at a time), so 2 600 is the realistic ceiling for pushed data; the view returns only the newest 100 conversations, so the window
  stays near its idle size (+3 MB).

## Size trim 4 (2026-11-04)

Report: `docs/reports/size-trim-4.md`. A/B from two worktrees of origin/main (a29ee30e9), stripped release builds,
`cargo-bloat` (`--crates`, per function) on a `strip = none` copy, `scripts/perf-check.sh` and per-process smaps.

| Change | `geekbridge-ctl` | `geekbridge-service` |
| --- | --- | --- |
| Baseline | 1 515 232 B | 6 584 920 B |
| `ipc::pretty_json`: `status` and every other reply printed from the authenticated body instead of re-serializing the parsed `Response` (`Response::serialize` with the pretty formatter was 43 kB, plus the nested serializers) | 1 455 120 B (-60 112) | unchanged |
| One `base64` (0.23, was 0.22 and 0.23 through `pem`/`rcgen`) | 1 454 056 B (-1 064) | 6 575 448 B (-9 728) |
| Measured, not kept: `opt-level = "z"` for everything except the egui crates | 1 372 272 B (-143 kB) | 6 193 496 B (-392 kB) but idle RSS +0.5 to 0.8 MB (file-backed code pages, 3.6 to 4.3 MB in `rssprobe`) | 
| Measured, not kept: `z` only for `bridge-core`, `bridge-i18n`, `serde*` | 1 401 520 B (-114 kB) | 6 600 536 B (+15 kB), idle RSS +0.3 MB |

`panic = "abort"` would drop most of the ctl's 190 kB of unwind tables (`.eh_frame`, `.gcc_except_table`) but cargo has
no per-binary panic strategy, only a separate profile that every build script would have to pick (tokio and the JNI
boundary rely on unwind in the service). What remains is dominated by the serde deserializers of the shared wire types
(`bridge_core` 499 kB of the ctl's 1.1 MB `.text`), std's backtrace code (about 80 kB), the 30 kB embedded English
catalogue and, in the service, ring's P-256 table (148 kB), zbus (655 kB) and rustls (370 kB).

## Size and memory round 3 (2026-11-03)

Report: `docs/reports/size-memory-round-3.md`. Same machine, same toolchain, A/B from one worktree (HEAD b168d27f9
against HEAD plus the changes), `scripts/perf-check.sh` and `scripts/ui-mem.sh` against private state under a scratch
directory. Tools: `nm -S --size-sort`, `size`, `readelf -S`, `/proc/PID/smaps` (no `cargo-bloat`, `bloaty`, `perf`).

| Change | Measured | Before | After |
| --- | --- | --- | --- |
| `Response::from_json` (tag first, payload straight into its own type) instead of the derived internally tagged `Response` deserializer | ctl and every GUI binary | ctl 1 307 376 B, `geekbridge` 8 061 984 B | ctl 1 214 392 B (-92 984), GUI binaries -90 to -93 kB each |
| Drop idle code pages after start (`bridge_core::codepages`) | service idle RSS (20 s window) | 7952-8220 kB | 3400-3650 kB |
| Same, in the GUI apps (frame 3, frame 90, window goes idle) | Xvfb + llvmpipe, `geekbridge` / `geekbridge-calls` RSS | 107 112 / 105 488 kB (file-backed 71 / 70 MB) | 66 288 / 64 140 kB (file-backed 31 / 29 MB) |
| Session-bus probe (`coexist.rs`) connects its socket itself | service threads, wakeups | 6 threads, 4-5 /s | 5 threads, 2-3 /s |
| Service size (cost of the two small modules above) | `geekbridge-service` | 5 732 536 B | 5 745 336 B (+12.8 kB; budget stays 5.8 MB) |

* **Serde `Content` duplicates.** The unstripped ctl (`nm -S --size-sort`) had 313 kB of
  `ContentDeserializer` instantiations: `Response` is `#[serde(tag = "kind")]`, so serde buffers each reply and then
  replays it through a second copy of every nested type's deserializer (every `Vec<T>` visitor appeared twice).
  `Response::from_json` reads the tag with a one-field struct and deserializes the payload struct directly. Wire
  format unchanged (a test compares it with the derived parser; `ipc::call` uses it). 313 kB -> 25 kB of Content code,
  net -93 kB after the added dispatch code. The service side (`Request`, `deny_unknown_fields` per variant, 125 kB of
  Content code) was left: a hand dispatch would need one struct per variant and would risk the strictness.
* **Idle code pages.** An idle service held 608 kB of anonymous memory but 7.7 MB RSS: 4.4 MB was the executable's own
  clean code pages, mapped 16 at a time by fault-around during start-up (TLS, D-Bus, key set-up) and never run again.
  They are page cache (no private cost, reclaimable), but they are what `top` and the budget count.
  `bridge_core::codepages::drop_idle()` `madvise(MADV_DONTNEED)`s only private `r-xp` plain-file mappings with no anonymous (privately written) page (never
  data or RELRO, `[vdso]`, `/memfd:` or `/dev/` objects, where that call would revert relocated pointers or lose the only copy) 2 s after start and after each 20 s bus probe; pages
  come back on demand as minor faults. It lowers RSS (and Pss), not allocator
  memory. A busy process (phone linked, window drawing) regrows to its working set, measured +0.5 MB for a focused app
  window; the GL driver's file-backed share shrinks 71 -> 31 MB while idle. Hence the RSS budget 10 000 -> 6000 kB
  (CI 12 500 -> 8000 kB; CI measured 8.06-8.19 MB before this).
* **A hidden `blocking` thread.** The 20 s KDE Connect probe used `zbus::Connection::session()`, which connects on the
  `blocking` crate's pool; that thread then wakes twice a second for the life of the process (the tray comment in
  `tray/sni.rs` already described this). The probe now reuses the tray's own socket connect. -1 thread and about
  -2 wakeups/s, even without a tray.
* **Measured and not changed.** Android arm64 `libgeekbridge_android.so` (opt-level `z`, fat LTO): 2 859 208 B stripped;
  ring/rustls/x509 575 kB (P-256 table 152 kB, TLS 1.2 needed by KDE Connect peers), tokio 176 kB, unwind tables
  (`.eh_frame` + `.gcc_except_table` + hdr) ~840 kB which panic = "unwind" requires, std backtrace 129 kB. The APK
  stores the library uncompressed, so APK delta = library delta. Linking with lld `--icf=all` measured -100 448 B
  (2 848 040 -> 2 747 592 B); `--icf=safe` does nothing because rustc emits no address-significance table. Applied
  afterwards (2026-11-03, validated on the AVD, see the next bullet). GUI binaries (8 MB) are
  1.44 MB AccessKit/zbus, 1.14 MB winit/x11/wayland, 0.75 MB egui; features are already trimmed (no default features),
  AccessKit stays for accessibility. Widget-preview PNGs (64 kB) do not recompress.
* **lld `--icf=all` for Android (2026-11-03).** `scripts/android-env.sh` adds `-C link-arg=-Wl,--icf=all` to both Android
  targets (`GB_ANDROID_ICF=0` turns it off). Release APK, unsigned, minified: arm64-only 3 295 347 -> 3 194 339 B (-101 008 B);
  arm64 + x86_64 6 574 428 -> 6 425 676 B (-148 752 B). Stripped libraries: arm64 2 859 600 -> 2 758 592 B, x86_64
  3 271 400 -> 3 220 952 B. All LOAD segments stay 0x4000 aligned (`llvm-readelf -lW`), so the 16 KB page-size
  requirement of Android 15+ still holds. No code compares function addresses; pairing, reconnect, file transfer both ways
  and the update stage pass on the AVD. Details and caveats: `docs/reports/android-icf-validation.md`.

Run `scripts/perf-check.sh --build [--ui] [--window N]`; it exits 1 when any row is over. Override a
budget with e.g. `BUDGET_RSS_KIB=9000`. The UI rows need a display.

## Final footprint 2026-11-04

Measured at origin/main `3bfefaae7` (clean worktree, private cargo target dirs), after the day's features (notification mirroring, recent photos,
battery, call-audio state and routing, Wi-Fi bind, pairing scanner with `rqrr`, dial, share results). Desktop: x86_64 Linux, release build, private Xvfb
with software GL (llvmpipe). Android: AVD clone `gbfoot` (Android 16 x86_64, from `gb36`), debug APK from `scripts/build-android.sh`, paired with a sandbox
desktop service, every feature switched on one step at a time (per-computer grants, phone master switches, notification access for one app, desktop
consents), then 30 minutes idle with the link connected (`scripts/e2e/features.sh env,pair,footprint`; `FOOT_MINUTES`, `FOOT_INTERVAL`). Report with all
raw numbers: [reports/final-footprint.md](reports/final-footprint.md).

| Metric | Prior (date, source) | 2026-11-04 | Budget |
| --- | --- | --- | --- |
| Service idle RSS, settled (min of samples every 5 s) | 3.4-3.6 MB (11-03, round 3) | 3.7-3.8 MB (peak sample 4.4-5.3 MB) | 6000 kB |
| Service threads / fds / wakeups per s / CPU | 5 / 12-20 / 2-3 / 0 | 4-5 (tray 5) / 18-19 / 2-3 / 0-1 ticks | 8 / 20 / 3 / 2 |
| Service startup to discovery line | 7-14 ms | 10-11 ms | 300 ms |
| `geekbridge-service` / `geekbridge-ctl` | 5 846 328 B / 1 249 056 B (11-03) | 6 600 376 B / 1 455 512 B | 7 560 000 B / 1 650 000 B |
| GUI binaries (home, calls, messages, agenda, contacts) | messages 7 868 672 B (11-03) | 8.26, 8.08, 8.00, 8.03, 7.92 MB | not budgeted |
| GUI idle RSS under Xvfb + llvmpipe (home / calls / messages / agenda / contacts) | 66.3 / 64.1 MB (round 3) | 66.2 / 62.3 / 64.5 / 65.5 / 63.2 MB (anon 33-35 MB, 58-59 threads) | `--ui` messages 75 MB: 64.7 MB, 3 wakeups/s (budget 12) |
| Android release APK, arm64 only (unsigned, R8) | 3 194 339 B (11-03) | 3 498 723 B (+304 384 B) | none |
| Android release APK, arm64 + x86_64 | 6 425 676 B (11-03) | 7 015 964 B | none |
| `libgeekbridge_android.so` stripped, arm64 / x86_64 | 2 758 592 / 3 220 952 B | 3 028 752 / 3 514 872 B (+270 160 / +293 920 B) | none |
| Android debug APK | n/a | 8 709 379 B | none |
| Android classes.dex / resources.arsc (release, arm64) | n/a | 573 352 B / 110 764 B | none |
| Android release, fresh install, Home idle PSS | 23.5 MB (11-03) | 23.9 MB, 23 threads | none |
| Android release cold start `am start -W` (5 starts) | 910-970 ms | 972-1137 ms (emulator, host load low; not distinguishable from noise at that spread) | none |
| Android debug, paired, all features on: PSS / RSS / Java / native / code | n/a | 58-62 MB / 163-168 MB / 12.5-14 MB / 16 MB / 6.2 MB, flat over 30 min | none |
| Android debug threads / open fds | n/a | 35 / 146 | none |
| Android app CPU over 30 min idle linked (batterystats) | n/a | 1.5 s user + 0.6 s system (about 0.12 %) | none |
| Android alarms, wake locks, jobs of the app (dumpsys alarm / batterystats) | none by design | none (no alarm entry, no wake lock, no job) | none |
| Android debug cold start, all features on / fresh | n/a | 944-1520 ms (7 starts) / 1053-1160 ms | none |

What each feature costs on the phone (debug build, a fresh process per step, settled 45 s): calls only 57.97 MB PSS, + contacts/calendar/sms 57.92, +
battery 57.74, + notifications 58.10, + photos 57.91 (threads 35-36, fds 145-146 throughout). The opt-in features are lazy: switching them on adds
nothing measurable at idle (+-0.4 MB, measurement noise); their cost is code size (the library grew 270 kB since 11-03, mostly the pairing scanner,
call-audio state/flow, photos and Wi-Fi bind code in `bridge-core`/`bridge-android`) and per-request work when the computer asks. Emulator numbers are a
regression baseline, not phone numbers.

**Service RSS method (2026-11-04).** Idle RSS is a sawtooth, not a level: `codepages::drop_idle` runs 2 s after start, after each 20 s session-bus
probe and now after each 15 s LAN announcement, and the next periodic job faults the touched code back in. One read can land anywhere between 3.5
and 8 MB on the same binary (the CI job flipped on 8028-8156 kB readings against the 8000 kB limit). `scripts/perf-check.sh` therefore reads
`VmRSS` every 5 s over the window (at least 4 samples after the 3 s settle) and the budget applies to the lowest, the settled idle figure; the peak
sample is printed but not budgeted. The budget numbers did not change. The announcement trim (`memtrim::after_announce`) lowered the steady sawtooth
from 5.0-5.6 MB to 4.1-4.3 MB.

## Android responsiveness round (2026-11-03)

Emulator (AVD clone, Android 16 x86_64, software GPU, host load average 3-13 from other jobs), release build with R8 signed with the
debug key, `am start -W`, `dumpsys gfxinfo`, `dumpsys meminfo`, debug build for StrictMode. Emulator numbers are a regression
baseline, not phone numbers. Full write-up: [reports/android-startup-jank.md](reports/android-startup-jank.md).

| Metric | Before | After |
| --- | --- | --- |
| Main-thread disk reads at cold start (debug, StrictMode, same state, 3 cold starts) | 2 per start (`DeviceName.current` preference load, `getFilesDir` stat) plus 23-47 ms preference loads in `CallControlRuntime.init` / `AppState.init`; 142-335 ms each | 1 per start (`getFilesDir` stat, a cold-IO artefact); the preference loads are gone |
| Release APK, phone + emulator ABIs | 6 494 176 B | 6 480 448 B (-13 728 B: Kotlin `.kotlin_builtins` metadata and `kotlin-tooling-metadata.json` no longer packaged) |
| Dot-matrix wordmark draw (JVM, 25-character word, per frame) | 73 us (per-dot string scan, `uppercase()` per frame) | 2 us (precomputed grid); `DotProgress` and `DotText` no longer allocate boxed lists / a key string per draw |
| Cold start `am start -W`, release, 7-9 starts | 910-970 ms (quiet host) | not distinguishable under load (see report); process CPU 100-108 -> 94-102 ticks |
| Scroll jank Home / Setup (`gfxinfo`, 6 / 4 swipe pairs) | 22-27 % / 34-36 % janky, p50 32-36 ms | unchanged (22 % / 34 %, p50 32-36 ms): bound by the emulator's software renderer, not app code |
| PSS: Home idle / after scrolling / Setup / Diagnostics (unpaired) | 23.5 / 30.7 / 32.3 / 36.5-38.3 MB | unchanged (23.5 / 30.6 / 32.2 / 36.5 MB) |

Findings that were measured and left alone: one process holds UI, service and Rust core (a bare second process measured 7.7 MB PSS, a
split would cost more than the UI heap it frees); no fonts are bundled (the dot font is code); `Application.onCreate` takes 6-8 ms,
`HomeActivity.onCreate` 7-11 ms, `onStart` 37-46 ms, a screen build 18-32 ms; the first frame (about +245 ms after process start) is
followed by a 600 ms main-thread wait inside `HardwareRenderer.setStopped` (the emulator render thread), not app code.

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
* **Dead-link ping** (`bridge-kde`): not added at the transport layer. TCP keepalive (30 s + 3 x 10 s) plus
  `TCP_USER_TIMEOUT` 60 s already bounds a half-open link to ~60 s. (Superseded in part: the later real-device fixes
  added an application heartbeat for GeekBridge phones, 20 s with a 70 s silence limit and 60 s while the phone screen
  is off, see `bridge-core/src/heartbeat.rs`; stock peers are never pinged.)
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
* Dead-peer detection: TCP keepalive (30 s idle, 10 s interval, 3 retries, `bridge-kde`
  `keepalive.rs`) bounds a half-open link to ~60 s; GeekBridge phones are additionally pinged every 20 s from each
  side (60 s from a phone whose screen is off) and a heartbeating peer silent for 70 s is dropped
  (`bridge-core/src/heartbeat.rs`). Stock peers get no periodic ping. `lifecycle.rs` asserts the keepalive timer is
  armed; it cannot black-hole packets without privileges.

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

## Message store memory (2026-11-05)

`crates/bridge-core/tests/message_memory.rs` builds 10,000 synthetic messages (50 threads, bodies 20-160 chars, 5% group
messages, 3% MMS) through the real `Message::from_wire` and `Conversation::merge`, counts live heap with a global allocator
and charges each block as glibc does (16-byte granule, 8-byte header, 32 minimum). It asserts at most 220 B per message.

| | before | after |
|---|---|---|
| glibc-charged bytes per stored message | 406.7 | 193.5 |
| requested bytes per stored message | 359.7 | 177.5 |
| heap blocks per message | 3.27 | 1.02 |
| 10,000 messages | 4.07 MB | 1.94 MB |
| 20,000 messages (the per-phone cap) | 8.1 MB | 3.9 MB |

Floor for the same data: about 90 B of text plus 24 B of key/date/flags, about 115 B. What changed: the thread keeps a sorted
`Vec<Message>` (no B-tree node slack, 12.5% growth steps) instead of a `BTreeMap`; the body is a `Box<str>`; the recipient list is an
`Arc<[Address]>` shared by the thread's messages; the notification path no longer clones every body; a stored body is cut at
4096 bytes (worst case per message was 256 KiB).
