#!/usr/bin/env bash
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
        -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
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
    echo "dry run: would bump $current -> $VERSION in Cargo.toml, Cargo.lock and CHANGELOG.md"
    exit 0
fi

# 4. Bump.
today="$(date -u +%F)"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
awk -v v="$VERSION" '/^\[workspace.package\]/{s=1} /^\[/ && !/^\[workspace.package\]/{s=0}
    s && /^version[[:space:]]*=/ && !done {print "version = \"" v "\""; done=1; next} {print}' Cargo.toml > "$tmp"
cat "$tmp" > Cargo.toml
awk -v v="$VERSION" -v d="$today" '/^## \[Unreleased\]/ {print; print ""; print "## [" v "] - " d; next} {print}' CHANGELOG.md > "$tmp"
cat "$tmp" > CHANGELOG.md
cargo update --workspace --offline >/dev/null 2>&1 || cargo check --workspace --offline >/dev/null 2>&1 || true

echo
echo "Bumped $current -> $VERSION (Cargo.toml, Cargo.lock, CHANGELOG.md). Nothing is committed, tagged or pushed."
echo "Review CHANGELOG.md, then run these yourself:"
echo
echo "  git diff"
echo "  git add Cargo.toml Cargo.lock CHANGELOG.md"
echo "  git commit -m \"Release v$VERSION\""
echo "  git push origin main            # the tag must be reachable from main"
echo "  git tag -a v$VERSION -m \"GeekBridge v$VERSION\""
echo "  git push origin v$VERSION       # starts .github/workflows/release.yml"
