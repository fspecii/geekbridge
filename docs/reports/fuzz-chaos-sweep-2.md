# Fuzz and chaos sweep 2

Scope: the code added on 2026-11-02 (phone debug link, reconnect book, doctor ufw parsing, SMS send-ledger pruning, Windows datagram handling in discovery).

## Method

`cargo-fuzz` is not installed here, so the coverage-guided libFuzzer run was not possible. Instead: the repo's deterministic structure-aware fuzzer (`gb-fuzz run all --seconds 60 --seed 7`, release build), the same targets and oracles the libFuzzer binaries use. All 24 targets ran 60 s each (a few hundred thousand to 1.8 million inputs per target; send_ledger is slower, 62k), plus the 2000-input CI pass (`cargo test -p bridge-fuzz`) and the chaos suites (`chaos_link`, `chaos_transfer`, `chaos_clipboard`, `discovery_api`, `leaks`).

## New targets (previously no fuzz case)

| target | covers | oracles |
| --- | --- | --- |
| `debug_link` | `parse_log`, `parse_state`, `sanitize_line`, `valid_request_id` | line count, line length, no control or invisible format characters, sanitiser idempotent, request id valid, bounded output |
| `hub_debug` | `PhoneDebug::on_log/on_state/request_state` over packet sequences | nothing stored without a directory, a state reply is only accepted for an outstanding request |
| `doctor_ufw` | `ufw_allows` and `doctor::run/render` over arbitrary rules text | a true answer needs a real accepting `ufw-user-input` line; bounded output |
| `reconnect` | `reconnect::Book` driven by random due/settled/pause/keep_only with advancing time | never two dials of one phone less than 2 s apart, total dials bounded by elapsed time, `trying()` only lists known phones |
| `send_ledger` | `SendLedger` submit/apply/`prune_settled` | prune removes only settled sends, never changes kept ones, stops early only when nothing settled is left |

libFuzzer wrappers and `[[bin]]` entries were added under `fuzz/`. Regression tests are in `crates/bridge-fuzz/tests/regressions.rs`.

## Findings

1. **Fixed: debug log sanitiser let the deprecated format characters U+206A..U+206F through** (found by `debug_link`, iteration 36796, input line `c<U+206E>d`). `debug_link::is_spoofing_format` covered U+2060..2064 and U+2066..2069 only. These are invisible (inhibit swapping, shaping, digit shapes) and could disguise a phone log line, which defeats the point of the bidi stripping added in 3cf144b77. The range is now U+2060..U+206F, matching the parser-side filters elsewhere. Tests: `bidi_overrides_and_zero_width_marks_cannot_disguise_a_line` (extended), `debug_log_lines_lose_the_deprecated_format_characters`. Re-run of `debug_link` for 60 s: 0 findings.
2. No panic, hang or unbounded growth in the other 23 targets, including all the new ones.

## Chaos and bounds checks

* `phone_debug`: new test `a_flood_of_maximum_packets_keeps_disk_use_bounded` sends 2000 near-64 KB log packets from a consented phone: the token bucket accepts about the burst, and the log directory stays within the rotation cap (5 MiB x (3 rotated + live)).
* Windows datagram handling: new unit test `only_per_datagram_receive_errors_are_skipped` for `is_per_datagram_error` (WSAEMSGSIZE 10040 and WSAECONNRESET 10054 only skipped on Windows; permission and other errors still end the listener). The existing `discovery_api` test (oversize and malformed datagrams do not kill the listener, boundary at `MAX_IDENTITY_BYTES`) passes on Linux. The Windows-specific error path cannot be exercised on this host; the Windows CI run recorded in `windows-ci.md` covers it.
* Chaos suites: all pass (chaos_link 3, chaos_transfer 5, chaos_clipboard 9, discovery_api 21, leaks 3).

## Observations, not changed

* `SendLedger::prune_settled` never drops in-flight or uncertain sends, by design. A phone that never answers can therefore leave those records until they are resolved by the user; growth is limited by the 10 sends/minute limit and the ledger cap of 10,000 records (`MAX_LEDGER_RECORDS`), not by `SEND_HISTORY`.
* `clipboard::tests::watcher_stops_and_forgets_text_when_last_device_is_disabled` failed once while the machine was busy with the full parallel test run and passed in 4 further runs (alone and in the whole `bridge-service` run): a timing flake in code not touched here.

## Results

`cargo fmt --all -- --check` 0, `cargo clippy -p bridge-fuzz -p bridge-core -p bridge-service --all-targets -- -D warnings` 0, `cargo test` for those crates 0 (after the flake above, `--no-fail-fast` full run 0). Fuzz output and crash files were written under the temp dir and deleted afterwards.
