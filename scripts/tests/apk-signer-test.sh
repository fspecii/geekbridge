#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# scripts/lib/apk-signer.sh against a stub apksigner (no SDK, no phone, no network):
#   apk_signer_sha256 prints the signer certificate digest (several signers: sorted, comma joined) and fails for an
#   unsigned or unreadable APK; apk_update_verdict says what Android will do when NEW is installed over OLD
#   (ok / same-version / downgrade / other-key), key mismatch first because it loses the data.
# Run: scripts/tests/apk-signer-test.sh
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/signertest-XXXXXX")"; : >"$T/.gb-owned"
trap 'rm -rf -- "$T"' EXIT
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }
eq() { if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1: got '$2', want '$3'"; fi; }

KA=aaaa111111111111111111111111111111111111111111111111111111111111
KB=bbbb222222222222222222222222222222222222222222222222222222222222
KC=cccc333333333333333333333333333333333333333333333333333333333333
mkdir -p "$T/bin"
# stub: the file's first line names the signers ("none" = unsigned, "bad" = apksigner fails); versionCode on line 2
cat >"$T/bin/apksigner" <<'STUB'
#!/bin/sh
# usage: apksigner verify --print-certs FILE
f="$3"; l1="$(sed -n 1p "$f")"
case "$l1" in
  bad) echo "ERROR: not a zip" >&2; exit 1 ;;
  none) echo "ERROR: Missing META-INF/MANIFEST.MF" >&2; exit 1 ;;
esac
n=0; for k in $l1; do n=$((n+1)); echo "Signer #$n certificate DN: CN=Android Debug, O=Android, C=US"
  echo "Signer #$n certificate SHA-256 digest: $k"; echo "Signer #$n certificate SHA-1 digest: 0123"; done
exit 0
STUB
cat >"$T/bin/aapt2" <<'STUB'
#!/bin/sh
# usage: aapt2 dump badging FILE
echo "package: name='dev.geekbridge.android' versionCode='$(sed -n 2p "$3")' versionName='x' platformBuildVersionName='16'"
STUB
chmod +x "$T/bin/apksigner" "$T/bin/aapt2"
mk() { printf '%s\n%s\n' "$2" "$3" >"$T/$1.apk"; } # name signers versionCode
mk a10099 "$KA" 10099; mk a10051 "$KA" 10051; mk a20099 "$KA" 20099; mk b10099 "$KB" 10099
mk ab "$KB $KA" 10099; mk none "none" 1; mk bad "bad" 1

PATH="$T/bin:$PATH"
# shellcheck disable=SC1091
source "$src/scripts/lib/apk-signer.sh"

eq "one signer" "$(apk_signer_sha256 "$T/a10099.apk")" "$KA"
eq "two signers: sorted, comma joined" "$(apk_signer_sha256 "$T/ab.apk")" "$KA,$KB"
if apk_signer_sha256 "$T/none.apk" >/dev/null 2>&1; then bad "unsigned APK must fail"; else ok "unsigned APK fails"; fi
if apk_signer_sha256 "$T/bad.apk" >/dev/null 2>&1; then bad "unreadable APK must fail"; else ok "unreadable APK fails"; fi
if apk_signer_sha256 "$T/missing.apk" >/dev/null 2>&1; then bad "missing file must fail"; else ok "missing file fails"; fi

eq "same key, higher versionCode" "$(apk_update_verdict "$T/a10051.apk" "$T/a10099.apk")" "ok"
eq "same key, same versionCode" "$(apk_update_verdict "$T/a10099.apk" "$T/a10099.apk")" "same-version"
eq "same key, lower versionCode" "$(apk_update_verdict "$T/a20099.apk" "$T/a10099.apk")" "downgrade"
eq "other key wins over everything" "$(apk_update_verdict "$T/a10099.apk" "$T/b10099.apk")" "other-key"
eq "other key, lower versionCode is still other-key" "$(apk_update_verdict "$T/a20099.apk" "$T/b10099.apk")" "other-key"
eq "unsigned new APK" "$(apk_update_verdict "$T/a10099.apk" "$T/none.apk")" "unsigned"

# without apksigner on PATH the helper says so instead of printing an empty digest
if (PATH="/usr/bin:/bin"; GB_APKSIGNER=/nonexistent; source "$src/scripts/lib/apk-signer.sh"; apk_signer_sha256 "$T/a10099.apk") >/dev/null 2>&1; then
  bad "no apksigner must fail"; else ok "no apksigner fails"; fi

# ---- scripts/realdevice.sh install: stops before touching the phone when Android would refuse the update
cat >"$T/bin/adb" <<'STUB'
#!/bin/sh
# stub adb: one phone "PHONE1"; the installed copy is $STUB_INSTALLED; every install is logged to $STUB_LOG
[ "$1" = "-s" ] && shift 2
case "$1" in
  devices) printf 'List of devices attached\nPHONE1\tdevice\n' ;;
  pull) cp "$STUB_INSTALLED" "$3" ;;
  install) echo "install $*" >>"$STUB_LOG"; echo Success ;;
  shell) shift
    case "$*" in
      "pm path dev.geekbridge.android") [ -f "$STUB_INSTALLED" ] && echo "package:/data/app/~~x/dev.geekbridge.android-1/base.apk" ;;
      "dumpsys package "*) echo "    versionName=0.1.0" ;;
    esac ;;
esac
exit 0
STUB
chmod +x "$T/bin/adb"
run_install() { # installed-apk new-apk -> output in $T/out, exit code echoed
  rm -f "$T/install.log"; : >"$T/install.log"
  STUB_INSTALLED="$1" STUB_LOG="$T/install.log" GEEKBRIDGE_ADB="$T/bin/adb" "$src/scripts/realdevice.sh" --serial PHONE1 install --apk "$2" --no-grant --no-start >"$T/out" 2>&1
  echo $?
}
eq "install: same key, newer build -> installs" "$(run_install "$T/a10051.apk" "$T/a10099.apk")" 0
[[ -s "$T/install.log" ]] && ok "install: adb install was called" || bad "install: adb install was not called"
eq "install: same version -> installs" "$(run_install "$T/a10099.apk" "$T/a10099.apk")" 0
grep -q "lastUpdateTime" "$T/out" && ok "install: same version tells how to confirm the update" || bad "install: same-version hint missing"
rc="$(run_install "$T/a10099.apk" "$T/b10099.apk")"
[[ "$rc" != 0 ]] && ok "install: different signing key -> refused (exit $rc)" || bad "install: different key was not refused"
[[ ! -s "$T/install.log" ]] && ok "install: different key never reached adb install" || bad "install: adb install ran despite the different key"
grep -q "DIFFERENT key" "$T/out" && grep -q "android-env.sh" "$T/out" && ok "install: the message names the cause and the fix" || bad "install: refusal message incomplete: $(cat "$T/out")"
rc="$(run_install "$T/a20099.apk" "$T/a10099.apk")"
[[ "$rc" != 0 && ! -s "$T/install.log" ]] && ok "install: downgrade refused before install" || bad "install: downgrade not refused (rc $rc)"
eq "install: nothing installed yet -> installs" "$(run_install "$T/none-here.apk" "$T/a10099.apk")" 0

exit "$fail"
