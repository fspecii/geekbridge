# Building GeekBridge

Everything is built with project-local tooling under the git-ignored `.tools/`
directory. No sudo, no system packages, no global `PATH`/config edits, and
`HOME` is never repurposed. Outputs go to the git-ignored `dist/`.

| Command | Result |
| --- | --- |
| `scripts/bootstrap-android.sh [--with-emulator]` | one-time toolchain install into `.tools/` |
| `scripts/build-android.sh [--skip-tests] [--skip-lint] [--skip-host-tests]` | `dist/android/geekbridge-debug.apk` |
| `scripts/build-desktop.sh [--locked]` | `dist/linux/geekbridge-{service,calls,messages,clipboard,agenda}` |
| `source scripts/android-env.sh` | puts the local toolchain on `PATH` for the current shell |
| `scripts/emulator-call.sh start\|stop\|status\|wait\|env\|state\|adb` | dedicated headless emulator (AVD `gbcall`) for emulated-call checks; see [emulated-calls.md](emulated-calls.md) |
| `scripts/emulate-call.sh fake\|emulator [--only S1,S4] [--json]` | emulated-call end-to-end scenarios S1-S7 on a private D-Bus bus; simulated signalling only, see [emulated-calls.md](emulated-calls.md) |

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
- `--with-emulator` additionally installs `emulator` and
  `system-images;android-36;google_apis;x86_64`. **That option has not been run or tested**
  (KVM is present, `/dev/kvm`). When this table was written no emulator had been booted; the emulator used for call-signalling checks is managed by `scripts/emulator-call.sh` (see [emulated-calls.md](emulated-calls.md) and [android-calls.md](android-calls.md)).

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
development, not a release/Play artifact.

## Desktop (Linux)

`scripts/build-desktop.sh` runs `cargo build --release -p bridge-service -p bridge-apps`
with the host (distro) cargo and installs five separate executables with distinct names
into `dist/linux/` plus `SHA256SUMS`. Only Linux x86_64 is built. No Windows or macOS
binaries exist or are implied.

## What was actually built and run (2026-11-01, this host)

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

What this does **not** establish: at build time the APK had not been installed on a device or emulator
(since then only the Calls card's call-state display was checked on an emulator, using simulated signalling; see
[android-calls.md](android-calls.md)), so JNI loading, the other UI and the other permissions remain unexercised. Build success says
nothing about phone pairing/transport, calls, or message delivery; those remain
incomplete and are owned elsewhere. The hashes above are of the tree as built and will
change on rebuild.


## Releases and versioning

**Single version source:** `[workspace.package] version` in the root `Cargo.toml`. Every Rust crate inherits it
(`env!("CARGO_PKG_VERSION")` is what the desktop update check compares), and `android/app/build.gradle.kts`
reads the same line for `versionName` and derives `versionCode = major*1_000_000 + minor*1_000 + patch`
(components above 2000/999/999 fail the build). A JVM test checks that `BuildConfig` matches `Cargo.toml`.

To release:

1. Bump `version` in the root `Cargo.toml` (e.g. `0.2.0`) and run `cargo check --workspace` so `Cargo.lock`
   follows; merge it.
2. Tag exactly that version with a leading `v` and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
   Pre-releases (`v0.2.0-rc.1`) are published as GitHub pre-releases and are never offered by the update check.
3. `.github/workflows/release.yml` (trigger: tag `v*`) fails fast if the tag does not equal the Cargo version,
   then builds `geekbridge-linux-x86_64.tar.gz`, `geekbridge-windows-x86_64.zip`,
   `geekbridge-macos-aarch64.zip`, `geekbridge-macos-x86_64.zip` (each holding `geekbridge-service`,
   `geekbridge-ctl` and the four app executables) and `geekbridge-android.apk`, writes `SHA256SUMS` over them
   and creates the GitHub Release with generated notes. The update checkers read those names and that file
   ([updates.md](updates.md)).
4. Android signing: if the repository secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`,
   `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` exist the release APK is aligned and signed with that keystore;
   without them the workflow publishes the **debug-signed** APK and prints a warning. Use one key for every
   release, or phones cannot update in place.

Windows and macOS binaries are unsigned (no Authenticode, no Developer ID/notarization), so those systems will
show their usual warnings. The workflow has not been run (no tag has been pushed).

## Cross-platform CI definition

`.github/workflows/check.yml` defines separate native Ubuntu, Windows, and macOS jobs for formatting, Clippy, workspace tests, and release executable builds. The Linux job requires its private D-Bus integration tests rather than silently skipping them. This workflow has been added locally; it has not been pushed or run, so it does not establish Windows/macOS build success. Android APK validation still uses the local build script and emulator work.


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
