# Contributor onboarding: verified from a clean clone

This records a literal walk through [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md) and
`scripts/dev-setup.sh` on a scratch copy, so a stranger's first hour works. Re-run it (steps below) before a release.

## How it was verified (2026-11-02)

* `git clone` of the repository into an empty directory, then every non-ignored file of the working tree overlaid and
  committed in the copy (the release-candidate gate's "clean clone + working tree" method), so
  the not-yet-committed files are exercised. No `.tools/`, no `target/`, no `android/build`, no `jniLibs`.
* Commands ran under `env -i` with only `HOME` and `PATH=/usr/bin:/bin` (distro `rustc`/`cargo` 1.99.1, Arch), and a
  **fresh, empty `GRADLE_USER_HOME`**. The sandbox had network access, so the Gradle wrapper really downloaded
  Gradle 8.13 and every dependency with checksum verification on. The JDK 17 and Android SDK (platform 36) were
  supplied through `JAVA_HOME` / `ANDROID_HOME`, standing in for what a contributor installs themselves.

| Step | Command | Result |
|---|---|---|
| Toolchain check | `scripts/dev-setup.sh` | Rust, rustfmt, clippy, C toolchain, system libraries OK; with no JDK/SDK it exits 1 and prints the exact fix (`sudo pacman -S --needed jdk17-openjdk`, or `scripts/bootstrap-android.sh`) |
| Rust gate | `make check` | pass: fmt, clippy `-D warnings`, 1334 tests in 87 test binaries, 0 failures (6 min 42 s cold) |
| Android | `make android` (no `.tools/`, so it uses `./gradlew`) | pass: wrapper downloaded Gradle 8.13 and checked its SHA-256, dependency verification on, 381 unit tests, 0 failures, lint OK (2 min 24 s cold) |
| Desktop | `geekbridge-service --api-port 27380 --control-port 27381 --state-dir DIR ...`, `geekbridge-ctl --state-dir DIR status` / `diagnose`, `geekbridge-calls --status`, `geekbridge` window | service starts, status and the redacted diagnose report answer, `geekbridge-calls --status` reports "No phone found yet", the Home window stays open until killed |
| Docs | `make docs-links` | 0 broken links |

Not done in the sandbox: `make desktop` as a long-running session (it needs a person at the screen), the APK/native
library build (`scripts/build-android.sh`, needs `scripts/bootstrap-android.sh` first), and the dev container (see below).

## Problems found and fixed

1. **Dependency verification failed on a cold cache.** The first `verification-metadata.xml` was generated on a warm
   Gradle cache and from a tree with existing build outputs, so it lacked the BOM/parent POMs
   (`guava-parent`, `junit-bom`, `kotlinx-coroutines-bom`) and the `aapt2` linux jar that a clean machine fetches.
   Regenerated in the clean clone with an empty Gradle home. **Always regenerate in a clean tree with a fresh
   `GRADLE_USER_HOME`:** `rm -rf android/app/build android/build; GRADLE_USER_HOME=$(mktemp -d) ./gradlew --write-verification-metadata sha256 help testDebugUnitTest lintDebug assembleDebug assembleRelease`
   (`assembleRelease` needs the native libs in `android/app/src/main/jniLibs`).
2. **`make android` hid a Gradle failure** (exit status of the fallback branch was swallowed by a trailing `echo`). Fixed.
3. **README claimed every app supports `--status`.** `geekbridge` (Home) does not; corrected.
4. `dev-setup.sh` now says Android items are optional (`--desktop-only`) when it reports "Not ready".

## What a newcomer does

```sh
git clone https://github.com/fspecii/geekbridge && cd geekbridge
scripts/dev-setup.sh            # or --desktop-only; --install to let it install the gaps
make check                      # Rust
make android                    # Android unit tests + lint
```

Contributors on the maintainer's machine can keep using the offline project-local flow: `source scripts/android-env.sh`,
then `gradle --offline ...` in `android/` (dependency verification applies to it as well).

## Dev container

`.devcontainer/` (Ubuntu 24.04, Rust stable via rustup, JDK 17, Android SDK platform 36 + build-tools 36.0.0, the desktop
system libraries) is meant for VS Code / Codespaces: `make check` and `make android-test` run inside it. It has **not been
built**: `docker` is installed here but the daemon socket is not accessible to this user, and `podman` is absent. Only the JSON
and the Dockerfile syntax were reviewed. The NDK and Rust Android targets are deliberately absent (APK building stays
with `scripts/bootstrap-android.sh`). If the first build fails, fix the Dockerfile and record it here.
