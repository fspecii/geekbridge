# Size trim 4 (2026-11-04)

Details: `docs/performance.md`, section "Size trim 4". Targets were ctl near 1.3 MB and service near 6.0 MB. Not reached:
the growth since round 3 is feature code (wire types, call-audio, photos, notifications, pairing) and the only
build-wide lever that reaches the targets costs resident memory.

| What | Before (origin/main a29ee30e9) | After | Budget |
| --- | --- | --- | --- |
| `geekbridge-ctl` | 1 515 232 B | 1 454 056 B (-61 176) | 1.55 MB -> 1.48 MB |
| `geekbridge-service` | 6 584 920 B | 6 575 448 B (-9 472) | 6.7 MB -> 6.65 MB |
| Service idle RSS (`perf-check`, tray on) | 4.1-4.2 MB | unchanged (no codegen change kept) | 6000 kB |

Kept (no behaviour change):

1. `ipc::call_with_body` + `ipc::pretty_json`: `ctl status` and the generic reply printer print the authenticated
   reply body re-indented in `serde_json::to_string_pretty` style instead of serializing the parsed `Response` again.
   Output is identical for service replies (key order is the service's own; a test compares with serde_json on nested,
   empty, escaped input). The pretty `Response::serialize` and its nested serializers were 43 kB+ of the ctl: -60 112 B.
2. One `base64`: workspace crates moved 0.22 -> 0.23, the version `pem` (via `rcgen`) already pulled. -1 064 B ctl,
   -9 728 B service; `THIRD_PARTY_NOTICES.md` regenerated.

Measured and not kept:

| Experiment | ctl | service | Why not |
| --- | --- | --- | --- |
| `opt-level = "z"` everywhere, egui crates stay `s` | 1 372 272 B | 6 193 496 B | service idle RSS +0.5 to 0.8 MB (file-backed code pages re-touched after `drop_idle`; 5 alternating runs, 3.6 -> 4.3 MB). A resident service pays RSS, not disk. |
| `z` only for `bridge-core`, `bridge-i18n`, `serde*`, `sha2`, `base64` | 1 401 520 B | 6 600 536 B | service grows 15 kB and RSS +0.3 MB. |
| `z` for a package list incl. zbus/tokio/rustls/ring (no profile-wide change) | 1 750 312 B | 8 509 040 B | mixed levels under fat LTO were far larger. |
| `panic = "abort"` for the ctl | not built | n/a | cargo has one panic strategy per profile; a ctl-only profile would need every build script and the release packaging changed, and tokio/JNI rely on unwind. Would save ~190 kB of unwind tables. |
| RELR relocations, `-no-pie` (35 kB `.rela.dyn`) | n/a | n/a | needs glibc 2.36+ / drops ASLR. |
| zbus `blocking-api` off | n/a | n/a | `bridge-calls` uses `zbus::blocking`. |
| Request deserializer (125 kB in the service) like `Response::from_json` | n/a | n/a | one helper struct per variant, strictness risk (as in round 3). |
| ctl without the English catalogue (30 kB) | n/a | n/a | only `call audio` output uses `tr`; splitting `en.properties` breaks the single Weblate file. |

What dominates now. ctl (`cargo bloat --crates`, `.text` 1.1 MB): `bridge_core` 499 kB (serde deserializers of every
`Response` payload type; `Status`, `EventKind`, `Setup`, `AgendaView`, `MessagesView` are 6-14 kB each), `std` 328 kB
(about 80 kB is backtrace/gimli, always linked), `geekbridge_ctl` 116 kB, `bridge_service` 88 kB (doctor, diagnose,
call_cli). No Opus, QR, image, vcard/ical, rustls or ring code is in the ctl (each under 3 kB); the TLS stack and the
parsers are not leaking in. Service (`.text` 4.9 MB, plus `.eh_frame` 527 kB, `.gcc_except_table` 272 kB, `.rodata`
509 kB): `bridge_service` 1.5 MB, zbus 655 kB, std 629 kB, rustls 370 kB, `bridge_calls` 305 kB, ring 195 kB (P-256 table
148 kB), `bridge_kde` 188 kB, `bridge_update` 113 kB, x509-parser 65 kB. All of it is used code.

Method: two worktrees of origin/main under `/home/ambsd/.cache`, own `CARGO_TARGET_DIR`s, `cargo-bloat 0.12.1`,
`CARGO_PROFILE_RELEASE_STRIP=none` copy for symbols, `size -A`, `nm -S --size-sort`, `/proc/PID/smaps` probe.
