# Dependabot triage 3 (2026-11-05)

Read-only refresh of [triage 2](dependabot-triage-2.md) at main 3af033de4. Nothing on GitHub was changed: no PR was merged, closed, commented on or rebased. Every command below is a proposal for the owner to run after a go.

## Summary

| # | Bump | Kind | CI now | Recommendation |
|---|------|------|--------|----------------|
| 1 | actions group (upload-artifact 7.0.2, download-artifact 8.0.2, setup-java 6.1.0, setup-android 4.0.4) | 4 majors, CI only | all green, run of 2026-11-05 12:39-13:01, 16 commits behind main | Merge |
| 2 | zbus 5.19.0 to 6.2.0 | major | red (run of 2026-11-02, 347 behind) | Close |
| 4 | eframe 0.31.1 to 0.37.0 | major | red (2026-11-04, 158 behind) | Close |
| 5 | windows-future 0.3.2 to 0.100.0 | major | red (2026-11-04, 132 behind) | Close |
| 6 | zbus_xml 5.2.1 to 6.0.0 | major | red (2026-11-02) | Close |
| 7 | kotlin.android 2.2.21 to 2.4.21 | minor by number | red (2026-11-02) | Defer |
| 8 | com.android.application 8.13.2 to 9.5.0 | major | red (2026-11-02) | Close |
| 9 | gradle-wrapper 8.13 to 9.8.2 | major | red (2026-11-02) | Close |

PR #3 (base64 0.22.1 to 0.23.1) is closed and is not part of this list.

Dependabot alerts: none open (`gh api repos/fspecii/geekbridge/dependabot/alerts` returns an empty list). So no bump here is a security fix, and none is urgent. `.github/dependabot.yml` is still the old one (no ignore rules, no PR limit); triage 2's proposal was not applied, so closing a major PR now means Dependabot may reopen a new one for the next release of the same crate.

## Gates used for every Rust bump

- MSRV: workspace `rust-version = "1.88"`, CI on stable.
- Licence: deny.toml allows MIT, Apache-2.0 (+LLVM), BSD, ISC, Zlib, Unicode-3.0, Unlicense, 0BSD, BSL-1.0, CDLA-Permissive-2.0, OFL-1.1, Ubuntu-font-1.0.
- Size budgets: `geekbridge-service` 7 500 000 B (about 2 kB headroom), `geekbridge-ctl` 1 650 000 B. Any new transitive dependency in the service can break the budget.
- Android: `bridge-android` does not depend on zbus, zbus_xml, eframe or windows-future (only bridge-calls, bridge-apps, bridge-service, bridge-calls-windows and bridge-phone-sim do). The Rust bumps therefore do not touch the Android build; the three Gradle bumps do.

Crates.io metadata for the new versions:

| Crate | New version | MSRV | Licence | Breaks the 1.88 MSRV |
|-------|-------------|------|---------|----------------------|
| zbus | 6.2.0 | 1.89 | MIT | yes |
| zbus_xml | 6.0.0 | 1.89 | MIT | yes |
| eframe | 0.37.0 | 1.95 | MIT OR Apache-2.0 | yes |
| windows-future | 0.100.0 | 1.95 | MIT OR Apache-2.0 | yes |

All licences are already allowed. All four raise the MSRV above the declared 1.88, so each would also need a deliberate `rust-version` bump (and a statement of the supported toolchain) first.

## Per-PR

### #1 actions group: Merge

- Four majors, but only workflow plumbing; no Rust, Android or binary-size effect.
- CI is fully green on the current head (670220b04, rebased 2026-11-05 12:39): Rust on Linux, macOS and Windows, Performance budget, type-check of Windows/macOS targets, cargo-deny and notices, Android build, docs links. Coverage and CodeRabbit are skipped, as designed.
- Triage 2 verified all four SHAs against the upstream tags (each matches its tag's commit), so there is no supply-chain concern. The blocker in triage 2 (Windows tests and Performance budget red) is gone.
- Remaining risk, unchanged from triage 2: upload/download-artifact v4 to v7/v8 carries the binaries between jobs in release and dev-release, and a break would show only at tag time. The CI run proves check.yml, not release.yml. Merge when no release is in flight, then watch the next Dev build (rolling) run, which uses the same hand-off.
- Not affected: Android crates, MSRV, licences, size budgets.

```
gh pr merge 1 --squash --delete-branch
gh run list --workflow "Dev build (rolling)" --limit 3   # check after the merge
```

### #2 zbus 5 to 6: Close

- Breaking major used by the call backend, the BlueZ agent, the tray (StatusNotifierItem) and the phone simulator. Must land together with zbus_xml 6 (#6).
- MSRV 1.89 over our 1.88. CI red on Clippy, notices, tests and type-check, but the run is from 2026-11-02, before the CI fixes, so the red is weak evidence.
- deny.toml ignores RUSTSEC-2026-0370/0371/0372 (zvariant, zbus_names, zvariant_derive "unmaintained") that come from zbus 5; a migration would let us drop them, which is the one real upside.
- Size: unmeasured. The service has about 2 kB of headroom and zbus is a large part of its binary, so any change in generated code needs a measured budget decision.
- Do it later as one planned change: zbus 6 + zbus_xml 6 + MSRV bump + size measurement + removing the three ignores.

### #4 eframe 0.31 to 0.37: Close

- Six minors of a 0.x line, so each is breaking. Touches the call card (WS_EX_NOACTIVATE hook via `eframe::Frame`), the accesskit feature, Wayland/X11 backends and the Nothing-OS-style UI code. 0.37 headlines colored emoji and system-font loading (new `egui_system_font_provider` and fontique dependencies), which adds code and would grow the binaries; the bundled-font licence note (OFL and Ubuntu font) would need rechecking.
- MSRV 1.95 over our 1.88.
- CI red on Clippy on all three OS, notices and type-check (2026-11-04).
- Upside to remember: this is also the route to clearing the RUSTSEC-2026-0192, 0194 and 0195 ignores (ttf-parser, quick-xml 0.30 via accesskit/atspi/zbus_xml 4). Those ignores are reviewed by 2027-02-01, so the egui upgrade should be planned before then.
- UI-only, so no Android effect, but ctl and service budgets are not touched either (bridge-apps is separate).

### #5 windows-future 0.3 to 0.100: Close

- Versioning scheme jump inside the windows-rs family; it should move with the `windows` crate family and the call backend for Windows, not alone. MSRV 1.95.
- CI red on Windows Rust, type-check, notices and Performance budget (2026-11-04).
- Windows only; no Linux service size effect, no Android effect.

### #6 zbus_xml 5 to 6: Close

- Cannot be merged separately from zbus 6 (#2). MSRV 1.89. Same plan as #2.

### #7 Kotlin 2.2.21 to 2.4.21: Defer

- The group is called minor-patch, but 2.2 to 2.4 is two Kotlin language releases (K2 compiler behaviour, deprecations turning into errors, Compose compiler plugin and KSP/kapt version coupling). The 2.4.21 release notes are bug fixes (JS, Wasm, reflection, serialization plugin) with nothing that fixes a Linux/Android-relevant security issue.
- CI red from 2026-11-02, before the CI fixes, so no useful signal until rebased and rerun.
- Needs a check that the Compose compiler (now part of Kotlin), AGP 8.13 and lint all accept 2.4. Do it as one Android toolchain change with #8 and #9, or on its own after a rebase run.
- If the owner wants a fresh signal first (this comments on the PR, so it needs a go): `gh pr comment 7 --body "@dependabot rebase"`.

### #8 AGP 8.13.2 to 9.5.0: Close

- Major. AGP 9 requires Gradle 9, ships a new DSL and built-in Kotlin, and removes legacy APIs the app's build.gradle may still use. Needs Gradle 9 (#9), and probably the Kotlin bump (#7), in one deliberate change.
- CI red from 2026-11-02 (no useful signal).

### #9 Gradle wrapper 8.13 to 9.8.2: Close

- Major. Gradle 9 runs on JVM 17+, uses Kotlin 2.2 for build scripts and makes the configuration cache the recommended mode. Pair with AGP 9 (#8). 9.8.2 itself is a patch of a stable line (Java 27 support).
- CI red from 2026-11-02.

## Commands

Safe to run once the owner says go. Order matters: apply the Dependabot config first if you want closes to stick (see triage 2 for the proposed `.github/dependabot.yml`, still unapplied).

```
# 1. merge the only green, low-risk PR
gh pr merge 1 --squash --delete-branch

# 2. close the migrations with a message; "ignore this major" is the Dependabot comment that also stops re-opens
for n in 2 4 5 6 8 9; do
  gh pr comment "$n" --body "@dependabot ignore this major version"
done
# or, to close without Dependabot rules (it may reopen on the next release):
# for n in 2 4 5 6 8 9; do gh pr close "$n" --delete-branch --comment "Planned migration, tracked in docs/reports/dependabot-triage-3.md"; done

# 3. Kotlin: leave open, or ask Dependabot for a fresh run
gh pr comment 7 --body "@dependabot rebase"
```

Note for #2 and #6: if you ignore the major of zbus, remember to remove that ignore when the migration is planned. Using `ignore this major version` on #2 and #6 together keeps zbus and zbus_xml aligned.

## Owner decisions

- [ ] Merge #1 now, or wait for the current rc/release window to end.
- [ ] Close #2, #4, #5, #6, #8, #9 (with ignore-major, or plain close).
- [ ] Keep #7 open for a rebase run, or close it with the other Android bumps.
- [ ] Apply the `.github/dependabot.yml` from triage 2 (limit 2, ignores) so the closes do not come back.
- [ ] Decide when to plan the two real migrations: zbus 6 + zbus_xml 6 (needs rust 1.89) and egui 0.37 (needs rust 1.95); both retire RUSTSEC ignores that are to be reviewed by 2027-02-01.
