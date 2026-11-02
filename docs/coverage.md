# Test coverage

Measured 2026-11-02 on Linux x86_64 with 1401 Rust tests (one more was added afterwards) and 436 Android JVM unit tests (1 skipped, 0 failed).
The percentages below are **targets and a baseline, not gates**: nothing in CI fails on them, and a drop is a
reason to look, not to block.

## Commands

```sh
make coverage                      # both, via scripts/coverage.sh
scripts/coverage.sh rust           # cargo-llvm-cov, whole workspace
scripts/coverage.sh android        # JaCoCo over the debug JVM unit tests
```

What `scripts/coverage.sh rust` runs (set `CARGO_TARGET_DIR`, `TMPDIR` to keep your normal build cache untouched):

```sh
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0   # no debug info: far smaller build
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --locked --no-report --no-fail-fast -- --test-threads=4
cargo llvm-cov report --summary-only        # per file: lines, functions, regions
cargo llvm-cov report --html                # target/llvm-cov/html
cargo llvm-cov report --lcov --output-path "$TMPDIR/geekbridge-cov.lcov"
python3 -I scripts/coverage-summary.py "$TMPDIR/geekbridge-cov.lcov" --files 15   # production code only
```

Android (`scripts/coverage.sh android`), from `android/`:

```sh
./gradlew --no-daemon -Pgb.coverage=true :app:jacocoDebugUnitTestReport
python3 -I scripts/coverage-android.py android/app/build/reports/jacoco/jacocoDebugUnitTestReport/jacocoDebugUnitTestReport.csv
```

Tooling notes:

- `cargo-llvm-cov` 0.9.1 (`cargo install cargo-llvm-cov --locked`, or a scratch `CARGO_HOME`). It needs `llvm-cov` and
  `llvm-profdata` of the **same LLVM major version as `rustc -vV`**: `rustup component add llvm-tools-preview`, or with a
  distro rustc set `LLVM_COV=/usr/bin/llvm-cov LLVM_PROFDATA=/usr/bin/llvm-profdata` (verified here: Arch rustc 1.99.1 / LLVM 23.1.3).
- `--test-threads=4` matters: instrumented code is slow enough that the cross-process lock tests in
  `bridge-kde/tests/persistence.rs` time out on a busy machine at the default thread count (they pass uncovered).
- Doc tests are not counted. `GEEKBRIDGE_REQUIRE_DBUS=1` (as in CI) was set for the measured run.
- JaCoCo is **opt-in** (`-Pgb.coverage=true`), pinned to 0.8.13 in `android/app/build.gradle.kts`, applied to
  `testDebugUnitTest` only, and never packaged: ordinary builds neither instrument tests nor download it. Its
  artifacts (and the `asm-bom` it pulls in) are in `android/gradle/verification-metadata.xml`; the new entries were
  added with `--write-verification-metadata sha256` and checked with `--offline` (verification on). They were **not**
  re-checked from a completely empty `GRADLE_USER_HOME` (see `docs/contributor-onboarding.md`); CI's `coverage` job is the
  first cold check, and it is non-blocking.
- Every number counts the inline `#[cfg(test)] mod tests` blocks as covered code unless stated "production only";
  `scripts/coverage-summary.py` strips them (they are always 100% and inflate a crate by 3-8 points).

## Rust: per crate

`cargo llvm-cov` totals (inline test modules included) and the production-only line figure:

| crate | lines | functions | regions | lines, production only |
|---|---|---|---|---|
| bridge-android | 93.9% | 92.1% | 93.4% | 93.7% |
| bridge-apps | 60.6% | 65.4% | 59.9% | 53.1% |
| bridge-calls | 83.0% | 81.8% | 81.6% | 80.1% |
| bridge-calls-macos | 90.8% | 90.5% | 91.0% | 86.7% |
| bridge-calls-windows | 80.4% | 75.7% | 77.5% | 78.4% |
| bridge-chaos | 93.3% | 92.3% | 94.1% | 94.1% |
| **bridge-core** | 95.7% | 95.7% | 95.4% | **93.8%** |
| bridge-fuzz | 70.6% | 49.5% | 74.5% | 73.5% |
| **bridge-kde** | 90.3% | 90.1% | 89.9% | **88.8%** |
| bridge-phone-sim | 83.0% | 86.0% | 81.8% | 79.1% |
| bridge-service | 80.2% | 75.5% | 81.2% | 76.3% |
| **bridge-update** | 93.5% | 88.6% | 91.8% | **93.1%** |
| **workspace** | 81.0% | 80.0% | 80.8% | 76.7% |

The three crates with an explicit 85% target (`bridge-kde`, `bridge-core`, `bridge-update`) were already above it
before this work (88.3 / 93.2 / 93.1% production only); the new tests moved the first two up by 0.5-0.6 points, which is
all there was to gain: what remains is OS-specific code and defensive branches (below).

## Rust: security, data-loss and parser files (production lines only)

| file | lines | uncovered | covered |
|---|---|---|---|
| `bridge-kde/src/pairing.rs` | 365 | 11 | 97.0% |
| `bridge-kde/src/qrpair.rs` | 200 | 6 | 97.0% |
| `bridge-kde/src/store.rs` | 361 | 8 | 97.8% |
| `bridge-kde/src/tls.rs` | 178 | 13 | 92.7% |
| `bridge-kde/src/cert.rs` | 251 | 19 | 92.4% |
| `bridge-kde/src/fsutil.rs`, `fsutil/unix.rs` | 245 | 16 | 93.5% |
| `bridge-kde/src/link.rs` (handshake, admission) | 486 | 27 | 94.4% |
| `bridge-kde/src/identity.rs`, `packet.rs` (parsers) | 393 | 14 | 96.4% |
| `bridge-kde/src/payload.rs` | 318 | 37 | 88.4% |
| `bridge-kde/src/share.rs` (receive, names) | 306 | 23 | 92.5% |
| `bridge-kde/src/bin/geekbridge-pair.rs` | 210 | 210 | 0.0% |
| `bridge-core/src/ipc.rs` (control API client, MACs) | 272 | 13 | 95.2% |
| `bridge-core/src/statefile.rs` (migrations, backup) | 282 | 17 | 94.0% |
| `bridge-core/src/clipboard_wire.rs`, `phone_link.rs` | 873 | 32 | 96.3% |
| `bridge-core/src/messages.rs` | 620 | 57 | 90.8% |
| `bridge-core/src/report.rs` | 242 | 48 | 80.2% |
| `bridge-update/src/lib.rs` (verify, install) | 638 | 58 | 90.9% |
| `bridge-update/src/http.rs` (policy, parser) | 394 | 16 | 95.9% |
| `bridge-update/src/release.rs`, `version.rs` | 204 | 11 | 94.6% |
| `bridge-service/src/gate.rs` (consent checks) | 78 | 0 | 100% |
| `bridge-service/src/control.rs` (control listener, auth) | 91 | 17 | 81.3% |
| `bridge-service/src/daemon.rs` (link admission) | 1495 | 141 | 90.6% |
| `bridge-service/src/share.rs` | 857 | 98 | 88.6% |
| `bridge-service/src/hypr_config.rs` | 311 | 16 | 94.9% |
| `bridge-service/src/phone_data.rs` | 1458 | 147 | 89.9% |
| `bridge-service/src/clipboard.rs` | 1244 | 274 | 78.0% |
| `bridge-service/src/call_control.rs` | 478 | 64 | 86.6% |

(The last test added to `control_abuse.rs` targets `control.rs` lines 137 and 142 (hang-up mid-request, oversize line); it
ran green but the report above was not regenerated after it. The other uncovered lines are the accept-error retry path and
the response-too-large fallback.)

### Risk ranking (risk x uncovered lines)

1. **`bridge-kde/src/link.rs`**: handshake refusals. Gaps were dialling ourselves, a stored protocol version newer than
   the one claimed (downgrade), and a trusted link accepting reserved/inconsistent packets. Tests added (below).
   What is left is unreachable by design: `secure.protocol_version != claimed_version` (`Identity::from_packet` already
   forces version 8), `certificate common name differs from the device id` and the second `PinMismatch` check (the TLS
   verifier rejects both first; they are defence in depth).
2. **`bridge-core/src/ipc.rs`** and **`bridge-service/src/control.rs`** (control API authentication): the client's handling
   of hostile or broken services (wrong secret, truncated, oversize, wrong-shaped replies) and the server's handling of
   half-sent and oversize request lines had no tests. Added.
3. **`bridge-core/src/statefile.rs`** (data loss): newer-format files, damaged files vs the good backup, oversize files,
   failing migrations, unknown-field round trip, revocation surviving a fallback. Added.
4. **`bridge-service/src/daemon.rs`** admission: link cap, duplicate dial, port-0 and non-local endpoints, unknown
   challenge ids, ping on an unconnected device. Added. Remaining uncovered lines are mostly error arms of
   notifier wiring, calendar/call forwards and `Err(_)` conversions.
5. **`bridge-service/src/share.rs`** send validation: path/size/type refusals, FIFO and dangling-symlink handling,
   per-device and global transfer caps, dismissing a live transfer. Added.
6. `bridge-update/src/lib.rs`, `http.rs`: already 91-96%; the leftovers are `LOCALAPPDATA` (Windows), `start_check` /
   `start_download` thread wrappers and `Busy`/`NothingToDownload` guards. Not added: low risk, no abuse path.
7. `bridge-service/src/clipboard.rs` (78%): mostly Wayland/X11 tool plumbing needing a compositor. Not added.

### Tests added (Rust)

| file | what it pins |
|---|---|
| `crates/bridge-kde/tests/handshake_abuse.rs` | self-dial refused before any trust lookup; stored version 9 vs claimed 8 is a downgrade, nothing is unpinned, no TLS bytes offered; `send`/`send_with_payload` refuse reserved types and inconsistent payload announcements and nothing reaches the wire; a second identity packet on a trusted link is fatal |
| `crates/bridge-core/tests/ipc_client_abuse.rs` | token rejected -> `Unauthorized`; reply signed with another secret refused; correctly signed reply of the wrong kind not accepted; malformed frame, hang-up, 1-byte-over-cap body, endless line, valid MAC over a non-`Response` body; hostile greetings (not our protocol, short nonce, over-long, `unavailable`); oversize request never sent; MACs bind nonce and request |
| `crates/bridge-core/tests/statefile_dataloss.rs` | newer file never rewritten/backed up (edit closure not run); damaged file kept as `.corrupt`, good `.bak` not displaced; damage with no backup per policy and file untouched; oversize file is damage; failing migration leaves the file untouched; unknown fields survive; `save_and_refresh_backup` keeps a revocation across a fallback |
| `crates/bridge-service/tests/control_abuse.rs` | port 0 / non-local endpoints refused with no link; malformed device ids refused with no trust record; unknown challenge ids and unconnected ping refused; link table capped (`TooMany`) and duplicate dial `Busy`; mid-request hang-ups and oversize request lines get no reply and leak no connection slot |
| `crates/bridge-service/tests/share_send_validation.rs` | empty/NUL/overlong/relative/missing/directory/empty/oversize paths refused and take no slot; exact-limit file accepted; FIFO and dangling symlink refused without blocking; per-device (2) and global (3) caps; live transfer cannot be dismissed |

### Not reachable without hardware or an OS (reported, not faked)

- `bridge-calls/src/audio/pulse.rs` (0%): needs a PulseAudio/PipeWire server and a Bluetooth HFP phone.
- `bridge-calls-windows`, `bridge-calls-macos`: compiled and unit-tested on Linux for their pure logic; the OS APIs are not.
- `bridge-service` tray, desktop notification, autostart, `os_notify`, `diagnose` (35-75%): need a session bus, a tray host, a desktop session.
- `bridge-apps` (53%): egui windows. Logic is covered through models; painting is covered by the golden-image job (`docs/golden-images.md`), which does not produce line coverage.
- `bridge-kde/src/bin/geekbridge-pair.rs` (0%) and `bridge-service/src/bin/geekbridge-ctl.rs` (13%): command-line front ends over the tested library calls.
- `bridge-fuzz` (74%): the fuzz targets run under `cargo fuzz`, not `cargo test`.

## Android (JVM unit tests, JaCoCo)

Whole app: **33.8% of lines, 40.1% of branches** (2,922 / 8,645 lines). The app is a single package
(`dev.geekbridge.android`), so a per-package split says nothing; the useful split is *logic* (pure Kotlin, injected
`Strings`/`KeyValues`/ports) versus *Android-bound* (Activities, Views, Services, providers, `Context`/`Looper`/`MediaStore`
users). Classes were sorted by name (Activity, Section, Receiver, Service, Provider, Port, Runtime, Dot*, Widget*, Notifications,
Dialogs, ...) into the two groups below; this is a one-off judgement, not something the script computes:

| part | lines | line coverage | branch coverage |
|---|---|---|---|
| logic classes | 2,943 | **88.0%** | 75.9% |
| Android-bound classes | 5,702 | 5.2% | 6.6% |

The Android-bound part cannot be raised by plain JVM tests without adding a Robolectric-style test dependency, which was
deliberately not done (this is a platform-only project with no runtime dependencies). On-device behaviour is covered by
the emulator scripts (`scripts/e2e-android.sh`, `docs/golden-images.md`), which JaCoCo does not see.

Logic classes the task named (lines / branches):

| class | lines | line coverage | branch coverage |
|---|---|---|---|
| `ClipboardEngine` | 78 | 100% | 88.2% |
| `ClipboardProtocol` | 52 | 96.2% | 80.0% |
| `PhoneDataEngine` / `PhoneDataProtocol` | 221 / 133 | 94.1% / 96.2% | 79.4% / 72.7% |
| `CallControlEngine`, `CallRules` | 49, 28 | 100%, 100% | 80.8%, 98.3% |
| `CallsProtocol` | 32 | 93.8% | 88.6% |
| `UpdateModel` / `UpdateChecker` | 71 / 126 | 100% / 89.7% | 75.0% / 63.0% |
| `ComputerGrants` | 33 | 100% | 89.3% |
| `WidgetModel` | 46 | 100% | 96.0% |
| `ShareProtocol`, `NameRules`, `ProgressText`, `ShareText` | 54, 24, 23, 22 | 100% each | 96.1%, 90.0%, 87.9%, 100% |
| `TransportProtocol` / `TransportText` | 135 / 58 | 99.3% / 100% | 92.1% / 97.2% |
| `TransportController` | 181 | 83.4% | 69.2% |
| `StateMigrations` + `BackedBlob` | 34 + 24 | 73.5% + 95.8% | 77.8% + 100% |
| `SetupModel`, `HomeModel` | 68, 37 | 80.9%, 100% | 74.6%, 94.9% |
| `SignerCheck` | 42 | 50.0% | 52.9% |
| **`ShareCoordinator`** | 239 | **0%** | 0% |

Why the rest is not higher:

- `ShareCoordinator` (0%), `ReceivedFiles` (0%): `Context`, `Handler`/`Looper`, `MediaStore`. The decisions they make are
  in `ReceivePolicy`, `RecentIds`, `NameRules`, `TargetPicker` and `ProgressText`, which are 100% covered.
- `SignerCheck` (50%), `StateMigrations` (73.5%), `DiagnosticsReport`, `CrashNote`: the uncovered lines are the
  `PackageManager`/`SharedPreferences`/`Build` wrappers; the comparison and migration logic is covered.
- `TransportController` (83%): `Context`-free; the remaining lines are `ActivityTracker`-dependent visibility and the
  process-wide singleton factory.

### Tests added (Android), all in `android/app/src/test/java/dev/geekbridge/android/`

| file | what it pins |
|---|---|
| `TransportParserAbuseTest.kt` | hostile `state`, missing/unsafe local identity, stopped snapshot carries no peers, entries with unsafe ids/missing ports/wrong types dropped one by one, 64-entry cap, bidi-override and length bounds on names, undisplayable pairing prompts counted not shown, wait reply trust rules, total error/event wording, security events never muted |
| `TransportControllerFailureTest.kt` | QR pairing success/refusal/unreadable/not-running, throwing worker, unreadable status keeps the snapshot, leaving the foreground does not cut a share but Stop does, failed stop, answered prompts not re-offered and pruned, core stopped underneath, bounded events and security tone |
| `StateMigrationsFailureTest.kt` | missing step and throwing step leave data untouched and flip to read-only, resume from the last completed version, version < 1, blob backup never overwritten by damaged/empty main, read-only blob ignores writes |
| `ShareParserAbuseTest.kt` | transfer-list parser (negative ids, unsafe device ids, unknown enums, caps, text vs file fields), received-file name sanitising (traversal, separators, controls, leading dots, empty, surrogate pairs at the byte limit), unique-name exhaustion, size/percent boundaries, wording totality |

## Targets (not gates)

| area | target (production lines) | now |
|---|---|---|
| `bridge-kde`, `bridge-core`, `bridge-update` | >= 85% | 88.8 / 93.8 / 93.1% |
| `bridge-service` gate and daemon admission paths (`gate.rs`, `control.rs`, `daemon.rs`) | >= 85% | 100 / 81.3 / 90.6% (`control.rs` is below target; see note) |
| Android logic classes (not Context-bound) | >= 85% lines | 88.0% |
| everything else | report only | see tables |

Rules of thumb: new security, parser, migration or consent code arrives with abuse-case tests (refused caller, consent
off, wrong certificate, truncated or corrupt input, boundary sizes, cancellation and cleanup); do not add tests that only
exercise getters to move a number. OS-specific code is listed above instead of being padded.

## CI

`.github/workflows/check.yml` has a `coverage` job (`continue-on-error`, nothing depends on it) that runs
`scripts/coverage.sh rust` and `android`, writes the tables to the job summary and uploads the HTML reports. Every action
in it uses the same SHA pins as the other jobs; `cargo-llvm-cov` is built from source at a fixed version with `--locked`.
It is a proposal until it has run once on GitHub-hosted runners (it has not: the measurements above are from this machine).
