#!/usr/bin/env bash
# Local dry run of .github/workflows/release.yml: the shell steps are extracted from the YAML and executed in
# throw-away directories with fake tags/binaries (no GitHub, no network, no artifacts of the working tree touched).
#   scripts/e2e/release-dryrun.sh
# Covered: YAML structure + pinned actions, tag/Cargo.toml version gate, desktop Package step (Linux branch),
# scripts/package-linux.sh, SHA256SUMS step, Android signing step (stable without keystore refused; prerelease
# debug-signed; with a generated keystore the real zipalign + apksigner path, plus signature/versionCode check),
# updater asset-name compatibility. NOT covered (only on GitHub): see docs/e2e-findings.md "release workflow".
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
WF=.github/workflows/release.yml
mkdir -p "$ROOT/target/e2e-update"; T="$(mktemp -d "$ROOT/target/e2e-update/dryrun.XXXXXX")"; trap 'rm -rf "$T"' EXIT
PASS=0; FAIL=0
ok()  { echo "PASS  $1${2:+  ($2)}"; PASS=$((PASS+1)); }
bad() { echo "FAIL  $1${2:+  ($2)}"; FAIL=$((FAIL+1)); }
t()   { if "${@:2}" >"$T/last.log" 2>&1; then ok "$1"; else bad "$1" "$(tail -2 "$T/last.log" | tr '\n' ' ')"; fi; }
tn()  { if "${@:2}" >"$T/last.log" 2>&1; then bad "$1" "expected failure"; else ok "$1" "refused: $(grep -m1 -E 'error|does not|not ' "$T/last.log" | cut -c1-90)"; fi; }

# step JOB STEP-NAME-PREFIX -> prints the step's run script
step() { python3 -I - "$WF" "$1" "$2" <<'EOF'
import sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
for s in wf["jobs"][sys.argv[2]]["steps"]:
    if s.get("name", "").startswith(sys.argv[3]) or s.get("id") == sys.argv[3]:
        print(s["run"]); break
else:
    sys.exit("step not found: " + sys.argv[3])
EOF
}

# ---- 1. structure
python3 -I - "$WF" <<'EOF' >"$T/struct.log" 2>&1
import re, sys, yaml
wf = yaml.safe_load(open(sys.argv[1]))
trig = wf.get("on", wf.get(True))
assert trig["push"]["tags"] == ["v*"], trig
jobs = wf["jobs"]
assert set(jobs) >= {"version", "desktop", "android", "publish"}, list(jobs)
assert jobs["publish"]["needs"] == ["version", "desktop", "android"], jobs["publish"]["needs"]
assert jobs["publish"]["permissions"]["contents"] == "write" and wf["permissions"]["contents"] == "read"
for j in jobs.values():
    for s in j["steps"]:
        u = s.get("uses")
        if u: assert re.search(r"@[0-9a-f]{40}\b", u), "unpinned action: " + u
print("jobs", list(jobs), "all actions pinned to a commit SHA")
EOF
if [[ $? == 0 ]]; then ok "workflow structure" "$(cat "$T/struct.log")"; else bad "workflow structure" "$(tail -3 "$T/struct.log" | tr '\n' ' ')"; fi
command -v actionlint >/dev/null && t "actionlint" actionlint "$WF" || echo "SKIP  actionlint (not installed)"

# ---- 2. version gate: the real step script, a scratch git repo, different tags
step version v >"$T/version.sh"
gate() { # tag cargo-version -> exit code of the real step
  local d="$T/gate"; rm -rf "$d"; mkdir -p "$d"; : >"$d/out"
  printf '[workspace.package]\nversion = "%s"\nedition = "2021"\n[other]\nversion = "9.9.9"\n' "$2" >"$d/Cargo.toml"
  (cd "$d" && GITHUB_REF_NAME="$1" GITHUB_OUTPUT="$d/out" bash "$T/version.sh")
}
t  "version gate: v0.2.0 == Cargo 0.2.0"              gate v0.2.0 0.2.0
grep -q "version=0.2.0" "$T/gate/out" && grep -q "prerelease=false" "$T/gate/out" && ok "gate outputs version/prerelease=false" || bad "gate outputs"
t  "version gate: prerelease v0.3.0-rc.1"              gate v0.3.0-rc.1 0.3.0-rc.1
grep -q "prerelease=true" "$T/gate/out" && ok "gate outputs prerelease=true" || bad "gate prerelease output"
tn "version gate: tag != Cargo version refused"        gate v0.2.1 0.2.0
tn "version gate: tag without v refused"               gate 0.2.0 0.2.0
tn "version gate: v1.2 (two parts) refused"            gate v1.2 1.2.0
tn "version gate: v1.2.3.4 refused"                    gate v1.2.3.4 1.2.3
# the real tree: current Cargo.toml version can be tagged
cur="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml)"
t "version gate on the real Cargo.toml (v$cur)" bash -c "mkdir -p $T/real && cp Cargo.toml $T/real/ && cd $T/real && : >out && GITHUB_REF_NAME=v$cur GITHUB_OUTPUT=$T/real/out bash $T/version.sh"
# the tag-on-main step needs a remote; check its logic with a throw-away repo
step version "Tag commit" >"$T/onmain.sh"
(
  set -e; cd "$T"; rm -rf origin.git work; git init -q --bare -b main origin.git; git clone -q origin.git work 2>/dev/null; cd work
  git config user.email t@t; git config user.name t; echo a >f; git add f; git commit -qm a; git push -q origin HEAD:main
  git checkout -q -b side; echo b >f; git commit -qam b
) >/dev/null 2>&1
( cd "$T/work" && git checkout -q main && GITHUB_SHA="$(git rev-parse HEAD)" GITHUB_REF_NAME=v0.0.1 bash "$T/onmain.sh" ) >"$T/last.log" 2>&1 && ok "tag on main accepted" || bad "tag on main accepted" "$(tail -1 "$T/last.log")"
( cd "$T/work" && git checkout -q side && GITHUB_SHA="$(git rev-parse HEAD)" GITHUB_REF_NAME=v0.0.1 bash "$T/onmain.sh" ) >"$T/last.log" 2>&1 && bad "tag off main refused" "accepted" || ok "tag off main refused" "$(grep -m1 error "$T/last.log" | cut -c1-80)"

# ---- 3. desktop Package step (Linux/tar.gz branch), with fake binaries
step desktop Package | sed -e 's/\${{ matrix.name }}/linux-x86_64/g; s/\${{ runner.os }}/Linux/g; s/\${{ matrix.archive }}/tar.gz/g; s/\${{ matrix.target }}/x86_64-unknown-linux-gnu/g' >"$T/package.sh"
P="$T/pkg"; mkdir -p "$P/target/x86_64-unknown-linux-gnu/release"; echo readme >"$P/README.md"
for b in geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share; do
  printf '#!/bin/sh\necho %s\n' "$b" >"$P/target/x86_64-unknown-linux-gnu/release/$b"; chmod +x "$P/target/x86_64-unknown-linux-gnu/release/$b"; done
t "Package step builds dist/geekbridge-linux-x86_64.tar.gz" bash -c "cd $P && bash $T/package.sh"
t "archive contains the expected binaries under geekbridge-linux-x86_64/" bash -c "cd $P && tar -tzf dist/geekbridge-linux-x86_64.tar.gz | grep -c 'geekbridge-linux-x86_64/geekbridge-service$' | grep -qx 1"
# every binary the step copies exists in the workspace as a bin target (a rename would break the release job)
miss=""; for b in geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share; do
  ls crates/*/src/bin/"$b".rs >/dev/null 2>&1 || grep -qE "^name *= *\"$b\"" crates/*/Cargo.toml 2>/dev/null || miss="$miss $b"; done
[[ -z "$miss" ]] && ok "every packaged binary is a workspace bin target" || bad "binaries not found as bin targets" "$miss"

# ---- 4. Linux installer packaging script (what the workflow runs next)
if command -v rsvg-convert >/dev/null; then
  t "package-linux.sh builds .deb + installer tarball" scripts/package-linux.sh --bin-dir "$P/target/x86_64-unknown-linux-gnu/release" --out "$P/dist" --version "$cur"
  ls "$P/dist"
else echo "SKIP  package-linux.sh (rsvg-convert missing)"; fi

# ---- 5. update-checker asset selection for the produced names (updater unit test lists the selector)
names="$(ls "$P/dist")"
echo "$names" | grep -qx "geekbridge-linux-x86_64.tar.gz" && ok "desktop updater asset name present" || bad "desktop updater asset name"
echo "$names" | grep -q "installer.tar.gz" && ! echo "geekbridge-$cur-linux-x86_64-installer.tar.gz" | grep -qx "geekbridge-linux-x86_64.tar.gz" && ok "installer tarball name cannot be mistaken for the updater archive"

# ---- 6. SHA256SUMS step with fake artifacts
step publish SHA256SUMS >"$T/sums.sh"
A="$T/artifacts"; mkdir -p "$A"; cd "$A" || exit 1
cp "$P/dist/"* . 2>/dev/null; for f in geekbridge-windows-x86_64.zip geekbridge-macos-aarch64.zip geekbridge-macos-x86_64.zip geekbridge-android.apk; do echo "$f" >"$f"; done
sed 's/^cd artifacts$/true/' "$T/sums.sh" >"$T/sums2.sh"
t "SHA256SUMS step" bash "$T/sums2.sh"
t "SHA256SUMS verifies with sha256sum -c" sha256sum -c SHA256SUMS
for f in geekbridge-linux-x86_64.tar.gz geekbridge-android.apk geekbridge-windows-x86_64.zip; do grep -qE "^[0-9a-f]{64}  $f$" SHA256SUMS && ok "SHA256SUMS line for $f (two-space format the updaters parse)" || bad "SHA256SUMS line for $f"; done
grep -q " SHA256SUMS$" SHA256SUMS && bad "SHA256SUMS lists itself" || ok "SHA256SUMS does not list itself"
rm geekbridge-android.apk; tn "SHA256SUMS step refuses a missing artifact" bash "$T/sums2.sh"
cd "$ROOT" || exit 1

# ---- 7. Android signing step (the real script; real zipalign/apksigner when the SDK is present)
step android Sign >"$T/sign.sh"
APKS="$ROOT/target/e2e-update"
if [[ -f "$APKS/gb-0.2.0.apk" ]]; then
  # shellcheck disable=SC1091
  source scripts/android-env.sh >/dev/null 2>&1
  S="$T/sign"; mkdir -p "$S/android/app/build/outputs/apk/debug" "$S/android/app/build/outputs/apk/release"
  cp "$APKS/gb-0.2.0.apk" "$S/android/app/build/outputs/apk/debug/app-debug.apk"
  cp "$APKS/gb-0.2.0.apk" "$S/android/app/build/outputs/apk/release/app-release-unsigned.apk"
  sign() { (cd "$S" && rm -rf dist && env "$@" RUNNER_TEMP="$S/tmp" bash "$T/sign.sh"); }
  mkdir -p "$S/tmp"
  tn "stable tag without keystore secrets is refused" sign PRERELEASE=false
  t  "prerelease without secrets: debug-signed APK" sign PRERELEASE=true
  # real keystore path
  keytool -genkeypair -keystore "$S/rel.jks" -storepass relpass1 -keypass relpass1 -alias rel -keyalg RSA -keysize 2048 -validity 100 -dname "CN=GeekBridge dryrun" >/dev/null 2>&1
  b64="$(base64 -w0 "$S/rel.jks")"; [[ -s "$S/rel.jks" ]] || bad "keytool could not create the test keystore"
  t  "stable tag with a (generated) release keystore: zipalign + apksigner" sign PRERELEASE=false ANDROID_KEYSTORE_B64="$b64" ANDROID_KEYSTORE_PASSWORD=relpass1 ANDROID_KEY_ALIAS=rel ANDROID_KEY_PASSWORD=relpass1
  fp="$(keytool -list -v -keystore "$S/rel.jks" -storepass relpass1 -alias rel 2>/dev/null | awk -F'SHA256: ' '/SHA256:/ {print $2; exit}' | tr -d ': ' | tr 'A-F' 'a-f')"
  t  "release signing with the correct pinned ANDROID_SIGNER_SHA256" sign PRERELEASE=false ANDROID_KEYSTORE_B64="$b64" ANDROID_KEYSTORE_PASSWORD=relpass1 ANDROID_KEY_ALIAS=rel ANDROID_KEY_PASSWORD=relpass1 ANDROID_SIGNER_SHA256="$fp"
  tn "wrong ANDROID_SIGNER_SHA256 fails the release" sign PRERELEASE=false ANDROID_KEYSTORE_B64="$b64" ANDROID_KEYSTORE_PASSWORD=relpass1 ANDROID_KEY_ALIAS=rel ANDROID_KEY_PASSWORD=relpass1 ANDROID_SIGNER_SHA256="$(printf '0%.0s' {1..64})"
  t  "signing again for the remaining checks" sign PRERELEASE=false ANDROID_KEYSTORE_B64="$b64" ANDROID_KEYSTORE_PASSWORD=relpass1 ANDROID_KEY_ALIAS=rel ANDROID_KEY_PASSWORD=relpass1 ANDROID_SIGNER_SHA256="$fp"
  apksigner verify --verbose "$S/dist/geekbridge-android.apk" 2>&1 | grep -q "scheme (APK Signature Scheme v3): true" && ok "APK verifies with v1, v2 and v3 schemes" || bad "v3 scheme"
  [[ -f "$S/dist/geekbridge-android.apk" ]] && apksigner verify --print-certs "$S/dist/geekbridge-android.apk" 2>&1 | grep -q "CN=GeekBridge dryrun" && ok "signed APK verifies with the release certificate" || bad "signed APK certificate"
  vc_in="$(aapt dump badging "$APKS/gb-0.2.0.apk" | sed -n "s/.*versionCode='\([0-9]*\)'.*/\1/p" | head -1)"
  [[ -n "$vc_in" && "$(aapt dump badging "$S/dist/geekbridge-android.apk" | grep -c "versionCode='$vc_in'")" == 1 ]] && ok "signed APK keeps its versionCode ($vc_in)" || bad "versionCode"
  zipalign -c -P 16 4 "$S/dist/geekbridge-android.apk" >/dev/null 2>&1 && ok "zipalign -P 16 check passes" || bad "zipalign check"
  [[ ! -e "$S/tmp/release.jks" ]] && echo "note: keystore removal is a separate step (always())" || true
else echo "SKIP  android signing (build $APKS/gb-0.2.0.apk first: scripts/e2e/update-install.sh builds nothing itself)"; fi

echo; echo "release dry run: $PASS passed, $FAIL failed"
[[ $FAIL == 0 ]]
