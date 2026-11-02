#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prepare a release: bump the single version source, move CHANGELOG "Unreleased" under the new version,
# and print the exact git steps. It never commits, tags or pushes.
#
#   scripts/release.sh [--dry-run] [--skip-tests] X.Y.Z[-pre]
#
#   --dry-run     run every check and show what would change; edit nothing
#   --skip-tests  skip fmt/clippy/tests (only for re-running after they already passed)
#
# Single version source: [workspace.package] version in Cargo.toml. The Android build reads it from there
# (versionName and versionCode) and .github/workflows/release.yml requires the tag vX.Y.Z to equal it.
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

DRY=0
TESTS=1
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY=1 ;;
        --skip-tests) TESTS=0 ;;
        -h|--help) sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 0 ;;
        -*) echo "unknown option: $arg" >&2; exit 2 ;;
        *) [[ -z "$VERSION" ]] || { echo "only one version expected" >&2; exit 2; }; VERSION="$arg" ;;
    esac
done
[[ -n "$VERSION" ]] || { echo "usage: scripts/release.sh [--dry-run] [--skip-tests] X.Y.Z" >&2; exit 2; }
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "not a version: $VERSION (want X.Y.Z or X.Y.Z-rc.1)" >&2; exit 2; }

current="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml)"
[[ -n "$current" ]] || { echo "no [workspace.package] version in Cargo.toml" >&2; exit 1; }
[[ "$current" != "$VERSION" ]] || { echo "Cargo.toml is already at $VERSION" >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
    echo "tag v$VERSION already exists" >&2; exit 1
fi

# 1. Clean tree: the release commit must contain only the bump.
if [[ -n "$(git status --porcelain)" ]]; then
    echo "working tree is not clean; commit or remove these first:" >&2
    git status --short >&2
    exit 1
fi
branch="$(git rev-parse --abbrev-ref HEAD)"
[[ "$branch" == "main" ]] || echo "warning: on '$branch', the release workflow only accepts tags reachable from main" >&2

# 2. Tests green.
if [[ "$TESTS" == 1 ]]; then
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
fi

# 3. CHANGELOG needs something to release.
grep -q '^## \[Unreleased\]' CHANGELOG.md || { echo "CHANGELOG.md has no '## [Unreleased]' section" >&2; exit 1; }
if [[ "$DRY" == 1 ]]; then
    echo "dry run: would bump $current -> $VERSION in Cargo.toml, Cargo.lock, CHANGELOG.md and packaging/arch/PKGBUILD"
    exit 0
fi

# 4. Bump.
today="$(date -u +%F)"
source "$ROOT/scripts/lib-tmp.sh"
tmp="$(gb_mktemp release)"
trap 'rm -f "$tmp"' EXIT
awk -v v="$VERSION" '/^\[workspace.package\]/{s=1} /^\[/ && !/^\[workspace.package\]/{s=0}
    s && /^version[[:space:]]*=/ && !done {print "version = \"" v "\""; done=1; next} {print}' Cargo.toml > "$tmp"
cat "$tmp" > Cargo.toml
awk -v v="$VERSION" -v d="$today" '/^## \[Unreleased\]/ {print; print ""; print "## [" v "] - " d; next} {print}' CHANGELOG.md > "$tmp"
cat "$tmp" > CHANGELOG.md
# The Arch package version must equal the Cargo one (packaging/arch/build-local.sh refuses otherwise). A hyphen is
# not allowed in a pkgver, so a prerelease (X.Y.Z-rc.1) is left for the maintainer to map by hand.
if [[ "$VERSION" != *-* ]]; then
    sed -i -e "s/^pkgver=.*/pkgver=$VERSION/" -e 's/^pkgrel=.*/pkgrel=1/' packaging/arch/PKGBUILD
else
    echo "note: packaging/arch/PKGBUILD pkgver not changed ('-' is not allowed there); set it by hand if you package this prerelease." >&2
fi
# The "What's new" texts (Android asset, desktop embed) come from the CHANGELOG section of this version.
scripts/gen-whats-new.sh >/dev/null || echo "warning: scripts/gen-whats-new.sh failed; run it by hand and add its output to the release commit." >&2
# Cargo.lock must carry the new version; if neither command works the bump is incomplete, so say so.
if ! cargo update --workspace --offline >/dev/null 2>&1; then
    cargo check --workspace --offline >/dev/null 2>&1 || {
        echo "error: could not refresh Cargo.lock for $VERSION ('cargo update --workspace --offline' and 'cargo check --offline' both failed)." >&2
        echo "       Cargo.toml, CHANGELOG.md and PKGBUILD were already changed; fix the cargo problem, run 'cargo update --workspace', then review 'git diff'." >&2
        exit 1
    }
fi

echo
echo "Bumped $current -> $VERSION (Cargo.toml, Cargo.lock, CHANGELOG.md, packaging/arch/PKGBUILD, what's-new files). Nothing is committed, tagged or pushed."
echo "Review CHANGELOG.md, then run these yourself:"
echo
echo "  git diff"
echo "  git add Cargo.toml Cargo.lock CHANGELOG.md packaging/arch/PKGBUILD crates/bridge-core/assets/whats_new.txt android/app/src/main/assets/whats_new.json"
echo "  git commit -m \"Release v$VERSION\""
echo "  git push origin main            # the tag must be reachable from main"
echo "  git tag -a v$VERSION -m \"GeekBridge v$VERSION\""
echo "  git push origin v$VERSION       # starts .github/workflows/release.yml"
