# Dependabot and repository triage 2 (2026-11-04)

Read-only review of fspecii/geekbridge at main 2c9f6103c. Nothing on GitHub was changed; `.github/dependabot.yml` is untouched.

## Owner decisions (checklist)

- [ ] PR #1 (actions group): approve a rebase and then merge once Windows tests and Performance budget are green on a fresh run (see below).
- [ ] PRs #2, #4, #5, #6, #7, #8, #9: close them (Dependabot will not reopen a closed major while `ignore` rules exist) or leave open and defer. Recommendation: close all seven after applying the proposed dependabot.yml.
- [ ] Apply the proposed `.github/dependabot.yml` below (weekly, limit 2, ignores).
- [ ] Delete the eight `dependabot/*` branches after their PRs are closed (GitHub does this on close for Dependabot branches; verify).
- [ ] Branch protection on main exists but requires nothing (no status checks, no reviews, admins not enforced). Decide whether to require the `Cross-platform Rust checks` jobs.
- [ ] Enable "Automatically delete head branches" (currently off).
- [ ] Consider Actions setting `sha_pinning_required: true` (currently false; the workflows are already SHA pinned).
- [ ] SECURITY.md line 8 still has an `OWNER:` TODO for a monitored contact. Private vulnerability reporting is enabled, so the fallback is optional; either add an address or remove the fallback sentence and the comment.
- [ ] Add a CODEOWNERS file (none exists) if you want review routing.
- [ ] Set a homepage URL (currently empty; for example the docs site or the release page).
- [ ] Code scanning is not set up (404 "no analysis found"). Decide whether to enable CodeQL for Rust/Kotlin/Actions.

## Open issues and alerts

- Open issues: none. Open PRs: 8, all Dependabot.
- Dependabot alerts: none (API reachable, empty list). Secret scanning alerts: none. Code scanning: not configured.

## Pull requests

CI caveat: most PR runs are from 2026-11-02, before the CI fixes now on main (at that time even the pinned `setup-android` step and `THIRD_PARTY_NOTICES.md` checks failed on every branch, including ones touching only unrelated files). Their red status is therefore weak evidence about the bump itself; only #1 (run 2026-11-04 00:43) and #4 (2026-11-04 05:16) are recent. A rebase gives a trustworthy signal.

| # | Bump | Kind | CI (latest run on that branch) | Recommendation |
|---|------|------|-----|----------------|
| 1 | actions group: upload-artifact v4 to v7.0.2, download-artifact v4 to v8.0.2, setup-java v4 to v6.1.0, setup-android v3 to v4.0.4 | 4 majors | Android, docs links, Linux/macOS Rust, type-check, cargo-deny pass; Windows tests and Performance budget fail | Rebase, then merge if green (details below) |
| 2 | zbus 5.19.0 to 6.2.0 (bridge-calls, bridge-phone-sim, bridge-service) | major | old run; Clippy, notices, type-check, tests fail | Close (defer to a planned migration) |
| 4 | eframe 0.31.1 to 0.37.0 (bridge-apps) | major (six minors of a 0.x line) | 2026-11-04: Clippy on all OS, notices, type-check fail; Android and perf pass | Close (planned UI migration) |
| 5 | windows-future 0.3.2 to 0.100.0 (bridge-calls-windows) | major in a 0.x line, versioning scheme change | old run, all Rust gates fail | Close; revisit with the whole `windows` crate family together |
| 6 | zbus_xml 5.2.1 to 6.0.0 (bridge-calls) | major | old run; notices, type-check fail | Close; must move together with zbus (#2) |
| 7 | org.jetbrains.kotlin.android 2.2.21 to 2.4.21 | minor by number, but Kotlin treats it as a language release | old run, Android job failed at setup-android | Defer; retest after rebase, needs AGP/KSP/Compose compatibility check |
| 8 | com.android.application 8.13.2 to 9.5.0 | major | old run | Close (AGP 9 needs Gradle 9, new DSL, built-in Kotlin) |
| 9 | gradle-wrapper 8.13 to 9.8.2 | major | old run | Close (pair with AGP 9 in one deliberate change) |

### PR #1 detail (touches SHA-pinned Actions)

All four new SHAs were verified against upstream with `gh api repos/<owner>/<repo>/git/ref/tags/<tag>`; each tag is a lightweight tag pointing straight at the commit, and every SHA matches the one in the PR:

| Action | Tag | SHA in PR | Upstream tag object | Match |
|--------|-----|-----------|---------------------|-------|
| actions/upload-artifact | v7.0.2 | cf430e030ddbb5b0abf93d22962f4752f3646cd9 | commit cf430e03... | yes |
| actions/download-artifact | v8.0.2 | 9000827ccba6bdab643e8b6fd33ac0654aef8333 | commit 9000827c... | yes |
| actions/setup-java | v6.1.0 | b843ebf9b913146b38da2eaa7aeddf76074ae1ad | commit b843ebf9... | yes |
| android-actions/setup-android | v4.0.4 | be39fa834029ff78f1a44aa3bb0819b8fc2bd8fd | commit be39fa83... | yes |

Touched workflows: build-binaries, check, dev-release, emulator-e2e, release.

Risk to the release process: moderate, and the reason to be careful. upload/download-artifact v4 to v7/v8 is the pair that matters: the release and dev-release workflows hand binaries between jobs through artifacts, so a behaviour change (name handling, digest verification on download, node runtime) would break a release only at tag time. Both sides must move together (the PR does that). The new SHAs are authentic, so there is no supply-chain concern. The Windows test and Performance budget failures on the 2026-11-04 00:43 run are in jobs that do not use the bumped actions in a way that explains them, and are most likely main-branch state at that time; a rebase onto 2c9f6103c will tell. Recommendation: rebase, confirm green, then run `dev-release.yml` once from the PR branch (or after merge on a throwaway tag) to prove the artifact hand-off before the next real release. Merge after the pending rc is settled, not during a release window.

### Rust majors (#2, #4, #5, #6)

These are API-breaking migrations (zbus 6 and zbus_xml 6 must land together; eframe 0.37 changes egui APIs used by the tray/call-card code, the Windows `WS_EX_NOACTIVATE` hook, accesskit feature, and the Wayland/X11 backends; windows-future 0.100 belongs to a newer `windows` family). Each changes THIRD_PARTY_NOTICES.md and needs the notices gate regenerated. None is a security fix (no alerts). Risk to release: high if merged blind, since the desktop and D-Bus call paths are validated on real BlueZ/PipeWire. Close and schedule as deliberate work after the 0.1.0 line.

### Android (#7, #8, #9)

AGP 9 and Gradle 9 must move together and need Kotlin built-in support, lint/baseline and plugin adjustments; the signing and versionCode pipeline for the APK is release critical. Kotlin 2.4 needs a matching Compose compiler and KSP. Close #8 and #9, defer #7.

## Repository hygiene

| Item | State | Note |
|------|-------|------|
| Visibility, default branch | public, main | ok |
| License | GPL-3.0 (detected) | matches the project decision |
| Description, topics | set; topics: android, clipboard, gpl, kde-connect, phone-link, rust, sms | consider adding `linux`, `bluetooth`, `windows`, `macos`, `egui` |
| Homepage | empty | set one |
| Branch protection on main | present but empty: no required checks, no required reviews, force pushes and deletions blocked, admins not enforced | decide on required checks |
| Rulesets | none | |
| Actions permissions | all actions allowed, SHA pinning not required; default workflow token read, Actions cannot approve PRs | good defaults |
| Secret scanning, push protection | enabled | ok |
| Dependabot security updates | enabled | |
| Private vulnerability reporting | enabled | ok |
| Code scanning | not configured | optional |
| Releases | one: GeekBridge 0.1.0-rc.1 (pre-release, v0.1.0-rc.1, 2026-11-02) | |
| Issue templates | bug_report.yml, feature_request.yml, config.yml | ok |
| PR template | present | |
| CONTRIBUTING, CODE_OF_CONDUCT, SECURITY | present; community health 100% | |
| CODEOWNERS | absent | optional |
| Discussions, wiki | off | fine |
| Delete head branches on merge | off | enable |

Stale branches: `ci-windows-debug` no longer exists on origin. The only non-main branches are the eight `dependabot/*` ones (cargo/eframe-0.37.0, cargo/windows-future-0.100.0, cargo/zbus_xml-6.0.0, cargo/zbus-6.2.0, github_actions/actions-2ac9e5f813, gradle/android/com.android.application-9.5.0, gradle/android/gradle-minor-patch-57914a3406, gradle/android/gradle-wrapper-9.8.2). Propose deleting all except the actions branch if #1 is kept; none was deleted by this review.

## Proposed `.github/dependabot.yml` (NOT applied)

Notes: `ignore` entries for majors stop Dependabot from reopening the closed PRs; the group uses minor and patch only so any major arrives alone and visible. Kotlin is ignored for minor and major because Kotlin's 2.x minors are language releases that must match Compose and KSP. Remove an ignore line when you decide to migrate. Actions majors are not ignored since they are SHA pinned and reviewed.

```diff
--- a/.github/dependabot.yml
+++ b/.github/dependabot.yml
@@
 version: 2
 updates:
   - package-ecosystem: github-actions
     directory: /
     schedule:
       interval: weekly
+    open-pull-requests-limit: 2
     groups:
       actions:
         patterns: ["*"]
+        update-types: [minor, patch]
   - package-ecosystem: cargo
     directory: /
     schedule:
       interval: weekly
+    open-pull-requests-limit: 2
     groups:
       cargo-minor-patch:
         update-types: [minor, patch]
+    ignore:
+      # Planned migrations, done by hand: eframe/egui together, zbus and zbus_xml together,
+      # the windows crate family together.
+      - dependency-name: "eframe"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "zbus"
+        update-types: ["version-update:semver-major"]
+      - dependency-name: "zbus_xml"
+        update-types: ["version-update:semver-major"]
+      - dependency-name: "windows-future"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
   - package-ecosystem: gradle
     directory: /android
     schedule:
       interval: weekly
+    open-pull-requests-limit: 2
     groups:
       gradle-minor-patch:
         update-types: [minor, patch]
+    ignore:
+      - dependency-name: "org.jetbrains.kotlin.*"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "com.android.application"
+        update-types: ["version-update:semver-major"]
+      - dependency-name: "com.android.library"
+        update-types: ["version-update:semver-major"]
+      - dependency-name: "gradle"
+        update-types: ["version-update:semver-major"]
```

Caveats: for 0.x crates Dependabot treats a minor bump as semver-minor, which is why eframe and windows-future ignore minor too (0.31 to 0.37 is breaking). A grouped actions PR with `update-types` excludes action majors from the group, so they come as separate PRs; drop that line if you prefer one combined actions PR. The `gradle` wrapper is tracked by name `gradle` in the gradle ecosystem; confirm after the first run.
