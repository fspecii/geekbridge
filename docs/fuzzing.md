# Fuzzing

GeekBridge's LAN-facing parsers read hostile input: UDP discovery datagrams, the plaintext identity line,
TLS-wrapped KDE packets, the GeekBridge extension packets, the loopback control API, the Android JNI
request parser, the updater's release JSON and `SHA256SUMS`, vCards, and file names. This page says how
they are fuzzed, how to run it, and what it has found.

## What is there

| Layer | Where | Runs |
|---|---|---|
| Targets (one `fn(&[u8])` per parser, each with its own oracle) | `crates/bridge-fuzz/src/targets.rs` | everywhere below |
| Deterministic structure-aware mutation fuzzer | `crates/bridge-fuzz` (`mutate.rs`, `harness.rs`, `bin/gb-fuzz.rs`) | `cargo test` (2000 inputs per target, fixed seed) and `scripts/fuzz.sh --det` |
| Coverage-guided libFuzzer + ASAN | `fuzz/` (cargo-fuzz layout, own workspace) | `scripts/fuzz.sh` |
| Seed corpus (fixtures only, small files) | `fuzz/corpus/<target>/` | both fuzzers |
| Minimal repros of every finding | `crates/bridge-fuzz/tests/regressions.rs` | `cargo test` |

`cargo-fuzz` is not installed on the dev machine and the distribution Rust is stable, so `scripts/fuzz.sh`
installs `cargo-fuzz` into a scratch `CARGO_HOME` (`~/.cache/gb-fuzz-cargo`) and sets `RUSTC_BOOTSTRAP=1`,
which lets stable accept the sanitizer flags. No `rustup`/nightly is needed. If there is no network, use the
deterministic mode, which needs nothing extra.

## Targets

| Target | Parser(s) | Oracle beyond "no panic" |
|---|---|---|
| `packet_line` | `Packet::from_line` / `to_line` | one-line output; parse → serialise → parse is the identity (floats compared modulo serde_json rounding) |
| `identity_line` | `Identity::from_packet`, `sanitize_device_name`, `validate_device_name`, `DeviceId::parse` | names: no control or invisible/bidi format characters, length cap; accepted identity survives re-encoding |
| `udp_discovery` | `bridge_service::discovery::parse_identity` | name has no control/invisible characters and ≤ 64 chars; id charset and length; port range; capabilities printable and bounded; address is the datagram source |
| `line_reader` | `LineReader`, `read_line_unbuffered` (random chunking and limits) | no line over its limit, no embedded newline, never more bytes out than in |
| `pairing` | `PairBody::parse` | any accepted token has the token shape |
| `qrpair` | `QrPayload::parse`, `TokenBook::redeem*`, `token_shape_ok` | name has no control characters; ≤ 8 addresses; a wrong token never redeems; re-encoding re-parses |
| `share` | `parse_share`, `sanitize_filename`, `is_openable_url`, `reserve_unique` | file name: one path component, no control characters, no `/ \ < > : " \| ? *`, not `.`/`..`/hidden, ≤ 200 bytes, no trailing space or dot, not a Windows device name; text/URL: no control characters, size caps; reserved path stays in the directory |
| `sms` | `parse_messages`, `parse_unread`, `parse_sims` | output size bounded by input size |
| `phone_wire` | `parse_uids`, `parse_vcards`, `parse_calendar_sync`, `parse_write_result`, `parse_sms_status`, `contact_from_vcard` | count caps; output size bounded by input size (amplification) |
| `vcard` | `contact_from_vcard`, `strip_vcard_blobs`, `contact_revision`, `vcard_rev` | blob stripping never grows text |
| `calls_wire` | `LinkCallReport::parse`, `parse_request`, `parse_result` | – |
| `notification_wire` | `notification_link::parse`, `parse_request` | id, app, title and text within their caps and free of control characters; at most 3 action titles |
| `photos_wire` | `photos_link::parse_list`, `parse_thumb`, `parse_request` | at most 24 photos with unique valid ids; names within their cap and free of control and bidi characters; a thumbnail within 24 KiB, 256 px and starting like a JPEG; list limit 1..=24 |
| `clipboard` | `clipboard_wire::parse`, `parse_item`, `parse_none`, `parse_pull`, `sniff_image` | – |
| `messages_wire` | `WireMessage` → `Message::from_wire`, `sms_segments` | – |
| `ipc` | control API `Request`/`Response`/`Event`, `Token::parse`, `split_frame`, `parse_hello`, `decode_hex`, `format_hello` | request round-trips; a Ready nonce is valid hex; hello is one ASCII line |
| `android_dispatch` | `geekbridge_android::Dispatcher::handle` (the JNI request parser) | response is always a `{version, ok}` envelope; bounded size |
| `update` | `Release::parse`, `select_asset`, `find_checksum`, `clean_notes`, `Url::parse`, `Policy`, `Version::parse`, `valid_repo`, `safe_file_name` | selected asset name is safe; digests are 64 lower-case hex; notes have no control characters; parsed host has no `/ @ ? # space` |
| `cert` | `CertInfo::parse` (DER), `DeviceCredentials::from_pem` | – |
| `hub_phone_data`, `hub_calls`, `hub_clipboard` | `PhoneData::on_packet`, `CallHub::on_packet`, `ClipboardService::on_packet`: a sequence of hostile packets (one per line) to a live hub with a trusted, fully consented link | clipboard text not amplified |

Resource checks, for every input (deterministic mode): a counting global allocator fails an input whose
peak extra live bytes exceed 24 MiB + 64 B per input byte; an input slower than 1.5 s is a timeout finding;
a watchdog thread saves the input and aborts the process if one input runs for 20 s. libFuzzer mode uses
its own `-timeout=10 -rss_limit_mb=2048`.

### How inputs are made (deterministic mode)

Per input, one of: byte-level mutation of a seed (bit flips, splices from another seed, truncation,
chunks repeated up to 30 000 times, NUL/CR/LF/escape bytes); field-level mutation of the seed parsed as
JSON (huge numbers, `i64::MIN`, `u64::MAX`, ±1e308, negative sizes, nesting up to 400, 5000-element
arrays, 2000-key objects, 70 000-byte strings, control characters, path-traversal and device names, bidi
overrides, dropped/renamed keys, type swaps); whole packets generated from a dictionary of keys and values
harvested from the seeds and the target's packet types; and a text patch on top (duplicate keys, lone or
reversed UTF-16 surrogate escapes, invalid UTF-8, the whole document re-encoded as UTF-16 LE/BE with or
without BOM, CRLF, doubled or missing newline, 100 000-deep raw nesting, unterminated strings, odd number
literals). The first pass replays every seed unchanged. Input *i* of a run is a pure function of
`(seed, i)`, so any finding replays without the rest of the run.

Seeds are the recorded real packets in `crates/bridge-kde/tests/fixtures/kdeconnect-real` and
`kdeconnect-android-real` (selected per target by packet type) plus hand-written seeds for non-packet
inputs (certificates, QR text, vCards, release JSON, `SHA256SUMS`, control-API lines, JNI requests).

## Running

```
scripts/fuzz.sh [target|all] [seconds]          # libFuzzer + ASAN, default 300 s per target
scripts/fuzz.sh --det [target|all] [seconds]    # deterministic mutation fuzzer (FUZZ_SEED=n to change the seed)
scripts/fuzz.sh --seed                          # rebuild fuzz/corpus from the fixtures
cargo test -p bridge-fuzz                       # the CI pass: 2000 inputs per target + regression repros
GEEKBRIDGE_FUZZ_ITERS=200000 cargo test -p bridge-fuzz --release
```

Logs and crash files go to `dist/fuzz/<timestamp>/` (`crash-*`, `timeout-*`, `oom-*`, `hang-*`). The
script is niced and runs one target at a time. Reproduce:

```
cargo run -p bridge-fuzz --bin gb-fuzz -- replay <target> <crash-file>
cargo run -p bridge-fuzz --bin gb-fuzz -- replay-iter <target> <seed> <iteration>   # deterministic mode
cd fuzz && cargo fuzz run <target> <crash-file>                                     # libFuzzer
```

To reduce a finding, minimise it (`cargo fuzz tmin`), then put the reduced input in
`crates/bridge-fuzz/tests/regressions.rs`.

## Not covered

* **Android engine packet handlers** (`incoming_data/calls/clipboard/share` in `bridge-android`) are
  `pub(in crate::transport)` and need a live TLS link to drive. They call the same `bridge-core` / `bridge-kde`
  parsers fuzzed here, and `crates/bridge-android/tests/chaos.rs` plus `crates/bridge-chaos` send hostile
  traffic through a real link. The UDP `parse_announcement` there is private; the desktop equivalent is
  fuzzed (`udp_discovery`) and the shared `Packet`/`Identity` parsers underneath both are fuzzed.
* **`ShareManager::on_packet`** needs a `PayloadContext`, which only a pinned `TrustedLink` can produce;
  its parser (`parse_share`) and the file-name path are fuzzed in `share`.
* **`Daemon::handle`** (the actions behind the authenticated control socket) is authenticated, and acts on
  the machine (dials, writes, opens files). Only the pre-handler parsing is fuzzed (`ipc`); the TCP framing and
  MAC check are covered by `crates/bridge-service/tests/control_api.rs`.
* `android_dispatch` skips requests that would start the transport with a state directory (they bind
  sockets and write files); the argument parsing for them is still reached by the rest of the corpus.

## Findings

Runs: libFuzzer + ASAN, 300 s per target (20 targets, 0.7–8.5 million executions each, coverage
590–3055 edges); then the deterministic fuzzer, 300 s per target (1–14 million inputs each, seed 7), on top of
the libFuzzer-grown corpus. The findings below came from the deterministic fuzzer's oracles (short smoke runs and the long run);
libFuzzer found no crash, timeout or OOM in the product code.

| # | Target | Finding | Status |
|---|---|---|---|
| 1 | `share` | `sanitize_filename` cut a long name to 200 bytes *after* its trailing-dot/space trim, so the result could end in a space or dot (invalid on Windows, where it is silently stripped or refused). The cut could also turn `CON` + 250 spaces + `x` into the reserved device name `CON`, because the device-name check ran before the cut. | **Fixed** (`bridge-kde/src/share.rs`: cut first, re-trim, then device-name check) |
| 2 | `udp_discovery` | The datagram parser kept zero-width and bidi-override characters in `deviceName` (`U+202E`, `U+200D`, …) while the TCP identity path strips them, so a LAN broadcaster could reorder or hide text in the device list before any pairing. | **Fixed** (`bridge-service/src/discovery.rs` strips them with `bridge_kde::identity::is_invisible_format`, now `pub`; a name that is only invisible characters is refused) |
| 3 | `phone_wire` | `parse_vcards` accepted the same uid up to 100 times (`VCARD_BATCH * 4`) and cloned its vCard each time: a 17.9 KB packet produced 714 KB of output, i.e. up to 100× the packet size (the line limit allows far larger vCards). | **Fixed** (`bridge-core/src/phone_link.rs`: repeated uids are skipped) |

False positives found while building the oracles (fixed in the harness, not the product):

* `packet_line` round-trip: serde_json without `float_roundtrip` can move a non-integer number by one ulp
  when it re-parses its own shortest form; floats are now compared modulo that.
* `qrpair` re-parse: a name of multi-byte characters can encode past the 700-byte payload cap, which `parse`
  refuses by design.
* `hub_clipboard` out-of-memory under libFuzzer: the service's watcher task never finishes on a runtime that
  is never polled, so one service per input accumulated. Each hub input now gets its own runtime.

Open: none. Known limitation: the deterministic fuzzer only sees what the oracles check; targets marked "–"
above are checked for panics, hangs and allocation only.

`cargo test -p bridge-service --test share_api cancel_and_unpair_close_the_payload_listener` failed once
while the fuzzers and a full workspace test run shared the machine (a port-close timing assertion) and passed
three of three on re-run; it is unrelated to these changes.
