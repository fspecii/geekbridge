# v0.1.0 go / no-go audit

Audit date: 2026-11-04, refreshed against `origin/main` 84dd37545 (previous audit: 2026-11-03 at 3db630ea3).
Method: `gh run list` / `gh run view` / `gh pr list` / `gh api` against `fspecii/geekbridge`, `cargo fmt --check`,
`scripts/tests/release-readiness-test.sh`, `scripts/check-doc-links.sh`, `scripts/gen-whats-new.sh --check`, and reading the
reports named below. Nothing was tagged, released, published or merged. Anything not checkable from here is marked
**UNVERIFIED**; the perf and size budgets were read from `scripts/perf-check.sh`, not re-measured.

## Verdict: still NO-GO

The code side moved a lot (call-chain review, security reviews 7 and 8, Wi-Fi call audio, firewall doctor, parity audit 3), but
the head has not had a settled green CI cycle since 09:25 UTC on 2026-11-04, and the owner-only items are all still open.

## Ordered blockers

### Needs the user

1. **Release signing key and signer pin.** `~/secrets` and `~/.config/geekbridge/android-signer-sha256` do not exist.
   `scripts/release-local.sh v0.1.0` refuses without them. Back the key up in two offline places.
   ([stable-release-readiness](stable-release-readiness.md), owner actions 1-2.)
2. **Real-device test.** One phone (SM-F946B) was paired; everything after is unverified on hardware: photos, goodbye,
   notifications, privacy switches, QR scan with the in-app camera, Bluetooth call audio (needs the dongle), Wi-Fi call audio
   (debug only, Samsung may block it). Plan: [real-device-test-plan](../real-device-test-plan.md),
   [morning-test-checklist](morning-test-checklist.md); its build is c08893f6a, so rebuild first.
3. **`SECURITY.md` contact** (placeholder comment at line 8) and the `[OWNER: ...]` placeholder in
   [release-notes-v0.1.0](../release-notes-v0.1.0.md). Private vulnerability reporting is enabled, so a route exists.
4. **GitHub settings.** The repository has **no environments, no secrets and no variables**. Security review 7 (residual 3)
   asks for environments restricted to `main` that hold the dev and release keystores, plus the `DEV_SIGNER_SHA256`
   variable; until then `release.yml` / dev signing cannot run in CI (releases are made locally, so this blocks only the
   dev channel from CI). Branch protection requires no status checks (decide whether to require the Rust jobs).
5. **Dependabot decision.** Eight open PRs (#1, #2, #4-#9). #3 (base64) is now closed. [dependabot-triage-2](dependabot-triage-2.md)
   recommends merging #1 after a green run and closing the seven majors with the proposed `dependabot.yml`. No Dependabot
   security alert is open (`gh api .../dependabot/alerts` returns 0). Not a release blocker, but decide before tagging.
6. **Accept residual risks** listed in security reviews 5-8 (table below), and confirm the Windows/macOS stance
   ("no prebuilt binaries, unsigned, CI-tested only") and the name/trademark and support-channel items in
   [release-checklist](../release-checklist.md).
7. **Date** in `## [0.1.0]` of the CHANGELOG (still 2026-11-03) and the release notes; set at tagging.

### Agent can do

1. **Settled green CI on the head** (below). The last push-triggered full Rust run that finished green is acbd605e7
   (2026-11-04 09:25). Every later head was cancelled by the next push, except 4 that failed. This update fixes one real
   cause (rustfmt, item A) and asks for a quiet period: no pushes to `main`, then confirm Windows and macOS.
2. A full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` once the key exists
   (the last full rehearsal, ed4070a1f, is stale).
3. Re-run the 4 h soak and the perf-check on the head (soak predates the photos, call-chain and Wi-Fi call audio code).
4. Optional: SBOM (`cargo cyclonedx`) in the local release script; a short outbound-traffic privacy audit.
5. Small review items from security 7/8: "this replaces your link" line on the QR confirm page (S7-14), call card answers
   on click only (residual 5).

## Changes made in this refresh

* A. `cargo fmt --check` failed on `main` since d827cd8e6 (`crates/bridge-callwifi/src/gate.rs`, a test assertion). Every
  full CI run on a head after that would have failed the format step. Formatted.
* B. `CHANGELOG.md` `[Unreleased]` folded into `[0.1.0]` (Added, Fixed, Changed). `[Unreleased]` is now an empty heading so
  `scripts/release.sh` still works. The stale "no real phone paired" line was already corrected. `gen-whats-new.sh --check`
  and `release-readiness-test.sh` pass.

## Table

GREEN = evidence seen, AMBER = acceptable with a stated gap, RED = blocks, OPEN-OWNER = only the owner can do it.

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | CI green on a settled head (Linux) | RED | Heads since acbd605e7 (success, Rust + Android + supply-chain + perf + type-check): all `Cross-platform Rust checks` runs were cancelled by newer pushes except fc92fa936, 3159e99b7, fedd45750 and eb24bc01c, which failed. Causes seen: fedd45750 and eb24bc01c failed in the setup/format/type-check steps (the Windows/macOS type-check error `missing field lan_v4 in Firewall` was fixed by 7e7892790); d827cd8e6 added a rustfmt failure (fixed here). The newest head 84dd37545 is `queued`. `Docs links` is green on every head. |
| 2 | CI green on Windows and macOS | RED | Same as 1: no head after acbd605e7 finished both jobs green. The phone_feed macOS flake from the last audit did not recur in the runs that finished. |
| 3 | Emulator end-to-end | AMBER (not a release gate) | eb24bc01c failed `battery-optimisation` (restricted: could not pair) and `call-ui` (`callui_overlay_ring`, no card after the ring); 9ab5acdef and acbd605e7 failed; a526050a2 and 35180f343 passed; d2c3cbc02 was running. Call-card failure is relevant to call-chain-review claims and is UNVERIFIED on a runner. |
| 4 | Security reviews | AMBER | Reviews 1, 2, 5, 6, 7, 8, blackbox and [followups-security-5](followups-security-5.md) written with fixes and tests. Residuals not changed: 5/6 (EXIF/GPS on Android 8-9 full pictures, unverified content when saving a photo, scratch file up to 10 min), 7 (confirmed-device Bluetooth pairing prompt in the 120 s window, S7-14 silent replace of a trusted link, environments not created, SIGKILL leaves `Pairable`, call card answers on Enter, first X socket not owner-checked, desktop installers verified by SHA-256 only), 8 (helper token in its command line, loopback port DoS during the window, capture may start before the computer accepts; Kotlin changes compile-checked only). Review 8 and the Wi-Fi call audio are debug-only and excluded from release artifacts (`check-release-isolation.sh`). |
| 5 | Code review | GREEN / AMBER | [code-review-4](code-review-4.md); [call-chain-review](call-chain-review.md) tests each fix by turning it off (every fix fails a timeline, except one respawn case covered by a unit test). Wi-Fi call audio and the firewall doctor were reviewed in their own reports; no review covers the whole range after d827cd8e6. |
| 6 | Soak | AMBER | [soak-4h](soak-4h-2026-11-03.md) driver verdict FAIL (151 control-API poll timeouts early, none after 2.5 h); [control-stall-investigation](control-stall-investigation.md) and [soak-2h-after-stall-fix](soak-2h-after-stall-fix.md) followed. Predates the newest code; stand-in phone only. |
| 7 | Size and perf budgets | AMBER | `scripts/perf-check.sh` strict defaults: RSS 6000 kB, 8 threads, 20 fds, 3 wakeups/s, startup 300 ms, service 7 360 000 B (CI measured 7 303 880 B), ctl 1 552 000 B, UI RSS 75 000 kB. CI applies documented slack for the load-dependent rows. The `perf` job was green on acbd605e7; not re-measured on the head (Wi-Fi call audio and the later changes added code; size headroom was 56 kB at the last bump). |
| 8 | Updater | GREEN / AMBER | [updater-e2e-3](updater-e2e-3.md): tests for stable-over-rc.1, no later rc offered, rate limits, offline, interrupted download, hash check; the emulator stage accepts the refusal of a same-version download. The real GitHub endpoint and the upgrade from the public rc.1 are UNVERIFIED until a stable is published. rc.1 (debug-signed) cannot update in place; the notes say so. |
| 9 | Dev channel | AMBER | [dev-channel](dev-channel.md): `release-dev.sh` is a dry run unless `--publish`; the `Dev build (rolling)` workflow succeeded on several heads and is cancelled by churn on others. It was not published from here. CI publishing needs the `DEV_SIGNER_SHA256` variable and keystore environment (owner item 4). Release builds opting into Dev fail the signer check by design. |
| 10 | Packaging / fresh install | GREEN | [fresh-install-audit](fresh-install-audit.md): defects 1-6 and open items A-F fixed afterwards (doctor compares the ufw source range with the LAN address, tarball and deb README links, status without service points to doctor). [install-upgrade-sandbox](install-upgrade-sandbox.md) 61/61 earlier. Not re-run on the head. |
| 11 | Cross-platform parity | AMBER | [cross-platform-parity-3](cross-platform-parity-3.md): six wording/behaviour defects fixed; code reading plus CI logs only, no Windows or macOS hardware. Known limit: Windows tray unread line is the first text cut when two long phone names are present. |
| 12 | Real-device morning checklist | OPEN-OWNER | [morning-test-checklist](morning-test-checklist.md) is written for build c08893f6a; not recorded as run. Needs phone, an iPhone to call it, and a Bluetooth dongle. |
| 13 | Documentation | GREEN | `check-doc-links.sh`: 0 broken links; release notes draft present. |
| 14 | CHANGELOG matches the release | GREEN (this refresh) | Folded; date still 2026-11-03. |
| 15 | Licence, notices | GREEN | GPL-3.0-or-later; `release-readiness-test.sh` passes (notices, SPDX, what's-new). |
| 16 | SBOM | AMBER | Not generated; substitutes are `THIRD_PARTY_NOTICES.md`, `Cargo.lock`, Gradle verification metadata ([sbom-reproducibility](sbom-reproducibility.md)). Owner decides. |
| 17 | Release script plumbing | GREEN, rehearsal stale | Readiness test passes (no keytool here, so keystore checks skipped). Full dry run needs the key. |
| 18 | Repository visibility, private reporting | GREEN | PUBLIC; private vulnerability reporting enabled. `SECURITY.md` contact still a placeholder. |
| 19 | Windows / macOS stance, name, support channel | OPEN-OWNER | Unticked in [release-checklist](../release-checklist.md). |

## What I would do next, in order

1. Owner: create the key and pin; fill the SECURITY.md contact and the release-notes placeholder.
2. Stop pushing to `main` for one CI cycle; confirm `Cross-platform Rust checks` is green on Linux, Windows and macOS on the
   same head, and `Emulator end-to-end` for `call-ui`.
3. Owner: real-device plan with phone and dongle; record it as a report.
4. Owner: create the GitHub environments and `DEV_SIGNER_SHA256`; decide on the Dependabot PRs.
5. Agent: full `release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md`, then fix the date.
6. Owner: `scripts/release-local.sh v0.1.0 --tag --push-tag --publish --notes-file docs/release-notes-v0.1.0.md`, then an
   update check from an rc.1 install.
