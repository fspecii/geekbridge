# Update checks

Both the desktop apps and the Android app can tell you when a newer GeekBridge release exists on GitHub
(`fspecii/geekbridge`). Nothing is ever installed or replaced automatically; there is no telemetry.

## What is contacted, and what is sent

| Request | Purpose |
| --- | --- |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases/latest` | the newest published, non-prerelease release |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases?per_page=30` | only when `releases/latest` answered 404/403: tells "no stable release yet" from "private repository" |
| `GET <SHA256SUMS asset URL>` (GitHub's release hosts, https only; see the allow-list below) | published digests, fetched only when a newer release has an installer for this system |
| `GET <installer asset URL>` | only after you click Download (desktop) or "Download and verify" (Android) |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases/tags/dev`, then `dev-manifest.json` | **instead of** `releases/latest`, only on the dev channel ([below](#dev-channel)) |

Each request carries a `User-Agent` of `geekbridge-updater/<version>` (Android:
`geekbridge-android-updater/<version>`), `Accept`, and, after the first answer, `If-None-Match` with GitHub's
ETag (a `304` does not count against GitHub's unauthenticated rate limit). No account, device id, install id,
IP-derived value or version history is sent beyond what any HTTPS request reveals. Redirects are followed only
to the same allow-list: https on port 443 to exactly `github.com`, `api.github.com`, `objects.githubusercontent.com`,
`release-assets.githubusercontent.com` and `github-releases.githubusercontent.com` (not other `*.githubusercontent.com`
hosts); responses are capped (1 MiB for release JSON, 64 KiB for checksums, 512 MiB / 200 MiB for installers) and every
request has an overall deadline (desktop: 30 s per check, 30 min per download; Android: 60 s per small response,
15 min per APK). On the desktop a rate-limit answer (403/429, also on the checksum file) pauses automatic checks
for the time GitHub names (at least a minute, at most a day); the pause and the time of the last failed check are
saved in `update-state.json`, so restarting the service does not skip them. On Android, after any failed automatic
check the next one waits 15 minutes (a screen visit does not retry at once); GitHub's rate limit is reported to the
user, not remembered beyond that.

## Choosing the release source

The built-in source is `fspecii/geekbridge`; a build can point elsewhere (`GEEKBRIDGE_RELEASE_REPO` /
`-Pgb.releaseRepo`, see [builds.md](builds.md#releases-and-versioning)), so releases may live in a separate
public releases-only repository. At run time:

* Desktop: `GEEKBRIDGE_UPDATE_REPO=owner/name` for `geekbridge-service` (malformed values are ignored).
* Android: debug builds only, the `updates` preference `debug_repo`
  (`adb shell run-as dev.geekbridge.android` and edit `shared_prefs/updates.xml`), like `debug_base_url`.

### Only pre-releases published (the state of `v0.1.0-rc.1`)

`releases/latest` never returns a pre-release or draft, so while the only release is `v0.1.0-rc.1` it answers
**404**. The checkers then read the release list once; a readable list (even empty, or only pre-releases)
means "no stable release to offer", shown as **"GeekBridge vX is up to date."** (desktop Details panel; Android
card; `geekbridge-ctl update status`: "no stable release published - you are up to date"). A release candidate
is never offered to a release candidate (`rc.1` does not see `rc.2`), but the stable `0.1.0` is offered to
`0.1.0-rc.1` (semver: a pre-release sorts below its release; Android version codes 10051 < 10099). Only when
the list is unreadable too is it the "No public release found" state below. Both requests are 404/200 answers
that count against GitHub's 60 requests/hour for anonymous callers, which is why the second one happens only
in this case and a manual re-check inside 5 minutes is answered from memory. GitHub also publishes a
`digest` (`sha256:<hex>`) per asset; when it disagrees with `SHA256SUMS` nothing is offered.

### Private repository, 404 and 403

An unauthenticated request for a private repository's releases gets **404** (and 403 when access is refused).
That is reported as its own state, not "up to date" and not a failure:
*"No public release found: the project repository may be private or has no releases yet."*
(`geekbridge-ctl update status`, the Details panel, the Android Diagnostics update card). It is re-asked
no more than every 5 minutes on a manual check and otherwise on the normal ~daily schedule, so there is no
retry storm. A real rate limit (429, or 403 with `Retry-After` / an exhausted quota) is still reported as
such and pauses checks as described above.

### Testers: `GEEKBRIDGE_UPDATE_TOKEN` (desktop only)

Set `GEEKBRIDGE_UPDATE_TOKEN` to a read-only fine-grained token (*Contents: read* on the one repository) in the
service's environment to see private releases. The token lives in memory only: it is never written to the state
file or logs (its `Debug` output is redacted). It is sent as `Authorization: Bearer` **only** to
`https://api.github.com` (release list and the API URL of assets); GitHub redirects that to a pre-signed
download host, which never receives it, and a redirect to any host outside the allow-list is refused.
Android has **no token support**: a secret shipped inside an APK, or kept in app storage, can be extracted by
whoever holds the app, so the Android updater only works with a public repository.

## Dev channel

Besides stable releases there is a **dev channel** for people who install development builds to debug or to try
new features: a rolling GitHub **prerelease with the fixed tag `dev`** that always carries the newest `main` build.
It is never "latest", never a version tag, and the stable channel never sees it (`releases/latest` skips
prereleases).

| Asset | What |
| --- | --- |
| `geekbridge-debug.apk` | the Android debug build, signed with the **developer's debug key** (below) |
| `geekbridge-linux-x86_64.tar.gz` | the Linux executables (same layout and name as a stable release, so the same selector and the same by-hand install apply); Windows and macOS archives when the CI artifacts were added |
| `SHA256SUMS` | digests of every asset above and of the manifest |
| `dev-manifest.json` | `{"commit": "<40 hex>", "versionCode": 20161, "builtAt": "2026-11-04T10:00:00Z", "notes": "..."}` |

**Which build is newer.** Dev builds have the version code `20000 + git rev-list --count HEAD`
(`scripts/version-code.sh --dev`; Android: `android/app/build.gradle.kts`, debug builds only). Main only grows, so the code
is monotonic. Android offers the dev build when the manifest's `versionCode` is higher than the installed one. The
desktop binaries embed their commit and commit count at build time (`crates/bridge-update/build.rs`; overridable with
`GEEKBRIDGE_BUILD_COMMIT` / `GEEKBRIDGE_BUILD_COUNT`) and offer the dev build when the commit differs **and** the
manifest's code is higher; a binary that does not know its count (a stable release, a source archive) is offered the
dev build whenever the commit differs. `geekbridge-ctl update status` shows the running commit.

**Choosing the channel.**

* Android: *App updates* card, **Update channel: Stable / Dev**. Debug builds default to **Dev**; release builds default
  to **Stable** and ask for confirmation before switching (the warning says Android will refuse a dev APK over a release
  build). The choice is the `channel` key of the `updates` preference; each channel has its own ETag cache.
* Desktop: `geekbridge-ctl update channel stable|dev`, the **Update channel** selector in the Details panel, or
  `GEEKBRIDGE_UPDATE_CHANNEL=stable|dev` in the service's environment (wins over the saved choice). Default `stable`.
  Switching drops the other channel's offer and any downloaded file. The banner reads "Dev build available: dev-<12 hex>",
  **Skip this version** skips that build only, and the download and by-hand install work as for a stable archive.

**Requests.** The same host as the stable check: `GET https://api.github.com/repos/<repo>/releases/tags/dev` (conditional,
ETag), then `dev-manifest.json` (at most 16 KiB; not re-fetched while the ETag is unchanged), then `SHA256SUMS` and the
installer only when a newer dev build exists. Rate limits, redirects, size caps and the offline behaviour are exactly
those of the stable check. A repository with no `dev` release is reported as up to date, not as an error.

**Signature, and why `versionCode` 20000+ is safe.** Android installs an update only over the same signing key and a
higher `versionCode`. The dev APK is debug-signed, with the key in `.tools/android-user/debug.keystore` on the owner's
machine, so phones that already run a debug build from there update in place; the key is never committed or uploaded
(only the signed APK is). The app compares the downloaded APK's signer with its own *before* offering Install and
explains a mismatch (another computer's debug key, or a release build) instead of letting the installer fail.
Stable APKs are signed with the release key, so a switch between the channels on one phone is always an uninstall
and a reinstall, whatever the version codes say; that is why dev codes may sit above the stable ones (stable 0.1.0 is
10099, 0.2.0 is 20099, dev is 20000 + commits). A debug-signed *stable* build (rehearsals only) below the dev code
cannot be installed over a dev build; `scripts/e2e/update-install.sh` builds its two rehearsal APKs with
`-Pgb.formulaVersionCode=1` for that reason. Debug builds from before the dev channel have code 10099, which is lower
than any dev code, so the first dev build installs over them.

**Trust, and what is shown.** The dev channel adds no trust beyond GitHub's account security. The manifest, the
`SHA256SUMS` file and the installer all come from the same release, so whoever can edit that release can change all
three; the checksum protects against corruption and a bad CDN edge, not against a compromised release. What is checked
where:

* **Android** is the strong case. Android installs an update only over the same signing key, and the app compares the
  downloaded APK's signer, package name and version code with the installed app's *before* offering Install
  (`SignerCheck`). A compromised release cannot get an attacker-signed APK onto a phone that runs a dev or release build
  signed by the owner's key; the user is told why instead. The pinned dev signer
  (`GB_DEV_SIGNER_SHA256` / `DEV_SIGNER_SHA256`) is enforced where the APK is made and published (`release-dev.sh`,
  `dev-release.yml`), not inside the app: the app's pin is the key of the build that is already installed.
  Switching a phone from a debug build to a release build (or the other way) is an uninstall, so the one moment a
  hostile APK could be accepted is when a user uninstalls on purpose to switch channels.
* **Desktop** archives are not signed. The app downloads into a private folder, verifies the SHA-256 (and GitHub's own
  digest, when both exist), and leaves installing to the user. The settings note says so.
* The manifest's `versionCode` only decides "newer?". It cannot downgrade anything: an APK that is not newer than the
  installed one is refused by the app and by Android.
* Release notes and the build time are shown with direction overrides and zero-width characters removed.

**Protecting the dev key.** `dev-release.yml` signs only on `main`, builds with no secret in the environment, and
decodes the keystore only in the signing step. A manual run on another branch still runs that branch's copy of the
workflow, so also store `DEV_KEYSTORE_B64`, `DEV_KEYSTORE_PASSWORD` and `DEV_KEY_ALIAS` as secrets of the *environment*
`dev-signing` (Settings, Environments; the `android` job names it, which is the only way it can read them) that allows
deployments from `main` only, and set the repository variable `DEV_SIGNER_SHA256`
(commands: [release-owner-steps.md](release-owner-steps.md), step 3a). The `publish` job runs in its own queue
(`dev-release-publish`) that a newer push never cancels, so a half-uploaded release cannot be left behind.
`release-dev.sh --publish --yes` refuses to publish an APK without a pinned signer (`--allow-unpinned` overrides, for
tests and a first publish from a new machine). `scripts/tests/workflow-secrets-test.sh` checks these rules.

**Publishing** (owner only): `scripts/release-dev.sh` builds locally and is a **dry run unless `--publish`**; the
GitHub Actions workflow `dev-release.yml` builds the desktop binaries on every push to main (artifacts only) and can
build and publish the APK too once the owner stores the debug keystore as the secret `DEV_KEYSTORE_B64`. See
[reports/dev-channel.md](reports/dev-channel.md) for the exact commands and the decisions.

## Version comparison

The release tag (`vX.Y.Z`) is parsed as semver and compared with the running build's version
(`env!("CARGO_PKG_VERSION")` on the desktop, `BuildConfig.VERSION_NAME` on Android). Prereleases and drafts are
never offered on the stable channel. On Android the version code derived from the tag must also exceed the installed one.
Single version source: see [builds.md](builds.md#releases-and-versioning).

## Desktop

* `crates/bridge-update` does the work inside `geekbridge-service`: a check about 15 s after start (skipped if
  the last one was under 10 minutes ago), then about every 24 h. Settings, the ETag and the last answer live in
  `<state dir>/update-state.json` (not secret).
* Turn automatic checks off: untick **Check automatically** in the app's Details panel, run
  `geekbridge-ctl update disable`, or start the service with `GEEKBRIDGE_NO_UPDATE_CHECK=1` (also stops the
  schedule). Manual checks still work when you ask for one.
* Test hook: `GEEKBRIDGE_UPDATE_BASE_URL=http://127.0.0.1:PORT` points the service at a mock release server
  (`scripts/e2e/mock_release_server.py`); any other value is ignored.
* `geekbridge-ctl update status | check | download | skip VERSION | enable | disable`.
* The apps show a banner "Update available: vX.Y.Z" with **Download**, **Release notes**, **Skip this version**
  and **Later**. **Download** is offered only when the release lists an installer for this OS/architecture
  *and* a `SHA256SUMS` line for it.
* The download goes into a fresh `geekbridge-update-<random>` directory (mode 0700 on Unix) under the per-user state
  directory (`$XDG_STATE_HOME/geekbridge/updates`, never the shared temp dir), is hashed while written, and is deleted if the size or SHA-256 differs from the published value.
  On success the file is made read-only and the app offers **Show file**. You then open/extract it yourself.
  No installer is executed, no files are replaced, no privileges are requested.
* TLS is rustls with the bundled Mozilla root set (`webpki-roots`); the operating system trust store and proxy
  settings are not used.

### Installing a downloaded archive by hand

The archive the checker fetches, `geekbridge-linux-x86_64.tar.gz`, holds the nine executables and `README.md` in one
directory; it has no `install.sh`, desktop entries or systemd unit (those come with the installer tarball and the `.deb`).
To update by hand: extract it, copy the executables over the installed ones (same directory as `geekbridge-ctl`, e.g.
`install -m 0755 geekbridge-linux-x86_64/geekbridge* ~/.local/bin/`), then restart the service
(`systemctl --user restart geekbridge-service`, or log out and in). Pairings and settings live in the state directory and
are not touched. Verified with real artifacts: [updater-e2e.md](updater-e2e.md).

### Experimental assets

The selector never offers an asset whose name contains `experimental`, `-gnu-`, `debug`, `sbom` or `sha256`, nor an
Android package, to a desktop. A local release built with `release-local.sh --with-windows` carries
`geekbridge-windows-x86_64-gnu-experimental.zip` (zig cross-build, unsigned, never run on Windows); a Windows machine
is therefore offered no update from such a release, and the regular `geekbridge-windows-x86_64.zip` that
`release.yml` publishes is what gets selected when present. Testers who want the experimental archive set
`GEEKBRIDGE_UPDATE_EXPERIMENTAL=1` for the service; it is then used only when no regular archive exists for the system.
`--with-windows` is off by default and the release notes say the zip is experimental.

### Asset naming (what the selector expects)

`geekbridge-<os>-<arch>.<tar.gz|zip>` with `os` in `linux|windows|macos` and `arch` in `x86_64|aarch64`
(aliases `amd64|x64|arm64`, `darwin|osx`, `universal` for macOS are accepted). Files containing `android`,
ending in `.apk`, or containing `sha256` are never picked as desktop installers. When several files fit (the release also
carries `geekbridge-<version>-linux-x86_64-installer.tar.gz`), the better extension wins, then the shortest name, i.e. the
canonical `geekbridge-<os>-<arch>.<ext>`. The release workflow produces
exactly these names.

## Android

* `UpdateChecker` (platform `HttpURLConnection`, no libraries) checks when the Diagnostics & about screen opens (the card lives there) at most once per 12 h
  (switch in the "App updates" card) and on **Check for updates**. It shows the version and notes.
* **Update notice.** The same check also runs, at most once per 24 h, when Home opens and when the background link
  service sees a Wi-Fi network (events that happen anyway: no timer, no JobScheduler/WorkManager, no extra wake-up).
  `UpdateNoticePolicy` decides: switch off or no network means no check; after a failed attempt the next waits 15 min;
  a metered network waits until the last good answer is a week old (the request is conditional, so usually a few hundred
  bytes). A newer version shows a small **Update available** chip on Home that opens the update card in Diagnostics.
  Settings on Home has **Tell me about updates** (off by default): one low-importance notification per version, posted
  only from the background and only if Android allows notifications. Nothing is downloaded or installed from the chip or
  the notification. The same rules apply as in the card: Stable never offers a pre-release, Dev only when selected, and an
  offer must have a higher version code than the installed build; the chip also disappears after an install or a channel
  switch. State: `notice_*` keys in the existing `updates` preferences.
* **Download and verify** fetches `geekbridge-android.apk` into the app-private cache (`cache/updates/`),
  verifies size and SHA-256 from `SHA256SUMS`, and deletes it on any mismatch. The download is abandoned if the
  app leaves the foreground.
* Downloads are not resumed: an interrupted one is deleted (never a half file) and the next tap starts over; the
  APK is about 6 MB. The pending file lives in `cache/updates/`; a new download replaces it, the app deletes it
  after an update (`MY_PACKAGE_REPLACED`) and when it is older than a day at process start.
* **Install update** re-hashes the file and hands it to Android's package installer through `UpdateFileProvider`
  (a minimal, non-exported, read-only `ContentProvider` standing in for AndroidX `FileProvider`) with a one-time
  read grant. If "Install unknown apps" is not allowed for GeekBridge, the button first opens that Android
  settings page (`REQUEST_INSTALL_PACKAGES`); the system then asks you to confirm.
* An update installs only over an app signed with the same key (or a v3 rotation lineage that contains it). The
  pipeline signs with the release keystore ([builds.md](builds.md#first-release-android-signing-key)); stable tags fail
  without it, and `release.yml` also refuses to publish unless the repository variable `ANDROID_SIGNER_SHA256` pins that
  key. The Android app compares the downloaded APK's signer certificate (SHA-256, via `PackageManager`
  `GET_SIGNING_CERTIFICATES`) with its own **before** showing Install; on a mismatch the file is deleted and the app
  explains it (a debug build never updates to a release build: uninstall once, then install the release APK). On the
  Dev channel the dev APK (`geekbridge-debug.apk`) is checked the same way, with its own explanation.

## Tested / not tested

See the final section of [builds.md](builds.md#update-checking-validation).
