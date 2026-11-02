# Contributing to GeekBridge

Thanks for helping. GeekBridge is free software under **GPL-3.0-or-later**; by contributing you agree that your
contribution is licensed under the same terms (see [LICENSE](LICENSE)). There is no CLA.

## Sign your commits (DCO)

Every commit must carry a `Signed-off-by` line, certifying the [Developer Certificate of Origin](https://developercertificate.org/):
you wrote the change or have the right to submit it under the project licence.

    git commit -s -m "Fix reconnect after sleep"

That adds `Signed-off-by: Your Name <you@example.com>` using your git identity. Do not paste code you cannot
relicense under GPL-3.0-or-later. In particular, do **not** copy or mechanically translate code from KDE Connect or any
other project: GeekBridge implements the KDE Connect protocol independently from its observed wire behaviour.

## Build and test

Start with `scripts/dev-setup.sh`: it checks Rust 1.88 or newer (`rust-version` in `Cargo.toml`; `rust-toolchain.toml` selects
stable for rustup users), the desktop system libraries, JDK 17 and an Android SDK, prints what is missing and installs
nothing unless you pass `--install`. Then `make help` lists the entry points. A [dev container](.devcontainer/devcontainer.json)
with the Linux toolchain is provided. Architecture overview: [ARCHITECTURE.md](ARCHITECTURE.md);
a walkthrough from a fresh clone: [docs/contributor-onboarding.md](docs/contributor-onboarding.md).

```sh
make check                         # = the three cargo commands below
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
make android                       # Android JVM unit tests + lint via ./gradlew (APK too with scripts/bootstrap-android.sh)
make perf                          # = scripts/perf-check.sh --build, idle service budget
make docs-links                    # relative links in the docs
make hygiene                       # SPDX headers + THIRD_PARTY_NOTICES.md current (run scripts/gen-notices.sh after changing Cargo.lock)
```

On the Android side, `./gradlew` (in `android/`) is the documented entry point. Dependencies are hash-verified
(`android/gradle/verification-metadata.xml`): when you add or bump one, regenerate it with
`./gradlew --write-verification-metadata sha256 help testDebugUnitTest lintDebug assembleDebug assembleRelease` and commit the diff.
`scripts/build-android.sh` builds the phone's native libraries and the debug APK with the project-local toolchain from
`scripts/bootstrap-android.sh`.

Cross-platform type-checks: `scripts/xcheck.sh`. Emulator end-to-end scripts are described in [docs/reports/e2e-findings.md](docs/reports/e2e-findings.md).

## Golden images

The desktop and Android screens are protected by golden-image tests: `make golden` (see `docs/golden-images.md`). If you
change the UI and it goes red, look at `dist/golden-diff/`; update with `scripts/golden-desktop.sh --update` /
`scripts/golden-android.sh --update` **only after a visual review** of every diff, and commit the new PNGs with the change.

## Translations and UI text

User-visible text lives in string resources (Android) and the catalogue `crates/bridge-apps/i18n/en.properties` (desktop), never
in a literal; tests and lint enforce it. To add a language, see `docs/translating.md`. Translations are GPL contributions.

## Code style

* Rust: `cargo fmt` defaults, clippy clean with `-D warnings`, no `unwrap()` on untrusted input, bounded parsing
  everywhere the network or the phone is involved. Match the surrounding comment density: explain why, not what.
* Kotlin: follow the existing files (plain Android views, no extra runtime dependencies without discussion).
* Every first-party source file starts with `SPDX-License-Identifier: GPL-3.0-or-later` (`scripts/add-spdx.sh` adds it).
* New dependencies must be compatible with GPL-3.0-or-later and pass `cargo deny check`; regenerate the notices.
* Privacy rules are strict: nothing from the phone is stored on the computer unless the user turned that feature
  on, and logs, diagnostics and tests must not contain real names, numbers, addresses or message text.
* Test fixtures must be synthetic or captured from your own throwaway setup, never real personal data or keys.

## Pull requests

Keep them small and focused, explain the user-visible effect, and say how you tested it (unit tests, emulator, real
phone). Update the docs and `CHANGELOG.md` when behaviour changes.

## Security

Do not open public issues for vulnerabilities. Follow [SECURITY.md](SECURITY.md).

## Conduct

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).
