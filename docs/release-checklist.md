# Release checklist

Steps for the maintainer. Nothing here is automated beyond `scripts/release.sh`, which never commits, tags or pushes, and
`scripts/release-local.sh`, which builds and verifies locally and publishes only when given `--publish` ([release-local.md](release-local.md)).

## GitHub Actions

The CI workflows in `.github/workflows` run on GitHub-hosted runners. Releases do not depend on them:
build, verify and upload from a Linux machine with [`scripts/release-local.sh`](release-local.md) (`gh release create` works
without Actions). Windows and macOS binaries need Actions or a contributor with those machines.

## Before the first public release: decisions the owner must make

Decided 2026-11-02 (see [licence-decision.md](licence-decision.md)): GPL-3.0-or-later, public repository, releases built locally.

- [x] **Licence for the code.** GPL-3.0-or-later: `LICENSE`, `license` in `[workspace.package]`, README, About screen, packages, SPDX headers (`scripts/add-spdx.sh --check`), dependency compatibility gate in `scripts/gen-notices.sh`.
- [x] **Relationship to KDE Connect.** Independent, protocol-compatible implementation (no upstream code); nominative use of the name only. Recorded in [licence-decision.md](licence-decision.md).
- [x] **Licences of bundled assets**: icons and screenshots are our own; fonts are OFL/MIT only (`third_party/fonts`); `THIRD_PARTY_NOTICES.md` is generated and shipped.
- [ ] **Android:** applicationId `dev.geekbridge.android`, release signing key. Procedure (exact commands in
  [builds.md](builds.md#first-release-android-signing-key)):
  - [ ] `scripts/make-keystore.sh --out <outside-repo>.p12 --password-file <outside-repo>.pass` run; fingerprint noted.
  - [ ] Keystore **and** password file backed up in two separate offline places (losing them ends updates for everyone).
  - [ ] Secrets set by you: `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`;
        variable `ANDROID_SIGNER_SHA256` set to the fingerprint (the release fails if another key signs).
  - [ ] `scripts/version-code.sh` prints the expected versionCode for the version in `Cargo.toml` (must exceed the last release).
  - [ ] After the run: release notes show the signer SHA-256 and it equals the one noted above.
  - [ ] Whether to publish on Google Play / F-Droid later.
- [ ] **Code signing** for Windows (Authenticode / MSIX) and macOS (Developer ID, notarisation). Until then the
  README must keep saying the binaries are unsigned.
- [ ] **Trademark / name**: confirm "GeekBridge" and the `dev.geekbridge` identifiers are yours to use.
- [ ] **Security contact** in `SECURITY.md` (replace the comment) and enable GitHub private vulnerability reporting.
- [ ] **Privacy statement**: the app has no accounts or servers; the only network call besides the LAN is the
  GitHub update check. Say so in the README and store listings.
- [ ] **Support channel** (issues only, discussions, email) and whether to ask for `diagnose` output in the issue template.

## Per release

1. Finish [real-device-test-plan.md](real-device-test-plan.md) on at least one phone and one PC; attach the
   `geekbridge-ctl diagnose` output and the Android report to the release notes' test record.
2. Update `CHANGELOG.md` under `## [Unreleased]`.
3. Run `scripts/release.sh X.Y.Z` (clean tree, `cargo fmt`/`clippy`/`test`, bumps `Cargo.toml` + `Cargo.lock` +
   `CHANGELOG.md`). Use `--dry-run` first.
4. Review the diff and run the printed commands yourself: commit, push `main`, then
   `git tag -a vX.Y.Z -m "GeekBridge vX.Y.Z"` and `git push origin vX.Y.Z`. The tag must equal the `Cargo.toml`
   version and be reachable from `main`; the release workflow checks both.
5. Build and publish: either let the pushed tag trigger the workflow (needs working Actions), or build on this
   machine with `scripts/release-local.sh vX.Y.Z --publish` (add `--tag --push-tag` to tag and push in the same run,
   `--prerelease` for `-rc.N`; [release-local.md](release-local.md)). Do `--dry-run` first.
6. Verify `SHA256SUMS` against the artefacts (the local script does this before uploading) and run **update check +
   install** (test plan section 10) from the previous version.
