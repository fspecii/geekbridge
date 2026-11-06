# v0.1.0 go / no-go audit

Audit date: 2026-11-05, refreshed against `origin/main` 84d361ecf (code head 3af033de4; previous audit: 2026-11-04 at 84dd37545).
Method: `gh run list` / `gh run view` / `gh pr list` / `gh api` against `fspecii/geekbridge`, `scripts/check-doc-links.sh`,
`scripts/gen-whats-new.sh --check`, and reading the reports named below. Nothing was tagged, released, published or merged.
Anything not checkable from here is marked **UNVERIFIED**; the perf and size budgets were read from `scripts/perf-check.sh`
and [performance](../performance.md), not re-measured.

## Verdict: still NO-GO, but only for owner items and one settled-CI confirmation

The code side is in much better shape than at the last audit. On 788543220 every job of `Cross-platform Rust checks` is green
(Linux, Windows, macOS, Android unit tests and lint, perf budget, supply chain, type-check), as are `Docs links` and
`Dev build (rolling)` (desktop builds for four targets and the debug APK). The emulator run 42042455032 (9b7db3d99) was green on
all blocking jobs. Since then reviews and fixes landed: [security-review-8](security-review-8.md), [call-features-review](call-features-review.md),
[updater-review](updater-review.md), [release-scripts-review](release-scripts-review.md), [android-release-audit](android-release-audit.md),
[first-run-review](first-run-review.md), [fresh-install-audit](fresh-install-audit.md), [soak-3](soak-3.md) and
[call-audio-decision](call-audio-decision.md).

What still blocks a tag:

* every owner-only item below is open (signing key, real-device test, security contact, decisions);
* there is no settled head: the newest code head 3af033de4 is still running, `main` was pushed to many times in two hours, and the
  newest emulator run (342454172, still in progress when this was written) has `call-ui` failed (cause not readable until the run
  ends: UNVERIFIED). The run before it was green for `call-ui`, so this may be the flake the e2e fixes have been chasing, or a regression.

## Dry-run rehearsal (2026-11-06)

Full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` (with fmt, clippy and tests), run in a
scratch `git clone` of `origin/main` 19881b568 with a THROWAWAY keystore and pin created by `scripts/make-keystore.sh` in a
scratch `HOME` (no real key was read or written; nothing was tagged, pushed or published; `gh` was the printing stub).

Passed: keystore creation and the printed pin; the pin check; fmt, clippy and the whole workspace test suite; the Linux build and
tarball; installer tarball and `.deb` (version, nine binaries); the Arch package; the CycloneDX SBOM (1.5, version, listed in
`SHA256SUMS`); both Android APKs (zipalign, v1+v2+v3 signing, signer equals the pin, `versionCode` 10099 / `versionName` 0.1.0,
minSdk 26); `SHA256SUMS` matches; release notes generated with the signer line; the tag and publish commands printed only.
The real desktop updater (`crates/bridge-update/tests/release_assets.rs`, new) accepted the produced assets from a mock release:
every `SHA256SUMS` line equals the file digest, the Linux archive is chosen over the installer tarball, `.deb`, Arch package, APKs
and SBOM, the API `digest` cross-check agrees, the download re-hashes, rc.1 is offered the release and 0.1.0 is not offered itself.
The Android updater compares the downloaded APK's signer with the installed app's: both produced APKs carry the throwaway signer
that equals the pin, so a phone with this key installed accepts them (the `SignerCheck` code itself was not run: no emulator here).

Found and fixed (same day):

* `check-release-isolation.sh` was not called by anything. `release-local.sh` now runs it on both signed APKs (and needs `unzip`, `strings`).
* A release-notes file still holding the `[OWNER: ...]` placeholder was published verbatim. `release-local.sh` now refuses (a warning in a dry run).
* A long `GB_RELEASE_DIR` makes the D-Bus based tests fail ("unexpected dbus-daemon address line", unix socket path limit); the script now warns.
* `release-owner-steps.md` named line 141 for the notes placeholder; it is line 147.
* On 19881b568 the `i18n_catalogue` test failed (a literal in `tray/model.rs`); already fixed upstream in 7b35acff9.

Observed, not fixed: `calls_api::ctl_calls_trace_shows_the_states_without_numbers_prints_json_and_clears` failed once in the
full-workspace run (the trace still held its events after `--clear`) and passed 5 of 5 alone and on the rerun: a flake under load, cause not found.
Manual steps that remain: the owner's real key, the `[OWNER]` notes placeholder, the date under `## [0.1.0]`, and a fresh clone needs `.tools`
(`scripts/bootstrap-android.sh`) before any Android step. A dry run in a dirty tree reports it as a warning, as designed.

## Ordered blockers

### Needs the owner

Copy-paste steps for every item, with what to send back: [release-owner-steps](../release-owner-steps.md).

1. **Release signing key and signer pin.** `~/secrets` and `~/.config/geekbridge/android-signer-sha256` do not exist.
   `scripts/release-local.sh v0.1.0` refuses without them. Back the key up in two offline places.
   ([stable-release-readiness](stable-release-readiness.md), owner actions 1-2.)
2. **Real-device test.** One phone (SM-F946B) was paired; everything after is unverified on hardware: calls and the in-call card,
   photos, goodbye, notifications, privacy switches, QR scan with the in-app camera, Bluetooth call audio (one-time pairing, needs
   a controller in the PC), and the Wi-Fi call audio experiment. [call-audio-decision](call-audio-decision.md) gives the order for
   the Fold5 (Bluetooth first, one ten-second Wi-Fi probe in the same session). Plan: [real-device-test-plan](../real-device-test-plan.md),
   [morning-test-checklist](morning-test-checklist.md) (round 8 build, head 07fa92a6f; rebuild first if `main` moved).
3. **`SECURITY.md` contact** (placeholder comment at line 8) and the `[OWNER: ...]` placeholder in
   [release-notes-v0.1.0](../release-notes-v0.1.0.md). Private vulnerability reporting is enabled, so a route exists.
4. **GitHub settings.** The environment `dev-signing` now exists (created 2026-11-05 12:04) but with **no branch restriction** and
   **no secrets**; the repository has no variables and no secrets, so `DEV_SIGNER_SHA256` and the keystore secrets are still
   missing. CI therefore cannot sign a dev APK with the pinned key and publishing stays skipped. Add the
   `main`-only deployment policy, the variable and the secrets (steps 3a), and decide whether branch protection should require
   the Rust jobs (3b; it requires no status checks today).
5. **Dependabot decision.** Eight open PRs (#1, #2, #4-#9), no security alert open. [dependabot-triage-3](dependabot-triage-3.md):
   merge #1 (actions) after a green run, close #2, #4, #5, #6, #8, #9 (all raise the MSRV or are red majors), defer #7. Not a release
   blocker; decide before tagging.
6. **Decisions** from [release-owner-steps](../release-owner-steps.md) section 6: accept residual risks of security reviews 5-8 (row 4),
   the Windows/macOS stance ("no prebuilt binaries, unsigned, CI-tested only"), name/trademark, support channel, SBOM as a release
   asset, and the Android size budgets (see row 7: the release APK is within the raised budgets by 0.15 MB; dropping `x86_64` from the
   universal APK is the cheap trim).
7. **Date** in `## [0.1.0]` of the CHANGELOG (still 2026-11-03) and the release notes; set at tagging.

### Agent can do

1. **Settled green CI on one head** (rows 1-3): stop pushing to `main`, then confirm `Cross-platform Rust checks` on Linux, Windows
   and macOS and `Emulator end-to-end` (blocking jobs `e2e-android`, `features`, `call-ui`, `battery-optimisation`) on the same head;
   find the cause of the `call-ui` failure on 342454172 once its log is readable.
2. A full `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md` once the key exists (the last full
   rehearsal, ed4070a1f, is stale; [release-scripts-review](release-scripts-review.md) fixes landed after it).
3. Re-measure on the head: the 4 h soak (soak-3 was 45 minutes and passed after one fix; two watch items, a 5.5 MB anonymous
   creep and the control-API criterion on a stalled disk) and `scripts/perf-check.sh` (service size headroom is about 2 kB).
4. Apply the updater findings that need code and were not applied: Android learns of an update only when Diagnostics is opened
   (A1) and the desktop never installs an update by itself (D1, a design choice the notes already state).
5. First-run items still open ([first-run-review](first-run-review.md), "Remaining"): the "Answer calls" page is too long, three
   equal ways to link on step 1, Back on the QR page does not cancel an attempt.
6. Small review items from security 7/8: "this replaces your link" line on the QR confirm page (S7-14), call card answers on
   click only (residual 5). The [release-scripts-review](release-scripts-review.md) maintainer list (non-reproducible Windows zip,
   `gen-whats-new.sh` locale on macOS, `perf-check.sh` port collisions, `install.sh` prefix with `|`).
7. Optional: SBOM (`cargo cyclonedx`) in the local release script; a short outbound-traffic privacy audit.

## Changes made in this refresh

* A. `stable-go-no-go.md` re-verified against the runs, reports and budgets named here.
* B. `CHANGELOG.md`: the call features that had piled up under `[Unreleased]` (keypad, Decline with message, Recents and missed-call
  banner, Do Not Disturb, dialer suggestions, call trace, Wi-Fi call audio, link wording) are folded into `[0.1.0]` again;
  `[Unreleased]` is an empty heading so `scripts/release.sh` still works. Known limitations now name the Bluetooth pairing and
  the experimental Wi-Fi call audio. `gen-whats-new.sh --check` and `check-doc-links.sh` pass.
* C. `docs/release-notes-v0.1.0.md`: highlights and limitations match the CHANGELOG (Do Not Disturb, dialer suggestions, Bluetooth
  call audio "needs a one-time pairing, unverified on real hardware by the maintainers", Wi-Fi call audio experimental, desktop
  updater downloads only).
* D. `README.md` and `docs/FAQ.md`: link wording as the apps now say it, call audio rows, and what each emulator job covers. No
  feature is described as verified that was not.

## Table

GREEN = evidence seen, AMBER = acceptable with a stated gap, RED = blocks, OPEN-OWNER = only the owner can do it.

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | CI green on a settled head (Linux) | AMBER | 788543220: `Rust / ubuntu-latest`, perf budget, Android unit tests and lint, supply chain, type-check Windows and macOS targets, coverage: all success (run 42049249368). `Docs links` green on every head. Not settled: 342454172 and many earlier heads were cancelled by the next push, 9d6f95223 failed, and 3af033de4 / 84d361ecf are running or queued. The last two clean heads in a row were 07fa92a6f and 788543220. |
| 2 | CI green on Windows and macOS | AMBER | 788543220: `Rust / windows-latest` and `Rust / macos-latest` success (07fa92a6f too). The macOS clipboard and keypad timing flakes were loosened in 4faa59b33, 3ffef339a and 788543220; they are the usual cause of red heads. The `Desktop / windows-x86_64` and `macos` builds of the dev workflow are green. Not hardware. |
| 3 | Emulator end-to-end | AMBER (not a release gate) | 42042455032 (9b7db3d99): green on all blocking jobs and on photos, share, pair-background, update-install, call-audio-wifi; failed only the experimental `contacts-calendar-cases` (invitation not-organiser refusal) and `sms` (limiter setup: "computer refused text 5"), both fixed in 342454172. Its run 42049065692 had `sms`, `contacts-calendar-cases`, `features`, `photos`, `share`, `e2e-android` green but `call-ui` **failed**, with `battery-optimisation`, `update-install` and `call-audio-wifi` still running (UNVERIFIED which step; log unreadable until the run ends). Earlier heads 2427850da, 1bb9f747d, 8e204c1ed and 448fb083f failed e2e before the fixes. |
| 4 | Security reviews | AMBER | Reviews 1, 2, 5, 6, 7, 8, blackbox and [followups-security-5](followups-security-5.md) written with fixes and tests. [security-review-8](security-review-8.md) fixed five defects in Wi-Fi call audio (offer pre-granting speak, speak-off answered with on, helper listener window, hidden indicator, dying runtime loop) and its Speak addendum covered consent, endings, echo guard, hostile microphone frames and release isolation. [call-features-review](call-features-review.md) HIGH-1 (card closes in a service blip) and the other call findings were addressed in 24521e90f, 65453622b, 9029cd15a and the tests after; the report was written before decline-with-message, DND and dialer suggestions and its checklist for them was covered by 9029cd15a (DND never blocks a caller, the banner sends only the words it showed). Residuals unchanged: 5/6 (EXIF/GPS on Android 8-9 full pictures, unverified content when saving a photo, scratch file up to 10 min), 7 (confirmed-device Bluetooth pairing prompt in the 120 s window, S7-14 silent replace of a trusted link, SIGKILL leaves `Pairable`, call card answers on Enter, first X socket not owner-checked, desktop installers verified by SHA-256 only), 8 (helper token in its command line, loopback port DoS during the window, capture may start before the computer accepts). Review 8 and the Wi-Fi call audio are debug-only and excluded from release artifacts (`check-release-isolation.sh`, hardened in be757d752). |
| 5 | Code review | GREEN / AMBER | [code-review-4](code-review-4.md), [call-chain-review](call-chain-review.md), [call-features-review](call-features-review.md), [android-release-audit](android-release-audit.md) (release variant: no debuggable flag, no debug-only components, 27 permissions all justified and test-guarded; CI built a different native library than the measured one, fixed in 4b27b4321). Static reviews that could not compile (call-features, updater, release-scripts) had their edits run by CI afterwards. No review covers the whole range after 3af033de4. |
| 6 | Soak | AMBER | [soak-3](soak-3.md): 45 min of rings, Wi-Fi listen sessions, bursts, privacy toggles and re-pairing on the real service: PASS after one fix (an `fsync` on an async worker stalled the control API; `Privacy::set`, 6c4c047db), no stuck ring, orphaned card or `pw-cat`, fds and threads flat, RSS creeping about 5.5 MB while decelerating. The earlier [4 h soak](soak-4h-2026-11-03.md) failed on early control-API timeouts, analysed in [control-stall-investigation](control-stall-investigation.md) and [soak-2h-after-stall-fix](soak-2h-after-stall-fix.md). No 4 h run on the head; stand-in phone only. |
| 7 | Size and perf budgets | AMBER | `scripts/perf-check.sh` strict defaults: RSS 6000 kB, 8 threads, 20 fds, 3 wakeups/s, 2 CPU ticks, startup 300 ms, service **7 500 000 B** (CI measured 7 370 952 B at the step, about 2 kB of headroom now per dependabot-triage-3), ctl **1 650 000 B** (CI 1 608 632 B), UI RSS 75 000 kB. CI applies documented slack for the load-dependent rows (RSS 8500 kB, 10 threads, 24 fds, 6 wakeups/s, startup 1000 ms). The `perf` job is green on 788543220. [performance](../performance.md) "Phone linked" section (2026-11-05): 200 call cycles leave no fd, thread or child growth, 2 000 contacts and 2 600 texts held give service RSS 11-13.7 MB. Android release APK per [android-release-audit](android-release-audit.md): universal 7.10 MB (budget raised to 7.25 MB), arm64 3.55 MB (3.6 MB), dex 0.58 MB (0.6 MB), idle PSS 21.3 MB, cold start median 877 ms on an emulator under host load. Android budgets were raised, not met, in 72336a027; the cheap trim is dropping `x86_64` from the universal APK. Not re-measured on the head. |
| 8 | Updater | GREEN / AMBER | [updater-e2e-3](updater-e2e-3.md) plus [updater-review](updater-review.md): stable never takes prereleases or drafts (three independent checks per side), downgrade and replay refused, every redirect hop host-checked, size, SHA-256 and `digest` cross-check, re-hash before use; the review's code fixes landed (rate-limit pause persisted, download deadline, attempt gate: 8e81ad80e). Open by design: the desktop never installs anything (the user unpacks; D1), Android learns of an update only from the Diagnostics screen (A1), `SignerCheck.identity` fails open when an identity is unreadable (the system installer still checks). The real GitHub endpoint and the upgrade from the public rc.1 are UNVERIFIED until a stable is published; rc.1 (debug-signed) cannot update in place and the notes say so. `update-install` emulator job green on 9b7db3d99. |
| 9 | Dev channel | AMBER | [dev-channel](dev-channel.md): `release-dev.sh` is a dry run unless `--publish`; `Dev build (rolling)` is green on 788543220 (four desktop targets plus the debug APK), "Update the rolling dev prerelease" skipped. The `dev-signing` environment exists but without a `main` policy or secrets (owner item 4), so CI does not sign. Not published from here. |
| 10 | Packaging / fresh install | GREEN | [fresh-install-audit](fresh-install-audit.md): defects 1-6 and open items A-F fixed afterwards; [install-upgrade-sandbox](install-upgrade-sandbox.md) 61/61 earlier. [release-scripts-review](release-scripts-review.md): keystore passwords only travel as files, `rm -rf` guarded, reproducible tarball and deb members; two silent-exit bugs in `check-release-isolation.sh` and `release-dev.sh` fixed. Not re-run on the head. |
| 11 | Cross-platform parity | AMBER | [cross-platform-parity-3](cross-platform-parity-3.md): six defects fixed; code reading plus CI logs only, no Windows or macOS hardware. Known limit: Windows tray unread line is the first text cut when two long phone names are present. |
| 12 | Real-device morning checklist | OPEN-OWNER | [morning-test-checklist](morning-test-checklist.md) (round 8, head 07fa92a6f, versionCode 20348) is written; not recorded as run. Needs phone, an iPhone to call it, and a Bluetooth controller (the PC now has an integrated one, `hci0`, per [call-audio-decision](call-audio-decision.md)). |
| 13 | Documentation | GREEN | `check-doc-links.sh`: 0 broken links; release notes draft present with one `[OWNER]` placeholder; `docs/privacy.md` lists every release permission (guarded by a test); README, FAQ and release notes agree on what is unverified. |
| 14 | CHANGELOG matches the release | GREEN (this refresh) | New call features folded into `[0.1.0]`; `[Unreleased]` empty; `gen-whats-new.sh --check` passes; date still 2026-11-03. |
| 15 | Licence, notices | GREEN | GPL-3.0-or-later; supply-chain job (cargo-deny, notices) green on 788543220. |
| 16 | SBOM | AMBER | Not generated; substitutes are `THIRD_PARTY_NOTICES.md`, `Cargo.lock`, Gradle verification metadata ([sbom-reproducibility](sbom-reproducibility.md)). Owner decides. |
| 17 | Release script plumbing | GREEN, rehearsed 2026-11-06 with a throwaway key (see above) | [release-scripts-review](release-scripts-review.md) fixes applied (ac6fab587). Full dry run needs the key. Android CI and local builds now use the same native profile and an ELF 16 KiB alignment check. |
| 18 | Repository visibility, private reporting | GREEN | PUBLIC; private vulnerability reporting enabled; no open Dependabot alert. `SECURITY.md` contact still a placeholder. |
| 19 | Windows / macOS stance, name, support channel | OPEN-OWNER | Unticked in [release-checklist](../release-checklist.md). |

## What I would do next, in order

1. Owner: create the key and pin; fill the SECURITY.md contact and the release-notes placeholder.
2. Stop pushing to `main` for one CI cycle; confirm `Cross-platform Rust checks` is green on Linux, Windows and macOS on the
   same head, and `Emulator end-to-end` for `call-ui`.
3. Owner: real-device plan with phone and Bluetooth: pair as hands-free, one answered call, the Wi-Fi probe; record it as a report.
4. Owner: finish the GitHub environment (`main` policy, `DEV_SIGNER_SHA256`, secrets); decide on the Dependabot PRs.
5. Agent: full `release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md`, then fix the date.
6. Owner: `scripts/release-local.sh v0.1.0 --tag --push-tag --publish --notes-file docs/release-notes-v0.1.0.md`, then an
   update check from an rc.1 install.
