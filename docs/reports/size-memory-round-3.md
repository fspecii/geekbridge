# Size and memory, round 3 (2026-11-03)

Details and numbers: `docs/performance.md`, section "Size and memory round 3". Summary.

| What | Before | After |
| --- | --- | --- |
| `geekbridge-ctl` | 1 307 376 B | 1 214 392 B (budget back to 1.3 MB) |
| GUI binaries (`geekbridge`, calls, messages, ...) | 8 061 984 B (`geekbridge`) | -90 to -93 kB each |
| `geekbridge-service` | 5 732 536 B | 5 745 336 B (+12.8 kB, budget 5.8 MB unchanged) |
| Service idle RSS | 7952-8220 kB | 3400-3650 kB (budget 10 000 -> 6000 kB; CI 12 500 -> 8000 kB) |
| Service threads / wakeups per s | 6 / 4-5 | 5 / 2-3 |
| GUI idle RSS, Xvfb + llvmpipe (`geekbridge`, calls) | 107.1, 105.5 MB | 66.3, 64.1 MB (real GPU numbers not measured here) |
| Android arm64 library (unstripped build, then stripped) | 2 859 208 B | 2 848 040 B (APK stores it uncompressed) |

Wins applied:

1. `Response::from_json`: tag first, payload deserialized directly; avoids serde's `Content` buffering copy of every
   nested deserializer (ctl and all GUI apps). Wire format unchanged; test compares with the derived parser.
2. `bridge_core::codepages::drop_idle`: `MADV_DONTNEED` on private `r-xp` file mappings only, 2 s after service start
   and after each bus probe; GUI apps call it from `memtrim::trim`. Lowers resident set, not private memory (the
   service had 608 kB anonymous); a busy process regrows to its working set.
3. The KDE Connect bus probe connected through zbus' `blocking` pool thread (wakes 2x/s for the life of the process);
   it now connects its own socket like the tray does.

Measured, not applied: lld `--icf=all` for Android (-100 448 B, needs a device run), service `Request` Content code
(125 kB, strictness risk), TLS 1.2 / RSA / AccessKit (behaviour or accessibility cost).

Method: one worktree at HEAD b168d27f9 vs HEAD plus changes, `scripts/perf-check.sh`, `scripts/ui-mem.sh`, `nm -S
--size-sort`, `/proc/PID/smaps`; all state under `/home/ambsd/.cache/gb-tmp`, no live service or real config touched.
Checks: `cargo fmt --all` clean, `cargo clippy --workspace --all-targets --locked -D warnings` exit 0, `cargo test
--workspace --locked` exit 0, `scripts/perf-check.sh` within budget (local and `GB_CI=1`).
