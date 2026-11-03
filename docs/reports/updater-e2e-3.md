# Updater end to end, round 3

Scope: the whole update chain, desktop (`bridge-update`) and Android (`UpdateChecker` / `UpdateModel`), checked against
local fake GitHubs only (the Rust mock in `crates/bridge-update/tests/mock_server.rs`, the Python replica
`scripts/e2e/github_replica.py`, a loopback socket server in the Android unit tests). Nothing was published; the
live service and its config were not touched (own state dir, own ports, scratch directory). The only contact with
real GitHub was read-only: `gh api repos/fspecii/geekbridge/releases` and the `SHA256SUMS` of v0.1.0-rc.1, used to
model the payloads.

## The real state, and the defect it exposed

`https://api.github.com/repos/fspecii/geekbridge/releases/latest` answers **404** today: the only release is
`v0.1.0-rc.1` (`prerelease: true`) and GitHub never returns pre-releases there. Before this round both clients
treated that 404 as "private repository or no releases": a desktop or phone on `0.1.0-rc.1` said
*"No public release found: the project repository may be private or has no releases yet."*, which is wrong for a
public repository. Now, on 404/403 the checker reads `releases?per_page=30` once:

* list readable (also `[]`, also only pre-releases) -> **"GeekBridge vX is up to date."** (desktop Details panel,
  `geekbridge-ctl update status`: "no stable release published - you are up to date"; Android card: "GeekBridge
  0.1.0-rc.1 is up to date.")
* list unreadable (404/403) -> "No public release found" as before (private or misspelt repository)
* list rate limited -> rate-limit error (not "no public release")

Cost: one extra anonymous request, only in the 404 case, behind the existing 5 minute cooldown.

## Findings per question

Desktop (`crates/bridge-update`):

| Question | Result |
| --- | --- |
| Asset per OS/arch | Correct against the real asset set (installer tarball, `.deb`, Arch `.pkg.tar.zst`, both APKs, SBOM, `SHA256SUMS`, plus Windows zip and the macOS tar.gz x2 that `build-binaries.yml` makes): Linux x86_64 -> `geekbridge-linux-x86_64.tar.gz` (not the installer tarball, deb or pkg), Windows x86_64/aarch64 -> the zip (ARM via emulation), macOS -> the matching tar.gz, Linux aarch64 -> nothing (no APK, no pkg). Test `realistic_stable_release_picks_the_right_asset_per_platform_and_verifies_digests`. |
| SHA-256 | Mandatory `SHA256SUMS` line; hashed while written; mismatch or size mismatch deletes the file (pre-existing tests). **New:** GitHub's `digest` field is parsed (`Asset::api_digest`); if present and different from `SHA256SUMS` nothing is offered ("GitHub's digest ... and SHA256SUMS disagree"). Real rc.1: all 7 `digest` values equal the `SHA256SUMS` lines. |
| Prerelease/draft | Never offered: flags, or a pre-release tag with a clean flag (tests, now also with the full real payload shape). |
| Ordering | semver precedence: `0.1.0` offered to `0.1.0-rc.1`; `rc.2` not offered to `rc.1`; `0.9.9 < 1.0.0-alpha < ... < rc.1 < 1.0.0`. |
| Rate limits | 403 with `X-RateLimit-Remaining: 0`/`Retry-After`, 429 + `Retry-After: 1800` ("about 30 min"); backoff clamped 1 min..24 h, no request while backing off, also for manual checks; a rate-limited release *list* is reported as a rate limit. |
| ETag | `If-None-Match` sent after the first answer; 304 reused (e2e: 304 seen at the replica). A release with no checksum is deliberately not pinned behind its ETag. |
| Offline | **Fixed wording**: was "Connection refused (os error 111)", now "Couldn't reach 127.0.0.1 (...). Check your internet connection." The cached offer survives (state stays `Available` plus the problem text), a first check offline is an error (never "up to date"), and the next check works when the network is back. |
| UI states | Up to date / Update available vX (banner, Download, Skip, Later) / Downloading N% / "Downloaded and checked against the published SHA-256. Open it to install." + Show file / error appended to the summary / "No public release found". Tests added in `update_ui.rs`. |

Android (`UpdateChecker`, `UpdateModel`, `UpdateSection`):

| Question | Result |
| --- | --- |
| Same semantics | Same parser, ordering, 304/ETag, 403/429 -> rate-limit text, size and host limits. |
| versionCode vs semver | Offer needs `semver >` **and** `versionCode >`: rc.1 = 10051, 0.1.0 = 10099, 0.1.1 = 10199 (`scripts/version-code.sh --self-test`, `UpdateModelTest`, now also an end-to-end check with the real rc.1 payload: phone on rc.1 is offered 0.1.0, phone on 0.1.0 is up to date). |
| Defect fixed | A release flagged `prerelease`/`draft` produced a **warning "Not a published release."**; the desktop says "up to date". Now `UpToDate`. |
| Digest | Same `digest` cross-check as the desktop ("GitHub's checksum for the app and the release's SHA256SUMS disagree"). APK choice is `geekbridge-android.apk`, never the arm64 split. |
| Installer handoff | Verified APK re-hashed right before install, then `ACTION_VIEW` with a one-time read grant through the non-exported, read-only `UpdateFileProvider` (authority `<applicationId>.updates`); `REQUEST_INSTALL_PACKAGES` declared; `canRequestPackageInstalls()` false -> message + `ACTION_MANAGE_UNKNOWN_APP_SOURCES` for the package (manual-path message if no activity). Unchanged, reviewed. |
| Signature mismatch | `SignerCheck` compares the downloaded APK's certificate with the installed app before showing Install, deletes the file and says "The update is signed with a different key (...) than the installed app (...). Android would refuse it ... Uninstall and reinstall the release APK to switch keys; app data is lost." Debug builds get the dedicated "developer (debug-signed) build ... uninstall this build once" text, so a debug build never ends up auto-updating to a release build (and nothing ever installs silently). Covered by the existing `SignerCheckTest`. |
| Download resumption | Not implemented, by design: no `Range`, a failed download deletes the partial file, a retry starts over (about 6 MB). New test `anInterruptedDownloadLeavesNothingAndARetryStartsCleanly` pins that behaviour (no partial, no leftovers, retry works). |
| Storage cleanup | **Gap fixed**: the verified APK stayed in `cache/updates/` forever after the update installed (the card forgets its offer on restart). Now `UpdateStorage.purge`: on `MY_PACKAGE_REPLACED` everything goes, at process start anything older than 24 h. Unit tests `UpdateStorageTest`. |
| Offline | Failed (`Couldn't reach GitHub: ...`), never "up to date"; failures do not stamp the 12 h auto-check clock. |

## Verification

* New tests: Rust `mock_server.rs` (+8 including the verbatim rc.1 fixture `tests/fixtures/github_release_v0.1.0-rc.1.json`),
  `update_ui.rs` (+2), Android `UpdateCheckerTest` (+4, one amended), `UpdateModelTest` (+1, one amended),
  `UpdateStorageTest` (2), fixture copy under `android/app/src/test/resources`. Each defect test failed before the fix
  (404-fallback, digest, offline wording, flagged prerelease, purge) and passes after.
* `scripts/e2e/updater-state.sh` drives a real `geekbridge-service` + `geekbridge-ctl` (debug build, v0.1.0) against the
  replica, which now also serves `GET /repos/o/n/releases`: only rc.1 published -> up to date (one `/latest` 404 + one list
  200); `v0.1.1` stable appears -> right asset offered with its SHA-256, downloaded, verified, 0700 directory; second check
  answered 304; 403 rate limit and 429 `Retry-After: 120` ("about 2 min") with no request while backing off; replica killed ->
  "Couldn't reach ..." and the offer is kept; replica back -> recovers; `notfound` mode -> "No public release found".
  All checks PASS.
* The built-in service is v0.1.0 (the workspace version), so "a running rc.1 says up to date" is proven by the unit/integration
  tests that construct an `Updater` / `UpdateChecker` for `0.1.0-rc.1`, not by the e2e service.

## Not covered / notes

* Real installation of the downloaded APK on a device (system installer UI, unknown-sources screen) needs a phone; the earlier
  emulator run (`scripts/e2e/update-install.sh android`) covers it and was not re-run here.
* The Windows asset in a local release is `geekbridge-windows-x86_64-gnu-experimental.zip`; the selector would offer it to
  Windows users as the update (only candidate). It is still SHA-256 verified; flagging in case that should not be auto-offered.
* A phone/desktop on rc.1 will not be told about `rc.2` (pre-releases are never offered, by design).
