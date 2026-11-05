# Building GeekBridge

Everything is built with project-local tooling under the git-ignored `.tools/`
directory. No sudo, no system packages, no global `PATH`/config edits, and
`HOME` is never repurposed. Outputs go to the git-ignored `dist/`.

| Command | Result |
| --- | --- |
| `scripts/bootstrap-android.sh [--with-emulator]` | one-time toolchain install into `.tools/` |
| `scripts/build-android.sh [--skip-tests] [--skip-lint] [--skip-host-tests]` | `dist/android/geekbridge-debug.apk` |
| `scripts/build-desktop.sh [--locked]` | `dist/linux/` with the nine binaries listed in `scripts/release-binaries.txt` (`geekbridge-agenda` is the Calendar executable) |
| `source scripts/android-env.sh` | puts the local toolchain on `PATH` for the current shell |
| `scripts/emulator-call.sh start\|stop\|status\|wait\|env\|state\|adb` | dedicated headless emulator (AVD `gbcall`) for emulated-call checks; see [emulated-calls.md](emulated-calls.md) |
| `scripts/emulate-call.sh fake\|emulator [--only S1,S4] [--json]` | emulated-call end-to-end scenarios S1-S8 on a private D-Bus bus; simulated signalling only, see [emulated-calls.md](emulated-calls.md) |

## Toolchain (pinned, installed 2026-11-01)

| Component | Version | Location under `.tools/` | Provenance / integrity |
| --- | --- | --- | --- |
| JDK | Eclipse Temurin 17.0.21+9 | `jdk/jdk-17.0.21+9` | Adoptium API; SHA-256 `8b9b611d…dfe8711` pinned |
| Gradle | 8.13 | `gradle/gradle-8.13` | services.gradle.org; SHA-256 `20f1b117…0aed78` pinned |
| rustup | 1.29.2 | `cargo/bin` (`RUSTUP_HOME=rustup`, `CARGO_HOME=cargo`) | static.rust-lang.org archive; SHA-256 `5be191e8…ec24f4` pinned |
| Rust | 1.99.1 + `aarch64-linux-android`, `x86_64-linux-android` | `rustup/toolchains/1.99.1-…` | rustup verifies manifest hashes. Needed because the distro rustc has no Android std; `/usr/bin` is untouched |
| Android cmdline-tools | 23.0 (build 16111833) | `android-sdk/cmdline-tools/latest` | dl.google.com; SHA-1 `e025545c…ec5f60` (published in `repository2-3.xml`) pinned; SHA-256 observed `0877a1d0…cfa8` |
| Platform | android-36 (rev 2) | `android-sdk/platforms/android-36` | installed by `sdkmanager` |
| Build-tools | 36.0.0 | `android-sdk/build-tools/36.0.0` | installed by `sdkmanager` |
| Platform-tools (adb) | 37.0.1 | `android-sdk/platform-tools` | installed by `sdkmanager` |
| NDK | r30 stable, 30.0.16248370 | `android-sdk/ndk/30.0.16248370` | installed by `sdkmanager` |

Notes:

- cmdline-tools 23.0's `sdkmanager` is a deprecated shim that downloads
  Google's "Android CLI" into `.tools/android-user/` (version 1.1.16717390 at
  install time) and delegates to it. That helper binary is fetched from
  Google and is not hash-pinned by us; the SDK packages it installs are
  checked against Google's repository manifest. The CLI states it collects
  usage metrics; nothing here sets or disables that.
- Android SDK licences were accepted non-interactively, for the project-local SDK only
  (`.tools/android-sdk/licenses`).
- `ANDROID_USER_HOME`, `ANDROID_AVD_HOME`, `GRADLE_USER_HOME` all live in `.tools/`.
- Total `.tools/` size is about 5.8 GB. Remove it with `rm -rf .tools` to uninstall.
- `--with-emulator` additionally installs the SDK packages `emulator` and
  `system-images;android-36;google_apis;x86_64`. Both are now present under `.tools/android-sdk/`
  (Android Emulator 37.2.13; KVM, `/dev/kvm`, is usable). `.tools/` is now much larger than the 5.8 GB above
  (about 18 GB at last look, including the emulator, system image and AVDs). Not verified here: that the
  emulator packages came from this exact flag rather than a manual `sdkmanager` call, and that `.tools/` can be
  rebuilt from scratch, because no script in the repository creates the AVD (`gbcall` lives under
  `.tools/android-user/avd/`) and the inner launcher `.tools/gbcall/start.sh` that `emulator-call.sh` runs is
  inside the git-ignored `.tools/`, not in `scripts/`.

## Emulator for call-signalling checks

Verified facts, measured on 2026-11-01 (details and commands in
[emulated-calls.md](emulated-calls.md)):

- AVD `gbcall` (pixel_6, API 36 `google_apis` x86_64, 2 GB RAM, 4 cores), console port 5584, serial
  `emulator-5584`, dedicated adb server on port 16037 (never the default 5037).
- Launch flags (in `.tools/gbcall/start.sh`): `-no-window -no-audio -no-snapshot -no-boot-anim -gpu
  swiftshader_indirect -no-metrics -verbose`, with a private `TMPDIR` under `.tools/gbcall/tmp`. Boot to
  `sys.boot_completed=1` took about 50 to 195 s depending on host load; the script waits up to 420 s per attempt
  and retries up to 3 times.
- **Segfault fix.** Started from inside an agent's process tree, the emulator crashed with SIGSEGV 1 to 4 minutes
  after boot: host tooling port-probes localhost ports of processes in that tree, and the modem's random localhost
  port mishandles the repeated connect/disconnect. `scripts/emulator-call.sh start` therefore launches it in its own
  transient systemd user unit (`systemd-run --user --unit=gbcall-emulator --collect`), which produced zero probe
  lines in more than 25 minutes on two emulators. Without a systemd user session it falls back to `setsid nohup`
  with a warning, and that fallback is exposed to the same crash (the same crash was observed for an emulator
  started directly from an agent shell).
- `scripts/emulator-call.sh stop` only signals the PID it recorded; the status file is `.tools/gbcall/STATUS`
  (`booting` / `ready` / `failed: <reason>`).
- Scope: simulated cellular call signalling only. No Bluetooth, HFP, SCO audio or real phone is involved. The
  full pipeline is in [emulated-calls.md](emulated-calls.md); the Android app's call-state display was checked on
  this emulator ([android-calls.md](android-calls.md)).

Unverified: the crash cause is established from log patterns and core dumps, while "host tooling probes the port"
is an inference; behaviour on hosts without KVM, other emulator or image versions, and the systemd fallback path
has not been tested.

## Android

`scripts/build-android.sh` does, in order:

1. `cargo test -p bridge-android` on the host (skip: `--skip-host-tests`).
2. `cargo build --release -p bridge-android --target {aarch64,x86_64}-linux-android`
   using the NDK r30 `*-linux-android26-clang` linkers and
   `-Wl,-z,max-page-size=16384`. Cargo output goes to `target/android/` so it does
   not collide with the distro-rustc desktop build in `target/`.
3. Strips with the NDK `llvm-strip` (AGP 8.13.2 reports "Unable to strip" on NDK r30
   output) and copies `libgeekbridge_android.so` to
   `android/app/src/main/jniLibs/{arm64-v8a,x86_64}/`. Checks ELF machine type,
   that every LOAD segment is aligned to at least 16 KiB, and that the JNI symbol
   `Java_dev_geekbridge_android_NativeBridge_nativeRequest` is exported.
4. Writes the ignored `android/local.properties` and runs
   `gradle assembleDebug testDebugUnitTest lintDebug` (`--skip-tests`, `--skip-lint`).
5. Copies the APK to `dist/android/geekbridge-debug.apk` and verifies it contains both
   native libs (byte-identical to the staged copies, ELF-checked again),
   `zipalign -c -P 16`, `apksigner verify`, and via `aapt2 dump badging`: application
   ID `dev.geekbridge.android`, minSdk 26, targetSdk 36.

The APK is **debug-signed** with the local debug keystore: fine for sideloading during
development, not a release/Play artifact. Release signing is done only by the release workflow
([Releases and versioning](#releases-and-versioning)); no keystore lives in the repository (`*.jks`, `*.p12`,
`*.keystore` are git-ignored).

## Desktop (Linux)

`scripts/build-desktop.sh` runs `cargo build --release -p bridge-service -p bridge-apps`
with the host (distro) cargo and installs the nine binaries named in `scripts/release-binaries.txt` (the entry window
`geekbridge`, the service, the `geekbridge-ctl` helper and the Calls, Messages, Clipboard, Calendar (`geekbridge-agenda`),
Contacts and Share executables) into `dist/linux/` plus `SHA256SUMS`. The same list drives `scripts/package-linux.sh` and
`release.yml`; `scripts/check-binary-list.sh` fails if any of them drifts. Only Linux x86_64 is built. No Windows or macOS
binaries exist or are implied.

## What was actually built and run (2026-11-01, this host; historical record)

The table and hashes below describe the first build on 2026-11-01 and are kept as a record; they are not the current
binaries (the desktop has since gained Contacts, Calendar, Share and the entry window). Use the current
`dist/linux/SHA256SUMS`.


Commands: `scripts/bootstrap-android.sh`, `scripts/build-android.sh`,
`scripts/build-desktop.sh`.

Android (final run, exit 0): `cargo test -p bridge-android` passed (0 failures); both
Android targets compiled; Gradle `assembleDebug testDebugUnitTest lintDebug` succeeded;
all APK verifications above passed. APK badging: `native-code: 'arm64-v8a' 'x86_64'`.

| Artifact | SHA-256 |
| --- | --- |
| `dist/android/geekbridge-debug.apk` (2.8 MB) | `dc84f3728fcda0fc23d87192370145aeb886ba7da03be9c88f6c16f17a614f40` |
| `jniLibs/arm64-v8a/libgeekbridge_android.so` | `4f2dc42ca544609553403854f5f173f03add76cf6fc77acd869f2d6a2a426094` |
| `jniLibs/x86_64/libgeekbridge_android.so` | `3797de8fe03e42f3197eb466c3b1c642dc7ccbc3f1b836c3e64336272e6e8725` |

Linux release binaries (`dist/linux/`, built from the tree at this time; they change as
sources change):

| Binary | SHA-256 |
| --- | --- |
| `geekbridge-service` | `ea8e73145e0dd988b7360a3a31858caee8ac45d462a68c9d78097cb38b18889a` |
| `geekbridge-calls` | `b5f54f6b7b984bed543b8de75cf642e6af00442812da7d7ae896c32da9188ddc` |
| `geekbridge-messages` | `3bbabfdbb2fe09b4892fc9d457035dd1418403c5606ec238be17c46f8109ace1` |
| `geekbridge-clipboard` | `4a6b8941a0e829f837ab69c5e10f073bc45e1a297f9db01bfb17ebd44df05ec2` |
| `geekbridge-agenda` | `30313026598fded50c4321f77bce3b49b3943513794b2fbd46daf58381908055` |

What this does **not** establish: at the time of this first build the APK had not been installed anywhere. It has since
been installed and exercised on an Android 16 emulator (pairing, transfers, SMS, calendar, call answering, self-update;
see [e2e-findings.md](reports/e2e-findings.md)); no physical phone has been used. Build success alone says nothing about
phone pairing, calls, or message delivery. The hashes above are of the tree as built and will
change on rebuild.


## Releases and versioning

**Single version source:** `[workspace.package] version` in the root `Cargo.toml`. Every Rust crate inherits it
(`env!("CARGO_PKG_VERSION")` is what the desktop update check compares), and `android/app/build.gradle.kts`
reads the same line for `versionName` and derives `versionCode` from it (below). A JVM test checks that `BuildConfig`
matches `Cargo.toml`; `scripts/version-code.sh` prints the same numbers for CI and `--self-test`s the formula.

### Android version code

Android rejects an update whose `versionCode` is not strictly greater than the installed one, so the code is
derived from the version, never edited by hand (`build.gradle.kts`, `AppVersion.toVersionCode` in `UpdateModel.kt`
and `scripts/version-code.sh` implement one formula; tests in `UpdateModelTest.versionCodeFormula` and
`scripts/version-code.sh --self-test`):

    versionCode = (major*10000 + minor*100 + patch) * 100 + slot
    slot = 99 for a final release; for a prerelease: alpha.N = 0+N, beta.N = 25+N, rc.N = 50+N  (N capped at 24)

| version | versionCode |
|---|---|
| 0.1.0 | 10099 |
| 0.1.1 | 10199 |
| 0.2.0-rc.1 | 20051 |
| 0.2.0 | 20099 |
| 1.0.0 | 1000099 |

**Debug builds are different** (the dev update channel, [updates.md](updates.md#dev-channel)): `versionCode = 20000 + git rev-list --count HEAD`
(`scripts/version-code.sh --dev`), set for the `debug` variant only in `build.gradle.kts`, so each new main build is a
strictly higher code and the Dev channel can compare them. Release codes stay on the formula above. Without git (a source
archive) the count is 0; `-Pgb.commitCount=N` sets it, `-Pgb.formulaVersionCode=1` gives debug builds the formula code
(used by `scripts/e2e/update-install.sh`). CI's debug-APK check compares with `version-code.sh --dev` and therefore needs
the full history (`fetch-depth: 0`).

Limits (the build fails beyond them): major <= 214, minor <= 99, patch <= 99. A release candidate therefore always
sorts below its final, and every release in semver order has a larger code. Never reuse a version after a tag was
pushed. (Before the first public release the formula was `major*1_000_000 + ...`; no APK with those codes was ever
published, so there is nothing to migrate.)

**Releases are built locally; GitHub Actions is optional.** The maintainer has no Actions budget, so the supported
path is `scripts/release-local.sh` (checks, Linux tarball/`.deb`/installer/Arch package, signed Android APK,
`SHA256SUMS`; full procedure in [release-local.md](release-local.md)); it only tags, pushes or publishes when asked.
The Android release key is created once with `scripts/make-keystore.sh` and kept outside the repository. Windows and macOS assets need those systems and
are not part of a local Linux release. `.github/workflows/release.yml` remains as an optional, manually started
(`workflow_dispatch`) equivalent; pushing a tag does not start it. (Public repositories get free standard runners
for `check.yml`, so CI on pushes costs nothing once the repository is public.)

To release locally:

1. Bump `version` in `Cargo.toml`, run `cargo check --workspace`, update `CHANGELOG.md`, commit.
2. `scripts/release-local.sh vX.Y.Z --dry-run`, then the real run; see [release-local.md](release-local.md).

The numbered list below describes what the optional workflow does when it is started by hand.

To release with the optional workflow:

1. Bump `version` in the root `Cargo.toml` (e.g. `0.2.0`) and run `cargo check --workspace` so `Cargo.lock`
   follows; merge it.
2. Tag exactly that version with a leading `v` and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
   Pre-releases (`v0.2.0-rc.1`) are published as GitHub pre-releases and are never offered by the update check.
3. `.github/workflows/release.yml` (trigger: manual `workflow_dispatch` on the tag ref) fails fast if the tag does not equal the Cargo version,
   then builds `geekbridge-linux-x86_64.tar.gz`, `geekbridge-windows-x86_64.zip`,
   `geekbridge-macos-aarch64.zip`, `geekbridge-macos-x86_64.zip` (each holding `geekbridge-service`,
   `geekbridge-ctl`, the entry window and the app executables) and `geekbridge-android.apk`, writes `SHA256SUMS` over them
   and creates the GitHub Release with generated notes. The update checkers read those names and that file
   ([updates.md](updates.md)).
4. Android signing: the job aligns the release APK and signs it with `apksigner` using the secrets
   `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` (v1 + v2 + v3
   schemes), then runs `apksigner verify --print-certs`, requires v2 and v3 to verify and compares the signer
   SHA-256 with the repository **variable** `ANDROID_SIGNER_SHA256`, which is required (64 hex characters) when the release
   keystore is used: a missing pin or a mismatch fails the release. The signer
   fingerprint is appended to the release notes. **A stable tag without the secrets fails** (a debug-signed APK could
   never be updated over); only prereleases fall back to the debug key, with a warning. See
   [First release: Android signing key](#first-release-android-signing-key).

5. **Publishing to a different repository** (e.g. a public releases-only repo while the source is private): set the
   repository variable `RELEASE_REPO` (`owner/name`) and the secret `RELEASES_TOKEN` (fine-grained token, only that
   repo, *Contents: read and write*). Without `RELEASE_REPO` the release goes to the same repository with the job
   token, as before. With it, `publish` uses `RELEASES_TOKEN`, creates the tag there (the repo needs one commit)
   and skips `--verify-tag`/generated notes. The apps read asset URLs from the API response (`browser_download_url`),
   so they follow the release to wherever it is hosted; what they must know at build time is *which repo to ask*:
   `GEEKBRIDGE_RELEASE_REPO=owner/name` for the Rust build and `-Pgb.releaseRepo=owner/name` for Gradle
   (default `fspecii/geekbridge`). Decision guide: [release-checklist.md](release-checklist.md#decide-repo-visibility-before-the-first-release).

### First release: Android signing key

Do this once, before the first stable tag. One key signs every release forever.

1. Create the keystore outside the repository (needs a JDK; `source scripts/android-env.sh` provides one here):

       scripts/make-keystore.sh --out ~/secrets/geekbridge-release.p12 --password-file ~/secrets/geekbridge-release.pass

   It creates a PKCS12 keystore (RSA 4096, 30 years; `--keyalg ec` for P-384), writes a random 32-character password
   only to the file you chose (mode 0600, never printed), refuses to overwrite either file or to write inside a git
   work tree, prints the signer SHA-256 and the exact `gh` commands. It does not run them.
2. **Back up the keystore and the password file** to two separate offline places. Losing them means no phone can ever
   update GeekBridge again.
3. Run the printed commands yourself: `gh secret set ANDROID_KEYSTORE_B64 / ANDROID_KEYSTORE_PASSWORD /
   ANDROID_KEY_PASSWORD / ANDROID_KEY_ALIAS` and `gh variable set ANDROID_SIGNER_SHA256` (lowercase hex, colons
   optional) to pin the key.
4. Bump `version` in `Cargo.toml`, merge, tag `vX.Y.Z`, push the tag. In the run log, the Android job prints
   `apksigner verify --print-certs`; the release notes end with the signer fingerprint.
5. Sanity check on a phone: install the first APK, publish a higher version, use Settings > Updates. The app compares
   the signer of the downloaded file with its own and only then offers Install.

Signature schemes: the APK carries v1 (JAR), v2 and v3 signatures. minSdk is 26, so Android 8+ verifies v2/v3 and
v1 is not required by the platform; it stays enabled for older tooling. Debug builds are signed with the local debug
key, so a debug build can never update to a release (and vice versa); the app says so instead of offering Install
(uninstall once, then install the release APK).

**Key rotation.** v3 signing supports a signing-certificate lineage: the new APK carries proof that the new key
descends from the old one, and phones with the old key accept it. To rotate: create the new keystore with
`scripts/make-keystore.sh`, then create a lineage with the SDK's `apksigner rotate --out lineage --old-signer --ks
old.p12 --new-signer --ks new.p12` and sign with `apksigner sign --ks new.p12 --lineage lineage ...` (extend
`release.yml` for that release only, and update `ANDROID_SIGNER_SHA256` to the new key). Lineage support needs
Android 9 (API 28)+; on Android 8 an update signed with a rotated key is refused, so keep that in mind before rotating.
The in-app check accepts a rotated update when the lineage contains the installed certificate. Rotation is a
recovery tool (suspected leak), not routine: keep the old key until all users have moved.

Windows and macOS binaries are unsigned (no Authenticode, no Developer ID/notarization), so those systems will
show their usual warnings. The workflow has not been run (no tag has been pushed).

## Cross-platform CI definition

`.github/workflows/check.yml` defines native Ubuntu, Windows, and macOS jobs for formatting, Clippy, workspace tests (which include `bridge-update`, `bridge-calls-windows`, `bridge-calls-macos` and `bridge-phone-sim`) and release executable builds, plus the Windows/macOS call backends on their own OS. The Linux job requires its private D-Bus integration tests, runs `scripts/emulate-call.sh fake` and the `scripts/perf-check.sh` idle budget. A separate **Android** job builds `bridge-android` for arm64 and x86_64 with the NDK, then runs `:app:testDebugUnitTest :app:lintDebug :app:assembleDebug`. The workflow has run on GitHub (history: [reports/ci-first-run.md](reports/ci-first-run.md), [reports/windows-ci.md](reports/windows-ci.md)); the Windows and macOS jobs establish build and test success on those runners only, not behaviour on a real PC or Mac. The same checks pass locally on Linux (`scripts/build-android.sh`). `build-binaries.yml` (manual) builds Windows and macOS binaries as workflow artifacts only ([reports/windows-macos-artifacts.md](reports/windows-macos-artifacts.md)).

The desktop design pass refreshed `dist/linux/` and its verified SHA256SUMS. `dist/geekbridge-linux-x86_64.tar.gz` was repackaged from those binaries. Screenshots in `dist/screenshots/desktop/` are actual release-window captures; the earlier artifact hashes in this historical build record apply to the earlier tree. Use the current `dist/linux/SHA256SUMS` for the current release snapshot.


## Update-checking validation

Run locally (Linux x86_64, this host): `cargo test -p bridge-update` (semver ordering/parsing, asset selection,
checksum parsing, hash/size mismatch, malformed/oversized/chunked/truncated responses, ETag/304, rate-limit
back-off, redirect policy, skip/disable persistence, scheduler, all against a loopback mock HTTP server),
the IPC/UI unit tests in `bridge-apps`, and Android `:app:testDebugUnitTest` (version parsing, `Cargo.toml`
single-source check, release evaluation, and `UpdateChecker` against a local `HttpServer`).

Not exercised: a real request to api.github.com or a real TLS handshake (the TLS client config builds, but
tests use plain HTTP on loopback), the Windows/macOS compile of the new crate, the release workflow, a real
APK install through the package installer, and the desktop banner as rendered on screen.


## Installers and the entry app

`geekbridge` (crate `bridge-apps`, `home_app.rs`) is the single entry window: pairing, clipboard sync and shortcut,
a file drop target (hands files to `geekbridge-share`), update banner/settings and launchers. Packaging, additive to the archives:

| Script | Output |
| --- | --- |
| `scripts/package-linux.sh [--bin-dir D] [--out D]` | `.deb` and `geekbridge-<v>-linux-x86_64-installer.tar.gz` (install.sh); needs `rsvg-convert` |
| `packaging/arch/build-local.sh` | Arch `.pkg.tar.zst` built by makepkg in a scratch dir; see [packaging-linux.md](packaging-linux.md) |
| `packaging/windows/geekbridge.iss` (Inno Setup, CI only) | `geekbridge-<v>-windows-x86_64-setup.exe`, per-user, Run key autostart |
| `packaging/macos/build-app-dmg.sh` (macOS only) | `GeekBridge.app` + `geekbridge-<v>-macos-<arch>.dmg`, LaunchAgent written on first launch |

`release.yml` runs them after "Package". Note: the update selector ranks `.dmg` above `.zip` on macOS and ties the Linux
installer tarball with the archive (name order picks the installer tarball); both are covered by `SHA256SUMS`.
No AppImage is produced. See [user-guide.md](user-guide.md).

## Scratch space and big test files

`/tmp` is often a small tmpfs with a shared user quota, so the suite is written to need almost none.

- **Generate, don't store.** Big payloads (`crates/bridge-service/tests/chaos_transfer.rs`) are
  streamed from a deterministic generator (`Pattern`, byte `i` is a pure function of `i`) and
  checked by recomputing it; a file that only has to *exist* is sparse (`File::set_len`). Only what
  a receiver writes touches the disk.
- **Sizes.** The default (CI) mode moves at most 48 MB per transfer; `GEEKBRIDGE_CHAOS=soak` (what
  `scripts/soak.sh` sets; `GEEKBRIDGE_SOAK=1` is an alias) uses the 200 MB variants.
- **Where.** `GEEKBRIDGE_TEST_TMP` picks the directory for test scratch (default
  `std::env::temp_dir()`, i.e. `$TMPDIR`); helpers are in `crates/bridge-service/tests/support/space.rs`.
  Scripts honour `GB_TMP` (default `$TMPDIR`, else `/tmp`), which also exports `TMPDIR` and
  `GEEKBRIDGE_TEST_TMP` to children (`scripts/lib-tmp.sh`, `gb_mktemp NAME [-d]` creates
  `$GB_TMP/gb-NAME.XXXXXX`). Scripts remove their scratch on exit (`trap ... EXIT`).
- **Skip, don't fail, on a full disk.** A test that needs real room first tries to reserve it
  (`posix_fallocate`); if that fails with ENOSPC / EDQUOT (28 / 122, `StorageFull`,
  `QuotaExceeded`) it prints `SKIPPED: not enough scratch space ...` and returns. A transfer that
  fails with `io_error` is only excused when that same reservation also fails afterwards; with room
  to spare it is a product failure and the test fails.
- **Cleaning up.** `scripts/clean-tmp.sh` lists (dry run) or, with `--apply`, removes GeekBridge's
  own leftovers: `gb-<name>.XXXXXX`, `gb-test-*`, `geekbridge-emulate-call.*`,
  `gb-callui-xdg-<uid>`, owned by you and untouched for `--older-than` minutes (default 60). `--emu`
  also empties `.tools/*/tmp` when no emulator is running. Other projects' files are never matched.
- Tight on `/tmp`? `GB_TMP=$HOME/.cache/gb-tmp scripts/soak.sh` or
  `TMPDIR=$HOME/gbtmp cargo test` moves everything.
