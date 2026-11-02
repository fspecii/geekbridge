## What and why

<!-- User-visible effect in a sentence or two. Link the issue if there is one. -->

## How I tested it

<!-- Unit tests, `make check`, emulator, real phone (model + Android version), desktop OS. -->

## Checklist

- [ ] `make check` passes (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`)
- [ ] Android changes: `cd android && ./gradlew :app:testDebugUnitTest :app:lintDebug` passes
- [ ] Tests added or updated; fixtures are synthetic (no real names, numbers, addresses, message text or keys)
- [ ] Docs and `CHANGELOG.md` updated if behaviour changed; `make docs-links` is clean
- [ ] New first-party files start with `SPDX-License-Identifier: GPL-3.0-or-later` (`scripts/add-spdx.sh`)
- [ ] New or bumped dependencies are GPL-3.0-or-later compatible; `THIRD_PARTY_NOTICES.md` regenerated (`scripts/gen-notices.sh`), and for Android `android/gradle/verification-metadata.xml` updated
- [ ] No code copied or translated from KDE Connect or any other project that cannot be relicensed under GPL-3.0-or-later

## DCO

Every commit is signed off (`git commit -s`), certifying the [Developer Certificate of Origin](https://developercertificate.org/):
I wrote this change or have the right to submit it under GPL-3.0-or-later.

- [ ] All commits in this PR carry a `Signed-off-by:` line
