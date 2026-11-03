#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Cheap, offline checks that the stable-release plumbing still holds. Builds nothing and publishes nothing:
# release-local.sh is only run with --dry-run and always stops in preflight (it must refuse before any build).
# Keys are throwaway and live under $GB_SCRATCH (default ~/.cache/gb-scratch), never in the repository.
#   scripts/tests/release-readiness-test.sh
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/relready-XXXXXX")"
trap 'rm -rf -- "$T"' EXIT
fail=0
ok()  { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }
expect_fail() { # expect_fail DESC PATTERN cmd...  -> must exit non-zero and mention PATTERN
    local d="$1" pat="$2" out; shift 2
    if out="$("$@" 2>&1)"; then bad "$d: succeeded but must be refused"
    elif grep -qE -- "$pat" <<<"$out"; then ok "$d"
    else bad "$d: refused with the wrong message: $(tail -n1 <<<"$out")"; fi
}

# --- version codes: monotonic, final above its own release candidates, current above the last tag
scripts/version-code.sh --self-test >/dev/null && ok "version-code self-test" || bad "version-code self-test"
ver="$(scripts/version-code.sh --code)"
[[ "$ver" =~ ^[1-9][0-9]*$ ]] && ok "Cargo.toml versionCode $ver" || bad "bad versionCode '$ver'"
prev=0
for v in 0.1.0-rc.1 0.1.0-rc.2 0.1.0 0.1.1 0.2.0-rc.1 0.2.0; do
    c="$(scripts/version-code.sh --code "$v")"; (( c > prev )) || bad "versionCode of $v ($c) not above $prev"; prev=$c
done
cargo_v="$(scripts/version-code.sh | cut -d' ' -f1)"
while read -r t; do
    [[ -n "$t" ]] || continue
    (( $(scripts/version-code.sh --code "$cargo_v") >= $(scripts/version-code.sh --code "${t#v}") )) \
        && ok "Cargo version $cargo_v is not below tag $t" || bad "Cargo version $cargo_v is below existing tag $t"
done < <(git tag --list 'v[0-9]*' 2>/dev/null)

# --- changelog section for the version in Cargo.toml has a body (release-local uses it as the notes)
body="$(awk -v v="$cargo_v" '$0 ~ "^## \\[" v "\\]" {on=1; next} on && /^## \[/ {exit} on {print}' CHANGELOG.md)"
[[ -n "${body//[$' \t\n']/}" ]] && ok "CHANGELOG.md has a non-empty [$cargo_v] section" || bad "CHANGELOG.md [$cargo_v] section missing or empty"
grep -q '^## \[Unreleased\]' CHANGELOG.md && ok "CHANGELOG.md keeps an [Unreleased] heading" || bad "no [Unreleased] heading (release.sh needs it)"

# --- generated files are fresh
scripts/gen-notices.sh --check >/dev/null 2>&1 && ok "THIRD_PARTY_NOTICES.md fresh" || bad "THIRD_PARTY_NOTICES.md stale (scripts/gen-notices.sh)"
scripts/gen-whats-new.sh --check >/dev/null 2>&1 && ok "whats-new files fresh" || bad "whats-new files stale (scripts/gen-whats-new.sh)"

# --- SECURITY.md: contact placeholder is reported (owner action), the file must still offer private reporting
grep -q 'OWNER:' SECURITY.md && echo "note SECURITY.md still has the OWNER contact placeholder (owner-only; see docs/reports/stable-release-readiness.md)"
grep -qi 'Report a vulnerability' SECURITY.md && ok "SECURITY.md names private reporting" || bad "SECURITY.md lost the private reporting route"

# --- keystore script and signer pin, with a throwaway key
if ! command -v keytool >/dev/null && [[ -f scripts/android-env.sh ]]; then source scripts/android-env.sh >/dev/null 2>&1 || true; fi
if command -v keytool >/dev/null; then
    K="$T/k"; mkdir -p "$K"
    out="$(scripts/make-keystore.sh --non-interactive --keyalg ec --out "$K/a.p12" --password-file "$K/a.pass")" && ok "make-keystore creates a key" || bad "make-keystore failed"
    [[ "$(stat -c %a "$K/a.p12" "$K/a.pass" | sort -u)" == 600 ]] && ok "keystore and password file are 0600" || bad "key files are not 0600"
    pin="$(sed -n 's/.*(repo variable form: \([0-9a-f]\{64\}\)).*/\1/p' <<<"$out")"
    [[ -n "$pin" ]] && ok "fingerprint printed" || bad "no fingerprint printed"
    ! grep -qF -- "$(cat "$K/a.pass")" <<<"$out" && ok "password never printed" || bad "password leaked to stdout"
    expect_fail "make-keystore refuses to overwrite" "already exists" scripts/make-keystore.sh --non-interactive --out "$K/a.p12" --password-file "$K/b.pass"
    expect_fail "make-keystore refuses a path inside the repository" "git work tree" scripts/make-keystore.sh --non-interactive --out "$ROOT/zz-test.p12" --password-file "$K/c.pass"
    expect_fail "make-keystore needs >= 25 years" "25" scripts/make-keystore.sh --non-interactive --years 5 --out "$K/d.p12" --password-file "$K/d.pass"
    [[ ! -e "$ROOT/zz-test.p12" ]] || bad "a key file was left in the repository"

    # The signing path release-local.sh uses: apksigner must accept the password file make-keystore.sh wrote (no newline).
    if command -v apksigner >/dev/null; then
        source scripts/lib/signpass.sh
        python3 -I -c "import zipfile,sys; z=zipfile.ZipFile(sys.argv[1],'w'); z.writestr('a.txt','x'); z.close()" "$K/in.zip"
        gb_signing_pass_file "$K/a.pass" "$K/apk.pass"
        if apksigner sign --ks "$K/a.p12" --ks-pass "file:$K/apk.pass" --key-pass "file:$K/apk.pass" --ks-key-alias geekbridge-release \
              --min-sdk-version 26 --v4-signing-enabled false --out "$K/out.apk" "$K/in.zip" >/dev/null 2>&1; then
            [[ -s "$K/out.apk" ]] && ok "apksigner signs with the password file make-keystore.sh wrote (via gb_signing_pass_file)" || bad "apksigner wrote no output"
        else bad "apksigner could not sign with the password file written by make-keystore.sh"; fi
    else echo "skip apksigner signing check: no apksigner"; fi

    export GB_RELEASE_DIR="$T/rel" GB_SIGNER_FILE="$T/no-such-pin" GB_MIN_FREE_GB=1
    unset ANDROID_SIGNER_SHA256 GB_KEYSTORE GB_KEYSTORE_PASS_FILE || true
    rl=(scripts/release-local.sh v0.1.0 --dry-run)
    expect_fail "stable tag without keystore" "needs the release keystore" "${rl[@]}"
    GB_KEYSTORE="$K/a.p12" GB_KEYSTORE_PASS_FILE="$K/a.pass" expect_fail "stable tag without pinned signer" "without a pinned signer" "${rl[@]}"
    GB_KEYSTORE="$K/a.p12" GB_KEYSTORE_PASS_FILE="$K/a.pass" ANDROID_SIGNER_SHA256="$(printf 'ab%.0s' {1..32})" \
        expect_fail "wrong signer pin" "does not match the pinned" "${rl[@]}"
    GB_KEYSTORE="$K/a.p12" GB_KEYSTORE_PASS_FILE="$K/a.pass" ANDROID_SIGNER_SHA256="abc" \
        expect_fail "malformed signer pin" "64 hex" "${rl[@]}"
    expect_fail "stable tag with --prerelease" "looks stable" scripts/release-local.sh v0.1.0 --prerelease --dry-run
    expect_fail "pre-release tag without --prerelease" "pass --prerelease" scripts/release-local.sh v0.1.0-rc.2 --dry-run
    # --notes-file: the file replaces the CHANGELOG section; missing/empty files are refused. --print-notes stops before any build.
    printf 'Line one\n\nLine two\n' > "$T/notes.md"; : > "$T/empty.md"
    got="$(cd "$T" && "$ROOT/scripts/release-local.sh" v0.1.0 --dry-run --print-notes --notes-file notes.md 2>/dev/null | sed "1,/^----- release notes -----$/d")" || got="(failed)"
    [[ "$got" == $'Line one\n\nLine two' ]] && ok "--notes-file (relative path) supplies the notes body" || bad "--notes-file notes body wrong: $got"
    got="$(scripts/release-local.sh v0.1.0 --dry-run --print-notes --notes-file="$T/notes.md" 2>/dev/null | sed "1,/^----- release notes -----$/d")" || got="(failed)"
    [[ "$got" == $'Line one\n\nLine two' ]] && ok "--notes-file=PATH form works" || bad "--notes-file=PATH wrong: $got"
    expect_fail "--notes-file missing file" "not a readable file" scripts/release-local.sh v0.1.0 --dry-run --print-notes --notes-file "$T/none.md"
    expect_fail "--notes-file empty file" "is empty" scripts/release-local.sh v0.1.0 --dry-run --print-notes --notes-file "$T/empty.md"
    expect_fail "--notes-file without a value" "needs a path" scripts/release-local.sh v0.1.0 --dry-run --notes-file
    got="$(scripts/release-local.sh v0.1.0 --dry-run --print-notes 2>/dev/null)"
    grep -q '^### Highlights' <<<"$got" && ok "without --notes-file the CHANGELOG section is used" || bad "CHANGELOG section not used by default"
    ok "(release-local.sh was never run past preflight)"
else
    echo "skip keystore checks: no keytool (scripts/bootstrap-android.sh)"
fi
(( fail == 0 )) && echo "release-readiness OK" || { echo "release-readiness FAILED"; exit 1; }
