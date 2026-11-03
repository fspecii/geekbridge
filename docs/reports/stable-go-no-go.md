# v0.1.0 go / no-go audit

Audit date: 2026-11-03, against `origin/main` 3db630ea3 (plus the release-notes change that carries this file).
Method: read the reports, ran `gh run list` / `gh pr list` / `gh api` against `fspecii/geekbridge`, ran
`scripts/tests/release-readiness-test.sh` and `scripts/check-doc-links.sh`. Nothing was tagged, pushed to a tag, built for
release or published. Anything I could not check from here is marked **UNVERIFIED**; a green mark means I saw the evidence,
not that someone said so.

## Verdict: NO-GO today

Blocking, in order of how soon they bite:

1. The head is not settled: CI for 3db630ea3 was still running when I looked, and the last *completed* full run on
   `main` (35180f343) was red on macOS.
2. `CHANGELOG.md` `## [0.1.0]` does not contain what changed since rc.1 (it is under `[Unreleased]`), and still says no real
   phone was paired.
3. No release key exists on this machine and no signer pin is recorded (owner).
4. `SECURITY.md` has no contact (owner), and the release notes carry a visible placeholder for it.
5. The real-device test plan has not been completed (owner).

## Table

Status words: GREEN (evidence seen), AMBER (acceptable but with a stated gap), RED (blocks the release), OPEN-OWNER
(only the owner can do it), UNVERIFIED (not checked / cannot be checked here).

| # | Item | Status | Evidence | Owner / agent |
|---|---|---|---|---|
| 1 | CI green on a settled head, Linux | AMBER | Last completed full run 35180f343 (run 41684224810): `Rust / ubuntu-latest`, Android, supply-chain, perf, cross-target type-check all success. Head 3db630ea3 (run 41689495677) was `in_progress`. | agent: re-run `gh run list` after it finishes |
| 2 | CI green on Windows and macOS on a settled head | RED | Run 41684224810: `Rust / windows-latest` success, `Rust / macos-latest` **failure** (`phone_feed::tests::follows_params_shows_action_results_and_reports_a_down_service`, a timing flake on the macOS runner). The fix is in 3db630ea3, whose run had not finished. Three earlier runs on this stretch of `main` were cancelled by newer pushes, so no head has yet had a full green cycle. | agent: confirm run 41689495677 is green on both, and stays so on the next push |
| 3 | Emulator end-to-end workflow | AMBER (non-blocking by design) | Manual `Emulator end-to-end` runs 41680837634, 41678569248, 41675114736 failed; 41686018738 was still running. The workflow is not a release gate; the e2e suites passed locally on 2026-11-03 ([regression-2026-11-03](regression-2026-11-03.md)). `e2e-android` has a known `file_d2p` failure on the runner. | agent |
| 4 | Security reviews closed | AMBER | Reviews 1, 2, 5, 6 and the blackbox review are written with fixes and tests ([security-review](security-review.md), [-2](security-review-2.md), [-5](security-review-5.md), [-6](security-review-6.md), [blackbox](blackbox-security.md)). Review 5 and 6 each list **residual risks that were deliberately not changed** (for 6: EXIF/GPS on Android 8-9 full pictures, unverified content when saving a photo, scratch file lives up to 10 minutes). Review 5 item 1 (dial without a phone prompt) was later addressed by [followups-security-5](followups-security-5.md). No review covers the post-rc.1 `goodbye`/port-hint packets or the photos work on a real phone. | agent for any further review; owner accepts the residuals |
| 5 | Code review of post-rc.1 changes | GREEN | [code-review-4](code-review-4.md): two defects fixed (heartbeat marker regression, `--purge-data` target), rest checked. The review predates the later photos and goodbye commits. | agent |
| 6 | Soak | AMBER | [soak-2026-11-03](soak-2026-11-03.md) 67 min PASS; [soak-4h](soak-4h-2026-11-03.md) 4 h: no linear leak, but **the driver's verdict was FAIL** on 151 control-API poll timeouts (unexplained, first 90 min, none after 2.5 h) and a decelerating anonymous-memory creep (watch item). Soak ran a stand-in phone, not a real one; no multi-day real use. The soak predates 3db630ea3 and the photos code. | agent: record load/`/proc` state on a timeout, rerun; owner: live use |
| 7 | Privacy review | AMBER | No dedicated privacy review exists. Evidence that the claim holds: README "Privacy and security in one paragraph", [user-guide](../user-guide.md) Privacy section, [updates.md](../updates.md) network table (the update check is the only outbound call), per-feature consent in the security reviews. The release notes repeat the claim; nobody re-audited every outbound connection since photos/notifications/dial were added. | agent could do a short outbound-traffic audit; owner signs off the wording |
| 8 | Documentation | GREEN | `scripts/check-doc-links.sh`: 0 broken links; docs index and README updated through 2026-11-03; release notes written ([release-notes-v0.1.0](../release-notes-v0.1.0.md)). The README feature table says "not yet" for every real-phone column, which matches the notes. | agent |
| 9 | CHANGELOG matches the release | RED | `## [0.1.0]` (dated 2026-11-02) is the section `release-local.sh` and the in-app "What's new" card use, but photos, notification mirroring, battery, doctor, dial, why-disconnected, goodbye and both security reviews are still under `## [Unreleased]`, and the `[0.1.0]` Known limitations still say "No real phone has been paired". `--notes-file` fixes the GitHub release text only, not the What's new card (generated by `scripts/gen-whats-new.sh`). | agent can fold `[Unreleased]` into `[0.1.0]`, regenerate what's-new, set the date; owner confirms |
| 10 | Licence | GREEN | GPL-3.0-or-later: `LICENSE`, `Cargo.toml`, SPDX headers (`scripts/add-spdx.sh --check`), [licence-decision](../licence-decision.md); no GPL-incompatible dependency ([supply-chain-audit-2](supply-chain-audit-2.md)). | done |
| 11 | Third-party notices | GREEN | `scripts/gen-notices.sh --check` (run by `release-readiness-test.sh`) clean. | done |
| 12 | SBOM | AMBER / not done | None is generated or shipped. [supply-chain.md](../supply-chain.md) lists `cargo cyclonedx` as "optional" at release time. `THIRD_PARTY_NOTICES.md`, `Cargo.lock` and Gradle `verification-metadata.xml` are the substitutes. | owner decides; agent can add `cargo cyclonedx` to the local release script |
| 13 | Release script plumbing | GREEN | `scripts/tests/release-readiness-test.sh` passes with the new `--notes-file` / `--print-notes` checks (relative path, `=` form, missing, empty, no value, default still the CHANGELOG). Earlier rehearsals: [stable-release-readiness](stable-release-readiness.md) (dry run at ed4070a1f, all assets, signing, install sandbox 61/61). That full rehearsal is **stale**: many commits since; a new full `--dry-run` with the real key is still to be run. | agent: new full dry run with `--notes-file docs/release-notes-v0.1.0.md` once the key exists |
| 14 | Android signing key and pin | OPEN-OWNER | `~/secrets` and `~/.config/geekbridge/android-signer-sha256` do not exist on this machine. `release-local.sh v0.1.0` refuses without them. Steps: [stable-release-readiness](stable-release-readiness.md) owner actions 1-2; back up the key in two offline places. | owner |
| 15 | Real-device test | OPEN-OWNER | One phone (Galaxy Z Fold5, SM-F946B) has been paired and reports exist for pairing, link stability and SMS ([real-device-first-pairing](real-device-first-pairing.md), [-link-stability](real-device-link-stability.md), [-sms](real-device-sms.md)); the fixes after them are **UNVERIFIED on hardware**. The 12-step plan ([real-device-test-plan](../real-device-test-plan.md), kit: [real-device-kit](real-device-kit.md)) is not recorded as finished. | owner |
| 16 | Windows / macOS stance | AMBER | Decision needed and recorded in the notes: no prebuilt binaries, unsigned, CI-tested only ([platform-status](../platform-status.md), [windows-macos-review](windows-macos-review.md), [windows-macos-artifacts](windows-macos-artifacts.md)). Code signing and notarisation are not done. Known unfixed gaps are listed in the review. | owner confirms "ship without" |
| 17 | `SECURITY.md` contact | OPEN-OWNER | Placeholder comment still present. GitHub private vulnerability reporting **is enabled** (`gh api .../private-vulnerability-reporting` returns `enabled: true`), so a reporting route exists. The release notes contain `[OWNER: add the monitored security contact ...]`; it must be filled or removed before publishing. | owner |
| 18 | Dependabot PRs | OPEN-OWNER | Nine open (#1-#9: actions group, base64, zbus 5 to 6, zbus_xml, eframe 0.31 to 0.37, windows-future, Kotlin, AGP 9, Gradle 9). [dependabot-triage](dependabot-triage.md) (2026-11-02) recommends merging #1 and #3 and closing or hand-migrating the major bumps; its CI results were stale then. Nothing merged or closed. Not release blockers, but decide before tagging so the tag is not built from a head with pending security bumps (**UNVERIFIED** whether any carries an advisory; `cargo-deny` job is green). | owner |
| 19 | Repository visibility | GREEN | `gh repo view`: PUBLIC. The updater's unauthenticated release query can therefore work. The real GitHub endpoint has never been tried for a stable release (UNVERIFIED until the first publish, then check from rc.1). | done / owner after publish |
| 20 | Upgrade path from rc.1 | AMBER | Updater tests: stable offered to an rc.1 install, later rc never offered ([stable-release-readiness](stable-release-readiness.md)). Android rc.1 (debug-signed) cannot update in place; stated in the notes. The real update from the public rc.1 release is **UNVERIFIED**. | owner after publish |
| 21 | Name / trademark, support channel | OPEN-OWNER | Checklist items still unticked in [release-checklist](../release-checklist.md). | owner |
| 22 | Release notes | GREEN (draft) | [release-notes-v0.1.0](../release-notes-v0.1.0.md) usable via `scripts/release-local.sh v0.1.0 --notes-file docs/release-notes-v0.1.0.md`. Contains the security-contact placeholder (item 17) and no date; the script appends the signer fingerprint line. | agent / owner to review wording |

## What I would do next, in order

1. Wait for run 41689495677; if both Windows and macOS are green, make no further code pushes to `main` until the tag.
2. Fold `[Unreleased]` into `[0.1.0]` (and drop the stale "no real phone" line), regenerate what's-new, then re-run
   `scripts/tests/release-readiness-test.sh`.
3. Owner: create the key and pin, fill the security contact, finish the real-device plan, decide on the Dependabot PRs.
4. Agent: a full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` with the real key.
5. Only then `scripts/release-local.sh v0.1.0 --tag --push-tag --publish --notes-file docs/release-notes-v0.1.0.md`
   (the owner's command), followed by an update check from an rc.1 install.
