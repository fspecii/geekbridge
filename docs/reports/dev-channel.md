# Dev update channel

Date: 2026-11-04. Design: [updates.md#dev-channel](../updates.md#dev-channel). Nothing was published, tagged or uploaded
while building this; no key left this machine.

## What exists

* **Rolling prerelease `dev`**: assets `geekbridge-debug.apk`, `geekbridge-linux-x86_64.tar.gz` (plus Windows/macOS
  archives when CI artifacts are added), `SHA256SUMS`, `dev-manifest.json`
  (`{commit, versionCode, builtAt, notes}`; versionCode = 20000 + commit count).
* **`scripts/release-dev.sh`**: builds locally (Linux archive with the commit embedded, debug APK with the project
  debug keystore), checks the APK signature, versionCode and (if pinned) the signer, writes the manifest and
  `SHA256SUMS`. **Dry run by default**; `--publish` updates the release through `gh` and asks you to type `publish dev`.
  Test: `scripts/tests/release-dev-test.sh` (stub `gh`, no network).
* **`.github/workflows/dev-release.yml`**: on push to main, builds the desktop binaries for Linux, Windows and macOS
  (artifacts only, no secrets). The APK job runs only if the repo secret `DEV_KEYSTORE_B64` exists; publishing from CI
  only if the variable `DEV_AUTOPUBLISH` is `true` (or a manual run with `publish=true`) and the APK was built.
  Not run on GitHub yet.
* **Android**: `UpdateChannel` (debug default Dev, release default Stable with a confirmation dialog), dev check
  (`releases/tags/dev`, ETag, manifest cached with it), SHA-256 and signer verification, dev-specific signer messages,
  channel buttons in the App updates card, debug `versionCode = 20000 + commit count` (release codes unchanged),
  no What's-new card on dev-to-dev updates.
* **Desktop** (`bridge-update`): `build.rs` embeds commit and count; `Config.channel`, `GEEKBRIDGE_UPDATE_CHANNEL`,
  persisted choice, `Updater::set_channel`, IPC `UpdateSetChannel`, `geekbridge-ctl update channel stable|dev`, selector in
  the Details panel, banner "Dev build available". i18n keys `update.channel*`, `update.*_dev`.
* Docs: updates.md, privacy.md (extra request row), builds.md, CHANGELOG. CI: `check.yml` debug-APK code check now uses
  `version-code.sh --dev` (full history checkout); `release.yml` builds its debug-signed fallback with
  `-Pgb.formulaVersionCode=1`; `update-install.sh` likewise.

## Commands for the first dev build (owner)

```sh
cd ~/.t3/projects/geekbridge && git pull --rebase

# 1. Pin the signer once: it must be the key your phone's current debug build was signed with.
source scripts/android-env.sh
keytool -list -v -keystore .tools/android-user/debug.keystore -storepass android | grep 'SHA256:'
adb shell pm path dev.geekbridge.android          # then: adb pull <base.apk> /tmp/phone.apk
"$ANDROID_HOME/build-tools/$GB_BUILD_TOOLS/apksigner" verify --print-certs /tmp/phone.apk | grep 'SHA-256'   # must match
mkdir -p ~/.config/geekbridge && echo <that hex> > ~/.config/geekbridge/dev-signer-sha256

# 2. Dry run (builds, verifies, prints the gh commands, publishes nothing)
scripts/release-dev.sh
ls -l ~/.cache/gb-release-dev/*/

# 3. Publish (clean tree on main, gh logged in; asks for "publish dev")
scripts/release-dev.sh --publish
gh release view dev --repo fspecii/geekbridge        # prerelease, not latest

# 4. First install on the phone (the installed build has no dev channel yet): in place, same key
adb install -r ~/.cache/gb-release-dev/*/geekbridge-debug.apk
```
Later dev builds: repeat step 3, then on the phone App updates > Check for updates > Download and verify > Install.
Optional CI automation:
```sh
base64 -w0 .tools/android-user/debug.keystore | gh secret set DEV_KEYSTORE_B64
gh variable set DEV_SIGNER_SHA256 --body <hex>
gh variable set DEV_AUTOPUBLISH --body true     # leave unset to keep publishing manual
```
Desktop: `GEEKBRIDGE_UPDATE_CHANNEL=dev geekbridge-service` or `geekbridge-ctl update channel dev`, then `update check`.

## Decisions

1. **Tag `dev`**, one rolling release, assets replaced in place, manifest uploaded last, tag moved with a ref PATCH
   (force on that one tag only). Never `latest`; script re-checks after publishing.
2. **Dev versionCode = 20000 + commits.** Cannot stay below the next stable (10199 is only ~100 commits away) and does
   not need to: stable APKs use the release key, so switching channels on a phone is an uninstall regardless of codes.
   Cost: a debug-signed rehearsal "stable" with a code below the dev code cannot be installed over a dev build (e2e
   scripts use `-Pgb.formulaVersionCode=1`). Older debug builds (10099) are below every dev code.
3. **APK in CI is opt-in** via a secret, since the key must be the one on the phones; `secrets` is not usable in `if:`, so
   a job-level env var is the guard. A CI publish without an APK is refused (it would advertise an uninstallable code).
4. **CI publishing is off by default**; push-to-main builds only artifacts. Note: this workflow runs on every push to main
   and uses Actions minutes (docs/builds.md says the maintainer has no budget; fine once public, otherwise switch the trigger to
   `workflow_dispatch`).
5. **Desktop "newer"** = commit differs and code higher; unknown own count means offer. Dev state keys differ from stable
   so a channel switch never reuses the other channel's cache; switching deletes the downloaded file and skip choice.
6. **Release builds can opt into Dev** after a warning, but the install will fail the signer check with a clear message
   unless the dev APK were signed with the release key (not planned).
7. Windows/macOS in the local dev release only via `--extra-assets` from CI artifacts (`gh run download`).
8. Desktop default channel stays Stable (only Android debug builds default to Dev).

## Not verified

* Nothing was run against real GitHub (no dev release exists); behaviour is covered by mock servers (Rust: 9 dev
  tests in `bridge-update/tests/mock_server.rs` and `dev.rs`; Kotlin: `UpdateDevChannelTest`).
* No on-device install of a dev update; the in-app Install path is the unchanged stable one.
* `dev-release.yml` has not executed on GitHub; YAML parses.
* Windows/macOS build of the new `build.rs` (uses `git` if present, otherwise embeds nothing).
