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

Rust 1.88 or newer (see `rust-version` in `Cargo.toml`); the Android app needs the toolchain from `scripts/bootstrap-android.sh`.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
scripts/perf-check.sh --build      # idle service budget
scripts/build-android.sh           # Android native libs, unit tests, lint, debug APK
scripts/check-doc-links.sh         # relative links in the docs
scripts/add-spdx.sh --check        # every source file has its SPDX header
scripts/gen-notices.sh --check     # THIRD_PARTY_NOTICES.md is current (run without --check after changing Cargo.lock)
```

Cross-platform type-checks: `scripts/xcheck.sh`. Emulator end-to-end scripts are described in [docs/e2e-findings.md](docs/e2e-findings.md).

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
