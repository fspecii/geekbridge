# SBOM and reproducibility audit (2026-11-03)

Base: `origin/main` at `ab4609f30`. Method and details: [../reproducible-builds.md](../reproducible-builds.md).

## 1. SBOM
- No `cargo-cyclonedx`, `cargo-sbom`, `syft` or `cargo-deny` installed, so `scripts/gen-sbom.sh` (wrapper) + `scripts/gen-sbom.py` build CycloneDX 1.5 JSON from `cargo metadata --locked`, the `Cargo.lock` checksums and `android/gradle/verification-metadata.xml`.
- Output: 495 Rust packages (13 workspace crates among them, with SPDX licences, SHA-256, dependency graph) and 343 Android components (2 runtime, rest build/test, SHA-256 each); about 690 KB. Two runs are byte-identical; timestamp is `SOURCE_DATE_EPOCH` (else HEAD commit time), serial is a UUIDv5 of the content.
- Wired in as `geekbridge-sbom.cdx.json`: `scripts/release-local.sh` (generates, verifies shape/version/presence in `SHA256SUMS`, mentions it in the notes), `release.yml` (publish job checks out, generates, required before `SHA256SUMS`), `build-binaries.yml` (combined `SHA256SUMS` includes it, uploaded as artifact). The `geekbridge-*` glob already covered the name.
- Test: `scripts/tests/sbom-test.sh` (5 checks: generation, identical re-run, only timestamp/serial change with `SOURCE_DATE_EPOCH`, no host paths, schema/ordering/completeness against Cargo.lock and the Gradle file). Passes; added to `check.yml`.

## 2. Reproducibility
`scripts/repro-check.sh` built `ab4609f30` twice (`cargo build --release --locked -p bridge-apps -p bridge-service`, jobs 6) in two clean worktrees at different paths with separate target dirs, `SOURCE_DATE_EPOCH` from the commit, `--remap-path-prefix` (source, cargo home, sysroot) and `-C strip=symbols`.

| Artefact | Identical across the two builds |
|---|---|
| 9 binaries (`geekbridge`, `-service`, `-ctl`, `-calls`, `-messages`, `-clipboard`, `-agenda`, `-contacts`, `-share`) | yes |
| `geekbridge-linux-x86_64.tar.gz` (new sorted/fixed-mtime/`gzip -n` packing) | yes |
| installer tarball, `.deb` (with `ar rcD`, fixed tar mtimes) | yes |
| tarball packed the old way (no `--sort`/`--mtime`, plain gzip) | no (fixed) |

Cheap fixes made: `scripts/lib/repro.sh` (RUSTFLAGS, `gb_repro_tar_gz`), used by `release-local.sh`, `package-linux.sh` (also `ar rcD`, tar mtimes) and the Linux leg of `release.yml`; `release-local.sh` exports `SOURCE_DATE_EPOCH` from the commit.
Remaining and untested: APK (signing, R8/aapt2/NDK not compared), Arch package (`builddate`), Windows/macOS runner builds, a build without the remap flag, a second machine or toolchain version, signed `SHA256SUMS`. See the doc.

## 3. cargo-deny / licence output
Documented only (not attached): reasons and how to attach manually are in the last section of `reproducible-builds.md`. `cargo-deny` is not installed here, so it was not run.

## Checks (clean worktree, rustc 1.99.1)
- `cargo fmt --all -- --check`: exit 0
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 0
- `cargo test --workspace --locked --no-fail-fast`: exit 0
- `scripts/tests/sbom-test.sh`: exit 0; workflow YAML parses; `bash -n` on the edited scripts.
Disk: 54 GB free at start (limit 20 GB); build trees and scratch deleted afterwards.
