# Release checklist

Steps for the maintainer. Nothing here is automated beyond `scripts/release.sh`, which never commits, tags or pushes, and
`scripts/release-local.sh`, which builds and verifies locally and publishes only when given `--publish` ([release-local.md](release-local.md)).

## GitHub Actions is optional (account billing)

> Every CI run so far (check.yml on each push) was refused by GitHub before starting a job: "recent account
> payments have failed or your spending limit needs to be increased". So the Windows, macOS and Android CI jobs,
> the perf budget job and the supply-chain job have **never executed**. Releases do not depend on Actions any more:
> build, verify and upload them from the maintainer's Linux machine with
> [`scripts/release-local.sh`](release-local.md) (`gh release create` works without Actions). That path produces the
> Linux and Android assets only; **Windows and macOS binaries still need Actions or a contributor with those
> machines** (there is an experimental zig cross-build of the Windows zip, see release-local.md).
>
> Note: **public repositories get free GitHub Actions minutes**. Making the repo public (see the next section) lets
> `release.yml` and `check.yml` run at no cost, which is also the way to get Windows and macOS builds. Until then,
> fix Billing & plans or use the local script. When Actions does run, re-run the latest workflow
> (`gh run rerun <id> --repo fspecii/geekbridge`) and update `docs/platform-status.md` with what actually fails.

## Decide repo visibility before the first release

> **Blocker.** `fspecii/geekbridge` is currently **private**. The update checkers (desktop `bridge-update`,
> Android `UpdateChecker`) call `https://api.github.com/repos/<repo>/releases/latest` **without credentials**,
> which GitHub answers with **404 for a private repository**. Nobody would ever be offered an update, and
> the apps would say "No public release found: the project repository may be private or has no releases yet."
> Pick one before tagging `v0.1.0`:

- [ ] **A. Make `fspecii/geekbridge` public.** Simplest; nothing else to configure; source, issues and releases
  are public. Irreversible in practice (forks/caches); run `scripts/public-preflight.sh` and follow [public-release-runbook.md](public-release-runbook.md) first.
- [ ] **B. Keep the source private, publish releases to a separate PUBLIC releases-only repo** (the usual
  pattern for closed-source apps), e.g. `fspecii/geekbridge-releases`:
  1. Create it public with one commit (a README), so a tag can be created on its default branch.
  2. In the private repo add the variable `RELEASE_REPO=fspecii/geekbridge-releases` and the secret
     `RELEASES_TOKEN` (fine-grained token limited to that repo, *Contents: read and write*).
  3. Build the shipped apps with the same repo, or they keep looking at the private one:
     `GEEKBRIDGE_RELEASE_REPO=fspecii/geekbridge-releases cargo build --release …` (desktop) and
     `-Pgb.releaseRepo=fspecii/geekbridge-releases` (Android). `release.yml` does not set these for you.
  Consequences: binaries and `SHA256SUMS` are public even though the source is not; release notes are not
  generated from commits; the tag exists in both repos; `RELEASES_TOKEN` expires and must be rotated.
- [ ] **C. Stay private (testers only).** Desktop testers may set `GEEKBRIDGE_UPDATE_TOKEN` (read-only
  fine-grained token, *Contents: read*) so the updater can see private releases; the token is memory-only,
  sent to `api.github.com` only, never logged. **Android has no token support on purpose**: a token inside an
  APK, or stored by the app, can be extracted by anyone holding the file. Android testers install APKs by hand.
  Real users get no updates under this option.

Testing a repo other than the built-in one: `GEEKBRIDGE_UPDATE_REPO=owner/name` (desktop service) or the
debug-build preference `debug_repo` on Android ([updates.md](updates.md#choosing-the-release-source)).

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
