# CI queue audit (2026-11-03)

## Snapshot
At the start: 9 queued and 3 in-progress runs, 11 of them check.yml (3 legs of Rust x Linux/Windows/macOS plus perf, cross-targets, supply-chain, Android and coverage per run, so roughly 9 jobs each), one emulator-e2e dispatch. Of the last 60 runs, about 45 were check.yml. A single push on a Dependabot branch started check.yml twice (push and pull_request), and pushes to main arrived every 2-5 minutes with nothing superseding the older runs. No branch protection requires status checks, so cancelling runs weakens nothing.

## Who uses runner time
- check.yml: the main consumer. Per run: Windows and macOS legs (billed at a multiplier on private repos, slow queue on public), Android (up to 60 min), coverage (up to 90 min, non-blocking, ran on every PR and push).
- Dependabot: 9 open PRs, each running check.yml twice (push + pull_request). Weekly schedule, grouped; noise is the double run, not the cadence.
- windows-debug.yml: dispatch only (plus the ci-windows-debug branch); about 10 runs today, 15 min cap each. Left as is: parallel dispatches with different inputs must not cancel one another.
- build-binaries.yml, release.yml: dispatch only. Untouched.
- emulator-e2e.yml: nightly plus dispatch, already has a concurrency group without cancellation. Untouched.

## Runs cancelled (all check.yml, none a release or tag run, none the head of main 29bf250c)
41667182080, 41667170461 (dependabot actions af660909), 41666932020 (main 1cf6abca), 41666538100 (main bc8c86f4), 41665847055 (main a20091ea), 41665222245 and 41665236934 (dependabot actions ffc14087), 41664685274 (main 8c33bcbc), 41663945575 and 41663932810 (dependabot base64 3d67ff72).
Left running: check.yml on main 29bf250c (41667954976) and Emulator end-to-end 41666540220 (a deliberate dispatch on bc8c86f4, in progress; cancel by hand if not wanted).

## Changes
- check.yml: concurrency group per workflow and branch or PR number with cancel-in-progress (check.yml publishes nothing). push limited to main and ci-windows-debug, so PR branches run once (pull_request). paths-ignore for docs/** and the root prose files (README, SECURITY, CONTRIBUTING, CODE_OF_CONDUCT, ARCHITECTURE, SUPPORT). CHANGELOG.md and THIRD_PARTY_NOTICES.md are not ignored because a test and the notices check read them. Code changes run every job as before.
- coverage job: skipped on pull_request; runs on main pushes and manual dispatch. It is non-blocking and the longest job. No schedule was added.
- New docs.yml: runs scripts/check-doc-links.sh on every push to main and every PR, so docs-only changes still get a (5-minute, Linux) check.
- Not changed: release.yml, build-binaries.yml, windows-debug.yml, emulator-e2e.yml, dependabot.yml.

## Caveats and suggestions
- With cancel-in-progress on main, a burst of main pushes only completes the last run; a failure is attributed to the newest commit, which may include earlier ones. Switch to `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}` if per-commit results on main matter.
- Dependabot: consider `open-pull-requests-limit: 2` per ecosystem, and closing the major bumps that cannot pass (eframe 0.37, gradle 9, AGP 9) until planned.
- Windows and macOS legs stay on every code change; the big queue delay is GitHub-side runner capacity, which this reduces by removing roughly half the runs.
- actionlint was not installed; both workflow files were validated as YAML and reviewed by hand.
