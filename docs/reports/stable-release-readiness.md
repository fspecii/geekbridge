# Stable release readiness: v0.1.0

Prepared 2026-11-03. Nothing was published, tagged, pushed or uploaded. All signing used a throwaway keystore under
`~/.cache/gb-tmp`, no key is in the repository (`*.p12`, `*.pass` are git-ignored and the scripts refuse
paths inside a work tree).

## Result

| Area | State |
|---|---|
| `scripts/release-local.sh v0.1.0 --dry-run` (throwaway key, pin set) | fmt, clippy `-D warnings` and `cargo test --workspace` green (in a clean clone of HEAD plus these changes); Linux tarball, installer, `.deb`, Arch package and both APKs built; Android Gradle unit tests run. Found and fixed the defect below. |
| Keystore + pin path | `make-keystore.sh` creates a 0600 key and password file, never prints the password, refuses to overwrite or to write inside the repo, requires >= 25 years. `release-local.sh` refuses a stable tag with no keystore, no pin, a wrong pin or a malformed pin, and refuses `--prerelease` / suffix mismatches. All covered by `scripts/tests/release-readiness-test.sh`. |
| versionCode monotonicity | rc.1 = 10051, rc.2 = 10052, 0.1.0 = 10099, 0.1.1 = 10199, 0.2.0-rc.1 = 20051. Strictly increasing; `version-code.sh --self-test` green; Cargo version is not below any existing tag (tested). |
| CHANGELOG | `## [0.1.0]` is the section `release-local.sh` uses as release notes. The post-rc.1 items (What's new card, tray icon, Messages improvements) were sitting under `[Unreleased]` and would have been left out of the v0.1.0 notes; they are folded into `[0.1.0]` ("Also in this release") and `[Unreleased]` is empty. Two stale Known-limitations lines were corrected ("release workflow has not been run", "debug APK is debug-signed"; v0.1.0 is release-signed and the rc.1 uninstall caveat is stated). `gen-whats-new.sh --check` still passes. |
| `THIRD_PARTY_NOTICES.md` | `scripts/gen-notices.sh --check`: up to date. |
| Updater | Desktop (`bridge-update`) and Android (`UpdateModel`) skip `draft` / `prerelease` flags and any tag with a pre-release suffix, and offer only strictly greater semver and versionCode. New tests: a stable `v0.1.0` is offered to a `0.1.0-rc.1` install (Rust `mock_server.rs`, Kotlin `UpdateModelTest`); a later `v0.1.0-rc.2` is never offered. |
| `docs/release-checklist.md` | Was inaccurate for this release: it told the maintainer to run `scripts/release.sh`, which refuses because Cargo is already at 0.1.0. Added a "First stable release (v0.1.0)" section and the local-build signing inputs (pin file / `GB_KEYSTORE`). |
| `SECURITY.md` | Contact placeholder still present (owner action 3). |

## Defect found and fixed

`release-local.sh` would have failed at the signing step of the real release, after about 25 minutes of building:
`apksigner ... --ks-pass file:<pass>` died with "Failed to read Key ... end of file reached" because
`make-keystore.sh` writes the password without a trailing newline (so `gh secret set < file` stores it exactly) and
apksigner needs a newline-terminated line, reading consecutive lines when `--ks-pass` and `--key-pass` name the same
file. `keytool` accepted the file, so the pin check passed and hid the problem. Fix: `scripts/lib/signpass.sh`
(`gb_signing_pass_file`) writes a private 0600 temporary file with the password on two lines, `sign_apk` uses it and
deletes it right after signing. Regression test: the release-readiness test signs a zip with the file
`make-keystore.sh` writes.

## Not mine, observed

* `bridge-kde --test persistence` (3 file-lock tests, 10 s lock timeout) failed once while the machine had a load
  average of 40 to 50 from other jobs, and passes in 0.2 to 0.4 s otherwise (5 of 5 reruns). Flaky under heavy load, not a
  release defect; `RUST_TEST_THREADS=4` avoided it.
* The shared working tree contained other in-flight edits (calls work, rustfmt-dirty) during this task, so the dry run
  was done on a clean clone of HEAD with only the files below overlaid.
* `scripts/check-doc-links.sh` reports `docs/public-release-runbook.md` as git-ignored (pre-existing).
* `gh repo view` reports `fspecii/geekbridge` as PUBLIC already; the checklist's visibility blocker is therefore
  already resolved (option A) and can be ticked.

## Owner-only actions

1. **Create the real release key** outside the repository, once:
   `scripts/make-keystore.sh --out ~/secrets/geekbridge-release.p12 --password-file ~/secrets/geekbridge-release.pass`
   Back up both files in two separate offline places. Losing the key ends updates for everyone; a leak lets others
   ship updates.
2. **Record the pin**: `echo <64 hex digits> > ~/.config/geekbridge/android-signer-sha256` (or `ANDROID_SIGNER_SHA256`),
   and export `GB_KEYSTORE`, `GB_KEYSTORE_PASS_FILE`. A stable build is refused without them.
3. **Put a monitored security contact into `SECURITY.md`** (replace the `<!-- OWNER: ... -->` comment) and enable
   GitHub private vulnerability reporting (Settings, Code security).
4. **Finish the real-device test plan** on at least one phone and PC (nothing so far has been paired with a real phone;
   CHANGELOG known limitations say so). Update the `[0.1.0]` date if release day differs.
5. **Windows / macOS**: decide to ship without them (README says unsigned/unbuilt), or run Actions / a contributor.
   Code signing and notarisation are your accounts and money.
6. **Confirm trademark/name, privacy statement and support channel** (checklist items still open).
7. **rc.1 testers on Android**: rc.1 is debug-signed. It cannot update to the release-signed v0.1.0; uninstall first
   (app data is lost). Desktop rc.1 installs update normally.
8. **Release, in order** (each is yours to run): commit and push `main`; `scripts/tests/release-readiness-test.sh`;
   `scripts/release-local.sh v0.1.0 --dry-run` with the real key; then
   `scripts/release-local.sh v0.1.0 --tag --push-tag --publish` (no `--prerelease`). Check the release notes show the
   signer SHA-256 equal to step 1. Then update-check from rc.1 and from an older stable build.
9. **Do not run `scripts/release.sh 0.1.0`**: the version is already bumped and it refuses.

## Files changed

`CHANGELOG.md`, `docs/release-checklist.md`, `docs/reports/stable-release-readiness.md`, `scripts/release-local.sh`,
`scripts/lib/signpass.sh`, `scripts/tests/release-readiness-test.sh`, `crates/bridge-update/tests/mock_server.rs`,
`android/app/src/test/java/dev/geekbridge/android/UpdateModelTest.kt`.
