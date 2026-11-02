# Release-candidate gate report

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Run on 2026-11-01 (22:30 to 22:55 EET) by the test sub-agent, on the **uncommitted working tree** on top of `a46f372`
(main). Host: Linux 7.2.8-9-omarchy x86_64. Nothing was committed, tagged or pushed. The tree had been quiet for 10
minutes before the run started (other agents were working on desktop UI, chaos, e2e and Hyprland files).

Toolchain: cargo/rustc 1.99.1 (Arch), git 2.56.0, JDK 17.0.21+9, Gradle 8.13 (project-local, `source scripts/android-env.sh`),
Android platform 36, build-tools 36.0.0, NDK 30.0.16248370. actionlint, cargo-deny and yamllint are **not** installed.

## Result

**Green after 5 small fixes, with one open item**: the desktop binaries are over the size budget in `scripts/perf-check.sh`
(CI's Linux job runs that script, so CI would fail on it).

| # | Gate | Command | Result |
|---|---|---|---|
| 1 | Format | `cargo fmt --all -- --check` | pass (after `cargo fmt --all`, 2 files needed it) |
| 2 | Clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass (after 1 fix) |
| 3 | Tests | `cargo test --workspace --locked` | pass: 1132 passed, 0 failed, 3 ignored (after 1 fix) |
| 4 | Windows type-check, whole workspace | `scripts/xcheck.sh windows` with the private toolchain (`RUSTUP_HOME=.tools/windows-check/rustup`, `RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu`, `PATH=.tools/cargo/bin:$PATH`, `XCHECK_TARGET_DIR=target`); `cargo check --workspace --all-targets --locked --target x86_64-pc-windows-msvc`, warnings denied | pass (after 2 fixes) |
| 5 | Windows clippy (what CI's `cross-targets` runs) | `XCHECK_CLIPPY=1 scripts/xcheck.sh windows` using the macOS toolchain's clippy and the Windows std, joined through a scratch sysroot (`RUSTFLAGS=--sysroot ...`, symlinks only, since deleted) | pass |
| 6 | Windows call backend | `.tools/windows-calls-check/check-windows.sh check -p bridge-calls-windows --all-targets --locked` | pass |
| 7 | macOS type-check + clippy, whole workspace | `source crates/bridge-calls-macos/scripts/check-env.sh; XCHECK_CLIPPY=1 scripts/xcheck.sh macos-arm macos-x86` | pass (after 1 fix) |
| 8 | macOS native call backend | `crates/bridge-calls-macos/scripts/check-native.sh` (check + clippy both Apple targets, fmt, host `cargo test --features mock`: 119 passed) | pass |
| 9 | Perf budget | `scripts/perf-check.sh --build` | **FAIL, binary size only** (see below). Runtime budgets all pass: startup 6 ms, idle RSS 6564 kB (budget 10000), 3 threads, 14 fds, 3 wakeups/s (budget 3), 0 CPU ticks |
| 10 | Android unit tests, lint, debug APK | `cd android && gradle --offline --console=plain :app:testDebugUnitTest :app:lintDebug :app:assembleDebug --rerun-tasks` | pass: 290 tests, 0 failures, 0 skipped; lint 0 errors, 30 warnings (`app/build/reports/lint-results-debug.txt`); debug APK built |
| 11 | Android release (R8) build | `PERF_DIR=... PERF_SCRATCH=... scripts/android-perf.sh size` | pass: release APK 6 211 948 B (budget 6.5 MB), debug 7 310 079 B, dex (release) 412 124 B, `libgeekbridge_android.so` arm64 2 768 848 B / x86_64 3 165 808 B, res 41 816 B |
| 12 | Release flow, dry run | In a fresh clone overlaid with the working-tree files and committed there (never in the main checkout): `scripts/release.sh --dry-run 9.9.9-rc.1` (runs fmt, clippy, full tests with its own target dir) | pass: "would bump 0.1.0 -> 9.9.9-rc.1 in Cargo.toml, Cargo.lock and CHANGELOG.md"; 1132 tests passed |
| 13 | Release flow, real bump | Same copy: `scripts/release.sh --skip-tests 9.9.9-rc.1` | pass: edits Cargo.toml, Cargo.lock (22 lines) and CHANGELOG.md (new `## [9.9.9-rc.1] - date` heading under Unreleased); no tag or commit created; `cargo metadata --locked` and `cargo build --locked` still succeed afterwards |
| 14 | Release CI build step | In that copy: `cargo build --release --locked -p bridge-apps -p bridge-service -p bridge-kde -p bridge-calls -p bridge-update` | pass |
| 15 | Workflow YAML | No actionlint. Parsed `.github/workflows/check.yml` and `release.yml` with PyYAML: valid, all `needs:` resolve, every step has `run` or `uses`. `bash -n` on every `scripts/*.sh`, `scripts/e2e/*.sh`, `packaging/linux/*launch*`, `crates/bridge-calls-macos/scripts/*.sh`: clean | pass (syntax only, not a semantic lint) |
| 16a | Clean clone of **HEAD** | `git clone . <empty dir>`; `cargo build --workspace --locked && cargo test --workspace --locked` | pass: 1085 passed, 0 failed, 3 ignored. Proves the committed state needs no untracked or ignored files |
| 16b | Clean clone + **working tree** | Fresh clone, then every non-ignored file from `git ls-files -co --exclude-standard` (504 files) copied over and committed in the copy; built and tested as in 12 and 14 | pass: 1132 passed. Proves the RC content, once committed, needs no ignored files |
| 17 | Cargo.lock | `git ls-files Cargo.lock` lists it; `git check-ignore` does not match it; `cargo metadata --locked` succeeds on the working tree and in both clones, so it is complete and every `--locked` command above resolved without edits | pass |

Note on 16a vs 16b: HEAD alone passes 1085 tests, the working tree 1132. The 17 untracked source files and 70+ modified
files are **not in HEAD**, so a literal clone does not exercise them until they are committed.

## Fixes made (all small, root-cause)

1. `crates/bridge-service/tests/discovery_api.rs`: 3 tests (`cache_is_bounded_and_first_come_first_kept`,
   `api_stays_available_under_a_heavy_identity_flood`, `api_never_emits_more_than_max_api_bytes_even_when_the_cache_is_heavy`)
   failed deterministically. The new `SourceLimiter` in `bridge-service/src/lib.rs` (4 device ids per source address, 30
   packets per 10 s) stops a single loopback sender after 4 ids, so the tests could never fill the cache over UDP. They now
   seed the cache through `discovery::parse_identity` + `record_device` (the listener's own path minus the limiter). Added
   `one_udp_source_can_list_only_a_few_device_ids`, which covers the limiter on the real UDP path. Production code untouched.
2. `crates/bridge-service/src/hypr_config.rs` (new, untracked): clippy `cloned_ref_to_slice_refs` in two test asserts, now
   `std::slice::from_ref(&f)`; two tests that call `std::os::unix::fs::symlink` got `#[cfg(unix)]` (broke the Windows build).
3. `crates/bridge-android/tests/payload_nat.rs` (tracked since a46f372): uses `std::os::fd` and `as_raw_fd`, so it did not
   compile on Windows (`cargo test --workspace` on the Windows CI runner). Added `#![cfg(unix)]`.
4. `crates/bridge-kde/src/keepalive.rs`: `DEAD_LINK_LIMIT` is unused on macOS outside tests, so `clippy -D warnings` failed on
   the macOS CI runner. Added `#[cfg_attr(target_os = "macos", allow(dead_code))]` with a comment.
5. `cargo fmt --all` reformatted `desktop_notify.rs`, `hotkey_ctl.rs`, `hypr_config.rs` (mechanical).

## Open items (not fixed)

- **Binary size budget exceeded** (`scripts/perf-check.sh`, runs in CI's Linux job): `geekbridge-service` 5 080 472 B vs
  5 000 000 B budget; `geekbridge-ctl` 1 117 360 B vs 800 000 B (docs/performance.md baseline 719 192 B). The growth tracks the
  new Hyprland/Omarchy modules (`hypr_config`, `hotkey_cli`, `hotkey_help`, `waybar`, `desktop_notify`, `notify_text`,
  `session_env`) being linked into the ctl and service. Either trim (feature-gate or lazy-load them out of `geekbridge-ctl`) or
  raise `BUDGET_SERVICE_BYTES` / `BUDGET_CTL_BYTES` deliberately and update docs/performance.md. I did not change the budget.
- CI uses `actions/checkout@v7` (unpinned) and `dtolnay/rust-toolchain@stable` in `check.yml`, while `release.yml` pins by SHA.
  Not a failure; pin them if the policy is to pin everywhere.
- The Android lint run has 30 warnings (0 errors); not triaged.
- Several in-progress files from other agents were changed by them while I ran (`scripts/e2e/ux-sweep.sh`, `scripts/android-perf.sh`
  between 22:30 and 22:37). Re-run the gate if more edits land.

## Not verifiable here

- Real phones: pairing, transfers, SMS/contacts/calendar, notifications, battery, Doze/OEM background limits, the Android
  update flow on a device. (Emulator e2e is a separate suite and was not part of this gate.)
- Windows and macOS **runtime**: only type-checks and clippy ran, plus `bridge-calls-macos` host tests with the mock feature.
  Nothing was linked or executed on either OS (`cargo check` stops before the linker; the C compiler is stubbed for `ring`).
  Windows and macOS `cargo test --workspace` (run by CI) was not executed; only that the test targets compile.
- Bluetooth call audio (HFP/SCO routing, echo, latency, Windows/macOS audio devices, macOS TCC prompts).
- A real GitHub Release: the tag push, `release.yml` (version/tag check, matrix builds, signing, `SHA256SUMS`, publish), the
  "tag reachable from main" check, and the in-app updaters finding the real artifacts. The dry run and bump only prove the
  local script and that `--locked` still resolves after the bump.
- Android release signing with the real keystore, and Google Play policy (Data safety form, sensitive permission declarations
  for SMS/call log/contacts, foreground-service types, target API requirements). The release APK built here is signed with
  a throwaway key.
- Hyprland/Omarchy integration on a real Hyprland session (keybinding install, Waybar module, Wayland clipboard).
- The CI runners themselves (Ubuntu apt dependencies, `GEEKBRIDGE_REQUIRE_DBUS=1`, `scripts/emulate-call.sh fake`) and the
  Android CI job's SDK/NDK installation.
