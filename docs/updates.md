# Update checks

Both the desktop apps and the Android app can tell you when a newer GeekBridge release exists on GitHub
(`fspecii/geekbridge`). Nothing is ever installed or replaced automatically; there is no telemetry.

## What is contacted, and what is sent

| Request | Purpose |
| --- | --- |
| `GET https://api.github.com/repos/fspecii/geekbridge/releases/latest` | the newest published, non-prerelease release |
| `GET <SHA256SUMS asset URL>` (`github.com` / `*.githubusercontent.com`, https only) | published digests, fetched only when a newer release has an installer for this system |
| `GET <installer asset URL>` | only after you click Download (desktop) or "Download and verify" (Android) |

Each request carries a `User-Agent` of `geekbridge-updater/<version>` (Android:
`geekbridge-android-updater/<version>`), `Accept`, and, after the first answer, `If-None-Match` with GitHub's
ETag (a `304` does not count against GitHub's unauthenticated rate limit). No account, device id, install id,
IP-derived value or version history is sent beyond what any HTTPS request reveals. Redirects are followed only
to the same allow-list (https `github.com`, `api.github.com`, `*.githubusercontent.com`); responses are capped
(1 MiB for release JSON, 64 KiB for checksums, 512 MiB / 200 MiB for installers) and every request has a
deadline. A rate-limit answer (403/429) pauses automatic checks for the time GitHub names (at least a minute,
at most a day).

## Choosing the release source

The built-in source is `fspecii/geekbridge`; a build can point elsewhere (`GEEKBRIDGE_RELEASE_REPO` /
`-Pgb.releaseRepo`, see [builds.md](builds.md#releases-and-versioning)), so releases may live in a separate
public releases-only repository. At run time:

* Desktop: `GEEKBRIDGE_UPDATE_REPO=owner/name` for `geekbridge-service` (malformed values are ignored).
* Android: debug builds only, the `updates` preference `debug_repo`
  (`adb shell run-as dev.geekbridge.android` and edit `shared_prefs/updates.xml`), like `debug_base_url`.

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

## Version comparison

The release tag (`vX.Y.Z`) is parsed as semver and compared with the running build's version
(`env!("CARGO_PKG_VERSION")` on the desktop, `BuildConfig.VERSION_NAME` on Android). Prereleases and drafts are
never offered. On Android the version code derived from the tag must also exceed the installed one.
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
* The download goes into a fresh `geekbridge-update-<random>` directory (mode 0700 on Unix) under the OS temp
  directory, is hashed while written, and is deleted if the size or SHA-256 differs from the published value.
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

### Asset naming (what the selector expects)

`geekbridge-<os>-<arch>.<tar.gz|zip>` with `os` in `linux|windows|macos` and `arch` in `x86_64|aarch64`
(aliases `amd64|x64|arm64`, `darwin|osx`, `universal` for macOS are accepted). Files containing `android`,
ending in `.apk`, or containing `sha256` are never picked as desktop installers. When several files fit (the release also
carries `geekbridge-<version>-linux-x86_64-installer.tar.gz`), the better extension wins, then the shortest name, i.e. the
canonical `geekbridge-<os>-<arch>.<ext>`. The release workflow produces
exactly these names.

## Android

* `UpdateChecker` (platform `HttpURLConnection`, no libraries) checks when the Diagnostics & about screen opens (the card lives there, not on Home) at most once per 12 h
  (switch in the "App updates" card) and on **Check for updates**. It shows the version and notes.
* **Download and verify** fetches `geekbridge-android.apk` into the app-private cache (`cache/updates/`),
  verifies size and SHA-256 from `SHA256SUMS`, and deletes it on any mismatch. The download is abandoned if the
  app leaves the foreground.
* **Install update** re-hashes the file and hands it to Android's package installer through `UpdateFileProvider`
  (a minimal, non-exported, read-only `ContentProvider` standing in for AndroidX `FileProvider`) with a one-time
  read grant. If "Install unknown apps" is not allowed for GeekBridge, the button first opens that Android
  settings page (`REQUEST_INSTALL_PACKAGES`); the system then asks you to confirm.
* An update installs only over an app signed with the same key (or a v3 rotation lineage that contains it). The
  pipeline signs with the release keystore ([builds.md](builds.md#first-release-android-signing-key)); stable tags fail
  without it. The Android app compares the downloaded APK's signer certificate (SHA-256, via `PackageManager`
  `GET_SIGNING_CERTIFICATES`) with its own **before** showing Install; on a mismatch the file is deleted and the app
  explains it (a debug build never updates to a release build: uninstall once, then install the release APK).

## Tested / not tested

See the final section of [builds.md](builds.md#update-checking-validation).
