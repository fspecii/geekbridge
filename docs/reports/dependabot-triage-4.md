# Dependabot triage 4 (2026-11-06)

Read-only refresh of [triage 3](dependabot-triage-3.md) against origin/main 2eaadbc47. Nothing on GitHub was changed: no PR was merged, closed, commented on or rebased. Every command below is a proposal for the owner to run after a go. Eight Dependabot PRs are open (#1, #2, #4, #5, #6, #7, #8, #9). Size numbers were built and measured in scratch worktrees (`/home/ambsd/.cache/gb-dep-*`, own `CARGO_TARGET_DIR`, `scripts/perf-check.sh --build`), then removed.

## What changed since triage 3

- Main is 102 commits ahead of PR #1's base now and it is still `MERGEABLE`/`CLEAN`; its CI (2026-11-05) is still all green.
- #2 and #6 were rebased by Dependabot on 2026-11-06 (9 commits behind main). #2 now targets zbus 6.2.1 (triage 3 said 6.2.0). Both have a fresh CI run, so the red there is now real evidence, not a stale run.
- base64: PR #3 is closed (2026-11-04) and main is on base64 0.23 in every crate (core, kde, callwifi, android, service), so one base64 copy. No open base64 PR; nothing to do.
- Budgets are now service 7 560 000 B and ctl 1 790 000 B (triage 3 quoted 7 500 000 and 1 650 000). Main measures 7 511 192 B service (48 808 B headroom) and 1 769 736 B ctl (20 264 B headroom) locally; CI builds run about 17 kB above a local build.
- Dependabot alerts: none open (`gh api repos/fspecii/geekbridge/dependabot/alerts` returns 0 items). No bump here is a security fix.
- `.github/dependabot.yml` is still the old one (no ignore rules, no PR limit), so a plain close may be followed by a new PR for the next release.

## Summary

| # | Bump | Touches | Behind main | CI now | Size delta (service / ctl) | Recommendation |
|---|------|---------|-------------|--------|----------------------------|----------------|
| 1 | actions group (4 majors) | 5 workflow files | 102 | all green (2026-11-05) | none (no Rust) | Merge |
| 2 | zbus 5.19.0 to 6.2.1 | Cargo.lock, 3 Cargo.toml | 9 | red: does not compile | not measurable: build fails | Close (plan the migration) |
| 4 | eframe 0.31.1 to 0.37.0 | Cargo.lock, bridge-apps | 244 | red (2026-11-04) | service/ctl none; UI build fails | Close (plan the migration) |
| 5 | windows-future 0.3.2 to 0.100.0 | Cargo.lock, bridge-calls-windows | 218 | red (2026-11-04) | none on Linux (Windows-only) | Close |
| 6 | zbus_xml 5.2.1 to 6.0.0 | Cargo.lock, bridge-calls | 9 | red: notices, one test script | +16 896 B / +544 B | Close |
| 7 | kotlin.android 2.2.21 to 2.4.21 | android/build.gradle.kts | 433 | red (2026-11-02) | none (no Rust) | Defer |
| 8 | com.android.application 8.13.2 to 9.5.0 | android/build.gradle.kts | 433 | red (2026-11-02) | none | Close |
| 9 | gradle-wrapper 8.13 to 9.8.2 | wrapper jar, properties, gradlew(.bat) | 433 | red (2026-11-02) | none | Close |

No PR is a conflict with main (all report `MERGEABLE`); the red is build, notices or stale-base red, not merge conflicts.

## Gates and crate facts

- MSRV: workspace `rust-version = "1.88"`. From crates.io: zbus 6.2.1 needs 1.89, zbus_xml 6.0.0 needs 1.89, eframe 0.37.0 needs 1.95, windows-future 0.100.0 needs 1.95. All four exceed 1.88, so each needs a deliberate `rust-version` bump.
- Licences: zbus MIT, zbus_xml MIT, eframe and windows-future MIT OR Apache-2.0; all allowed by deny.toml. cargo-deny itself passes on #2 (bans, licenses and sources ok; only unmatched-licence-allowance warnings for Ubuntu-font-1.0, since eframe is not in the lock of that run).
- Every Cargo.lock-changing PR fails the `Supply chain (cargo-deny, notices)` job at `scripts/gen-notices.sh --check` (THIRD_PARTY_NOTICES.md not regenerated). That is a mechanical fix, not a defect in the bump, but it means no Rust Dependabot PR can go green by itself.
- Android: `bridge-android` does not depend on zbus, zbus_xml, eframe or windows-future. `cargo tree -p bridge-service` shows neither eframe nor windows-future, so #4 and #5 cannot change the service or ctl size.
- Release notes (from the PR bodies): zbus 6 merges zvariant and zbus_names into zbus (`zbus::wire`, `zbus::names`), changes the MSRV, removes the `blocking` module path used here, and adds validation of received messages (nesting limits, header checks) plus read-ahead performance work. zbus_xml 6 changes its API to borrowed strings and iterators and a unified error type. Upgrade guide: https://z-galaxy.github.io/zbus/upgrading-to-6.html

## Per-PR

### #1 actions group: Merge

- upload-artifact 7.0.2, download-artifact 8.0.2, setup-java 6.1.0, setup-android 4.0.4 in build-binaries, check, dev-release, emulator-e2e and release workflows. Workflow plumbing only; no Rust, Android source, size or licence effect.
- CI on head 670220b04 (2026-11-05 12:39): Rust on Linux, macOS and Windows, Performance budget, Windows/macOS type-check, cargo-deny and notices, Android build, docs links all pass; Coverage and CodeRabbit skipped by design. Mergeable and clean against current main.
- Triage 2 verified the four pinned SHAs against the upstream tags. Security relevance: none (no advisory), supply-chain posture unchanged.
- Remaining risk, unchanged: the artifact hand-off between jobs in release and dev-release is only exercised at tag or rolling-build time, not by check.yml. Merge when no release is in flight and watch the next `Dev build (rolling)` run.

### #2 zbus 5.19.0 to 6.2.1: Close (planned migration)

- Touches `zbus = "5"` in bridge-calls, bridge-phone-sim and bridge-service. Rebased 2026-11-06, mergeable.
- CI: Rust Ubuntu and Performance budget fail with `error[E0432]: unresolved import zbus::blocking`, `cannot find blocking in zbus` and `no method named map_err found for zbus::match_rule::Builder`; notices fail. Windows, macOS and type-check pass because the Linux-only D-Bus code is not built there. A local grep finds 25 `blocking::` / `zbus::blocking` sites that need porting.
- Size: not measurable, because the tree does not build. Triage 3's open question stays open. Note that zbus 6 folds zvariant and zbus_names into zbus, which should remove code duplication, but #2 alone does not show it: its lock still carries zvariant 5.15.1, zbus_names 4.3.5 and zvariant_derive 5.15.1 through zbus_xml 5.2.1.
- Advisories: no vulnerability. The RUSTSEC-2026-0370/0371/0372 ignores (zvariant, zbus_names, zvariant_derive unmaintained) can only be dropped after zbus_xml 6 (#6) lands with it, and zbus 4.4.0 (via the egui accessibility chain) still stays until eframe moves.
- Plan: one change = zbus 6.2.x + zbus_xml 6 + port of the 25 blocking sites + `rust-version` 1.89 + gen-notices + size measurement + remove the three ignores.

### #4 eframe 0.31.1 to 0.37.0: Close (planned migration)

- Touches `eframe` in bridge-apps only (the geekbridge-messages and other UI binaries). Six minors of a 0.x line, each breaking. A local release build of bridge-apps on the PR fails with 55 errors: `drag_to_scroll` gone, `SidePanel`/`TopBottomPanel` moved, `eframe::App::update` replaced by `ui`.
- CI red on Rust (all three OS), type-check, notices (2026-11-04); 244 commits behind.
- Size: service and ctl unchanged (bridge-apps is not in their dependency tree; 0 matches for eframe in `cargo tree -p bridge-service`). The UI binaries could not be measured because the build fails. Baseline for later: geekbridge-messages is 8 068 256 B unstripped on main.
- MSRV 1.95. Release notes mention coloured emoji and system-font loading, which add dependencies; font licence note (OFL, Ubuntu font) would need rechecking.
- Upside: this is the route to clear RUSTSEC-2026-0192, 0194, 0195 and the paste ignore (reviewed by 2027-02-01) and to retire zbus 4.4.0 from the lock (about 50 old crates drop out in the PR's lock diff). Plan it before February.

### #5 windows-future 0.3.2 to 0.100.0: Close

- Touches bridge-calls-windows only, Windows-only. CI (2026-11-04, 218 behind) fails type-check and Windows tests: 19 mismatched types and `PhoneCallOperationStatus: RuntimeType` not satisfied, because windows-future 0.100 pulls windows-core 0.100 while `windows` stays on 0.62, giving two incompatible windows-core copies.
- Needs to move with the whole `windows` family and an MSRV bump to 1.95. Size effect on the Linux service and ctl is zero; Windows size unmeasured.
- No advisory.

### #6 zbus_xml 5.2.1 to 6.0.0: Close

- Touches `zbus_xml = "5"` in bridge-calls. Unlike #2, it builds, because the introspection use compiles unchanged, but zbus_xml 6 depends on zbus 6, so the lock then carries both zbus 5.19.0 and zbus 6.2.1 (plus zbus_macros twice and the new zbus_utils).
- Measured with `perf-check.sh --build`: service 7 511 192 B to 7 528 088 B (+16 896 B), ctl 1 769 736 B to 1 770 280 B (+544 B). Both within budget, but the service headroom falls from 48 808 B to 31 912 B locally (about 15 kB expected on CI), for no benefit. Idle RSS, startup, threads, fds and wakeups unchanged in the same run (RSS reading is noisy between runs).
- CI: Rust Ubuntu fails only in `scripts/tests/realdevice-kit-test.sh` (`FAIL served APK hash missing`), a repo script unrelated to the crate and presumably fixed on main since the PR is 9 commits behind; notices fail. Performance budget passes on CI.
- Cannot be adopted usefully without #2. Close in favour of the combined migration.

### #7 Kotlin 2.2.21 to 2.4.21: Defer

- Only `android/build.gradle.kts`. Group named minor-patch, but 2.2 to 2.4 is two Kotlin language releases (K2 behaviour, deprecations turning into errors, Compose compiler plugin version coupling). Release notes are bug fixes only; no security relevance.
- CI red from 2026-11-02 and 433 commits behind: the run is stale and gives no signal. Needs a rebase and a re-run, and a check that AGP 8.13 and lint accept 2.4.
- Commenting `@dependabot rebase` is a write to the PR, so it needs a go.

### #8 AGP 8.13.2 to 9.5.0: Close

- Major. Requires Gradle 9 (#9), ships new DSL and built-in Kotlin, removes legacy APIs. Must be one deliberate Android toolchain change with #9 and probably #7. CI red from 2026-11-02, stale.

### #9 Gradle wrapper 8.13 to 9.8.2: Close

- Major, changes gradle-wrapper.jar, properties, gradlew and gradlew.bat (binary jar: review the wrapper checksum when it is redone). Pair with AGP 9. CI red from 2026-11-02, stale.

## Commands (after a go)

Apply the Dependabot config first if closes should stick (triage 2 has the proposed `.github/dependabot.yml`; still unapplied).

```
# 1. the only green, low-risk PR
gh pr merge 1 --squash --delete-branch
gh run list --workflow "Dev build (rolling)" --limit 3     # check after the merge

# 2. migrations: ignore the major so Dependabot stops reopening (this also closes the PR)
for n in 2 4 5 6 8 9; do
  gh pr comment "$n" --body "@dependabot ignore this major version"
done
# alternative without Dependabot rules (it may reopen on the next release):
# for n in 2 4 5 6 8 9; do gh pr close "$n" --delete-branch --comment "Planned migration, see docs/reports/dependabot-triage-4.md"; done

# 3. Kotlin: leave open, or ask for a fresh run
gh pr comment 7 --body "@dependabot rebase"
```

Ignoring the zbus major on #2 and #6 together keeps zbus and zbus_xml aligned; remove that ignore when the migration is planned.

## Owner decisions

- [ ] Merge #1 now, or after the current release window.
- [ ] Close #2, #4, #5, #6, #8, #9 (ignore-major or plain close).
- [ ] Keep #7 for a rebase run, or close it with the other Android bumps.
- [ ] Apply the triage 2 `.github/dependabot.yml` (limit 2, ignores).
- [ ] Schedule the two real migrations: zbus 6 + zbus_xml 6 (rust 1.89, 25 `blocking` sites) and egui 0.37 (rust 1.95, 55 compile errors); both retire RUSTSEC ignores due for review by 2027-02-01.
