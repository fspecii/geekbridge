#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# scripts/release-dev.sh against a stub `gh` and prebuilt dummy assets (no build, no network, nothing published):
#   default is a dry run that never calls gh; --publish refuses a dirty tree and a branch other than main; a first
#   publish creates a draft prerelease with --latest=false and then undrafts it; a later one replaces assets with
#   --clobber, uploads dev-manifest.json LAST and moves the tag; a release that became "latest" is reported;
#   dev-manifest.json carries the commit and versionCode 20000 + commit count; SHA256SUMS verifies.
# Run: scripts/tests/release-dev-test.sh
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/devreltest-XXXXXX")"; : >"$T/.gb-owned"
trap 'rm -rf -- "$T"' EXIT
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }
eq() { if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1: got '$2', want '$3'"; fi; }
has() { if grep -qF -- "$3" <<<"$2"; then ok "$1"; else bad "$1: missing '$3' in: $2"; fi; }

# a throw-away repo with an origin, so the script's ROOT is this repo
git init -q --bare "$T/origin.git"
git init -q -b main "$T/repo"
mkdir -p "$T/repo/scripts" "$T/assets" "$T/bin"
cp "$src/scripts/release-dev.sh" "$T/repo/scripts/"
cd "$T/repo"
git config user.email t@example.invalid; git config user.name t
git remote add origin "$T/origin.git"
echo a >a; git add -A; git commit -q -m "first"
echo b >b; git add -A; git commit -q -m "second; with semicolon"
git push -q origin main
echo apk >"$T/assets/geekbridge-debug.apk"; echo tgz >"$T/assets/geekbridge-linux-x86_64.tar.gz"

cat >"$T/bin/gh" <<'STUB'
#!/usr/bin/env bash
echo "gh $*" >>"$GH_LOG"
case "$*" in
  "release view dev"*"--json assets"*) printf 'geekbridge-debug.apk\nold-leftover.zip\n' ;;
  "release view dev"*"--json url"*) echo "published: https://example.invalid/dev prerelease=true" ;;
  "release view dev"*) [[ "${GH_EXISTS:-0}" == 1 ]] || exit 1 ;;
  "api repos/"*"/releases/latest"*) echo "${GH_LATEST:-v0.1.0}" ;;
esac
exit 0
STUB
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH" GH_LOG="$T/gh.log" HOME="$T/home" GB_DEV_RELEASE_DIR="$T/out"
mkdir -p "$HOME"
S=scripts/release-dev.sh
count="$(git rev-list --count HEAD)"; commit="$(git rev-parse HEAD)"

# 1. default = dry run
: >"$GH_LOG"
out="$($S --assets-from "$T/assets" 2>&1)"
eq "dry run never calls gh" "$(cat "$GH_LOG")" ""
has "dry run prints the release command" "$out" "--latest=false"
has "dry run says nothing is published" "$out" "nothing is published"
o="$T/out/${commit:0:12}"
eq "manifest commit" "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["commit"])' "$o/dev-manifest.json")" "$commit"
eq "manifest versionCode" "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["versionCode"])' "$o/dev-manifest.json")" "$((20000 + count))"
has "manifest notes" "$(cat "$o/dev-manifest.json")" "second; with semicolon"
( cd "$o" && sha256sum -c --quiet SHA256SUMS ) && ok "SHA256SUMS verifies" || bad "SHA256SUMS"
grep -q ' dev-manifest.json$' "$o/SHA256SUMS" && grep -q ' geekbridge-debug.apk$' "$o/SHA256SUMS" && ok "SHA256SUMS lists apk and manifest" || bad "SHA256SUMS content"

# 2. --publish refuses a dirty tree
echo c >>a
if $S --publish --yes --allow-unpinned --assets-from "$T/assets" >"$T/o2" 2>&1; then bad "publish accepted a dirty tree"; else has "dirty tree refused" "$(cat "$T/o2")" "uncommitted changes"; fi
git checkout -q a

# 3. first publish: draft prerelease, not latest, then undraft
: >"$GH_LOG"
$S --publish --yes --allow-unpinned --assets-from "$T/assets" >"$T/o3" 2>&1 || { cat "$T/o3"; bad "first publish failed"; }
log="$(cat "$GH_LOG")"
has "creates as draft prerelease" "$log" "release create dev"
has "create is a prerelease" "$log" "--prerelease --latest=false --draft"
has "targets the commit" "$log" "--target $commit"
has "undrafts without latest" "$log" "--draft=false --prerelease --latest=false"

# 4. later publish: clobber, manifest last, tag moved, stale asset deleted
: >"$GH_LOG"; export GH_EXISTS=1
$S --publish --yes --allow-unpinned --assets-from "$T/assets" >"$T/o4" 2>&1 || { cat "$T/o4"; bad "update publish failed"; }
log="$(cat "$GH_LOG")"
has "uploads with clobber" "$log" "--clobber"
has "deletes a stale asset" "$log" "release delete-asset dev old-leftover.zip"
up_apk=$(grep -n "geekbridge-debug.apk" <<<"$log" | grep "release upload" | head -1 | cut -d: -f1)
up_man=$(grep -n "dev-manifest.json" <<<"$log" | grep "release upload" | head -1 | cut -d: -f1)
[[ -n "$up_apk" && -n "$up_man" && "$up_man" -gt "$up_apk" ]] && ok "manifest uploaded after the assets" || bad "upload order"
has "moves the tag" "$log" "git/refs/tags/dev -f sha=$commit"
has "stays a prerelease, not latest" "$log" "release edit dev"
has "keeps prerelease" "$log" "--prerelease --latest=false"
case "$log" in *"release delete dev"*|*"push"*) bad "deleted the release or pushed";; *) ok "never deletes the release";; esac

# 5. dev must never end up as "latest"
export GH_LATEST=dev
if $S --publish --yes --allow-unpinned --assets-from "$T/assets" >"$T/o5" 2>&1; then bad "did not notice latest=dev"; else has "latest=dev reported" "$(cat "$T/o5")" "became the repository's latest"; fi
unset GH_LATEST

# 6. not on main
git checkout -q -b topic
if $S --publish --yes --allow-unpinned --assets-from "$T/assets" >"$T/o6" 2>&1; then bad "publish from a topic branch"; else has "non-main refused" "$(cat "$T/o6")" "not on main"; fi

# 7. no confirmation, no publish
git checkout -q main; : >"$GH_LOG"
if echo "no" | $S --publish --assets-from "$T/assets" >"$T/o7" 2>&1; then bad "published without confirmation"; else has "needs the typed confirmation" "$(cat "$T/o7")" "not confirmed"; fi
eq "no gh call without confirmation" "$(cat "$GH_LOG")" ""

# 8. security review 7: a publish with no typed confirmation (--yes) and no pinned signer is refused, because nobody
#    is looking at the signer line: the pin is the defence against a wrong or substituted keystore
git checkout -q main; : >"$GH_LOG"
unset GB_DEV_SIGNER_SHA256; export HOME="$T/home"; mkdir -p "$HOME"
if $S --publish --yes --assets-from "$T/assets" >"$T/o8" 2>&1; then bad "unattended publish without a pinned signer"; else has "unpinned unattended publish refused" "$(cat "$T/o8")" "pinned dev signer"; fi
eq "no gh call without a pin" "$(cat "$GH_LOG")" ""
# ... and with a pin that the APK cannot show (no apksigner here) it is refused too, never waved through
if GB_DEV_SIGNER_SHA256="$(printf 'ab%.0s' {1..32})" PATH="/usr/bin:/bin:$T/bin" $S --publish --yes --assets-from "$T/assets" >"$T/o9" 2>&1; then bad "publish with an unverifiable pin"; else has "unverifiable pin refused" "$(cat "$T/o9")" "differs from the pinned dev signer"; fi

[[ $fail == 0 ]] && echo "release-dev test OK"
exit $fail
