#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build, verify and (only on request) publish the ROLLING DEV build: a GitHub prerelease with the fixed tag `dev`
# that always carries the newest main build. The Dev update channel of the apps reads it (docs/updates.md#dev-channel).
#
#   scripts/release-dev.sh [--dry-run] [--publish] [--yes] [options]
#
#   --dry-run       the DEFAULT. Builds and verifies everything locally, prints the exact `gh` commands, publishes nothing.
#   --publish       after building and verifying, update the rolling `dev` prerelease: upload the assets (replacing the old
#                   ones, the manifest last), move the `dev` tag to this commit, keep the release a prerelease and never
#                   the repository's "latest". Asks you to type `publish dev` unless --yes. Needs `gh` logged in with write
#                   access. Nothing else is ever touched: no stable tag, no stable release.
#   --yes           skip the confirmation prompt of --publish (for a script you wrote yourself). Refused for an APK unless the
#                   signer is pinned (GB_DEV_SIGNER_SHA256 or the file below) or --allow-unpinned is given: nobody reads the
#                   signer line in an unattended run, and the pin is what stops a wrong or substituted keystore.
#   --allow-unpinned  with --yes: publish an APK although no signer is pinned (tests; the first publish from a new machine)
#   --skip-android  no APK (the release then carries only the desktop archives)
#   --skip-desktop  no Linux archive
#   --extra-assets DIR  also include geekbridge-{windows,macos}-*.{zip,tar.gz} found in DIR (artifacts of dev-release.yml
#                   downloaded with `gh run download`); other file names are ignored
#   --assets-from DIR   do not build: take geekbridge-debug.apk / geekbridge-linux-x86_64.tar.gz from DIR (tests)
#   --notes TEXT    one-line note for the manifest (default: the subjects of the last 5 commits)
#   --repo OWNER/NAME   release repository (default: the origin remote, else fspecii/geekbridge)
#   --out DIR       output directory (default: $GB_DEV_RELEASE_DIR or ~/.cache/gb-release-dev/<commit>)
#   -h, --help      this text
#
# The APK is signed with the project's local DEBUG keystore ($ANDROID_USER_HOME/debug.keystore, i.e.
# .tools/android-user/debug.keystore) so that phones which already run a debug build from this machine update IN PLACE.
# The key is never copied, committed or uploaded; only the signed APK is. The script prints the signer certificate
# SHA-256 and refuses --publish when GB_DEV_SIGNER_SHA256 (or ~/.config/geekbridge/dev-signer-sha256) is set and differs.
#
# versionCode of the APK = 20000 + `git rev-list --count HEAD` (monotonic on main), the same number as
# dev-manifest.json's "versionCode". Desktop binaries embed the commit and the count (crates/bridge-update/build.rs).
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

DRY=1 PUBLISH=0 YES=0 UNPINNED=0 ANDROID=1 DESKTOP=1 EXTRA="" FROM="" NOTES="" REPO="" OUT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY=1; PUBLISH=0 ;;
        --publish) PUBLISH=1; DRY=0 ;;
        --yes) YES=1 ;;
        --allow-unpinned) UNPINNED=1 ;;
        --skip-android) ANDROID=0 ;;
        --skip-desktop) DESKTOP=0 ;;
        --extra-assets) EXTRA="${2:?--extra-assets DIR}"; shift ;;
        --assets-from) FROM="${2:?--assets-from DIR}"; shift ;;
        --notes) NOTES="${2:?--notes TEXT}"; shift ;;
        --repo) REPO="${2:?--repo OWNER/NAME}"; shift ;;
        --out) OUT="${2:?--out DIR}"; shift ;;
        -h|--help) sed -n '3,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 64 ;;
    esac
    shift
done

log() { printf '==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

for c in git sha256sum tar gzip python3; do command -v "$c" >/dev/null || die "missing tool: $c"; done
DEV_CODE_BASE=20000   # keep in step with scripts/version-code.sh, android/app/build.gradle.kts, bridge-update

# ---------------------------------------------------------------- what is being built
COMMIT="$(git rev-parse HEAD)"
SHORT="${COMMIT:0:12}"
COUNT="$(git rev-list --count HEAD)"
CODE=$(( DEV_CODE_BASE + COUNT ))
EPOCH="$(git log -1 --format=%ct HEAD)"
DIRTY=0; [[ -z "$(git status --porcelain --untracked-files=no -- . ':!scripts/e2e/wayland.sh')" ]] || DIRTY=1
if [[ -z "$REPO" ]]; then
    REPO="$(git remote get-url origin 2>/dev/null | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##' || true)"
    [[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || REPO="fspecii/geekbridge"
fi
[[ -n "$OUT" ]] || OUT="${GB_DEV_RELEASE_DIR:-$HOME/.cache/gb-release-dev}/$SHORT"
log "Dev build $SHORT (commit count $COUNT, versionCode $CODE) for $REPO$([[ $PUBLISH == 1 ]] || echo ', DRY RUN')"

if (( PUBLISH )); then
    (( DIRTY == 0 )) || die "the working tree has uncommitted changes; the dev build must be exactly a commit"
    [[ "${GITHUB_REF_NAME:-$(git rev-parse --abbrev-ref HEAD)}" == main ]] || die "not on main; the rolling dev build is built from main"
    git fetch -q origin main 2>/dev/null || warn "could not fetch origin/main"
    if git rev-parse -q --verify origin/main >/dev/null; then
        git merge-base --is-ancestor origin/main HEAD || die "HEAD does not contain origin/main (pull first)"
        # A lower commit count than the published build would publish a build older than the one phones already have.
        [[ "$(git rev-list --count origin/main)" -le "$COUNT" ]] || die "origin/main is ahead of HEAD"
    fi
    command -v gh >/dev/null || die "gh is required for --publish"
else
    (( DIRTY == 0 )) || warn "uncommitted changes: this dry run builds them, but --publish would refuse"
fi

mkdir -p "$OUT"
rm -f "$OUT"/geekbridge-* "$OUT"/dev-manifest.json "$OUT"/SHA256SUMS

# ---------------------------------------------------------------- assets
if [[ -n "$FROM" ]]; then
    log "Using prebuilt assets from $FROM (no build)"
    for f in geekbridge-debug.apk geekbridge-linux-x86_64.tar.gz; do
        [[ -f "$FROM/$f" ]] && cp "$FROM/$f" "$OUT/$f"
    done
else
    if (( DESKTOP )); then
        log "Linux desktop archive"
        source "$ROOT/scripts/lib/repro.sh"
        mapfile -t BINS < <(grep -v -e '^#' -e '^[[:space:]]*$' "$ROOT/scripts/release-binaries.txt")
        GEEKBRIDGE_BUILD_COMMIT="$COMMIT" GEEKBRIDGE_BUILD_COUNT="$COUNT" \
            RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$(gb_repro_rustflags "$ROOT")" \
            cargo build --release --locked -p bridge-apps -p bridge-service
        stage="$(mktemp -d "${TMPDIR:-/tmp}/gb-dev-stage.XXXXXX")"
        trap 'rm -rf "$stage"' EXIT
        d="$stage/geekbridge-linux-x86_64"; mkdir -p "$d"
        for b in "${BINS[@]}"; do cp "${CARGO_TARGET_DIR:-$ROOT/target}/release/$b" "$d/"; done
        cp README.md LICENSE THIRD_PARTY_NOTICES.md "$d/" 2>/dev/null || cp README.md "$d/"
        SOURCE_DATE_EPOCH="$EPOCH" gb_repro_tar_gz "$OUT/geekbridge-linux-x86_64.tar.gz" "$stage" geekbridge-linux-x86_64
    fi
    if (( ANDROID )); then
        log "Android debug APK (versionCode $CODE, debug keystore)"
        GB_ANDROID_DIST_DIR="$OUT/android-build" "$ROOT/scripts/build-android.sh" --skip-tests --skip-lint --skip-host-tests
        cp "$OUT/android-build/geekbridge-debug.apk" "$OUT/geekbridge-debug.apk"
        rm -rf "$OUT/android-build"
    fi
fi

if [[ -n "$EXTRA" ]]; then
    shopt -s nullglob
    for f in "$EXTRA"/geekbridge-windows-*.zip "$EXTRA"/geekbridge-macos-*.zip "$EXTRA"/geekbridge-macos-*.tar.gz; do
        cp "$f" "$OUT/"; note "extra asset: $(basename "$f")"
    done
    shopt -u nullglob
fi

compgen -G "$OUT/geekbridge-*" >/dev/null || die "no assets were produced"

# ---------------------------------------------------------------- verify the APK
SIGNER=""
if [[ -f "$OUT/geekbridge-debug.apk" ]]; then
    log "Verifying the APK"
    if [[ -n "${ANDROID_HOME:-}" || -d "$ROOT/.tools/android-sdk" ]]; then
        bt="$(ls -d "${ANDROID_HOME:-$ROOT/.tools/android-sdk}"/build-tools/* 2>/dev/null | sort -V | tail -1)"
        if [[ -n "$bt" && -x "$bt/apksigner" ]]; then
            verify="$("$bt/apksigner" verify --print-certs "$OUT/geekbridge-debug.apk")" || die "APK signature does not verify"
            SIGNER="$(sed -n 's/^Signer #1 certificate SHA-256 digest: *//p' <<<"$verify" | head -1 | tr 'A-F' 'a-f')"
            code="$("$bt/aapt2" dump badging "$OUT/geekbridge-debug.apk" | sed -n "s/^package: .*versionCode='\([0-9]*\)'.*/\1/p")"
            [[ "$code" == "$CODE" ]] || die "APK versionCode is $code, expected $CODE (20000 + $COUNT commits)"
            note "versionCode $code, signer certificate SHA-256 $SIGNER"
        else
            warn "no apksigner/aapt2 found: the APK signature and versionCode were NOT checked"
        fi
    fi
    want="$(tr -d ': \n' <<<"${GB_DEV_SIGNER_SHA256:-$(cat "$HOME/.config/geekbridge/dev-signer-sha256" 2>/dev/null || true)}" | tr 'A-F' 'a-f')"
    if [[ -n "$want" ]]; then
        [[ "$SIGNER" == "$want" ]] || die "APK signer $SIGNER differs from the pinned dev signer $want: wrong keystore, phones could not update in place"
        note "signer matches the pinned dev signer"
    elif (( PUBLISH && YES && ! UNPINNED )); then
        die "no pinned dev signer (GB_DEV_SIGNER_SHA256 or ~/.config/geekbridge/dev-signer-sha256) and no one to read the signer line: pin it, or pass --allow-unpinned knowingly"
    else
        warn "no dev signer is pinned (GB_DEV_SIGNER_SHA256 or ~/.config/geekbridge/dev-signer-sha256); write $SIGNER there once you have checked it is the key of your phones' current debug builds"
    fi
fi

# ---------------------------------------------------------------- manifest, checksums
[[ -n "$NOTES" ]] || NOTES="$(git log -5 --format=%s HEAD | paste -sd'|' - | sed 's/|/ | /g')"
BUILT_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
python3 -I - "$OUT/dev-manifest.json" "$COMMIT" "$CODE" "$BUILT_AT" "$NOTES" <<'PY'
import json, sys
path, commit, code, built, notes = sys.argv[1:6]
with open(path, "w") as f:
    json.dump({"commit": commit, "versionCode": int(code), "builtAt": built, "notes": notes[:1500]}, f, indent=2)
    f.write("\n")
PY
( cd "$OUT" && sha256sum -- geekbridge-* dev-manifest.json > SHA256SUMS && sha256sum -c --quiet SHA256SUMS ) || die "SHA256SUMS does not match"
log "Assets in $OUT"
( cd "$OUT" && ls -l geekbridge-* dev-manifest.json SHA256SUMS && cat SHA256SUMS )

# ---------------------------------------------------------------- publish (or show how)
ASSETS=()
for f in "$OUT"/geekbridge-*; do ASSETS+=("$f"); done
TITLE="Dev build $SHORT"
BODY="Rolling development build of main at $COMMIT (commit count $COUNT, versionCode $CODE). Not a release: for testing, replaced by every newer build. The Android APK is debug-signed. Verify downloads against SHA256SUMS."

run() { if (( PUBLISH )); then "$@"; else printf '  [dry-run] %q' "$1"; shift; printf ' %q' "$@"; printf '\n'; fi; }

publish_all() {
    if (( PUBLISH )) && gh release view dev --repo "$REPO" >/dev/null 2>&1; then
        exists=1
    else
        exists=0
        (( PUBLISH )) || exists=1   # show the update path in a dry run
    fi
    if (( exists )); then
        # Update in place: new assets replace old ones, the manifest goes last (clients decide by it), then the tag moves.
        run gh release upload dev "${ASSETS[@]}" "$OUT/SHA256SUMS" --clobber --repo "$REPO"
        if (( PUBLISH )); then
            local have name keep
            have="$(gh release view dev --repo "$REPO" --json assets --jq '.assets[].name')"
            while IFS= read -r name; do
                [[ -n "$name" ]] || continue
                keep=0
                for f in "${ASSETS[@]}" "$OUT/SHA256SUMS" "$OUT/dev-manifest.json"; do [[ "$(basename "$f")" == "$name" ]] && keep=1; done
                (( keep )) || gh release delete-asset dev "$name" --yes --repo "$REPO"
            done <<<"$have"
        else
            printf '  [dry-run] (assets of the old dev release that are not part of this build are deleted)\n'
        fi
        run gh release upload dev "$OUT/dev-manifest.json" --clobber --repo "$REPO"
        run gh api -X PATCH "repos/$REPO/git/refs/tags/dev" -f "sha=$COMMIT" -F force=true
        run gh release edit dev --repo "$REPO" --title "$TITLE" --notes "$BODY" --prerelease --latest=false
    else
        run gh release create dev "${ASSETS[@]}" "$OUT/SHA256SUMS" "$OUT/dev-manifest.json" --repo "$REPO" \
            --target "$COMMIT" --title "$TITLE" --notes "$BODY" --prerelease --latest=false --draft
        run gh release edit dev --repo "$REPO" --draft=false --prerelease --latest=false
    fi
    if (( PUBLISH )); then
        local latest
        latest="$(gh api "repos/$REPO/releases/latest" --jq .tag_name 2>/dev/null || true)"
        [[ "$latest" != dev ]] || die "the dev release became the repository's latest release; fix it with: gh release edit dev --latest=false --repo $REPO"
        gh release view dev --repo "$REPO" --json url,isPrerelease --jq '"published: \(.url) prerelease=\(.isPrerelease)"'
    fi
}

if (( PUBLISH )); then
    if (( ! YES )); then
        printf 'This updates the rolling `dev` prerelease of %s to %s (versionCode %s) and moves its tag.\nType `publish dev` to continue: ' "$REPO" "$SHORT" "$CODE" >&2
        read -r answer || answer=""
        [[ "$answer" == "publish dev" ]] || die "not confirmed; nothing was published"
    fi
    publish_all
else
    log "Dry run: nothing is published. These are the commands --publish would run:"
    publish_all
    note "To publish: scripts/release-dev.sh --publish"
fi
