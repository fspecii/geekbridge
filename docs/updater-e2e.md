# Updater end to end with real release artifacts (2026-11-02)

`scripts/e2e/update-install.sh` proves the updater logic against a mock with tiny synthetic assets. The real GitHub
repository is private and has no release, so this page records the same flow with **real packages built from this tree**,
served by a replica of GitHub's REST API and download hosts. Nothing here contacts GitHub's release endpoints (only the
public OpenAPI description was fetched, to pin the payload schema).

Outcome in one paragraph: the whole chain works with real artifacts on desktop and Android (check, offer, download,
SHA-256 verification, reveal-installer state, manual install, state survival, APK upgrade over the same key). Real assets
exposed **one defect** (the Linux installer tarball outranked the documented archive; fixed) and three smaller findings
(see [Findings](#findings)). Numbers below are from one run on an Arch/Omarchy host, emulator API 36.

## What was built

All in a scratch directory (`UPD_REAL=$TMPDIR/upd-real`, `TMPDIR=$HOME/.cache/gb-tmp`); the main checkout was not
modified by the builds, and nothing was committed.

| | v0.1.0 | v0.1.1 |
| --- | --- | --- |
| source | snapshot of the working tree (`git ls-files --cached --others --exclude-standard`, minus `fuzz/corpus`) | the same snapshot, bumped **in the scratch copy only** with `scripts/release.sh --skip-tests 0.1.1` (+ `pkgver` in its PKGBUILD) |
| `scripts/package-linux.sh` | `geekbridge-0.1.0-linux-x86_64-installer.tar.gz` 29.3 MB, `geekbridge_0.1.0_amd64.deb` 14.6 MB | `geekbridge-0.1.1-linux-x86_64-installer.tar.gz` 29.3 MB, `geekbridge_0.1.1_amd64.deb` 14.6 MB |
| `packaging/arch/build-local.sh` (`GB_JOBS=3`) | `geekbridge-0.1.0-1-x86_64.pkg.tar.zst` 28.8 MB (+ debug split package) | `geekbridge-0.1.1-1-x86_64.pkg.tar.zst` 28.8 MB |
| Android (debug-signed, same debug key, x86_64 only) | versionCode **10099**, 4.73 MB | versionCode **10199**, 4.73 MB |
| release asset `geekbridge-linux-x86_64.tar.gz` | made exactly as the "Package" step of `release.yml` (9 binaries + README) | same |
| `SHA256SUMS` | the release job's own line: `sha256sum -- geekbridge-* $(compgen -G "geekbridge_*")` | same |

Why **v0.1.1, not v0.1.1-test**: `0.1.1-test` is a semver *prerelease*. The updater never offers prereleases (by design,
[updates.md](updates.md#version-comparison)), `release.yml` would publish it with `--prerelease`, and GitHub's own
`/releases/latest` never returns one, so a `-test` build can never be the target of an update. The offer flow therefore uses
the stable 0.1.1. The `-test` tag is still exercised, as the negative case (a `v0.1.1-test` release with
`prerelease: true` and v0.1.1's files, see check `desktop-prerelease-*`).

Commands (run from the repository root; the scratch copy is a plain directory with its own throw-away git repo):

```bash
export TMPDIR=$HOME/.cache/gb-tmp; S=$TMPDIR/upd-real; mkdir -p $S
# 1. snapshots (never bump in the main checkout)
for d in src-010 src-011; do mkdir -p $S/$d; done
git ls-files -z --cached --others --exclude-standard | grep -zv '^fuzz/corpus/' | rsync -a --from0 --files-from=- ./ $S/src-010/
cp -a --reflink=auto $S/src-010 $S/src-011
for d in src-010 src-011; do (cd $S/$d && git init -q . && git add -A >/dev/null); done
(cd $S/src-011 && git -c user.name=s -c user.email=s@l commit -qm snapshot && scripts/release.sh --skip-tests 0.1.1)
# 2. desktop binaries + packages, per version (CARGO_TARGET_DIR shared so dependencies compile once)
export CARGO_TARGET_DIR=$S/target
for v in 010 011; do
  (cd $S/src-$v && cargo build --release --locked -p bridge-apps -p bridge-service)
  mkdir -p $S/out/$v/bin; cp $CARGO_TARGET_DIR/release/geekbridge* $S/out/$v/bin/   # the 9 executables
  (cd $S/src-$v && scripts/package-linux.sh --bin-dir $S/out/$v/bin --out $S/out/$v)
  (cd $S/src-$v && GB_JOBS=3 packaging/arch/build-local.sh --out $S/out/$v/arch)       # slow: run in the background with a log
done
# 3. Android pair: same recipe as scripts/build-android.sh, x86_64 only, per snapshot (.tools symlinked to the main one)
for v in 010 011; do (cd $S/src-$v && ln -s $REPO/.tools .tools &&
  GB_ANDROID_TARGET_DIR=$S/target-android bash -c 'source scripts/android-env.sh
    cargo build --profile android -p bridge-android --target x86_64-linux-android
    "$GB_NDK_BIN/llvm-strip" --strip-unneeded -o android/app/src/main/jniLibs/x86_64/libgeekbridge_android.so "$CARGO_TARGET_DIR/x86_64-linux-android/android/libgeekbridge_android.so"
    printf "sdk.dir=%s\n" "$ANDROID_HOME" > android/local.properties
    gradle -p android --no-daemon assembleDebug -Pgb.abis=x86_64'
  cp android/app/build/outputs/apk/debug/*.apk $S/out/$v/geekbridge-debug.apk); done
```

Package sanity: the `.deb` control file says `Version: 0.1.1`, the Arch `.PKGINFO` says `pkgver = 0.1.1-1`; both Android
APKs verify with `apksigner` and carry the **same** debug certificate.

## The replica (`scripts/e2e/github_replica.py`)

A Python standard-library server on `127.0.0.1` that serves the real files from a directory of release directories
(`DIR/v0.1.1/{geekbridge-linux-x86_64.tar.gz, …, SHA256SUMS, NOTES.md, release.json}`). It models:

* `GET /repos/{o}/{n}/releases/latest`: newest **published, non-draft, non-prerelease** release (404 otherwise, as GitHub
  does), the full documented body (`url`, `html_url`, `assets_url`, `upload_url`, `tarball_url`, `zipball_url`, `id`,
  `node_id`, `tag_name`, `target_commitish`, `name`, `body`, `draft`, `prerelease`, `immutable`, `created_at`,
  `published_at`, `author`, `assets[]` with `id`, `node_id`, `name`, `label`, `uploader`, `content_type`, `state`, `size`,
  `digest: "sha256:…"`, `download_count`, `created_at`, `updated_at`, `url`, `browser_download_url`);
* `ETag: W/"…"` and `304` on `If-None-Match` (a 304 does not consume quota), `X-RateLimit-Limit/Remaining/Reset/Used/Resource`
  (60/h unauthenticated), `X-GitHub-Api-Version-Selected`, `X-GitHub-Media-Type`, `Cache-Control: public, max-age=60`, `Vary`;
* the `browser_download_url` redirect: `…/releases/download/<tag>/<file>` → `302` to a pre-signed
  `/release-assets/github-production-release-asset/<repo id>/…?sp=r&sv=…&se=…&rscd=…&sig=…` (HMAC, expires after 5 minutes;
  an expired or forged signature gets the CDN's `403 AuthenticationFailed` XML), then `200` with `Content-Disposition`,
  `Accept-Ranges`, `206` for `Range`, `HEAD`;
* the 404 body (`{"message":"Not Found","documentation_url":…,"status":"404"}`), the primary rate limit (`403` +
  `X-RateLimit-Remaining: 0` + the documented message), the secondary one (`429` + `Retry-After`), `502`;
* fault modes switched at run time through a mode file: `only=<tags>`, `tag=<tag>` (force a prerelease into `/latest`),
  `corrupt`, `truncate`, `tamper_sums`, `nosums`, `ratelimit`, `secondary`, `notfound`, `error502`, `expired`,
  `evil_redirect`, `noetag`;
* a JSON-lines request log (UA, `Accept`, `X-GitHub-Api-Version`, `If-None-Match`, whether `Authorization` was present,
  bytes sent) so the scripts assert on what the client really sent.

Deliberate limits: the real API, `github.com` and `release-assets.githubusercontent.com` are three hosts; the updaters'
allow-list admits only https GitHub hosts or the single configured loopback origin, so the replica serves all three
behind one port and tells them apart by path. The bearer-token path (`GEEKBRIDGE_UPDATE_TOKEN`) is TLS- and
`api.github.com`-only by design, so it cannot be driven against a loopback server (it stays covered by `bridge-update`'s
unit tests).

### Payload shape is checked against GitHub's published schema, not our assumptions

`scripts/e2e/fixtures/github-latest-release.schema.json` is the 200-response schema of *Get the latest release*, extracted
from `github/rest-api-description` (`descriptions/api.github.com/dereferenced/api.github.com.deref.json`, commit
`f9ed74230d54847b5ca00a1343e8e349e858b8fb`, 2026-10-31). `scripts/e2e/validate_release_payload.py` (no dependencies)
validates required properties, types, `nullable`, enums and date-time/URI formats:

```bash
curl -sL -o $S/spec.json https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/dereferenced/api.github.com.deref.json
python3 -I - <<'EOF'   # refresh the vendored schema
import json,os; d=json.load(open(os.environ['S']+'/spec.json'))
print(json.dumps(d['paths']['/repos/{owner}/{repo}/releases/latest']['get']['responses']['200']['content']['application/json']['schema'])[:80])
EOF
python3 -I scripts/e2e/validate_release_payload.py --url http://127.0.0.1:18893/repos/acme/geekbridge-releases/releases/latest
```

Both v0.1.0 and v0.1.1 payloads validate (`replica-schema-*`). One replica bug was found this way and fixed: a random asset
`node_id` made the body (and so the ETag) change on every request, which would have defeated 304.

Note on `digest`: real GitHub now returns `assets[].digest` (`sha256:…`). The updaters ignore it and rely on `SHA256SUMS`,
which is what `release.yml` publishes. The replica asserts the two always agree (`replica-sums-match-digests`).

## Desktop run (`scripts/e2e/updater-real.sh all`)

```bash
export TMPDIR=$HOME/.cache/gb-tmp          # UPD_REAL defaults to $TMPDIR/upd-real
scripts/e2e/updater-real.sh all                  # assemble + replica + desktop (about 12 minutes: 31 s manual-check cooldowns)
# or:  scripts/e2e/updater-real.sh assemble | replica | desktop
```

* `assemble` builds `run/releases/{v0.0.9,v0.1.0,v0.1.1,v0.1.1-test,v0.1.2}` from the packages: the release-job recipe
  above, markdown notes (`## What's Changed`, the CHANGELOG section, `**Full Changelog**` link, signer note),
  `release.json` publish dates. `v0.1.1-test` is a `prerelease: true` release with v0.1.1's files; `v0.0.9` is older than the
  installed app; `v0.1.2` is v0.1.1's APK re-signed with a throw-away key (see Android).
* "Installed v0.1.0" is real: the v0.1.0 installer tarball is extracted and `./install.sh --prefix $UPD_REAL/run/desktop/prefix-010`
  is run; the service is started from there with
  `GEEKBRIDGE_UPDATE_REPO=acme/geekbridge-releases GEEKBRIDGE_UPDATE_BASE_URL=http://127.0.0.1:18893 XDG_STATE_HOME=…`
  (a releases-only repository name, to exercise the override). Its state directory starts from
  `crates/bridge-service/tests/fixtures/state-v1` (the state-versioning fixtures: identity, a trusted peer, consent, share,
  clipboard settings; `update-state.json` removed) so there is a pairing to preserve.

Results (`dist/android/logs/updater-real-results.tsv`; all PASS in the final run):

| Check | What it proves |
| --- | --- |
| `desktop-installed-v0.1.0` | `geekbridge-ctl --version` → `0.1.0` from the installed prefix |
| `replica-schema-*`, `-asset-fields`, `-sums-match-digests`, `-headers`, `-etag-304`, `-404-shape`, `-download-302/-bytes/-range`, `-expired-signature-403`, `-ratelimit-shape` | the replica is faithful (above); the served bytes equal the files on disk |
| `desktop-auto-check-ran`, `-request-headers` | the startup check (~15 s after start) reaches the override; `User-Agent: geekbridge-updater/0.1.0`, `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, no `Authorization` |
| `desktop-etag-304` | the next manual check sends `If-None-Match: W/"…"` and gets 304 |
| `desktop-prerelease-not-latest`, `-refused-if-served` | with `v0.1.1-test` published, `/latest` still answers v0.1.0; and a prerelease payload forced into `/latest` is refused by the client |
| `desktop-offer-v0.1.1`, `-selected-asset`, `-notes` | v0.1.1 is offered; the chosen installer is `geekbridge-linux-x86_64.tar.gz` with the digest from `SHA256SUMS` (not the `-installer.tar.gz`, see finding 1); the markdown body is kept in `update-state.json` |
| `desktop-download-corrupt/truncate/expired/evil_redirect/tampered-sums-refused` | one flipped byte → "does not match the published SHA-256; it was deleted"; half the body → "truncated body"; an expired signature → "HTTP 403"; a redirect to another host:port → "refusing to contact …"; a wrong published digest → refused. After each, no `geekbridge-update-*` file is left |
| `desktop-download-verified`, `-permissions`, `-redirect-chain`, `-no-token-sent` | 29,300,792 bytes in 0.5 s; SHA-256 equals the release's; directory `0700`, file `0444`; request chain `302, 200`; no request carried `Authorization` |
| `desktop-state-reveal-installer` | `ctl update status` → `verified download: <path>` (the state behind the apps' **Show file**) |
| `desktop-manual-install-v0.1.1` | the archive is extracted and its binaries copied into a scratch prefix; `geekbridge-ctl --version` → `0.1.1` |
| `desktop-state-pairing-survives`, `-files-unchanged` | the v0.1.1 service on the **same state directory**: `ctl status` (paired device, links) identical except `service_version`; `identity.pem`, `trusted_peers.json`, `consent.json` byte-identical; the old `update-state.json` is read |
| `desktop-after-update-up-to-date` | the new service reports `running v0.1.1 … up to date` |
| `desktop-state-notfound/-ratelimit/-secondary/-error502` | on fresh state: "No public release found: the project repository may be private or has no releases yet."; "rate limit reached; trying again in about 56 min" (403 + remaining 0) and "about 2 min" (429 + `Retry-After: 120`), and the second manual check sends **no** further request; "GitHub answered HTTP 502" |

### Manual install of the downloaded archive

What `geekbridge-linux-x86_64.tar.gz` contains is the release job's `stage` directory: nine executables and `README.md`
(no `install.sh`, no desktop files, no systemd unit; those are in the installer tarball and the `.deb`). The documented
procedure ([README](../README.md), step 1) covers the installer tarball only, so for the downloaded archive the steps used
were:

```bash
tar xzf geekbridge-linux-x86_64.tar.gz -C "$X"
install -m 0755 "$X"/geekbridge-linux-x86_64/geekbridge* ~/.local/bin/   # or any prefix; then restart the service
geekbridge-ctl --version                                                  # geekbridge-ctl 0.1.1
```

now documented in [updates.md](updates.md#installing-a-downloaded-archive-by-hand).

## Android run

```bash
S=$TMPDIR/upd-real
E2E_UPD_REPLICA=$S/run/releases E2E_UPD_OLD=0.1.0 E2E_UPD_NEW=0.1.1 \
E2E_UPD_APK_OLD=$S/out/010/geekbridge-debug.apk E2E_UPD_APK_NEW=$S/out/011/geekbridge-debug.apk \
  scripts/e2e/update-install.sh android      # emulator gb36u (console 5624, adb server 16048) is started if it is not running
```

`update-install.sh` now takes the old/new version, the two APKs and `E2E_UPD_REPLICA` (a directory of release directories)
from the environment; without them it behaves as before (mock server, 0.1.0 → 0.2.0). The debug build under test was
pointed at the replica with the existing `debug_base_url` preference, over `adb reverse`.

On the **current UI** (Diagnostics & about → App updates card): the throttle (12 h), downgrade/same-version, tampered
digest, corrupted APK, missing `SHA256SUMS` and the real v0.1.0 → v0.1.1 install over the same key all behave as before,
now with real 4.7 MB APKs and the real redirect chain. New in this run:

| Check | Result |
| --- | --- |
| `android-no-public-release-state` | replica answers 404 → the card says **"No public release found: the project repository may be private or has no releases yet."**, not "up to date" |
| `android-rate-limit-state` | 403 with `X-RateLimit-Remaining: 0` → "GitHub is rate limiting requests. Try again later." |
| `android-signer-mismatch-refused`, `-file-deleted` | a *newer* release (v0.1.2) whose APK is correctly hashed in `SHA256SUMS` but signed with another key: **Download and verify** downloads it, the signer-certificate pre-check compares it with the installed app's certificate, **deletes the file** and shows the debug-build wording ("This is a developer (debug-signed) build. The release is signed with the release key, which Android won't accept over it…"); Install is never offered |
| `android-offer-v2`, `-download-verified`, `-upgrade-installed`, `-version-code` | v0.1.1 offered, downloaded and verified, installed through the package installer: `versionName=0.1.1`, `versionCode=10199` |
| `android-data-survived`, `-clipboard/-device-name/-pairing-and-optins/-widget-survived`, `-schema-version-stamped` | app data hashes unchanged; clipboard sync, device name, paired computer + contacts opt-in + auto-accept, the home widget survive; `gb_state.xml` `schema_version=1` is stamped |
| `android-request-user-agent`, `-etag-304-seen`, `-asset-redirect-chain` | `geekbridge-android-updater/0.1.0` (and `/0.1.1` after the upgrade); an unchanged release is answered 304 to `If-None-Match`; the APK came through `…/releases/download/…` → 302 → `/release-assets/…` |

Screenshots: `dist/screenshots/android/e2e-upd-*.png` (gitignored).

## Findings

1. **Defect, fixed: the Linux installer tarball was chosen over the documented archive.** `release.yml` uploads both
   `geekbridge-linux-x86_64.tar.gz` and (from `package-linux.sh`) `geekbridge-<ver>-linux-x86_64-installer.tar.gz`. Both match
   the OS and architecture words and the same extension, and the tie was broken alphabetically (`0` sorts before `l`), so
   the updater offered `…-installer.tar.gz` although `package-linux.sh` states that the name "deliberately differs" so that
   the checker does *not* read it. Invisible to the mock tests (they have one asset per platform). Fix in
   `crates/bridge-update/src/release.rs` (`select_asset`): after the extension rank, the shortest name wins (the canonical
   `geekbridge-<os>-<arch>.<ext>`), then alphabetical; test `real_release_asset_set_picks_the_canonical_archive` uses the
   exact asset list of a release. The `.deb` was never a candidate (no OS word, unknown extension). Product question left
   open: the installer tarball is arguably the more useful thing to hand a Linux user (it has `install.sh`); if you prefer
   that, change the rank in one place and update the doc comment in `package-linux.sh`.
2. **Fixed (cosmetic): `geekbridge-ctl update status` said "unknown (no check has completed)"** after a check that
   found only prereleases/drafts. It now says "no stable release published - you are up to date"
   (`crates/bridge-service/src/bin/geekbridge-ctl.rs`).
3. **Fixed: `scripts/release.sh` did not bump `packaging/arch/PKGBUILD`**, so `packaging/arch/build-local.sh` refused to
   build right after a release bump ("PKGBUILD pkgver != Cargo.toml version"). It now bumps `pkgver` (and resets `pkgrel=1`)
   for stable versions; a prerelease (a hyphen is illegal in a `pkgver`) prints a note to set it by hand.
4. **Not changed:** the Android card shows the release body as plain text, so markdown (`**Full Changelog**`, backticks,
   `##`) appears literally (the GitHub-generated notes use it). The desktop keeps it as text too. Worth a small markdown
   strip or a "Release notes" link only. Also, during a running check the card still shows the previous result's text, and a
   tap on **Download and verify** in that moment is ignored (busy); the e2e script now retries until the APK is requested.

## What this does not prove

* Real GitHub, real TLS and the `webpki-roots` trust path (the replica is loopback http, the only plain-http origin the
  updaters admit); the `GEEKBRIDGE_UPDATE_TOKEN` path (TLS + `api.github.com` only).
* A real release-keystore-signed APK (these are debug-signed; the pre-check was exercised with two different debug-style keys).
* Windows/macOS assets, an actual `dpkg -i` / `pacman -U` of the packages (they were inspected, and the tarball install was
  run), a real phone.

## Clean-up

```bash
rm -rf $TMPDIR/upd-real/{target,target-android,target-dev,run/desktop,src-010,src-011}   # the large scratch (dependencies, builds, copies)
scripts/clean-tmp.sh --apply --older-than 0                                              # gb-* leavings (arch build dirs, package-linux)
```
