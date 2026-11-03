# Reproducible builds and the SBOM

What a stranger can check about a GeekBridge release: that the Linux files can be rebuilt bit for bit from the tagged commit, which dependencies went in (SBOM), and which parts cannot be rebuilt identically yet. Audit of 2026-11-03; the measured results are in [reports/sbom-reproducibility.md](reports/sbom-reproducibility.md).

## Software bill of materials

`scripts/gen-sbom.sh [--out FILE]` writes a CycloneDX 1.5 JSON file (default `dist/geekbridge-sbom.cdx.json`). It needs only `python3` (3.11+) and `cargo`, no network once the crates are in the cargo cache, and none of `cargo-cyclonedx`, `cargo-sbom` or `syft` (none is installed here or a project dependency).

- **Rust:** every package of `cargo metadata --locked` (all platforms, so it includes Windows and macOS-only crates), with name, version, `pkg:cargo` purl, SPDX licence expression, the SHA-256 from `Cargo.lock` (the checksum of the `.crate` file on crates.io), and the dependency graph. The 13 workspace crates are listed without hash.
- **Android:** every component of `android/gradle/verification-metadata.xml` (`pkg:maven` purl, SHA-256 of the jar/aar, or the pom if there is no binary). Only `kotlin-stdlib` and `org.jetbrains:annotations` reach the APK (`scope: required`); the other ~340 are build tools and test dependencies (`scope: excluded`, property `geekbridge:role`). The metadata file has no dependency graph, so Android components carry no edges.
- **Deterministic:** components sorted by purl, keys sorted, no host paths; `metadata.timestamp` is `SOURCE_DATE_EPOCH` (default: committer time of `HEAD`); `serialNumber` is a UUIDv5 of the content. Two runs produce identical bytes.
- **Not in it:** the Rust toolchain, NDK, JDK and Gradle themselves (pinned elsewhere, see [supply-chain.md](supply-chain.md)), the fonts under `third_party/` (listed in `THIRD_PARTY_NOTICES.md`), and which crates the Linux binaries actually link (the SBOM is the superset the lock file allows).

It is a release asset named `geekbridge-sbom.cdx.json`, so `SHA256SUMS` (`sha256sum -- geekbridge-*`) covers it. `scripts/release-local.sh` generates and verifies it, `release.yml` generates it in the publish job, and `build-binaries.yml` adds it to its combined `SHA256SUMS`. Test: `scripts/tests/sbom-test.sh` (shape, completeness against `Cargo.lock` and the Gradle file, determinism, `SOURCE_DATE_EPOCH`, no host paths; runs in `check.yml`).

## Reproducing the Linux release files

```sh
scripts/repro-check.sh            # ~25 minutes, ~12 GB scratch under ~/.cache/gb-repro, deleted afterwards
```

It builds `HEAD` twice in two clean git worktrees at different paths, each with its own target directory, and compares every binary and archive. What makes them identical:

| Source of difference | Fix (in `scripts/lib/repro.sh`, used by `release-local.sh`, `package-linux.sh`, `release.yml`) |
|---|---|
| Absolute source, `~/.cargo` registry and sysroot paths in panic messages and `file!()` | `RUSTFLAGS=--remap-path-prefix=<src>=/build/geekbridge --remap-path-prefix=<cargo home>=...` and `-C strip=symbols` (the release profile already strips) |
| Crate versions floating | `cargo build --release --locked` (checksums come from `Cargo.lock`) |
| tar entry order, mtimes, uid/gid, gzip header name and time | `tar --sort=name --format=gnu --mtime=@$SOURCE_DATE_EPOCH --owner=0 --group=0 --numeric-owner` piped to `gzip -n -9` |
| `.deb`: `ar` member timestamps, tar mtimes | `ar rcD`, the same tar options for `control.tar.xz` and `data.tar.xz` |
| Time source | `SOURCE_DATE_EPOCH` = `git log -1 --format=%ct HEAD` |

Result (commit `ab4609f30`, rustc 1.99.1 Arch Linux, two paths, builds started minutes apart): all nine binaries, `geekbridge-linux-x86_64.tar.gz`, the installer tarball and the `.deb` are byte-identical. The previous packaging (`tar` without ordering and mtime options, plain `gzip`) differs between the two runs, which is why it was changed. Not measured: a build without the remap flag (it was always on in the audit), a different machine, or a different rustc/linker version. Identical output needs the same toolchain, the same C compiler (`ring` compiles C and assembly through `cc`) and a different distro compiler can still change bytes. Pin the toolchain when verifying (`rust-toolchain.toml` says `stable`, which moves).

## What is not reproducible (yet)

- **Android APK.** The release signature differs by design (key, and the v2/v3 schemes are over the whole file). Even unsigned, the APK is not checked: Gradle/AGP builds embed no timestamps in recent versions, but R8, `aapt2` and the NDK output were not compared, `zipalign`'d and signed files hold the signer's certificate, and `android/local.properties` and the SDK/NDK paths are machine-specific. Verify an APK by signer (`apksigner verify --print-certs`, pinned in the release notes) and by `SHA256SUMS`, not by rebuilding.
- **Gradle dependencies** are pinned by SHA-256 (`verification-metadata.xml`, the Gradle distribution checksum), so inputs are fixed even though the output is not compared.
- **Arch package** (`makepkg`): records build date and packager in `.PKGINFO` and `builddate`; not made reproducible here. `SOURCE_DATE_EPOCH` is honoured by `makepkg` for file mtimes but the package was not rebuilt twice.
- **Windows and macOS** zips/installers are built only on GitHub-hosted runners, with their own toolchains (MSVC, Apple SDK); not audited. The Windows/macOS zip tools (`Compress-Archive`, `zip`) do not sort or fix timestamps.
- **The release itself:** `SHA256SUMS` is not signed (no GPG/minisign/sigstore); publishing a detached signature is future work.

## Licence and advisory checks as a release asset (decision: document, do not attach)

`cargo deny check` (policy: `deny.toml`) is run in CI and by hand ([supply-chain.md](supply-chain.md)). Its output is not attached to releases: it depends on the advisory database of the day, so it would be a snapshot that goes stale and would need its own signing story, and the licence information is already in `THIRD_PARTY_NOTICES.md` (shipped inside every archive and the .deb) and in the SBOM's licence expressions. To publish a snapshot anyway, run `cargo deny --format json check > deny.json 2> deny.log` before the release and upload it by hand with `gh release upload` after `SHA256SUMS` is regenerated to include it (a name starting with `geekbridge-`, e.g. `geekbridge-cargo-deny.json`, is picked up by the existing `geekbridge-*` glob). `cargo-deny` is not installed on the maintainer machine at the time of writing.
