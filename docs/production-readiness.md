# Production readiness audit

Scope: the existing workspace crates `bridge-core`, `bridge-service`, `bridge-apps`, `bridge-kde`,
`bridge-calls`, `bridge-android`, `bridge-calls-windows`, `bridge-calls-macos`. The phone simulator,
Android app, emulator scripts and CI workflow were out of scope. Audit date 2026-11-01, toolchain
rustc 1.99.1 (Linux x86_64). The audit was a code read plus the CI commands below; nothing was run
against a real phone, a real Bluetooth stack or a real Windows/macOS host.

## Commands run (what `.github/workflows/check.yml` runs, minus `bridge-phone-sim`)

| Command | First run | Final run |
| --- | --- | --- |
| `cargo fmt --all -- --check` | FAIL: about 26 files (bridge-calls audio, bridge-calls-macos, bridge-calls-windows, three bridge-kde files, bridge-apps/connection.rs) | pass |
| `cargo clippy --workspace --exclude bridge-phone-sim --all-targets --locked -- -D warnings` | FAIL: 6 errors in bridge-calls-windows (4x `collapsible_if`, `large_enum_variant`) and 1 in bridge-calls-macos (`serde` tag/field conflict, later further `collapsible_if` / `single_match`) | pass |
| `GEEKBRIDGE_REQUIRE_DBUS=1 cargo test --workspace --exclude bridge-phone-sim --locked` | not reachable until clippy-clean builds existed; bridge-core, -kde, -service, -apps, -calls, -android passed individually (about 500 tests, mock D-Bus present) | pass: 39 suites, 658 passed, 0 failed, 3 ignored (live PipeWire inventory, OpenSSL interop, stock-cert fixture; need external setup) |
| `cargo build --release --locked -p bridge-apps -p bridge-service -p bridge-kde -p bridge-calls` | pass | pass |
| `cargo check --target x86_64-pc-windows-gnu` | not run: only `x86_64-unknown-linux-gnu` is installed and nothing may be installed | not run |

All commands used `CARGO_TARGET_DIR=target/agent-audit`. While `crates/bridge-phone-sim` had no
`Cargo.toml` yet, an isolated copy of the workspace without that member was used, so results for
the audited crates are equivalent. The Windows and macOS crates and `bridge-apps/connection.rs`
were being edited by other workers during the audit; the formatting and the `bridge-calls-windows`
`bin` clippy error that appeared mid-audit were resolved by those workers, not by this audit.

## Findings

Severity: High = remotely triggerable or data exposure; Medium = reliability/operability;
Low = hygiene.

| # | Sev | Where | Finding | Status |
| --- | --- | --- | --- | --- |
| 1 | Medium | `bridge-service/src/lib.rs` `run` | Only Ctrl-C was handled. `systemd`/`launchd` stop with SIGTERM, which killed the daemon without running the cleanup path (task aborts, listener close). | Fixed: `shutdown_signal()` handles Ctrl-C and SIGTERM (handlers installed before the listeners start); regression test `tests::sigterm_ends_the_service_cleanly`. |
| 2 | Medium (CI) | `bridge-calls-windows` `engine.rs`, `readiness.rs`, `service.rs`, `testing.rs` | Clippy `collapsible_if` x4 and `large_enum_variant` (`Response::Command`, 384 bytes vs 168) failed the `-D warnings` CI gate on all three OSes (the crate compiles on Linux). | Fixed: let-chains; `Response::Command(Box<CommandReport>)` (4 call sites). Behaviour unchanged, tests pass. |
| 3 | Low (CI) | `bridge-kde/src/fsutil/{unix,windows}.rs`, `tests/persistence.rs` | rustfmt drift (fails the `fmt` gate). | Fixed with rustfmt on those files only. |
| 4 | Low (CI) | `bridge-calls`, `bridge-calls-macos`, `bridge-calls-windows`, `bridge-apps` | rustfmt drift and clippy errors in files under active edit by other workers. | Resolved by those workers; final run is clean. |
| 5 | Info | `bridge-service/src/lib.rs` UDP discovery | UDP 1716 is bound on `0.0.0.0` and parsed with a hard datagram cap; records are capped (`MAX_DEVICES`), expire after 90 s, are never trusted and only used for addresses. A LAN host can still fill the device list with fake entries (denial of presentation, not of security). | Open (accepted; design documented). |
| 6 | Info | `bridge-service/src/daemon.rs` | `ConsentStore` file writes and the trust store run synchronously under a `std::sync::Mutex` on the async executor. Writes are tiny and happen only on explicit user commands; no `.await` is held across a lock. | Open (acceptable; move to `spawn_blocking` if the store grows). |
| 7 | Info | `bridge-core/src/ipc.rs` | The control-token HMAC is hand-written (RFC 2104 over `sha2`). Comparison is constant time, the token is redacted in `Debug` and wiped on drop. Unit tests exist; no external vector run was done in this audit. | Open (recommend swapping to the `hmac` crate or adding RFC 4231 vectors). |
| 8 | Info | `bridge-core` `Token` derives `PartialEq` | Non-constant-time `==` exists on the token type; the service uses `constant_time_eq` for authentication and does not use `==` on tokens. | Open (low). |

### Checked and found sound (evidence, no change needed)

* Untrusted-input panics: a scan of non-test code in all eight crates found no `unwrap`/`expect`
  on network, JSON, or JNI input. The remaining `expect`/`unreachable!` calls guard local
  invariants (just-checked `Option`s, thread spawn, system RNG, `Value` serialisation). Mutex
  `lock().unwrap()` sites in `bridge-kde/src/store.rs`, `tls.rs` and `bridge-calls-windows/src/service.rs`
  only panic after another thread already panicked (poisoning).
* `bridge-kde` packets (`packet.rs`): newline framing with a hard line limit, JSON depth limit 64
  before parsing, type length cap, id/payload validation, deadlines on every read (`timeout_at`),
  reader poisoned after any error (no resynchronisation). Plaintext identity is read byte by byte so
  TLS data is never consumed.
* Control API (`bridge-service/src/control.rs`): loopback only (`Ipv4Addr::LOCALHOST`), 8 concurrent
  exchanges, 3 s overall deadline, request and response size caps, MAC verified before the body is
  parsed, constant-time compare, nonce per connection.
* Discovery API: loopback only, exact request match (`GET /v1/discovery HTTP/1.0`), so browser
  requests (HTTP/1.1 with `Host`/`Origin` headers) get 404 and cannot be used from a web page; 16
  concurrent clients, 2 s deadlines, response size cap.
* Key and state files (`bridge-kde/src/fsutil/*`, `bridge-service/src/private.rs`): directories
  0700 and files 0600 on Unix, owner check, symlink refusal (`O_NOFOLLOW`), FIFO-safe open,
  size-capped reads, atomic write (O_EXCL temp file, fsync, rename, directory fsync); Windows uses
  owner/DACL checks. Insecure state makes the control API report `unavailable`; it never falls back.
  Tests assert the modes.
* Logging: no logging framework; `eprintln!` is limited to startup and fatal errors. Device paths in
  error detail go to the operator terminal only. Daemon events carry connection facts, never
  message, contact, clipboard text, tokens or pairing codes. `Token` and `DeviceCredentials` have
  redacted `Debug` implementations.
* `bridge-android`: the JNI export is wrapped in two panic guards, rejects `null`, oversized input
  (before decoding) and unpaired surrogates, error messages are static (no request data). Transport
  is bounded (connection, unpaired, prompt, event and discovery caps, handshake and park timeouts).
  The `Strict` JSON deserializer rejects duplicate keys; serde_json's depth limit applies.
* `unsafe`: every `unsafe` block outside generated macro code has a `SAFETY` comment (bridge-core,
  bridge-apps, bridge-kde unix/windows). `bridge-android` and `bridge-calls-windows` use
  `#![deny(unsafe_code)]` with narrowly scoped allows.
* Leftovers: no `todo!`, `unimplemented!`, `dbg!`, `TODO` or `FIXME` in the audited sources.
  `println!` appears only in CLI binaries (`geekbridge-ctl`, `geekbridge-pair`, `geekbridge-call-probe`,
  `--help`), where it is the intended output. The service never prints a token (only its path).
* No subprocess is spawned from any library crate; PulseAudio/D-Bus access has connect and
  operation timeouts (3 s) and the calling probe runs in `spawn_blocking` under a 2 s deadline with a
  single-flight gate.

## Remaining risks (honest list)

1. **No hardware verification.** Nothing here has run against a real phone, a real Bluetooth HFP
   audio gateway, a real microphone/speaker path or a real SIM. The emulated call exercises protocol
   and state logic, not radio or audio quality.
2. **KDE Connect interoperability is untested against the stock app.** The pairing and TLS
   behaviour is tested against this repository's own implementation (and an optional OpenSSL
   interop test that is ignored by default). Protocol version, certificate and identity edge cases
   with the real Android/desktop KDE Connect clients are unproven.
3. **Windows and macOS are not exercised.** CI runs them, but this audit could only compile and
   test on Linux; the Windows (WinRT / `windows` crate) and macOS (IOBluetooth / CoreAudio via
   `objc2`) native layers are new, cannot be built here, and have `unsafe` code that was not
   reviewed because it was still being written. `cargo check --target x86_64-pc-windows-gnu` could
   not be run (target not installed).
4. **Android build artefacts are debug-signed** and not hardened for release (no release keystore,
   no R8/ProGuard review, no Play integrity or permission-audit pass). Treat any APK from this tree
   as a development build.
5. **Discovery/inbound surface on the LAN.** UDP 1716 is open on all interfaces and the optional
   `--listen-inbound` TCP listener accepts unauthenticated TLS handshakes (an address policy and
   global concurrency caps apply, but there is no per-source-address rate limiting).
6. **Local trust boundary.** Any process running as the same user can read the control token and
   drive pairing and consent. Other local users are blocked by file modes only. There is no
   sandboxing and no OS keystore integration for the device private key (stored as a 0600 file).
7. **Dependency supply chain.** `cargo audit`/`cargo deny` were not run (no network tooling
   installed); the dependency tree is large (eframe/egui, zbus, rustls, windows, objc2).
8. **Graceful shutdown is process-level.** After the SIGTERM/Ctrl-C fix the service stops promptly,
   but live links are dropped rather than closed with a protocol-level goodbye.
9. **No soak, fuzz or load testing.** Parsers have unit and adversarial-input tests but no
   `cargo fuzz` targets; long-running behaviour (leaks, reconnect storms) is unmeasured.
