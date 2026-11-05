# Updater review (static)

Scope: `crates/bridge-update` (lib, http, release, version, dev), the Android update classes
(`UpdateChecker`, `UpdateModel`, `UpdateSection`, `SignerCheck`, `UpdateFileProvider`), `dev-release.yml`,
`release.yml`, `scripts/release-dev.sh`, `docs/updates.md`. Method: reading only. Bash was unusable in the
review session (EDQUOT on the temp volume), so **nothing was compiled or run**: the edits below are small and
mechanical but must go through `cargo test -p bridge-update` and `./gradlew :app:testDebugUnitTest` before pushing.

## What the updater actually does (answers "what is updated, how")

* **Desktop (Linux, Windows, macOS): nothing is installed.** The service checks, downloads the archive into
  `<state dir>/updates/geekbridge-update-<random>/` (0700 on Unix), hashes while writing, makes it read-only and
  shows the path. The user extracts it and copies the executables by hand (docs/updates.md). So there is no atomic
  replace, no service restart, no privilege change, no rollback and also no half-updated install. If "reliable
  autoupdater" means the desktop replaces itself, that is **not built**; see finding D1.
* **Android:** download, size + SHA-256 + signer + package + versionCode checks, then the system installer opens
  through a non-exported `ContentProvider` with a one-shot read grant; the user confirms in Android's installer.

## What is solid (checked, no change needed)

* Stable never takes prereleases or drafts: `releases/latest` plus `draft || prerelease` plus
  `version.is_prerelease()` (lib.rs `fetch_latest`; Android `evaluate`: `unpublished`, `remote.isPrerelease`).
  Three independent checks on each side.
* Downgrade and replay: desktop offers only `version > current`; Android needs `remote > local` **and**
  derived `code > installed`, and `SignerCheck.identity` re-checks the real APK (package name, versionCode newer)
  before Install; Android itself refuses a downgrade. An old release served again is "up to date".
* Redirects: every hop is re-checked against an exact-host https:443 allow-list (both clients); `http`, userinfo,
  other ports and sibling hosts (`raw.`, `gist.`, `user-images.`) are refused; the token goes to `api.github.com` only.
* Download integrity: size cap from the API, `written == asset.size`, SHA-256 against `SHA256SUMS`, cross-checked
  with GitHub's per-asset `digest` (a disagreement offers nothing); partial files are deleted on every failure
  (desktop removes the whole private dir); no resume, so no corrupt-resume class of bugs.
* Re-hash right before use on both platforms (`verified_installer`, `startInstall`).
* Signer: Android compares signer set and v3 lineage of the **downloaded file** with the installed app and
  fails closed when either is unreadable. Dev APK is debug-signed with the owner's key; a phone with another key
  gets an explanation, not an install.
* Clock skew: future `last_checked` ignored (desktop, Android `autoCheckDue`), ETag keyed by running version and
  channel, dev code uses commit count not time.
* Version parsers (Rust and Kotlin) are strict semver with the same rejects and precedence tests.

## Findings, ranked

### High

**D1. No desktop self-update.** `lib.rs` module doc and docs/updates.md: "Nothing is executed, installed or
replaced". Scenario: a Windows/macOS user sees "Update available", downloads, must unzip and overwrite running
executables by hand (on Windows the running `.exe` cannot be overwritten at all; service must be stopped first).
Fix (not minimal, design needed): per OS, a helper that (1) verifies again, (2) extracts to a sibling staging dir
on the same volume, (3) stops the service, (4) renames old -> `.old`, new -> final (atomic per file; on Windows
`MoveFileEx` with delayed replace or a small updater exe), (5) starts the service, (6) keeps `.old` for rollback.
Linux user-level install via `~/.local/bin` is the easy first target (rename is atomic and a running binary can be
replaced). Package-installed copies (`.deb`, Inno, dmg) should keep going through their installers.

**A1. Android only learns about an update when "Diagnostics & about" is opened.** `UpdateSection.onStart` is the only
trigger and the card lives in `MainActivity`, which Home does not show. A user who never opens it never hears of a
fix; there is no notification, badge or Home chip. Scenario: a security fix ships and most phones stay on the old
build. Fix: run the (already ETag-cheap) check from Home at most once per 12 h and show a Home chip, and optionally
one `JobScheduler` periodic job (network-required, unmetered) posting a notification through the existing
`POST_NOTIFICATIONS` channel. Not done here: it touches Home and notification code outside my scope.

**R1. Stable signing key pin is optional in CI.** `release.yml` (sign step) only warns when `vars.ANDROID_SIGNER_SHA256`
is unset. Scenario: a wrong or regenerated keystore secret publishes a release that every installed phone then rejects
("App not installed"), and the failure is found by users. Fix: `exit 1` when the pin is empty. **Fixed afterwards**:
`release.yml` now refuses to publish a release-key build unless `ANDROID_SIGNER_SHA256` is 64 hex characters and equals the
signer of the built APK.

### Medium

**D2. Rate-limit pause and failed-attempt time were in memory only** (`Runtime.backoff_until`, `last_attempt_unix`).
Scenario: the service crashes or restarts in a loop (systemd `Restart=`, laptop resume) while offline or limited:
each start asked GitHub again after 15 s, ignoring the pause the docs promise, and 60 anonymous requests/h are shared
by everyone behind one NAT. **Fixed**: both fields are now in `Persisted` (serde default, old files load), the pause is
ignored if it is more than 24 h ahead (clock set forward then back), and the scheduler saves the failed attempt.
Test: `a_rate_limit_pause_is_persisted_and_a_wild_one_is_ignored`.

**D3. A rate-limited `SHA256SUMS` fetch was silent.** `attach_installer` read only `status == 200`; a 429/403-with-limit
on the checksum file meant "no checksum" (not downloadable) and no backoff, so the next check asked again. **Fixed**:
`f.rate_limited` returns `Error::RateLimited` (the check reports it and pauses).

**N1. Android: no overall deadline.** `HttpURLConnection` read timeout is per read; a peer sending a byte every
14 s kept `busy` set (buttons disabled) for hours. **Fixed**: `readBounded` has an overall deadline (60 s small
responses, 15 min APK) and reports "interrupted".

**N2. Android: failed or rate-limited checks were retried on every screen visit.** `last_check_ms` is only written on
success, so the 12 h gate never closed after a failure; CGNAT mobile IPs make the 60/h limit easy to hit, and the
docs claimed a rate-limit pause that only the desktop had. **Fixed**: `last_attempt_ms` plus `attemptDue` (15 min
after any automatic attempt). Test in `UpdateModelTest`. A true persisted `Retry-After` is not kept on Android.

**P1. Dev publish can be left half done by the workflow's own cancellation.** `dev-release.yml` has
`concurrency: dev-release, cancel-in-progress: true` for the whole workflow, including the `publish` job. A push while
publish runs cancels it between "upload assets + SHA256SUMS" and "upload manifest / move tag"
(`release-dev.sh` publish_all). Result: new APK and SHA256SUMS next to the old manifest and tag until the next
run; clients stay safe (manifest decides, APK versionCode and signer are re-checked on the phone) but a desktop
download can hit a hash mismatch in that window. Fix: give `publish` its own `concurrency: {group: dev-publish,
cancel-in-progress: false}` (and keep the build jobs cancellable). **Fixed afterwards**: the build jobs have their own
cancellable groups and `publish` uses `dev-release-publish` with `cancel-in-progress: false`
(`scripts/tests/workflow-secrets-test.sh` checks it). `release-dev.sh`
already refuses an older commit than `origin/main` (replay of an old run is blocked).

**P2. Dev manifest is not bound to the assets.** `dev-manifest.json` has `commit` and `versionCode`, but no hash of the
APK or archive; the binding is only "same release + SHA256SUMS". Impact is limited because the phone verifies the
real APK (signer, package, newer code) and SHA256SUMS lists the manifest too. Hardening: put `apkSha256` into the
manifest and have clients require equality with `SHA256SUMS`, which makes a mixed upload visible instead of
accidentally consistent.

**T1. Bundled root store (desktop).** TLS uses `webpki-roots` baked into the build; the OS store and proxy are not used. A
long-lived install whose roots go stale (GitHub moves CA) can no longer reach the update server, i.e. the updater
dies exactly when it is most needed; corporate TLS-inspecting proxies also fail. Mitigation: the Android and Linux
packages already update by hand; document the symptom ("couldn't reach github.com") and release a point build when
GitHub's chain changes. Optionally try `rustls-native-certs` as a fallback.

### Low

* **L1. Docs said any `*.githubusercontent.com` was followed;** code has an exact five-host list. **Fixed in docs.**
* **L2. `SignerCheck.identity` is fail-open** when either identity is unreadable (`return Verdict.Match`). The signer
  check just before it fails closed and Android enforces both rules, so impact is only a later, cryptic error.
  Suggest `Unreadable(false)` for `offered == null`.
* **L3. Android disk full** is reported as "interrupted" (`IOException` branch of `download`). Check `ENOSPC`
  (`e.message?.contains("ENOSPC")`) or `usableSpace < size` before the download and say "not enough space".
* **L4. Metered network:** no check on either platform. Downloads are tap-initiated and the checks are tiny
  (ETag, 304), so this is cosmetic; a "mobile data" hint before a 6 MB APK would be polite
  (`ConnectivityManager.isActiveNetworkMetered`).
* **L5. `releases/latest` is "newest created non-prerelease", not "highest semver".** A backport `v0.1.5` published after
  `v0.2.0` hides 0.2.0 from users on 0.1.4 until a newer release appears. Process rule: publish in order, or fall
  back to `releases?per_page=30` and take the maximum semver.
* **L6. No jitter** on the 24 h desktop schedule (all clients that started together check together). Add +/- 10 %.
* **L7. macOS:** the archive is fetched without the quarantine attribute, so Gatekeeper never sees it; fine for a CLI
  tarball, but a future self-updating app bundle must be signed and notarized.
* **L8. Dev desktop with unknown count** (a stable build) is offered the dev build whenever the commit differs, which can
  be an older dev build. Documented; only matters after switching channel.
* **L9. Android dev-signer pin lives only in CI/`release-dev.sh`**, the app trusts "same as installed". That is correct
  for updates (Android enforces it too); a debug-build-only compile-time pin (`DEV_SIGNER_SHA256`) would add a second
  barrier against a phone first installed from an untrusted debug APK.

## TOCTOU summary

Desktop: verify -> user opens. The file lives in a 0700 directory as a read-only file; the same user can swap it, so
`verified_installer` re-hashes right before anything opens it (and nothing launches it). Android: hash and signer
check -> `startActivity` on the same private-cache file served only through the non-exported provider with a read
grant; another app cannot write there. Residual: the installer opens the file after the check, same UID only.

## Edits made

* `crates/bridge-update/src/lib.rs`: D2, D3 (+ test).
* `android/.../UpdateChecker.kt`, `UpdateSection.kt`, `UpdateModel.kt`, `UpdateModelTest.kt`: N1, N2 (+ test).
* `docs/updates.md`: allow-list wording, deadlines, rate-limit persistence (L1).
* Not touched in the review itself: `.github/workflows/*` and `scripts/release-dev.sh` (R1 and P1 were applied later, see
  above; P2 is still a suggestion).
