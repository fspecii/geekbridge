#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pre-flight for the owner's upgrade on a real phone, simulated on an emulator and in scratch directories only.
#
#   scripts/e2e/upgrade-preflight.sh phone  --old OLD.apk --new NEW.apk [--name LABEL]
#   scripts/e2e/upgrade-preflight.sh desktop --state-dir DIR [--bin-dir DIR]
#   scripts/e2e/upgrade-preflight.sh signer --apk A.apk --apk B.apk ...
#
# phone    install OLD, finish setup, pair for real with a desktop service (own state dir), turn consents on at both
#          ends, then `adb install -r NEW` (what the package installer does for a downloaded APK) and check that the
#          pairing, the trust records, the consents and the private files survive, that the computer reconnects
#          without a new code, and that a downgrade or a different signing key is refused with the data intact.
#          Own AVD (gb36u), console 5724, adb server 16124, service ports 17580/17581/17516/1830. Never 1716/17381/8000.
# desktop  copy DIR (a real state directory, read only) to scratch, start the new service on the COPY with
#          --no-announce and loopback-only listeners, and check that it opens, lists the paired phone, keeps the
#          consents and the pinned certificate, and rewrites nothing it should not. DIR itself is never written.
# signer   print the signing certificate SHA-256 of each APK and say whether they all match.
#
# Results: dist/android/logs/upgrade-preflight-results.tsv. Exit 1 on any FAIL.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
MODE="${1:-}"; shift || true
OLD="" NEW="" LABEL="upgrade" STATE_SRC="" BIN_DIR="" APKS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --old) OLD="$2"; shift 2 ;; --new) NEW="$2"; shift 2 ;; --name) LABEL="$2"; shift 2 ;;
    --state-dir) STATE_SRC="$2"; shift 2 ;; --bin-dir) BIN_DIR="$2"; shift 2 ;; --apk) APKS+=("$2"); shift 2 ;;
    -h|--help) sed -n '3,22p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 64 ;;
  esac
done
UP_RESULTS="$ROOT/dist/android/logs/upgrade-preflight-results.tsv"; mkdir -p "$(dirname "$UP_RESULTS")"
urec() { printf '%s\t%s\t%s\t%s\n' "$LABEL" "$1" "$2" "${3:-}" | tee -a "$UP_RESULTS" >&2; }
uchk() { if [[ "$2" == 0 ]]; then urec "$1" PASS "${3:-}"; else urec "$1" FAIL "${3:-}"; fi; }

# ------------------------------------------------------------------------------------------------ signer
if [[ "$MODE" == signer ]]; then
  # shellcheck disable=SC1091
  source scripts/android-env.sh >/dev/null 2>&1
  # shellcheck disable=SC1091
  source scripts/lib/apk-signer.sh
  first="" rc=0
  for a in "${APKS[@]}"; do
    s="$(apk_signer_sha256 "$a")" || { echo "no signature: $a" >&2; rc=1; continue; }
    printf '%s  %s\n' "$s" "$a"
    [[ -z "$first" ]] && first="$s"; [[ "$s" == "$first" ]] || rc=1
  done
  ((rc == 0)) && echo "all signed with the same key" || echo "DIFFERENT SIGNING KEYS: Android refuses an update between them" >&2
  exit "$rc"
fi

# ------------------------------------------------------------------------------------------------ desktop
if [[ "$MODE" == desktop ]]; then
  [[ -d "$STATE_SRC" ]] || { echo "--state-dir DIR is required" >&2; exit 64; }
  BIN_DIR="${BIN_DIR:-${CARGO_TARGET_DIR:-$HOME/.cache/gb-scratch/target-shared}/release}"
  SVC="$BIN_DIR/geekbridge-service" CTLB="$BIN_DIR/geekbridge-ctl"
  [[ -x "$SVC" && -x "$CTLB" ]] || { echo "binaries not found in $BIN_DIR" >&2; exit 64; }
  GB_TMP="${GB_TMP:-$HOME/.cache/gb-tmp}"; W="$GB_TMP/upgrade-preflight-desktop"; rm -rf "$W"; mkdir -p "$W"; chmod 700 "$W"
  ST="$W/state"; cp -a --no-preserve=ownership -- "$STATE_SRC" "$ST"; chmod 700 "$ST"
  src_sum() { (cd "$1" && find . -type f ! -name '*.lock' -print0 | sort -z | xargs -0 sha256sum); }
  before="$(src_sum "$STATE_SRC")"
  PORT_CTRL=17591 PORT_API=17590 PORT_DISC=17526 PORT_IN=1840
  ctl() { timeout 20 "$CTLB" --state-dir "$ST" --control-port "$PORT_CTRL" "$@"; }
  jget() { python3 -I -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE GEEKBRIDGE_NO_UPDATE_CHECK=1 setsid nohup "$SVC" \
     --state-dir "$ST" --api-port "$PORT_API" --control-port "$PORT_CTRL" --discovery-port "$PORT_DISC" \
     --listen-inbound "127.0.0.1:$PORT_IN" --dev-loopback --no-calling-probe --no-announce --name Preflight-Copy \
     >"$W/service.log" 2>&1 &)
  up=1; for _ in $(seq 40); do ctl status >/dev/null 2>&1 && { up=0; break; }; sleep 0.5; done
  uchk desktop-opens-old-state $up "service on a copy of $STATE_SRC; log: $W/service.log"
  if ((up == 0)); then
    st="$(ctl status)"
    old_peers="$(python3 -I -c "import json,sys;print(' '.join(sorted(p['device_id'] for p in json.load(open(sys.argv[1]))['peers'])))" "$STATE_SRC/trusted_peers.json")"
    new_peers="$(jget '" ".join(sorted(p["device_id"] for p in d["paired"]))' <<<"$st")"
    uchk desktop-lists-paired $([[ -n "$old_peers" && "$old_peers" == "$new_peers" ]]; echo $?) "state: [$old_peers]; service: [$new_peers]"
    # consents as the service reports them == the file
    cons_file="$(python3 -I -c "import json,sys;d=json.load(open(sys.argv[1]))['devices'];print(json.dumps({k:sorted(v) for k,v in d.items()},sort_keys=True))" "$STATE_SRC/consent.json" 2>/dev/null || echo '{}')"
    cons_now="$(python3 -I -c "import json,sys;d=json.load(open(sys.argv[1]))['devices'];print(json.dumps({k:sorted(v) for k,v in d.items()},sort_keys=True))" "$ST/consent.json" 2>/dev/null || echo '{}')"
    uchk desktop-consents-kept $([[ "$cons_file" == "$cons_now" && "$cons_file" != '{}' ]]; echo $?) "$cons_file"
    pin_old="$(python3 -I -c "import json,sys;print(' '.join(sorted(p['cert_sha256'] for p in json.load(open(sys.argv[1]))['peers'])))" "$STATE_SRC/trusted_peers.json")"
    pin_new="$(python3 -I -c "import json,sys;print(' '.join(sorted(p['cert_sha256'] for p in json.load(open(sys.argv[1]))['peers'])))" "$ST/trusted_peers.json")"
    uchk desktop-cert-pins-kept $([[ -n "$pin_old" && "$pin_old" == "$pin_new" ]]; echo $?) "${pin_old:0:16}..."
    id_old="$(sha256sum <"$STATE_SRC/identity.pem")" id_new="$(sha256sum <"$ST/identity.pem")"
    uchk desktop-identity-unchanged $([[ "$id_old" == "$id_new" ]]; echo $?) "same device certificate, so the phone keeps trusting this computer"
    sh_old="$(cat "$STATE_SRC/share.json" 2>/dev/null)"; sh_new="$(cat "$ST/share.json" 2>/dev/null)"
    uchk desktop-share-settings-kept $([[ "$sh_old" == "$sh_new" ]]; echo $?) "share.json identical"
    nc="$(ls "$ST" | grep -c '\.corrupt$')"; uchk desktop-no-corrupt-files $([[ "$nc" == 0 ]]; echo $?) "$nc .corrupt files"
    grep -iE "reset|corrupt|recovered|newer|regenerat|migrat" "$W/service.log" >"$W/notes.txt" 2>/dev/null
    uchk desktop-no-state-notes $([[ ! -s "$W/notes.txt" ]]; echo $?) "$(head -c 300 "$W/notes.txt" | tr '\n' '|')"
    ctl doctor >"$W/doctor.txt" 2>&1; urec desktop-doctor INFO "exit $?: $(grep -ciE 'fail' "$W/doctor.txt") FAIL lines (firewall/Wayland lines are expected in a scratch run)"
    # the phone is not on the air, so the link list is empty and "Try:" explains why
    ctl status >/dev/null 2>&1; uchk desktop-status-still-ok $? "status answers after doctor"
  fi
  pkill -f "^$SVC --state-dir $ST" 2>/dev/null; sleep 1
  after="$(src_sum "$STATE_SRC")"
  uchk desktop-original-untouched $([[ "$before" == "$after" ]]; echo $?) "sha256 of every file in $STATE_SRC identical before and after (scratch copy: $ST)"
  ! grep -q -P "^\Q$LABEL\E\t[^\t]*\tFAIL\t" "$UP_RESULTS"; exit $?
fi

# ------------------------------------------------------------------------------------------------ phone
[[ "$MODE" == phone ]] || { echo "usage: $0 phone|desktop|signer ... (see --help)" >&2; exit 64; }
[[ -f "$OLD" && -f "$NEW" ]] || { echo "--old and --new APK files are required" >&2; exit 64; }
export GB_SCRATCH="${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
export E2E_SOURCE_ONLY=1 E2E_ADB_PORT=16124 E2E_EMU_PORT=5724 E2E_AVD="${E2E_AVD:-gb36u}" E2E_TAG=pf
export E2E_RUN="$GB_SCRATCH/upgrade-preflight-run" E2E_RESULTS="$GB_SCRATCH/upgrade-preflight-e2e.tsv"
export E2E_CTRL_PORT=17581 E2E_API_PORT=17580 E2E_DISC_PORT=17516 E2E_LISTEN_PORT=1830 E2E_REDIR_PORT=17261 E2E_XDISP=:96
export E2E_TARGET="${E2E_TARGET:-$GB_SCRATCH/target-shared}"
mkdir -p "$E2E_RUN"
set --
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set +e
BUILD=no
SVC="$E2E_TARGET/release/geekbridge-service"
pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
rm -rf "$RUN/state"
start_xvfb && start_service && start_emulator || { urec env FAIL "xvfb/service/emulator did not start"; exit 1; }
$ADB shell svc power stayon true; wake
desk_id

redial() { # the phone may still be starting: dial again until connected (max ~40 s)
  local i; for i in 1 2 3 4 5 6 7 8; do desk_dial; wait_for 5 is_connected && return 0; done; return 1
}
pkgline() { $ADB shell dumpsys package $PKG | tr -d '\r' | grep -E "^\s+(versionCode|versionName)=" | sed 's/^ *//' | cut -d' ' -f1-2 | tr '\n' ' '; }
debuggable() { local d; d="$($ADB shell dumpsys package $PKG | tr -d '\r')"; [[ "$d" == *"pkgFlags=[ DEBUGGABLE"* ]]; }
phone_files() { # sha256 of the private files that matter (debuggable builds only)
  $ADB shell "run-as $PKG sh -c 'cd /data/data/$PKG; find files shared_prefs -type f 2>/dev/null | sort | xargs sha256sum 2>/dev/null'" | tr -d '\r'
}
phone_pref() { $ADB shell "run-as $PKG cat shared_prefs/$1.xml" 2>/dev/null | tr -d '\r'; }
putpref() { # file body-lines (force-stops the app first)
  $ADB shell am force-stop $PKG
  printf "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n%s</map>\n" "$2" >"$RUN/pref.xml"
  $ADB push "$RUN/pref.xml" /data/local/tmp/pref.xml >/dev/null
  $ADB shell run-as $PKG cp /data/local/tmp/pref.xml "shared_prefs/$1.xml"
}

# ---- 1. install OLD, finish setup through the real wizard, pair for real
$ADB uninstall $PKG >/dev/null 2>&1
for p in $(paired_ids); do ctl unpair "$p" >/dev/null 2>&1; done
$ADB install "$OLD" 2>&1 | grep -q Success; uchk install-old $? "$(basename "$OLD") -> $(pkgline) debuggable=$(debuggable && echo yes || echo no)"
OLD_DEBUGGABLE=0; debuggable && OLD_DEBUGGABLE=1
OLD_SIGNER="$(source scripts/lib/apk-signer.sh; apk_signer_sha256 "$OLD")"
stage_battery
stage_pair
load_ids
[[ -n "$PHONE_ID" ]] || { urec pair FAIL "no pairing, cannot continue"; exit 1; }
# consents, desktop side (consent.json) and, where the build is debuggable, phone side (per-computer grants)
ctl consent "$PHONE_ID" sms on >/dev/null; ctl consent "$PHONE_ID" clipboard on >/dev/null; ctl consent "$PHONE_ID" contacts on >/dev/null
if ((OLD_DEBUGGABLE)); then
  putpref geekbridge_grants "    <string name=\"grants\">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[&quot;contacts&quot;,&quot;sms&quot;]}}</string>
"
  putpref share_policy "    <boolean name=\"auto_accept:$DESK_ID\" value=\"true\" />
"
  putpref geekbridge_device_name '    <string name="name">Upgrade Test Phone</string>
'
fi
$ADB shell am start -f 0x04000000 -n "$PKG/.HomeActivity" >/dev/null; sleep 4
redial
uchk connected-before-upgrade $(is_connected; echo $?) "phases=[$(link_phases)]"
cons_before="$(python3 -I -c "import json;print(json.dumps({k:sorted(v) for k,v in json.load(open('$RUN/state/consent.json'))['devices'].items()},sort_keys=True))")"
peers_before="$(sha256sum <"$RUN/state/trusted_peers.json")"
((OLD_DEBUGGABLE)) && files_before="$(phone_files)" || files_before=""
shot upg-before

# ---- 2. upgrade in place, as the package installer does
ctl disconnect "$PHONE_ID" >/dev/null 2>&1
$ADB logcat -c >/dev/null 2>&1
res="$($ADB install -r "$NEW" 2>&1 | tr -d '\r' | tail -2 | tr '\n' ' ')"
uchk install-new-over-old $([[ "$res" == *Success* ]]; echo $?) "$res -> $(pkgline)"
NEW_SIGNER="$(source scripts/lib/apk-signer.sh; apk_signer_sha256 "$NEW")"
uchk signing-keys-match $([[ -n "$OLD_SIGNER" && "$OLD_SIGNER" == "$NEW_SIGNER" ]]; echo $?) "old ${OLD_SIGNER:0:16} new ${NEW_SIGNER:0:16}"
[[ "$res" == *Success* ]] || { urec upgrade FAIL "cannot continue"; exit 1; }
sleep 8; pid0="$($ADB shell pidof $PKG | tr -d '\r')"
uchk app-restarts-by-itself $([[ -n "$pid0" ]]; echo $?) "no tap on the icon: MY_PACKAGE_REPLACED -> BootReceiver -> LinkService (pid ${pid0:-none})"
debuggable && files_after="$(phone_files)" || files_after=""
if ((OLD_DEBUGGABLE)); then
  # the files may only differ in the app's own bookkeeping (versions file, updates.xml, gb_state.xml, caches)
  diffs="$(diff <(echo "$files_before") <(echo "$files_after") | grep '^[<>]' | grep -v -E 'updates\.xml|gb_state\.xml|WebView|app_|cache' | head -8)"
  uchk phone-private-files-intact $([[ -z "$diffs" ]]; echo $?) "${diffs:-no private file changed apart from bookkeeping}"
  uchk phone-trust-record-kept $([[ -n "$(echo "$files_after" | grep 'files/kde/trusted_peers.json')" ]]; echo $?) "files/kde/trusted_peers.json present after the upgrade"
  g="$(phone_pref geekbridge_grants)"; uchk phone-grants-kept $([[ "$g" == *"$DESK_ID"* && "$g" == *sms* ]]; echo $?) "per-computer opt-ins for the desktop still in geekbridge_grants"
  uchk phone-device-name-kept $([[ "$(phone_pref geekbridge_device_name)" == *"Upgrade Test Phone"* ]]; echo $?) "device name"
  uchk phone-auto-accept-kept $([[ "$(phone_pref share_policy)" == *"auto_accept:$DESK_ID"* ]]; echo $?) "share_policy"
else
  urec phone-private-files SKIP "OLD is not debuggable (release build): run-as is refused, so files are checked through behaviour below"
fi

# ---- 3. the new app runs and the paired computer comes back without a code
$ADB shell am force-stop $PKG; $ADB shell am start -f 0x04000000 -n "$PKG/.HomeActivity" >/dev/null; sleep 6; shot upg-after-home
pid="$($ADB shell pidof $PKG | tr -d '\r')"; uchk app-starts-after-upgrade $([[ -n "$pid" ]]; echo $?) "pid ${pid:-none}"
n="$($ADB logcat -d -b crash 2>/dev/null | grep -c "FATAL EXCEPTION")"; uchk no-crash-in-log $([[ "$n" == 0 ]]; echo $?) "$n FATAL EXCEPTION lines in the crash buffer since the upgrade"
redial
if is_connected && ! ui code >/dev/null 2>&1; then urec reconnect-without-code PASS "phases=[$(link_phases)], no pairing dialog"
else urec reconnect-without-code FAIL "phases=[$(link_phases)] (a code dialog would mean the trust record was lost)"; shot upg-reconnect-failed; fi
uchk desktop-still-lists-phone $([[ " $(paired_ids) " == *" $PHONE_ID "* ]]; echo $?) "$(paired_ids)"
cons_after="$(python3 -I -c "import json;print(json.dumps({k:sorted(v) for k,v in json.load(open('$RUN/state/consent.json'))['devices'].items()},sort_keys=True))")"
# every opt-in given before must still be there; an upgrade may add one (the debug consent appears when a debuggable build connects)
lost="$(python3 -I -c "import json,sys;b=json.loads(sys.argv[1]);a=json.loads(sys.argv[2]);print(' '.join(f'{d}:{f}' for d,fs in b.items() for f in fs if f not in a.get(d,[])))" "$cons_before" "$cons_after")"
uchk desktop-consents-kept $([[ -z "$lost" && "$cons_before" != '{}' ]]; echo $?) "before $cons_before; after $cons_after; lost: ${lost:-none}"
uchk desktop-trust-record-unchanged $([[ "$peers_before" == "$(sha256sum <"$RUN/state/trusted_peers.json")" ]]; echo $?) "trusted_peers.json identical (cert pin kept)"
ctl ping "$PHONE_ID" >/dev/null 2>&1; uchk ping-after-upgrade $? "round trip over the upgraded app"
lut="$($ADB shell dumpsys package $PKG | tr -d '\r' | grep -m1 lastUpdateTime | sed 's/^ *//')"; uchk update-time-visible $([[ -n "$lut" ]]; echo $?) "$lut"

# ---- 4. the same APK again (same versionCode): what re-installing today's build does
res="$($ADB install -r "$NEW" 2>&1 | tr -d '\r' | tail -1)"
uchk reinstall-same-version $([[ "$res" == Success* ]]; echo $?) "$res"
redial; uchk reconnect-after-reinstall $(is_connected; echo $?) "phases=[$(link_phases)]"

# ---- 5. refused installs leave the data alone
res="$($ADB install -r "$OLD" 2>&1 | tr -d '\r' | grep -m1 -E 'Success|Failure')"
vcode() { .tools/android-sdk/build-tools/36.0.0/aapt2 dump badging "$1" | grep -o "versionCode='[0-9]*'" | head -1; }
if [[ "$(vcode "$OLD")" != "$(vcode "$NEW")" ]]; then
  uchk downgrade-refused $([[ "$res" == *VERSION_DOWNGRADE* ]]; echo $?) "$res"
else urec downgrade-refused SKIP "same versionCode: $res"; fi
# an APK signed with a throwaway key
W2="$RUN/foreign"; rm -rf "$W2"; mkdir -p "$W2"
( source scripts/android-env.sh >/dev/null 2>&1
  keytool -genkeypair -keystore "$W2/other.jks" -storepass foreignpw -keypass foreignpw -alias other -keyalg RSA -keysize 2048 -validity 3650 -dname "CN=Foreign" >/dev/null 2>&1
  cp "$NEW" "$W2/foreign.apk"
  apksigner sign --ks "$W2/other.jks" --ks-pass pass:foreignpw --key-pass pass:foreignpw "$W2/foreign.apk" ) >/dev/null 2>&1
res="$($ADB install -r "$W2/foreign.apk" 2>&1 | tr -d '\r' | grep -m1 -o 'Failure \[[A-Z_]*')"
uchk foreign-key-refused $([[ "$res" == *UPDATE_INCOMPATIBLE* ]]; echo $?) "$res"
( source scripts/android-env.sh >/dev/null 2>&1; source scripts/lib/apk-signer.sh
  urec signer-guard INFO "installed=$(apk_signer_sha256 "$NEW" | cut -c1-16) foreign=$(apk_signer_sha256 "$W2/foreign.apk" | cut -c1-16) (differ -> scripts/realdevice.sh install stops before touching the phone)" )
redial; uchk data-intact-after-refused-installs $(is_connected; echo $?) "still connected, no code"

echo; grep -P "^\Q$LABEL\E\t" "$UP_RESULTS" | column -t -s $'\t'
! grep -q -P "^\Q$LABEL\E\t[^\t]*\tFAIL\t" "$UP_RESULTS"
