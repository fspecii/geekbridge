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

### Asset naming (what the selector expects)

`geekbridge-<os>-<arch>.<tar.gz|zip>` with `os` in `linux|windows|macos` and `arch` in `x86_64|aarch64`
(aliases `amd64|x64|arm64`, `darwin|osx`, `universal` for macOS are accepted). Files containing `android`,
ending in `.apk`, or containing `sha256` are never picked as desktop installers. The release workflow produces
exactly these names.

## Android

* `UpdateChecker` (platform `HttpURLConnection`, no libraries) checks when the app opens at most once per 12 h
  (switch in the "App updates" card) and on **Check for updates**. It shows the version and notes.
* **Download and verify** fetches `geekbridge-android.apk` into the app-private cache (`cache/updates/`),
  verifies size and SHA-256 from `SHA256SUMS`, and deletes it on any mismatch. The download is abandoned if the
  app leaves the foreground.
* **Install update** re-hashes the file and hands it to Android's package installer through `UpdateFileProvider`
  (a minimal, non-exported, read-only `ContentProvider` standing in for AndroidX `FileProvider`) with a one-time
  read grant. If "Install unknown apps" is not allowed for GeekBridge, the button first opens that Android
  settings page (`REQUEST_INSTALL_PACKAGES`); the system then asks you to confirm.
* An update installs only over an app signed with the same key. The pipeline signs with a release keystore when
  the secrets in [builds.md](builds.md#releases-and-versioning) exist; otherwise the APK is debug-signed and
  will not update an app installed from a differently signed build (Android refuses it).
* The app opens no listening socket for this; all connections are outbound HTTPS.

## Tested / not tested

See the final section of [builds.md](builds.md#update-checking-validation).
