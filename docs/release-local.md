# Building and publishing a release on this machine

GitHub Actions needs billing the project does not have, so releases are built **locally on the Linux (Arch/Omarchy)
maintainer machine** and uploaded with `gh release create`, which works without Actions. One command does it:

    scripts/release-local.sh vX.Y.Z [--prerelease] [--dry-run] [--publish] [--skip-tests] [--tag] [--push-tag] [--notes-file FILE]

`.github/workflows/release.yml` still exists and still works the day Actions does (for example once the repository is
public: public repositories get free Actions minutes). The local script copies its asset names, `SHA256SUMS` format,
signing steps and draft-then-publish order, so the in-app updaters cannot tell the two apart.

## What you get, and what you do not

| Asset | Built here |
|---|---|
| `geekbridge-linux-x86_64.tar.gz` (all nine binaries; what the desktop updater reads) | yes |
| `geekbridge-<ver>-linux-x86_64-installer.tar.gz`, `geekbridge_<ver>_amd64.deb` | yes (`scripts/package-linux.sh`) |
| `geekbridge-<ver>-1-x86_64.pkg.tar.zst` (Arch) | yes (`packaging/arch/build-local.sh`), `--skip-arch` to omit |
| `geekbridge-android.apk` (arm64-v8a + x86_64, R8, release-signed; what the Android updater reads) | yes |
| `geekbridge-android-arm64.apk` (phone only, about half the size) | yes |
| `geekbridge-sbom.cdx.json` (CycloneDX 1.5 SBOM: Rust workspace + Android build dependencies; `scripts/gen-sbom.sh`) | yes, deterministic; covered by `SHA256SUMS` |
| `SHA256SUMS` | yes, same `sha256sum -- geekbridge-* geekbridge_*` as `release.yml` |
| `geekbridge-windows-x86_64.zip`, `geekbridge-macos-*.zip`, `.dmg`, Inno Setup installer | **no** |
| `geekbridge-windows-x86_64-gnu-experimental.zip` | only with `--with-windows`; see below |

**Windows and macOS: this machine cannot honestly produce them.** The Windows MSVC build wants the Windows SDK and
`link.exe`; the macOS builds want Apple's SDK and frameworks, whose licence limits them to Apple hardware; the Windows
installer needs Inno Setup and the `.dmg` needs `hdiutil`. None of those exist on Linux without root or without
downloading things we are not allowed to redistribute. Nothing built here for those systems could be signed, and nothing
has ever been run on them. So the release notes and the README say **"Linux and Android builds available; Windows and
macOS: build from source."** They get prebuilt binaries when GitHub Actions runs (free for a public repository) or a
contributor with those machines runs `.github/workflows/release.yml`'s steps.

### The experimental Windows cross-build (`--with-windows`)

Investigated on this machine, no root needed: `which zig x86_64-w64-mingw32-gcc wine cargo-xwin` finds none of them, but
a `pip install ziglang` into a scratch venv gives a working `zig cc`, `zig ar` and `zig dlltool`. With the
`x86_64-pc-windows-gnu` rust-std (added to the project-local rustup in `.tools/windows-check`) and a small linker
wrapper (the script writes it to `$GB_RELEASE_DIR/zig-wrappers/`), `cargo build --release --target
x86_64-pc-windows-gnu -p bridge-apps -p bridge-service` links all nine executables; `file` reports
`PE32+ executable for MS Windows, x86-64` (console for `geekbridge-ctl`/`geekbridge-service`, GUI for the apps) and the
imports are system DLLs only. The step takes about a minute. Limits, stated plainly:

- It is the **GNU ABI** target, not the MSVC one `release.yml` ships, and **nothing has been run**: there is no Wine
  and no Windows machine here. The Windows audio and tray code paths are exactly the ones that were never run on Windows.
- Unsigned (SmartScreen will warn). No installer.
- cargo-xwin (MSVC) would work only after downloading Microsoft's SDK under their licence; not attempted.
- Therefore the zip is named `geekbridge-windows-x86_64-gnu-experimental.zip`, **not** `geekbridge-windows-x86_64.zip`:
  the in-app updater must never offer an untested build to Windows users. It is opt-in and the release notes say so.

macOS has no such route (no SDK), and was not attempted.

## One-time setup

1. Toolchain: `scripts/bootstrap-android.sh` (project-local JDK, Gradle, SDK, NDK, rustup). For the Arch package:
   `makepkg`, `rsvg-convert`, `zstd` (Arch base-devel and `librsvg`). Needs `gh auth login` for publishing.
2. **Release keystore** (stable tags refuse to build without it), outside the repository, backed up twice:

       scripts/make-keystore.sh --out ~/secrets/geekbridge-release.p12 --password-file ~/secrets/geekbridge-release.pass

   It prints the signer SHA-256. Store it as the pin:

       mkdir -p ~/.config/geekbridge && echo <the-64-hex-digits> > ~/.config/geekbridge/android-signer-sha256

3. Put the keystore variables in your shell profile or a private env file (never in the repository):

       export GB_KEYSTORE=~/secrets/geekbridge-release.p12
       export GB_KEYSTORE_PASS_FILE=~/secrets/geekbridge-release.pass
       export GB_KEY_ALIAS=geekbridge-release          # default; only if you chose another alias

   `ANDROID_SIGNER_SHA256` in the environment overrides the pin file.

## Procedure

1. Finish the real-device test plan; update `CHANGELOG.md` under `## [Unreleased]`.
2. `scripts/release.sh X.Y.Z` bumps `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and the Arch `pkgver`. Review, **commit**
   and push `main`. (`release-local.sh` never bumps and never commits.)
3. Rehearse: `scripts/release-local.sh vX.Y.Z --dry-run` (see below). Optional but cheap insurance.
4. Build and verify, publishing nothing:

       scripts/release-local.sh vX.Y.Z               # or vX.Y.Z-rc.1 --prerelease
       ls ~/.cache/gb-release/vX.Y.Z/out

   It ends by printing the exact `git tag` / `git push` / `gh release` commands.
5. Publish. Either run those printed commands, or in one go (this rebuilds everything):

       scripts/release-local.sh vX.Y.Z --tag --push-tag --publish

   `--publish` is the only way anything reaches GitHub. It checks that the tag exists on the remote, that no release
   with that name exists, creates the release as a **draft**, uploads every asset, then undrafts (the updaters read
   `releases/latest` and must never see a release without `SHA256SUMS`), and prints the URL. The release page is
   created with `--verify-tag`, so a tag is never created implicitly by `gh`.
6. On a phone and a PC: update check and install from the previous version (test plan section 10).

### What the script checks, in order

Preflight (before the long builds): tag shape; `--prerelease` agrees with the tag (a `-` means prerelease, as in
`release.yml`); at least `GB_MIN_FREE_GB` (10) of free disk; **on `main`, tree clean, `Cargo.toml` version equals the
tag** (reusing `release.sh`'s version-source rule; it tells you to run `release.sh`, never edits), HEAD contained in
`origin/main`; a `## [X.Y.Z]` section in `CHANGELOG.md` (required for stable); `cargo fmt --check`, `clippy -D
warnings`, `cargo test --workspace` unless `--skip-tests`; the Android tools; the **keystore**: stable tags without
`GB_KEYSTORE` are refused, a prerelease without it builds **debug-signed with a loud warning**; the keystore's
certificate must equal the pinned SHA-256 (required for stable, a warning for prerelease).

Verification (before any tag or upload): `SHA256SUMS` checks out; the tarball is extracted and
the first line of `geekbridge-ctl --version` must be exactly `geekbridge-ctl <version>` (the rest is the About text); installer tarball has `install.sh`; the `.deb`
control `Version` matches and all nine binaries are listed; the Arch package `.PKGINFO` `pkgver` matches and lists all
nine binaries; both APKs pass `apksigner verify` with v2 and v3, the signer equals the pin, `aapt2 dump badging` shows
the right `versionName` and the `versionCode` from `scripts/version-code.sh` (formula in
[builds.md](builds.md#android-version-code)), minSdk 26, and `scripts/check-release-isolation.sh` (no debug receivers, cleartext, debug permissions or debug-only classes). Release notes that still contain an `[OWNER: ...]` placeholder are refused (a warning for `--dry-run`). Any failure aborts before a tag is made or `gh` is called.

To check that the updater accepts what the script produced, run the real desktop updater against the finished assets
(a local mock "GitHub", nothing leaves the machine; the test is skipped without the variable):

```sh
GB_RELEASE_OUT=~/.cache/gb-release/vX.Y.Z/out GB_RELEASE_TAG=vX.Y.Z cargo test -p bridge-update --test release_assets -- --nocapture
```

It checks every `SHA256SUMS` line against its file, the asset the updater picks for Linux (the plain archive, never the
installer tarball, `.deb`, Arch package, APKs or SBOM), the API `digest` cross-check, the download and re-hash from an
rc.1 install, and "nothing newer" from the same version. Keep `GB_RELEASE_DIR` short (under about 25 characters): the
test suites bind unix sockets below it and the path limit is 107 bytes.

### Tag rules

The script never creates a tag on its own. `--tag` creates it locally, annotated, message `GeekBridge vX.Y.Z`, no
trailers and no attribution lines (and with `tag.gpgSign=false`; sign by hand if you want signed tags). `--push-tag`
pushes `refs/tags/vX.Y.Z` to `origin` (or `GB_REMOTE`). If the tag exists already it must point at HEAD.
Tag operations happen only after verification succeeded.

## `--dry-run`

`scripts/release-local.sh v0.0.1-dryrun --prerelease --dry-run --skip-tests` exercises everything except publishing:
it copies the working tree (tracked and untracked, not ignored) to `$GB_RELEASE_DIR/v0.0.1-dryrun/src`, patches the
version of **the copy** (Cargo.toml, Cargo.lock, PKGBUILD) to the tag, and builds and verifies all assets from it. A dirty
tree, a branch other than `main` and a version mismatch are reported as `a real release would refuse here` warnings
instead of errors, and the final summary counts them. `gh` and the mutating `git` calls are printing stubs, so the real
repository and GitHub are never touched; `--dry-run` wins over `--publish`. Keystore and signer-pin rules are **not**
relaxed: a stable dry run without a keystore fails like a real one.

## Parallelism, time and disk

Cargo and Gradle run with `GB_JOBS` jobs (default 3). Unlimited parallelism once exhausted 31 GB of RAM while the
fat-LTO release profile linked nine binaries at the same time; raise `GB_JOBS` only if you watch `free -g`.

All scratch lives in `$GB_RELEASE_DIR` (default `~/.cache/gb-release`), one directory per tag, never in the repository:

    ~/.cache/gb-release/vX.Y.Z/out/              the finished assets (and SHA256SUMS)
                          /release-notes.md      CHANGELOG section (or --notes-file) + signer line
                          /logs/<step>.log       one log per step (tail shown on failure)
                          /target-desktop, target-android, target-windows, src, stage, tmp, arch-out   build trees
                          /r8-mapping-*.txt      R8 mappings (to decode release crash traces; not uploaded)
    ~/.cache/gb-release/zig-venv, zig-wrappers   only with --with-windows

Measured on this machine (24 threads, 31 GB RAM) with `GB_JOBS=3`, tests skipped, for the rehearsal `v0.0.1-dryrun`
(`--prerelease --dry-run --skip-tests --with-windows`):

| Step | Time | Scratch it leaves |
|---|---|---|
| Linux release build (nine binaries, fat LTO) + tarball, installer, `.deb` | 4 to 8 min | 1.0 GB |
| Arch package (`makepkg`, a second full release build) | 9 to 11 min | 2.9 GB in `tmp/` (cargo home + makepkg tree) |
| Android: two native libs, R8, two signed APKs | about 11 min cold, 3 min with warm target dir | 0.6 GB |
| Windows zig cross-build (`--with-windows`) | 3 to 4 min (plus a one-time `pip install ziglang`) | 0.7 GB |
| Verification, `SHA256SUMS` | under 1 min | |
| `cargo fmt` + `clippy` + `test --workspace` (not in that table) | add 10 to 20 min, uses `<repo>/target` | the existing `target/` |
| **Total** | **about 30 to 40 min** | **about 5.5 GB scratch, 123 MB of finished assets** |

Keep at least 10 GB free (`GB_MIN_FREE_GB`); `--prune` gives the scratch back. Cargo's and Gradle's own caches
(`.tools/cargo`, `.tools/gradle-home`) are shared with the normal builds and not counted.

The rehearsal tag cannot be `v0.0.0-dryrun`: Android's `versionCode` for 0.0.0 is 0, which Gradle rejects (the script
says so up front). Use `v0.0.1-dryrun` or higher.

Cleanup: `--prune` removes the build trees after a successful run and keeps `out/`. By hand:
`rm -rf ~/.cache/gb-release/vX.Y.Z/{target-*,src,stage,tmp,arch-out,apk}`, or `rm -rf ~/.cache/gb-release/vX.Y.Z`
for everything. `scripts/clean-tmp.sh` removes the `gb-*` leavings of our other scripts in `$TMPDIR`; the release script
keeps its own scratch under `$GB_RELEASE_DIR` (it points `TMPDIR` there), so `clean-tmp.sh` has nothing of it to clean.

## Environment

| Variable | Meaning | Default |
|---|---|---|
| `GB_JOBS` | parallel jobs for cargo and Gradle | 3 |
| `GB_RELEASE_DIR` | scratch and output root | `~/.cache/gb-release` |
| `GB_KEYSTORE`, `GB_KEYSTORE_PASS_FILE`, `GB_KEY_ALIAS` | release key (PKCS12; one password for store and key) | none, `geekbridge-release` |
| `ANDROID_SIGNER_SHA256`, `GB_SIGNER_FILE` | pinned signer fingerprint (hex, colons optional) | `~/.config/geekbridge/android-signer-sha256` |
| `GB_RELEASE_REPO` | `owner/name` to publish to; also passed to Gradle as `-Pgb.releaseRepo` | the `origin` remote |
| `GB_REMOTE` | git remote | `origin` |
| `GB_MIN_FREE_GB` | refuse to start below this much free disk | 10 |
| `GB_CHECK_TARGET_DIR` | cargo target dir for fmt/clippy/test | `<repo>/target` |
| `GB_ZIG_VERSION` | `ziglang` wheel for `--with-windows` | 0.17.0 |

## Failure modes

- *"working tree is not clean" / "Cargo.toml version is X"*: commit, or run `scripts/release.sh`; this script will not.
- *"keystore signer ... does not match the pinned ..."*: wrong keystore. Do not "fix" it by editing the pin unless you
  know the key rotated deliberately ([builds.md](builds.md#first-release-android-signing-key)).
- *step failed*: the tail of `logs/<step>.log` is printed; fix and rerun. Nothing was tagged or uploaded.
- Publishing failed halfway: the release is still a **draft** (invisible to the updaters). Fix the cause, delete the
  draft (`gh release delete vX.Y.Z --repo owner/name`; add `--cleanup-tag` only if you want the tag gone) and rerun.
- A published version can never be reused (Android `versionCode`); bump and release again.
