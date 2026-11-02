# CI dry run (2026-11-02)

The GitHub Actions workflows had not yet run on GitHub-hosted runners when this was written, so `.github/workflows/check.yml` and
`release.yml` had never run. Before the first real run, every step that can execute on Linux was run locally, as
written, and the rest was checked by reading and by scripted logic checks. **Nothing was pushed, tagged or triggered.**

## Method

- Fresh `git clone` of HEAD (`d12b9c5`) in a scratch directory under `~/.cache/gb-tmp/ci`, then every non-ignored working-tree
  file (`git ls-files -co --exclude-standard`) copied over and committed *in the clone* (same overlay as
  `docs/reports/release-candidate-report.md`), so the state of the working tree is what ran.
- `run:` blocks were extracted from the YAML by script and executed unchanged (`${{ }}` expressions substituted by hand
  for matrix values). `sudo apt-get install` was replaced by a check that the Arch equivalents are installed:
  `pkgconf libxkbcommon wayland mesa libxrandr libxi libpulse dbus librsvg` (all present).
- The workflows were parsed with PyYAML and validated against the SchemaStore GitHub Actions schema
  (`https://www.schemastore.org/github-workflow.json`, `jsonschema` Draft 7): 0 errors in both files, every `needs:`
  resolves, every step has exactly one of `run`/`uses`.
- Every `uses: owner/repo@sha` was resolved with `gh api repos/<owner>/<repo>/git/commits/<sha>` and matched to its tag:
  `actions/checkout` v7.0.1, `actions/setup-java` v4.9.1, `actions/upload-artifact` v4.6.2, `actions/download-artifact` v4.3.0,
  `android-actions/setup-android` v3.2.2, `EmbarkStudios/cargo-deny-action` v2.1.1 (runs cargo-deny 0.20.2 in a Docker image),
  `dtolnay/rust-toolchain` = current head of branch `stable`. All 7 resolve.
- Not available here: `pwsh`, `actionlint`, `shellcheck`, a Windows or macOS machine.

## check.yml

| Job / step | Local result |
|---|---|
| all jobs: YAML, schema, SHA pins, `timeout-minutes` (30/30/30/15/60), `permissions: contents: read` | ok |
| rust (Linux): Linux dependencies | Arch equivalents present. On `ubuntu-latest` the apt names exist (`libxkbcommon-dev libwayland-dev libgl1-mesa-dev libxrandr-dev libxi-dev libpulse-dev dbus pkg-config`); the only `-sys` crates needing them are `libpulse-sys` and `wayland-sys` |
| rust: `cargo fmt --all -- --check` | pass |
| rust: `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| rust: `cargo build --workspace --locked` | pass |
| rust: `cargo test --workspace --locked --no-fail-fast` with `GEEKBRIDGE_REQUIRE_DBUS=1` | pass, 1267 passed / 0 failed / 4 ignored. Run twice: once in the desktop session, once with `env -i`, empty `HOME`, no `DISPLAY`/`WAYLAND_DISPLAY`/session bus (what a runner looks like): same result |
| rust: smoke test, `--live` smoke test, `emulate-call.sh fake` | pass (also in the clean environment) |
| rust: release build of the 5 packages | pass (171 s) |
| rust (Windows, macOS): same steps, plus `bridge-calls-windows` / `bridge-calls-macos` release build | **not run** (see below). Type-check and clippy of the whole workspace for all three targets: pass, via the `cross-targets` job below |
| perf | pass: startup 10 ms, RSS 8032 kB, 5 threads, 18 fds, 1 wakeup/s, service 5 506 744 B (budget 5 600 000), ctl 1 146 568 B (1 200 000) |
| cross-targets (`XCHECK_CLIPPY=1 scripts/xcheck.sh`) | **failed twice, fixed, now passes** for `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, `x86_64-apple-darwin`. Ran with the private toolchains in `.tools` joined through a scratch sysroot (no `rustup` on this machine) |
| supply-chain: cargo-deny 0.20.2 (same version as the action's image) | `advisories FAILED, licenses FAILED, bans ok, sources ok`. (historic; now all ok, see supply-chain.md) See "Open items" |
| supply-chain: `scripts/gen-notices.sh --check` | **failed (stale file), fixed**, now "up to date" |
| android: Gradle 8.13 download + sha256 | real download, checksum matches `services.gradle.org/.../gradle-8.13-bin.zip.sha256` |
| android: SDK packages | platform 36, build-tools 36.0.0, NDK 30.0.16248370 present in the project-local SDK (the `sdkmanager` install itself runs only on a runner) |
| android: native libraries step | pass (26 s), both ABIs, 16 KiB page size flag, strip |
| android: JDK 17 / Gradle 8.13 version check | pass |
| android: `gradle --no-daemon :app:testDebugUnitTest :app:lintDebug :app:assembleDebug` | pass; 352 unit tests, debug APK 7 479 000 B, lint report written; all artifact paths used by the two upload steps exist. Used the project-local `GRADLE_USER_HOME` (27 of 56 tasks came from the build cache; a cold runner takes longer) |

## release.yml (fake tag, nothing executed against GitHub)

| Job / step | Result |
|---|---|
| version: "Tag commit must be on main" | pass for a commit on `origin/main`; fails with the intended error for an unmerged commit |
| version: tag vs `Cargo.toml` | `v0.1.0` ok (`prerelease=false`); `v0.1.0-rc.1` with a `0.1.0-rc.1` Cargo version gives `prerelease=true`; `v0.1.1`, `0.1.0`, `v0.1`, `v0.1.0-` rejected |
| desktop (Linux x86_64): build with `--target`, Package, Installer | pass after a fix (below): `geekbridge-linux-x86_64.tar.gz` (all 9 binaries + README), `geekbridge-0.1.0-linux-x86_64-installer.tar.gz`, `geekbridge_0.1.0_amd64.deb` (control file and payload listed) |
| android: build unsigned APKs | pass (release APK `app-release-unsigned.apk` 6 353 525 B) |
| android: Sign APK, four scenarios with a throw-away EC keystore from `scripts/make-keystore.sh` | right pin: signed, v2+v3 verified, `signer_note` written; wrong pin: fails with the intended error; no pin: warning; no keystore + prerelease: debug-signed with warning; no keystore + stable: refuses |
| publish: `SHA256SUMS` | pass: lists all ten fake assets (tar.gz, zips, apk, deb, installer tarball, setup.exe, two dmgs). `compgen -G "geekbridge_*"` is what picks up the `.deb` |
| publish: `gh release create` arguments (`gh` replaced by a stub that prints) | same-repo prerelease / final, `RELEASE_REPO` set with and without `RELEASES_TOKEN`, malformed `RELEASE_REPO`: all as designed; draft first, then `gh release edit --draft=false` |

## Defects found and fixed

1. **Windows checkout would break every shell script.** There was no `.gitattributes`, and the Windows runner checks out with
   `core.autocrlf=true`, so `scripts/smoke-binaries.sh` (and the packaging scripts) would get CRLF and fail with `$'\r': command not found`.
   Added `.gitattributes` (`* text=auto eol=lf`, binary types listed). No tracked file currently has CRLF, so nothing is renormalised.
2. **Windows installer step (release.yml).** Git Bash rewrites the `/DAppVersion=...` switches for `ISCC.exe` into Windows paths.
   Added `MSYS2_ARG_CONV_EXCL: '*'` to that step. `choco install innosetup` is now pinned to `--version 6.7.1`, because the step
   hardcodes the Inno Setup 6 install path.
3. **`scripts/package-linux.sh` could not build the `.deb` when `--out` is relative**, which is exactly how `release.yml` calls it
   (`--out dist`): `ar` runs in another directory and failed with "No such file or directory". `--out` is now created and made absolute.
4. **cross-targets, Windows clippy**: `crates/bridge-service/src/tray/windows.rs` used `chunks_exact(4)`, which Rust 1.99's
   `clippy::chunks_exact_to_as_chunks` rejects under `-D warnings`. Now `as_chunks::<4>().0.iter()` (stable since 1.88 = the workspace `rust-version`).
5. **cross-targets, macOS**: `crates/bridge-service/tests/support/space.rs` called `libc::posix_fallocate`, which does not exist on macOS, so
   three integration-test targets did not compile (`cargo test` on the macOS runner would have failed too). The call is now Linux-only; other
   systems extend the file instead.
6. **supply-chain**: `THIRD_PARTY_NOTICES.md` was stale since AccessKit was enabled (451 vs 483 crates). Regenerated with `scripts/gen-notices.sh`.
7. Hardening: `timeout-minutes` on the release `version` (10) and `publish` (20) jobs, which had the 6 h default; `dbus` added to the perf job's
   apt line (it is preinstalled on `ubuntu-latest`, but `perf-check.sh` quietly drops the tray measurement without `dbus-daemon`).

## Open items

- **cargo-deny advisories (report-only job).** **Resolved 2026-11-02: see "cargo-deny triage" in docs/supply-chain.md; all four sections pass, ignores documented in deny.toml.** Original note: New since the 2026-11-02 review in `docs/supply-chain.md` / `deny.toml`:
  `RUSTSEC-2026-0194` and `RUSTSEC-2026-0195` (quick-xml 0.30 via zbus_xml <- atspi <- accesskit_unix <- egui-winit; fix needs
  quick-xml >= 0.41, which zbus_xml 4 does not allow) and `RUSTSEC-2024-0436` (`paste` unmaintained). Not ignored in `deny.toml`:
  that is a decision for the owner. The licence failures are only the 11 workspace crates having no licence yet
  (`docs/licence-decision.md`); all third-party crates pass. `docs/licence-decision.md` and `docs/supply-chain.md` still say 451 third-party crates (now 483).
- The action's Docker image uses Rust 1.85 for `cargo metadata`. Here cargo-deny ran with 1.99; if the image's cargo rejects a
  dependency manifest, the job fails at the very start (it is report-only).
- `dtolnay/rust-toolchain` is pinned to `stable`, so a runner will use whatever stable is current on the day (verified here with 1.99.1).
  A new clippy lint can turn `-D warnings` red without any code change; pin `toolchain: 1.99.1` if that becomes a nuisance.
- There is no `concurrency:` group (push + pull_request double-runs a branch with an open PR) and no Cargo cache; both only cost minutes.

## Unverifiable until GitHub runs it

- Everything on `windows-latest` and `macos-latest`: `cargo test`, the two smoke steps (continue-on-error), `bridge-calls-windows` /
  `bridge-calls-macos` release builds, and the whole Windows and macOS packaging in release.yml (`Compress-Archive`, Inno Setup via
  `choco`, `brew install librsvg`, `build-app-dmg.sh` with `hdiutil`/`iconutil`). Only type-check, clippy and `bash -n` were done. PowerShell
  syntax could not be parsed (`pwsh` is not installed), but the only steps on the default Windows shell are single `cargo` command lines.
- Actions behaviour itself: `GITHUB_OUTPUT`/`GITHUB_PATH` plumbing, artifact upload/download with `merge-multiple`, `needs.<job>.outputs.signer_note`
  passing, `gh release create --verify-tag --generate-notes --notes` on the runner's `gh`, creating the tag in a separate releases repo.
- `android-actions/setup-android` plus `sdkmanager` installs and a cold Gradle build on a runner (time and the 60-minute limit).
- Real `ubuntu-latest` package versions and the Docker-based cargo-deny run.
- The `RELEASE_REPO` / `RELEASES_TOKEN` path and the release-keystore secrets: only exercised with stubs and a throw-away key.
