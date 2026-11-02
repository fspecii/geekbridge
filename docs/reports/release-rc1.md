# Release candidate v0.1.0-rc.1 (prepared 2026-11-02, NOT published)

Nothing has been pushed, tagged or uploaded. Everything below was built and verified locally from a **snapshot**, not from
the main checkout.

## How it was built

* Scratch clone of the public repo: `~/.cache/gb-release/src` (`origin/main` = `4bb7309`).
* On top of it, two plain commits by `fspecii <fspecii@gmail.com>` (no trailers):
  * `84e3d86` "Real-device fixes: ..." = the whole non-ignored working tree of the main checkout at snapshot time
    (`git ls-files --cached --others --exclude-standard`; about 150 changed files, 164 in the commit with new ones):
    reconnect/link stability, SMS readiness, debug diagnostics, new test files, docs and reports. Re-takeable with
    `~/.cache/gb-release/out/prep.sh`.
  * `8237e02` "Release v0.1.0-rc.1" = `scripts/release.sh 0.1.0-rc.1` (Cargo.toml, Cargo.lock, CHANGELOG section with
    Highlights, what's-new files) + `packaging/arch/PKGBUILD` `pkgver=0.1.0_rc.1` (hand-set: `release.sh` leaves
    prereleases to you) + the pre-release notice at the top of the changelog section (so it is in the release notes).
* A first dry run failed in `cargo test` (`i18n_catalogue`) because another agent was mid-edit (new `*_tests.rs`, stale
  allowlist). After their scanner/allowlist update the snapshot was retaken and everything passed.
* `GB_JOBS=3 GB_RELEASE_DIR=~/.cache/gb-release/out nice scripts/release-local.sh v0.1.0-rc.1 --prerelease --dry-run`:
  fmt, `clippy -D warnings`, `cargo test --workspace --locked` (1531 passed, 0 failed, 5 ignored), Android unit tests,
  Linux build (5 min), packages (7 min), Arch (10 min), Android (3.5 min). Exit 0.
* Android version: `versionName 0.1.0-rc.1`, `versionCode 10051` (final 0.1.0 would be 10099, so the rc sorts below it).

Outputs: `~/.cache/gb-release/out/v0.1.0-rc.1/{out,release-notes.md,logs}`.

## Artifacts (`~/.cache/gb-release/out/v0.1.0-rc.1/out`)

| File | Bytes | SHA-256 |
|---|---:|---|
| `geekbridge-linux-x86_64.tar.gz` | 28,971,977 | `d7cca9988cc9e4431aea0f3c24b719446bb6bed3f1d78dd81444ef6372109a6c` |
| `geekbridge-0.1.0-rc.1-linux-x86_64-installer.tar.gz` | 29,072,793 | `3887c96bb93b28fa75f4cf3e815c452e913168adb4a392385daed6875be303c6` |
| `geekbridge_0.1.0~rc.1_amd64.deb` | 14,518,608 | `14ead6185d79e5ceefcb5cefc564f1c35cfa224af03092e055e4adff4103134d` |
| `geekbridge-0.1.0_rc.1-1-x86_64.pkg.tar.zst` | 28,495,335 | `9d263d7a4126070243cb69a84d9691e365f3e6d1f3fe25532b82fd73f8b97811` |
| `geekbridge-android.apk` (arm64-v8a + x86_64) | 6,565,231 | `23cc95ac7f8febfd22bc6ca8831d9f6eb77575aa84479f1cc6c577a51b9d1920` |
| `geekbridge-android-arm64.apk` (phone only) | 3,292,446 | `70875dc8501a316e56a05d2f364384ac85f88818c59a6b8fc857096aca95d750` |
| `SHA256SUMS` | 606 | |

Independent re-checks (beyond the script's own): `sha256sum -c SHA256SUMS` all OK; the tarball's `geekbridge-ctl --version`
= `geekbridge-ctl 0.1.0-rc.1`; installer tarball installed into a scratch prefix (10 binaries/files, 54 files, valid
`.desktop` files, `--uninstall` leaves only icon/mime caches); `.deb` control `Version: 0.1.0~rc.1`, its `geekbridge-ctl`
runs, no missing shared libraries for the service; Arch `.PKGINFO` `pkgver = 0.1.0_rc.1-1`, all nine binaries,
its `geekbridge-ctl` runs. Not done: a real `dpkg -i` / `pacman -U` (needs root).
Both APKs: `apksigner verify` OK with v2 and v3 (no v1: minSdk 26), `aapt2` shows `dev.geekbridge.android`, versionCode 10051,
versionName `0.1.0-rc.1`, minSdk 26, native code `arm64-v8a`+`x86_64` / `arm64-v8a`.

### Android signing: DEBUG-signed, on purpose

There is no release keystore, so the script fell back (with its loud warning) to the machine's
`~/.android/debug.keystore`: signer `CN=Android Debug`, SHA-256
`4d711ab401391ad4041c7dadd070371eb658d303a08b39de29d1df690a9ca3c3`. Consequences:

* It installs over the debug builds already on the phone (same key), but **cannot be updated to a stable release signed
  with the release key; the phone must uninstall it first** (this sentence is in the release notes, with the signer).
* Anyone with that key file (this machine) can sign an APK Android treats as an update. Fine for a pre-release, not for a
  stable. **Owner action for the stable release:** `scripts/make-keystore.sh --out ~/secrets/geekbridge-release.p12 --password-file ~/secrets/geekbridge-release.pass`, back it up twice, store the printed pin
  (`docs/release-local.md`, one-time setup). I did not create one.

## Updater against the real artifacts (replica server; `~/.cache/gb-release/out/updater-rc1.sh`, log `~/.cache/gb-release/logs/updater-rc1.log`)

Installed = the rc.1 service binary from the tarball, isolated state and ports, `GEEKBRIDGE_UPDATE_BASE_URL` at the replica
(`scripts/e2e/github_replica.py`, as in [updater-e2e.md](../updater-e2e.md)). The live service and port 8000 were not touched.

| Case | Result |
|---|---|
| Only `v0.1.0-rc.1` (`prerelease: true`) published | `/releases/latest` is 404 (as on GitHub); the app reports "no stable release published - you are up to date". **Prereleases are never offered, by design.** (Right after start, before the first check finishes, `update status` says "unknown (no check has completed)".) |
| The rc forced into `/latest` | refused by the client, same message |
| Positive control: a stable `v0.1.0` (same bytes relabelled, a simulation) published | rc.1 offers `v0.1.0 - newer`, selects `geekbridge-linux-x86_64.tar.gz` (not the installer tarball) with SHA-256 from `SHA256SUMS`, downloads it through the 302 chain, verifies; the stored file hash equals the real artifact's. UA `geekbridge-updater/0.1.0-rc.1`, no `Authorization` |

Therefore an rc is installed by hand (download from the release page). Users on rc.1 will be offered the stable 0.1.0 once it
is a normal (non-prerelease) release. Not tested: the Android in-app updater against rc.1 (it uses the same prerelease rule;
covered by the earlier emulator run in [updater-e2e.md](../updater-e2e.md)), real GitHub TLS.

## Known limitations (also in the notes)

* No Windows or macOS builds. The optional experimental Windows GNU cross-build (`--with-windows`, zig, unsigned, never run)
  was **not** built; it needs a one-time `pip install ziglang` and was out of scope for a Linux/Android rc.
* The changelog "Added" list does not call out the day's real-device fixes (reconnect, link stability, SMS readiness) beyond
  the existing text; add a line under `[0.1.0-rc.1]` before publishing if you want them named (then rebuild, since the notes and
  the what's-new file derive from it; or edit the release notes on GitHub afterwards).
* The snapshot includes `docs/reports/real-device-*.md`, which mention private LAN addresses (`192.168.1.x`) and the
  owner's phone model. `scripts/public-preflight.sh` is gitignored, so it is not in the clone and was not run; a grep for
  secrets, passwords and e-mail addresses found nothing. Review those reports before pushing.
* The snapshot was taken while other agents were editing; it is exactly commit `84e3d86` and nothing newer.
* `--dry-run` was used because the clone's `HEAD` is not on `origin/main` yet (a real run refuses that). The assets are
  built from the same tree that will be tagged.

## Release notes (what GitHub will show)

`~/.cache/gb-release/out/v0.1.0-rc.1/release-notes.md`, starting with:

> **Release candidate, debug-signed Android APK.** This pre-release is not signed with the release key: you will have to
> uninstall it before installing a stable release signed with the release key. Pre-releases are not offered by the in-app
> updaters. Linux and Android builds only; Windows and macOS: build from source.

followed by Highlights, Added, the "Android APK is DEBUG-signed ... Signer SHA-256" line and the asset/commit line.

## Publish (one command)

```bash
~/.cache/gb-release/out/publish-v0.1.0-rc.1.sh
```

It refuses unless the clone is at `8237e02` and clean, checks `SHA256SUMS`, that neither the release nor the tag exists and
that `origin/main` is an ancestor, then: `git push origin HEAD:main` (fast-forward), annotated tag `v0.1.0-rc.1`
(`GeekBridge v0.1.0-rc.1`), `git push origin refs/tags/v0.1.0-rc.1`, `gh release create ... --draft --prerelease --verify-tag`
with all seven files, `gh release edit --draft=false`, prints the URL. It does **not** rebuild. (The documented alternative,
`scripts/release-local.sh v0.1.0-rc.1 --prerelease --tag --push-tag --publish` from the clone, rebuilds everything
(~35 min) and, once `main` is pushed, would stop at the ancestry check otherwise; it also needs `GB_KEYSTORE` for a signed APK.)

After publishing, bring the main checkout in line without losing its working tree:
`git fetch && git reset --mixed origin/main` (the tree then shows only edits made after the snapshot, plus the reverse of the
version bump, which `git checkout origin/main -- Cargo.toml Cargo.lock CHANGELOG.md packaging crates/bridge-core/assets/whats_new.txt android/app/src/main/assets/whats_new.json` restores).

## Undo

```bash
gh release delete v0.1.0-rc.1 --repo fspecii/geekbridge --yes --cleanup-tag   # release and its remote tag
git -C ~/.cache/gb-release/src tag -d v0.1.0-rc.1                             # local tag
git push --delete origin v0.1.0-rc.1                                          # if the remote tag is still there
```

The two commits on `main` cannot be undone without a force-push (confirm first); a revert commit is the safe way. A published
`versionCode` (10051) must not be reused for different bits.

## Clean-up

Kept: `~/.cache/gb-release/out/` (assets, notes, logs, the three scripts). Removable when decided:
`rm -rf ~/.cache/gb-release/{src,upd,tools,logs,v0.0.1-dryrun}` (the build trees, `src/target` and the updater test copies were already removed; the publish script needs `src/`).
