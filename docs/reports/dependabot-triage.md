# Dependabot triage (2026-11-02)

Nine open Dependabot PRs on `fspecii/geekbridge`, triaged read-only. Nothing was merged, closed, commented or pushed.
Local tests ran on a scratch clone of `origin/main` (d187dc1) with PR heads fetched, a separate `CARGO_TARGET_DIR`,
`nice cargo -j3`. The scratch dir is deleted at the end.

## Read this first: the PR CI results are mostly stale

- PRs 1, 4, 5, 6, 7, 8, 9 branch from `4bb7309`, PRs 2 and 3 from `bb2ff09`. All predate `d187dc1` ("CI: fix the first real GitHub runner failures").
  The red Windows / macOS / xcheck / Performance / Coverage jobs fail identically on `bb2ff09` itself (main's run 41503330811), so they are
  **pre-existing, not caused by any bump**.
- Android job on PRs 7/8/9: fails in `sdkmanager` ("Failed to find package 'tools'", SDK licence prompt), before Gradle runs. Same infra failure as `bb2ff09`.
  **PR 1 (setup-android v3 → v4.0.4) is the only PR where the Android job is green**, so it probably fixes that. Merge PR 1 first.
- cargo-deny failures on PRs 2-6 (`bb2ff09`/`4bb7309` bases): the log shows the `rust-toolchain.toml` / musl override error, i.e. the runner problem
  fixed on main. Not a licence or advisory verdict. `cargo-deny` is not installed here, so I could not run it locally. Licences were checked from crates.io instead (below).
- Main's run for d187dc1 (41506237970) was still in progress when I looked. After it is green, comment `@dependabot rebase` on the PRs you keep to get
  honest CI. That is a GitHub write, so I did not do it.

## Summary

| PR | Bump | Recommendation |
|----|------|----------------|
| 1 | actions group (setup-java 6.1.0, setup-android 4.0.4, upload-artifact 7.0.2, download-artifact 8.0.2) | **MERGE** |
| 3 | base64 0.22.1 → 0.23.1 | **MERGE** (after rebase + green main CI) |
| 2 + 6 | zbus 5.19 → 6.2, zbus_xml 5.2 → 6.0 | **CLOSE both, redo as one hand-made migration** (code breaks; MSRV 1.89) |
| 4 | eframe 0.31.1 → 0.37.0 | **DEFER / CLOSE** (37 compile errors; MSRV 1.95; real egui port) |
| 5 | windows-future 0.3.2 → 0.100.0 | **CLOSE** (cannot work alone; needs the `windows` crate bumped in lockstep) |
| 7, 8, 9 | Kotlin 2.2.21 → 2.4.21, AGP 8.13.2 → 9.5.0, Gradle 8.13 → 9.8.2 | **DEFER**, one coordinated Android toolchain migration |

## Per-PR detail

### PR 1: actions group: MERGE
All four tags checked with `gh api repos/<owner>/<repo>/git/ref/tags/<tag>` (all return `type: commit`, so no annotated-tag indirection).
The PR pins by full SHA with the version comment, like our workflows, and the SHAs match the tags:

| Action | Tag | SHA in PR = tag SHA |
|--------|-----|----|
| actions/setup-java | v6.1.0 | b843ebf9b913146b38da2eaa7aeddf76074ae1ad ✔ |
| android-actions/setup-android | v4.0.4 | be39fa834029ff78f1a44aa3bb0819b8fc2bd8fd ✔ |
| actions/upload-artifact | v7.0.2 | cf430e030ddbb5b0abf93d22962f4752f3646cd9 ✔ |
| actions/download-artifact | v8.0.2 | 9000827ccba6bdab643e8b6fd33ac0654aef8333 ✔ |

- CI: Rust/ubuntu, supply chain, Android, Coverage green. Windows/macOS/xcheck/Performance red = pre-existing.
- Risk: upload v7 / download v8 are in different major lines but both use the v4 artifact backend, so they interoperate. Node runtime bumps need a recent runner; fine on GitHub-hosted.
  Majors can change defaults (e.g. download-artifact v8 fails on digest mismatch); the new Android job result is the proof.
- Security: positive (all pinned, supply-chain hygiene; setup-android v4 likely fixes our Android CI).
- Command: `gh pr merge 1 --repo fspecii/geekbridge --squash --delete-branch`

### PR 3: base64 0.23.1: MERGE
- Licence MIT OR Apache-2.0, MSRV 1.71 (< our 1.88). Changelog: 0.23.0 adds consts, custom padding, SIMD engines (`simd-unsafe`, default on) and a clearer `InvalidLastSymbol`; 0.23.1 only fixes tests on non-SIMD archs. No API removals listed.
- Local: merged onto main, `cargo build --workspace --locked` OK (1m34s), `cargo test -p bridge-kde` OK (all suites pass). Only `bridge-kde` uses it directly.
- Risk notes: the default-on `simd-unsafe` feature brings `unsafe` SIMD code into a crate that decodes network input (KDE Connect payloads). Runtime-detected, with scalar fallback, but it is a new attack surface. Optional hardening: `base64 = { version = "0.23", default-features = false, features = ["std"] }` to keep the scalar engine only. Not a blocker. Also the lock still has base64 0.22 transitively (duplicate warning in cargo-deny is only a warning).
- Command: `@dependabot rebase` (after main CI green), then `gh pr merge 3 --repo fspecii/geekbridge --squash --delete-branch`.
  Optional hardening commit after the merge: the `default-features = false` line above in `crates/bridge-kde/Cargo.toml`.

### PR 2 (zbus 6.2.0) + PR 6 (zbus_xml 6.0.0): CLOSE, redo together
- They must move together (`bridge-calls` uses both; merging both PRs conflicts in `Cargo.lock` and `crates/bridge-calls/Cargo.toml`, tested). PR 6 alone pulls zbus 6.2.1 into the tree beside zbus 5.
- zbus 6 is a documented breaking release ("Upgrading to zbus 6.0" book chapter). Locally (main + both Cargo.toml edits + `cargo update`): `bridge-calls` fails with **13 errors** in `pipewire.rs` and `probe_linux.rs`:
  `zbus::blocking` removed (blocking API moved), match-rule `Builder` methods no longer return `Result` (`map_err` errors), mismatched types, plus deprecation warnings
  (`zbus::zvariant::OwnedValue` → `zbus::OwnedVariant`, `OwnedObjectPath` → `zbus::OwnedObjectPath`). `bridge-service` (tray) and `bridge-phone-sim` use zbus too and will surface more once `bridge-calls` compiles.
- zbus 6 also changes the runtime story (tokio/`zruntime`, async-io deps gone from the tree) and **raises MSRV to 1.89** (we declare 1.88; needs `rust-version` bump in `Cargo.toml`, `rust-toolchain.toml` comment, docs).
- Licence: MIT (fine). Security relevance: 6.x tightens D-Bus message validation (header/body checks, nesting limits); useful but it is not a vulnerability fix for us.
  Note `deny.toml` ignores RUSTSEC-2026-0370/0371/0372 (zvariant/zbus_names unmaintained) because of zbus 5. zbus 6 folds `zvariant`/`zbus_names` into zbus, so the migration should let us drop those three ignores. Check before removing.
- Recommendation: close both with a note, and do a planned port (≈ 2 files, 13+ errors) as its own change; add `zbus`/`zbus_xml` to the Dependabot ignore-major list until then.
  Commands: `gh pr close 2 --repo fspecii/geekbridge --comment "Needs code port + MSRV 1.89; tracked as a manual migration"`, same for 6.

### PR 4: eframe 0.37.0: DEFER (close, ignore majors)
- Licence MIT OR Apache-2.0, **MSRV 1.95** (we are 1.88). Six minor releases of egui breaking changes.
- Locally: `cargo build -p bridge-apps` after the bump: **37 errors**: `egui::SidePanel` / `TopBottomPanel` gone (13), `eframe::App::update` replaced by required `ui` (3+3), `Context::screen_rect`/`style`/`wants_keyboard_input` removed, `Role::Checkbox`/`SelectableLabel` removed, `DroppedFile::path` now a method, etc. This is a real UI port.
- Upside to schedule it: moving off egui 0.31 retires the `deny.toml` ignores for RUSTSEC-2026-0192 (ttf-parser), -0194/-0195 (quick-xml 0.30 via accesskit/atspi/zbus_xml) and -2024-0436 (paste), review date 2027-02-01 already in the file. Also the lock jumps accesskit 0.17 → 0.25 and adds `accesskit_ios`.
- Command: `gh pr close 4 --repo fspecii/geekbridge --comment "Needs egui port (37 errors) and MSRV 1.95; tracked manually"`; plan the port before 2027-02-01.

### PR 5: windows-future 0.100.0: CLOSE
- Licence MIT OR Apache-2.0, MSRV 1.95. Our `bridge-calls-windows` pins `windows = "0.62"` and `windows-collections = "0.3"`. windows-future 0.100 depends on `windows-core 0.100`, so `IAsyncOperation` from it is a different type from what the `windows` 0.62 API returns (the lock shows two windows-core versions). It will not type-check against our code in `native/wait.rs`.
- I could not compile it: no `x86_64-pc-windows-msvc` std installed here (no rustup), and this is a `cfg(windows)` dep, so the Linux build never sees it. Conclusion is from the dependency graph; verify with `scripts/xcheck.sh windows` if you want proof.
- Recommendation: close; bump `windows`, `windows-future`, `windows-collections` together in one hand-made change when the `windows` crate reaches the matching line. Add the three to the Dependabot ignore list for major/minor-0.x bumps.
- Command: `gh pr close 5 --repo fspecii/geekbridge --comment "Must move in lockstep with windows/windows-collections"`

### PRs 7, 8, 9: Kotlin / AGP / Gradle: DEFER as one migration
- Licences: Gradle, AGP, Kotlin are Apache-2.0 (no change). PR 9's `distributionSha256Sum` 21bd5da2… matches `https://services.gradle.org/distributions/gradle-9.8.2-bin.zip.sha256`; the wrapper jar changes too (it is replaced, as expected for a wrapper bump; verify with `gradle wrapper --gradle-version 9.8.2` if you want a second source).
- These are three majors that depend on each other: AGP 9.x requires Gradle 9.x, and AGP 9 ships built-in Kotlin support (the separate `org.jetbrains.kotlin.android` plugin line in `android/build.gradle.kts` and the `kotlin { compilerOptions }` block may need migrating or an opt-out flag). Merging any one alone (e.g. Kotlin 2.4.21, which also affects AGP/Compose compatibility) risks a broken build. This is from upstream knowledge, not verified here.
- **Not tested locally**: this machine has no JDK and no Android SDK (`java: command not found`), so no `./gradlew` run. I did not install one (instructions: keep disk low; do not touch global setup).
- **Dependabot cannot update `android/gradle/verification-metadata.xml`** (it only edits the version string). Any Gradle plugin/wrapper bump changes the set of verified artefacts, so Android CI fails on dependency verification even when the code is fine. A maintainer must run, on a cold cache, as documented in `docs/contributor-onboarding.md`:
  `rm -rf android/app/build android/build; GRADLE_USER_HOME=$(mktemp -d) ./gradlew --write-verification-metadata sha256 help testDebugUnitTest lintDebug assembleDebug assembleRelease`
  and review the diff of that file before committing. Also update the pins in `docs/android-contract.md:9`.
- The PR CI failure on these three is the SDK `sdkmanager` setup failure (PR 1 fixes the setup action), so it says nothing about the bumps themselves.
- Recommendation: leave them open or close them; do a single manual migration after PR 1 lands: Gradle 9.8.2 → AGP 9.5.0 → Kotlin 2.4.21, regenerate verification metadata, run emulator tests. Until then ignore majors for these three in Dependabot.
  Commands to close: `gh pr close 7 8 9 --repo fspecii/geekbridge --comment "Coordinated Android toolchain migration planned manually (verification-metadata must be regenerated)"` (run per PR number).

## Suggested merge order
1. `gh pr merge 1 …` then let main CI run.
2. `@dependabot rebase` on 3, then merge.
3. Close 2, 6, 4, 5 (comments above); decide on 7/8/9.

## Licence check
`cargo-deny` is not installed, so licences came from crates.io for the new versions: base64 MIT OR Apache-2.0; zbus / zbus_xml MIT; windows-future MIT OR Apache-2.0; eframe MIT OR Apache-2.0. None is GPL-incompatible or AGPL. Gradle/AGP/Kotlin: Apache-2.0. New transitive crates in zbus 6 (`zbus_utils`, `zruntime`, `zcheapstr`) and eframe 0.37 were not individually checked; run `cargo deny check licenses` when you do those migrations.

## Suggested `.github/dependabot.yml`

Changes: weekly (Monday), PR limits, commit-message prefixes, ignore the majors that need code changes (and 0.x minors for the pre-1.0 crates that break on minor bumps), group action updates, group cargo patch-only separately, and keep the Gradle trio out of auto-bumps.

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
+      day: monday
+    open-pull-requests-limit: 3
+    commit-message:
+      prefix: "ci"
     groups:
       actions:
         patterns: ["*"]
   - package-ecosystem: cargo
     directory: /
     schedule:
       interval: weekly
+      day: monday
+    open-pull-requests-limit: 5
+    commit-message:
+      prefix: "deps"
+    ignore:
+      # Need code changes and a higher MSRV; migrate by hand (see docs/reports/dependabot-triage.md).
+      - dependency-name: "egui*"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "eframe"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "winit"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "windows*"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "zbus*"
+        update-types: ["version-update:semver-major"]
     groups:
       cargo-minor-patch:
         update-types: [minor, patch]
   - package-ecosystem: gradle
     directory: /android
     schedule:
       interval: weekly
+      day: monday
+    open-pull-requests-limit: 3
+    commit-message:
+      prefix: "android"
+    ignore:
+      # Gradle wrapper, AGP and Kotlin move together and need verification-metadata.xml regenerated by hand.
+      - dependency-name: "com.android.application"
+        update-types: ["version-update:semver-major"]
+      - dependency-name: "org.jetbrains.kotlin.*"
+        update-types: ["version-update:semver-major", "version-update:semver-minor"]
+      - dependency-name: "gradle-wrapper"
+        update-types: ["version-update:semver-major"]
     groups:
       gradle-minor-patch:
         update-types: [minor, patch]
```

Notes: `windows*` also matches `windows-sys`; that is intended (the `windows-*` crates pin to each other). Grouped minor/patch cargo PRs would still include a failing crate; if one breaks, ignore that crate rather than closing the group. The current grouping `update-types: [minor, patch]` already put Kotlin 2.2.21 → 2.4.21 into a "minor" group, which is why PR 7 arrived; the ignore above stops that. Dependabot cannot regenerate `android/gradle/verification-metadata.xml`, so every Gradle bump that is allowed through needs a manual metadata commit.

## Cleanup
`$HOME/.cache/gb-dependabot` removed at the end of this triage.
