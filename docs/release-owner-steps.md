# Release owner steps for v0.1.0

Everything here is something only the repository owner can do. Work top to bottom; each step says what to send back and
what it unblocks. Verdict and evidence: [stable-go-no-go](reports/stable-go-no-go.md). Nothing below publishes, tags or
merges anything. Replace `<...>` placeholders; never paste keys, passwords or keystore contents into a chat, issue or file in the repository.

Run commands from the repository root (`~/.t3/projects/geekbridge`) unless a step says otherwise.

## 1. Create the Android release key and pin it

Unblocks: `scripts/release-local.sh v0.1.0` (it refuses a stable tag without `GB_KEYSTORE`, `GB_KEYSTORE_PASS_FILE` and a
pinned signer), and with it the dry run, the release itself and updates in place for every user.

```sh
mkdir -p ~/secrets ~/.config/geekbridge && chmod 700 ~/secrets
# keytool comes from the project-local JDK; the subshell keeps android-env.sh out of your shell
( source scripts/android-env.sh
  scripts/make-keystore.sh --out ~/secrets/geekbridge-release.p12 --password-file ~/secrets/geekbridge-release.pass --repo fspecii/geekbridge )
```

The script refuses to overwrite either file, refuses paths inside a git work tree, and prints the signer SHA-256 twice
(colon form and plain hex). It does not upload anything; ignore the `gh secret set ANDROID_*` lines it prints, because
releases are built locally and the release key should not go to GitHub.

1. **Back up both files** (`geekbridge-release.p12` and `geekbridge-release.pass`) in two separate offline places. A lost
   key ends updates for every installed phone and cannot be recovered.
2. Pin the signer (plain hex, 64 characters, from the script output):

   ```sh
   printf '%s\n' <64-hex-signer-sha256> > ~/.config/geekbridge/android-signer-sha256
   chmod 600 ~/.config/geekbridge/android-signer-sha256
   ```
3. Use the key for a release build by exporting, in the terminal that runs the release (`GB_KEY_ALIAS` defaults to
   `geekbridge-release`):

   ```sh
   export GB_KEYSTORE=~/secrets/geekbridge-release.p12 GB_KEYSTORE_PASS_FILE=~/secrets/geekbridge-release.pass
   ```

Send back: the plain-hex signer SHA-256 (it is public: it goes into the release notes) and "backed up twice". Do not send
the files or the password. I then run `scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md`
with them. Background: [builds.md](builds.md#first-release-android-signing-key), [release-local.md](release-local.md).

Rehearsed on 2026-11-06 with a throwaway key in a scratch `HOME`: these exact commands worked, and the only manual extras are
that `scripts/android-env.sh` must be sourced in the subshell (as above) and that `scripts/release-local.sh` needs `unzip` and
`strings` (binutils) besides the tools it already checked. Timings of the full dry run on this machine: checks 10 min, desktop
build 11, Arch package 15, Android 4, about 45 min in total; 4 GB of scratch under `$GB_RELEASE_DIR`.

## 2. Security contact and the release-notes placeholder

Unblocks: the release notes (the script publishes whatever is in the file, placeholder included) and `SECURITY.md`.
Private vulnerability reporting is already enabled, so this is a monitored address in addition to it.

Edit two places (or tell me the address and I do it):

* `SECURITY.md` line 8: replace the sentence and the comment with the address, for example
  `- If that is unavailable, write to <security-address>.` (or delete the whole bullet to rely on the private report form only).
* `docs/release-notes-v0.1.0.md`, the `Security contact:` line near the end (line 147): replace `**[OWNER: add the monitored security contact here before publishing]**`
  with `<security-address>` (or "the repository's *Security* tab only").

Send back: the address, or "form only".

## 3. GitHub settings

Unblocks: dev APK signing and publishing from CI (releases themselves are local), and a CI gate on `main`. Needs
`gh auth login` as the repository owner. The repository currently has no environments, secrets or variables.

### 3a. Dev signing environment, restricted to `main`

The dev key is the debug key the phone's current debug build is signed with (`.tools/android-user/debug.keystore`, store
and key password `android`, alias `androiddebugkey`; see [reports/dev-channel](reports/dev-channel.md)). Anyone who can
run workflows on `main` can use it, so it must hold nothing valuable. Pick the environment name `dev-signing`:

```sh
gh api -X PUT repos/fspecii/geekbridge/environments/dev-signing --input - <<'JSON'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
JSON
gh api -X POST repos/fspecii/geekbridge/environments/dev-signing/deployment-branch-policies -f name=main -f type=branch

# the pin: lowercase hex SHA-256 of the dev certificate
( source scripts/android-env.sh
  keytool -list -v -keystore .tools/android-user/debug.keystore -storepass android | grep 'SHA256:' )
gh variable set DEV_SIGNER_SHA256 --repo fspecii/geekbridge --body <64-hex-dev-signer-sha256-no-colons-lowercase>

# secrets live in the environment only, not at repository level
base64 -w0 .tools/android-user/debug.keystore | gh secret set DEV_KEYSTORE_B64 --env dev-signing --repo fspecii/geekbridge
gh secret set DEV_KEYSTORE_PASSWORD --env dev-signing --repo fspecii/geekbridge --body android
gh secret set DEV_KEY_ALIAS --env dev-signing --repo fspecii/geekbridge --body androiddebugkey
```

Verify: `gh api repos/fspecii/geekbridge/environments/dev-signing --jq '.deployment_branch_policy'` shows
`custom_branch_policies: true`, and `gh api repos/fspecii/geekbridge/environments/dev-signing/deployment-branch-policies --jq '.branch_policies[].name'` prints `main`.

The `android` job in `.github/workflows/dev-release.yml` names `environment: dev-signing`, which is the only way it can
read environment secrets, so the environment above **must exist** (create it before the next push to `main`; a job naming
a missing environment would create it unprotected, with no branch restriction). If the environment exists but holds no
`DEV_KEYSTORE_B64`, the job skips cleanly with a notice. The publish job runs in its own non-cancelling queue
(`dev-release-publish`) and uploads `dev-manifest.json` last. Releases from `release.yml` (optional, manual) fail unless
the repository variable `ANDROID_SIGNER_SHA256` pins the signer. Leave `DEV_AUTOPUBLISH` unset (publishing stays manual) unless you want CI to
publish the rolling `dev` prerelease.

**Why there is no `dev` release yet (checked 2026-11-06).** The "Dev build (rolling)" runs are green because the
`android` job skips cleanly without `DEV_KEYSTORE_B64` (it reports `built=false`) and the `publish` job is then
skipped by design: it needs the APK, and `DEV_AUTOPUBLISH=true` or a manual run with `publish=true`. At that point the
repository had the `dev-signing` environment but no secrets or variables in it, so only desktop workflow artifacts
exist and `releases/tags/dev` answers 404 (the updater reports "no dev build published yet"). To get the channel, either
(a) set the 3a secrets and `DEV_SIGNER_SHA256`, then set `DEV_AUTOPUBLISH=true` (`gh variable set DEV_AUTOPUBLISH --body true --repo fspecii/geekbridge`)
or start the workflow once with `gh workflow run dev-release.yml -f publish=true`; or (b) run `scripts/release-dev.sh --publish`
on your machine. A green run does not mean published: look for the job "Update the rolling dev prerelease" not being "skipped".
The `dev-manifest.json` that `scripts/release-dev.sh` writes (`commit`, `versionCode`, `builtAt`, `notes`) is what
`crates/bridge-update/src/dev.rs` parses.

### 3b. Branch protection status checks

`main` is protected but requires nothing (force pushes and deletions are blocked, admins not enforced). To require the
Rust gates, using the job names in `.github/workflows/check.yml` (the `PUT` replaces the whole protection object, so the
body repeats the current state):

```sh
gh api -X PUT repos/fspecii/geekbridge/branches/main/protection --input - <<'JSON'
{
  "required_status_checks": {"strict": false, "contexts": [
    "Rust / ubuntu-latest", "Rust / windows-latest", "Rust / macos-latest",
    "Performance budget (Linux, no emulator)", "Type-check Windows and macOS targets (from Linux)"]},
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
```

Caveats: `check.yml` skips docs-only changes, so a docs-only pull request would wait forever for these checks (you, as an
admin, can still push; agents pushing as you are unaffected while `enforce_admins` is false). If a check is flaky on a
runner, it blocks pull requests until re-run. If you would rather not gate yet, skip 3b; nothing else depends on it.

Send back: "3a done" with the dev signer hex, and "3b yes/no".

## 4. Dependabot triage: say "go" or choose

Details and evidence: [reports/dependabot-triage-2](reports/dependabot-triage-2.md). "go" means: I apply the proposed
`.github/dependabot.yml` (weekly, limit 2, ignores for the migrations), rebase PR #1 (actions) and merge it only after a
green run on a quiet `main`, and close #2 and #4 to #9 with a comment. I do none of it without your word. Optional
extras from the same report: enable "Automatically delete head branches", set a homepage URL.

Send back: "go" or the PR numbers to leave open.

## 5. Real-device test

Needs the phone, an iPhone to call it, a USB Bluetooth dongle and the PC. Follow
[reports/morning-test-checklist](reports/morning-test-checklist.md) (plan: [real-device-test-plan](real-device-test-plan.md));
its APK is built from a specific head, so ask me to rebuild first if main moved. On the PC collect
`geekbridge-ctl doctor` and `scripts/realdevice-report.sh` output after each section.

Send back: pass or fail per section and the redacted reports. Unblocks: the "Known limitations" wording in the release
notes (every row not confirmed on hardware stays "not yet"), and a go on item 12 of the go/no-go table.

## 6. Decisions to accept (reply with the list, yes or no each)

1. **Residual risks** of security reviews 5 to 8, as listed in row 4 of the table in
   [reports/stable-go-no-go](reports/stable-go-no-go.md) (reviews: [security-review-7](reports/security-review-7.md) and its siblings).
2. **Windows and macOS**: no prebuilt binaries in v0.1.0, unsigned, CI-tested only (as the release notes already say).
3. **Name and trademark**: "GeekBridge" and the `dev.geekbridge` identifiers are yours to use.
4. **Support channel**: issues only, discussions, or an address.
5. **SBOM**: the CycloneDX file is generated by the release script; confirm you want it as a release asset.

Unblocks: ticking the open boxes in [release-checklist](release-checklist.md), and the final go.

## 7. The release itself (only after I report a green settled head and a passing dry run)

```sh
export GB_KEYSTORE=~/secrets/geekbridge-release.p12 GB_KEYSTORE_PASS_FILE=~/secrets/geekbridge-release.pass
scripts/release-local.sh v0.1.0 --dry-run --notes-file docs/release-notes-v0.1.0.md   # rehearsal, publishes nothing
scripts/release-local.sh v0.1.0 --tag --push-tag --publish --notes-file docs/release-notes-v0.1.0.md
```

Before it: set the date under `## [0.1.0]` in `CHANGELOG.md` to the release day. The script requires a clean tree on
`main` that is already on `origin/main`, creates a draft, uploads, and only then publishes. Afterwards check for an update
from an rc.1 install: the debug-signed rc.1 APK cannot update in place, so uninstall it first
(see "Upgrading from 0.1.0-rc.1" in the release notes).
