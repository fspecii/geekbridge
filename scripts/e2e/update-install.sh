#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-update end to end: desktop (geekbridge-service + geekbridge-ctl update ...) and Android (emulator,
# real package-installer upgrade) against scripts/e2e/mock_release_server.py.
#
#   scripts/e2e/update-install.sh [desktop|android|all] [--no-build]
#
# Isolated from the other e2e runs: own scratch dir (target/e2e-update), own mock port (18891), own desktop
# service ports (17680/17681/17616/1930) and state dir, own AVD gb36u (console 5624, adb server 16048).
# Needs: the release desktop binaries in $DESK_TARGET (built here when missing) and, for android, two debug
# APKs signed with the same key: v0.1.0 (versionCode 10099) and v0.2.0 (20099), built from scratch copies of
# android/ with a patched Cargo.toml version (the working tree is not touched).
# E2E_UPD_REBUILD=1 refreshes both scratch copies from the current android/ tree and rebuilds the two APKs first.
# Real artifacts (docs/updater-e2e.md): E2E_UPD_REPLICA=<dir of release dirs>, E2E_UPD_OLD=0.1.0, E2E_UPD_NEW=0.1.1,
# E2E_UPD_APK_OLD / E2E_UPD_APK_NEW = the two real debug-signed APKs. The mock server is then replaced by
# scripts/e2e/github_replica.py serving the real release directories (see updater-real.sh assemble), and the
# signer-certificate pre-check, the "No public release found" and rate-limit states are exercised as well.
# Results: dist/android/logs/e2e-update-results.tsv; screenshots dist/screenshots/android/e2e-upd-*.png.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
WHAT="${1:-all}"; BUILD=auto; [[ "${2:-}" == --no-build ]] && BUILD=no
W="$ROOT/target/e2e-update"; mkdir -p "$W" "$ROOT/dist/android/logs" "$ROOT/dist/screenshots/android"
RESULTS="$ROOT/dist/android/logs/e2e-update-results.tsv"; : >"$RESULTS"
MOCK_PORT=18891; MODE="$W/mock-mode"; MOCKLOG="$W/mock.log"
OLDV="${E2E_UPD_OLD:-0.1.0}"; NEWV="${E2E_UPD_NEW:-0.2.0}"; NEWCODE="$(scripts/version-code.sh --code "$NEWV")"
REPLICA="${E2E_UPD_REPLICA:-}"
DESK_TARGET="${E2E_UPD_DESK_TARGET:-$ROOT/target/e2e-update-desk}"
SVC="$DESK_TARGET/release/geekbridge-service"; CTLBIN="$DESK_TARGET/release/geekbridge-ctl"
DRUN="$W/desk-run"; CTRL=17681; PKG=dev.geekbridge.android

log() { printf '[upd %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" | tee -a "$RESULTS" >&2; }
check() { # name cond-exit-code detail
  if [[ "$2" == 0 ]]; then rec "$1" PASS "${3:-}"; else rec "$1" FAIL "${3:-}"; fi
}
# With the replica, "tag=X" (the mock's override) means "only release X exists".
mode() { if [[ -n "$REPLICA" ]]; then printf '%s\n' "${@/#tag=/only=}" >"$MODE"; else printf '%s\n' "$@" >"$MODE"; fi; }
start_mock() { # apk [extra assets]
  stop_mock; sleep 0.3
  mode ""; : >"$MOCKLOG"
  if [[ -n "$REPLICA" ]]; then
    (setsid nohup python3 -I scripts/e2e/github_replica.py --port "$MOCK_PORT" --repo fspecii/geekbridge --releases "$REPLICA" --mode-file "$MODE" --log "$MOCKLOG" >"$W/replica.out" 2>&1 &)
  else
    (MOCK_MODE_FILE="$MODE" MOCK_EXTRA="${2:-}" setsid nohup python3 -I scripts/e2e/mock_release_server.py "$MOCK_PORT" "$1" v0.2.0 >"$MOCKLOG" 2>&1 &)
  fi
  sleep 1
}
stop_mock() { pkill -f "^python3 -I scripts/e2e/(mock_release_server|github_replica).py (--port )?$MOCK_PORT" 2>/dev/null; true; }
api_hits() { grep -c "releases/latest" "$MOCKLOG" 2>/dev/null || true; }

# ================================================================== desktop
ctl() { timeout 90 "$CTLBIN" --state-dir "$DRUN/state" --control-port "$CTRL" "$@"; }
start_service() {
  mkdir -p "$DRUN/state" "$DRUN/xdg-state"; chmod 700 "$DRUN/state"
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     GEEKBRIDGE_UPDATE_BASE_URL="http://127.0.0.1:$MOCK_PORT" XDG_STATE_HOME="$DRUN/xdg-state" \
     setsid nohup "$SVC" --state-dir "$DRUN/state" --api-port 17680 --control-port "$CTRL" \
     --discovery-port 17616 --listen-inbound 127.0.0.1:1930 --dev-loopback --no-calling-probe --no-announce \
     --name E2E-Update >>"$W/service.log" 2>&1 &)
  for _ in $(seq 20); do ctl update status >/dev/null 2>&1 && return 0; sleep 0.5; done; return 1
}
stop_service() { pkill -f "^$SVC --state-dir $DRUN/state" 2>/dev/null; sleep 0.7; true; }

desktop() {
  [[ -x "$SVC" && -x "$CTLBIN" ]] || { [[ $BUILD == no ]] && { rec desktop FAIL "binaries missing"; return 1; }
    CARGO_TARGET_DIR="$DESK_TARGET" cargo build --release -p bridge-service --bins >"$W/desk-build.log" 2>&1 || { rec desktop FAIL "build failed"; return 1; }; }
  stop_service; rm -rf "$DRUN"; rm -f "$W/CANARY-EXECUTED"
  local assets="$W/desk-assets"; mkdir -p "$assets"
  printf '#!/bin/sh\ntouch %s/CANARY-EXECUTED\n' "$W" >"$assets/install.sh"; chmod +x "$assets/install.sh"
  (cd "$assets" && tar czf geekbridge-linux-x86_64.tar.gz install.sh)
  local tarsha; tarsha="$(sha256sum "$assets/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)"
  # the mock needs a primary "apk" argument; reuse the tarball (the android asset is simply unused here)
  start_mock "$assets/geekbridge-linux-x86_64.tar.gz" "geekbridge-linux-x86_64.tar.gz=$assets/geekbridge-linux-x86_64.tar.gz"
  start_service || { rec desktop-start FAIL "service did not start; see $W/service.log"; return 1; }
  local out

  out="$(ctl update status 2>&1)"; check desktop-initial-status $([[ "$out" == *"running:  v0.1.0"* && "$out" == *"unknown"* ]]; echo $?) "$(echo "$out" | head -2 | tr '\n' '|')"

  # 1. downgrade / same version is not offered
  mode "tag=v0.0.9"; out="$(ctl update check 2>&1)"
  check desktop-downgrade-not-offered $([[ "$out" == *"up to date"* ]]; echo $?) "$(echo "$out" | grep latest)"
  mode "tag=v0.1.0"; sleep 31; out="$(ctl update check 2>&1)"
  check desktop-same-version-not-offered $([[ "$out" == *"up to date"* ]]; echo $?) "$(echo "$out" | grep latest)"

  # 2. a newer release with a tampered SHA256SUMS: download is refused and nothing is left behind
  mode "tamper_sums"; sleep 31; out="$(ctl update check 2>&1)"
  ctl update download >"$W/dl-tamper.out" 2>&1; local rc=$?
  check desktop-tampered-sums-refused $([[ $rc != 0 && "$(cat "$W/dl-tamper.out")" == *"does not match"* ]]; echo $?) "rc=$rc: $(grep -i problem "$W/dl-tamper.out")"
  check desktop-tampered-leaves-nothing $([[ -z "$(find "$DRUN/xdg-state" -type f -path '*geekbridge-update-*' 2>/dev/null)" ]]; echo $?) "no file under the download root"

  # 3. no SHA256SUMS at all: offered, but not downloadable
  mode "nosums"; sleep 31; out="$(ctl update check 2>&1)"
  ctl update download >"$W/dl-nosums.out" 2>&1; rc=$?
  check desktop-no-sums-no-download $([[ "$out" == *"no published SHA-256"* && $rc != 0 ]]; echo $?) "$(echo "$out" | grep installer)"

  # 4. genuine release; the bytes on the wire are corrupted after the check (hash mismatch)
  mode ""; sleep 31; out="$(ctl update check 2>&1)"
  check desktop-newer-offered $([[ "$out" == *"v0.2.0 - newer"* && "$out" == *"geekbridge-linux-x86_64.tar.gz (SHA-256 $tarsha)"* ]]; echo $?) "$(echo "$out" | grep -E 'latest|installer' | tr '\n' '|')"
  mode "corrupt"; ctl update download >"$W/dl-corrupt.out" 2>&1; rc=$?
  check desktop-corrupt-refused $([[ $rc != 0 && "$(cat "$W/dl-corrupt.out")" == *"does not match"* ]]; echo $?) "rc=$rc"
  check desktop-corrupt-leaves-nothing $([[ -z "$(find "$DRUN/xdg-state" -type f -path '*geekbridge-update-*' 2>/dev/null)" ]]; echo $?) "deleted"

  # 5. good download: verified, read-only, private directory, nothing executed or extracted
  mode ""; out="$(ctl update download 2>&1)"; rc=$?
  local path; path="$(echo "$out" | sed -n 's/^verified download: //p')"
  check desktop-download-verified $([[ $rc == 0 && -f "$path" && "$(sha256sum "$path" | cut -d' ' -f1)" == "$tarsha" ]]; echo $?) "$path"
  local perm dperm; perm="$(stat -c %a "$path" 2>/dev/null)"; dperm="$(stat -c %a "$(dirname "$path")" 2>/dev/null)"
  check desktop-download-permissions $([[ "$dperm" == 700 && "$perm" == 4?? ]]; echo $?) "dir $dperm file $perm"
  check desktop-nothing-executed $([[ ! -e "$W/CANARY-EXECUTED" && "$(ls "$(dirname "$path")" | wc -l)" == 1 ]]; echo $?) "no canary, only the archive in the download dir"
  out="$(ctl update status 2>&1)"
  check desktop-status-shows-download $([[ "$out" == *"verified download: $path"* ]]; echo $?) ""

  # 6. skip this version: persists across a service restart; a newer release is offered again
  ctl update skip 0.2.0 >/dev/null 2>&1; out="$(ctl update status 2>&1)"
  check desktop-skip-recorded $([[ "$out" == *"(skipped)"* ]]; echo $?) "$(echo "$out" | grep latest)"
  stop_service; start_service; out="$(ctl update status 2>&1)"
  check desktop-skip-persists-restart $([[ "$out" == *"(skipped)"* ]] && grep -q '0\.2\.0' "$DRUN/state/update-state.json"; echo $?) "$(grep -o '"skipped[^,]*' "$DRUN/state/update-state.json" | head -1)"
  mode "tag=v0.3.0"; sleep 31; out="$(ctl update check 2>&1)"
  check desktop-newer-than-skipped-offered $([[ "$out" == *"v0.3.0 - newer"* && "$out" != *"(skipped)"* ]]; echo $?) "$(echo "$out" | grep latest)"
  # 7. disable persists
  ctl update disable >/dev/null 2>&1; stop_service; start_service; out="$(ctl update status 2>&1)"
  check desktop-disable-persists $([[ "$out" == *"automatic off"* ]]; echo $?) ""
  ctl update enable >/dev/null 2>&1
  check desktop-nothing-executed-final $([[ ! -e "$W/CANARY-EXECUTED" ]]; echo $?) "canary absent at the end"
  stop_service; stop_mock
}

# ================================================================== android
android() {
  local A1="${E2E_UPD_APK_OLD:-$W/gb-0.1.0.apk}" A2="${E2E_UPD_APK_NEW:-$W/gb-0.2.0.apk}"
  if [[ "${E2E_UPD_REBUILD:-}" == 1 ]]; then
    for v in 0.1.0 0.2.0; do
      mkdir -p "$W/v$v/android"
      # build.gradle.kts reads the version (and so versionName/versionCode) from ../Cargo.toml.
      printf '[workspace.package]\nversion = "%s"\n' "$v" >"$W/v$v/Cargo.toml"
      rsync -a --delete --exclude build --exclude .gradle --exclude .kotlin android/ "$W/v$v/android/"
      # The bundled "What's new" text is generated for the repository's version; label this scratch build's copy.
      sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$v\"/" "$W/v$v/android/app/src/main/assets/whats_new.json"
      (cd "$W/v$v/android" && gradle --no-daemon -q assembleDebug -Pgb.abis=x86_64 -Pgb.formulaVersionCode=1 >"$W/build-$v.log" 2>&1) || { rec android FAIL "build v$v failed"; return 1; }
      cp "$W/v$v/android/app/build/outputs/apk/debug/"*.apk "$W/gb-$v.apk"
    done
  fi
  [[ -f "$A1" && -f "$A2" ]] || { rec android FAIL "missing $A1 / $A2 (see header)"; return 1; }
  export ADBUI_TIMEOUT=60 E2E_SOURCE_ONLY=1 E2E_ADB_PORT="${E2E_UPD_ADB_PORT:-16048}" E2E_EMU_PORT="${E2E_UPD_EMU_PORT:-5624}" E2E_AVD="${E2E_UPD_AVD:-gb36u}" E2E_RUN="$W/and-run" E2E_RESULTS="$W/and-results.tsv"
  # shellcheck disable=SC1091
  set -- ; source scripts/e2e-android.sh
  set +e
  # the sourced file redefines these names; restore ours
  MOCK_PORT=18891; RESULTS="$ROOT/dist/android/logs/e2e-update-results.tsv"
  log() { printf '[upd %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
  rec() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" | tee -a "$RESULTS" >&2; }
  start_emulator >/dev/null 2>&1   # no-op when it is already up
  $ADB get-state 2>/dev/null | grep -q device || { rec android FAIL "no emulator on $SERIAL (adb server $ANDROID_ADB_SERVER_PORT)"; return 1; }
  $ADB shell svc power stayon true; wake
  local shotn=0
  upshot() { $ADB exec-out screencap -p >"$SHOTS/e2e-upd-$1.png" 2>/dev/null; }
  pkgver() { $ADB shell dumpsys package $PKG | grep -E "versionName|versionCode" | head -2 | tr -d '\r' | sed 's/^ *//' | tr '\n' ' '; }
  appstate() { $ADB shell "run-as $PKG sh -c 'cd /data/data/$PKG; find files shared_prefs databases -type f 2>/dev/null | grep -v -e \"^shared_prefs/updates.xml\" -e \"^shared_prefs/gb_state.xml\" | sort | xargs sha256sum 2>/dev/null'" | tr -d '\r'; }

  # fresh v0.1.0 with some private data
  $ADB uninstall $PKG >/dev/null 2>&1
  $ADB install "$A1" 2>&1 | grep -q Success || { rec android-install-v1 FAIL "adb install"; return 1; }
  $ADB shell appops set $PKG REQUEST_INSTALL_PACKAGES allow
  $ADB shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 4
  # A fresh install opens the setup wizard. Mark it finished through the app's own preference file (as the
  # script does for `updates`); that file is also part of the state that must survive the upgrade.
  $ADB shell am force-stop $PKG
  printf "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n    <boolean name=\"setup_finished\" value=\"true\" />\n    <string name=\"e2e_marker\">before-update</string>\n</map>\n" >"$W/state.xml"
  $ADB push "$W/state.xml" /data/local/tmp/state.xml >/dev/null
  $ADB shell run-as $PKG sh -c "mkdir -p shared_prefs"; $ADB shell run-as $PKG cp /data/local/tmp/state.xml shared_prefs/geekbridge_state.xml
  # Settings that must survive the upgrade (state-formats): clipboard sync on, device renamed, a paired computer
  # with a per-computer opt-in, an auto-accept choice. Written as the app's own v1 preference files.
  local PEER=0123456789abcdef0123456789abcdef
  putpref() { # file body-lines
    printf "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n%s</map>\n" "$2" >"$W/pref.xml"
    $ADB push "$W/pref.xml" /data/local/tmp/pref.xml >/dev/null
    $ADB shell run-as $PKG cp /data/local/tmp/pref.xml "shared_prefs/$1.xml"
  }
  putpref geekbridge_clipboard '    <boolean name="enabled" value="true" />
    <long name="window_ms" value="30000" />
'
  putpref geekbridge_device_name '    <string name="name">Upgrade Test Phone</string>
'
  putpref geekbridge_grants "    <string name=\"grants\">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$PEER&quot;:[&quot;contacts&quot;]}}</string>
"
  putpref share_policy "    <boolean name=\"auto_accept:$PEER\" value=\"true\" />
"
  putpref geekbridge_state "    <boolean name=\"setup_finished\" value=\"true\" />
    <int name=\"paired_count\" value=\"1\" />
    <string name=\"paired_peers\">$PEER	Desk</string>
    <string name=\"e2e_marker\">before-update</string>
"
  # A real trust record for that computer in the transport state dir (files/kde/trusted_peers.json, format v1),
  # with a throwaway self-signed certificate: the app derives its "paired" list from it, so without it the app
  # would (correctly) forget the computer and its opt-ins.
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3000 -subj "/CN=$PEER" \
    -keyout "$W/peer.key" -out "$W/peer.crt" 2>/dev/null
  openssl x509 -in "$W/peer.crt" -outform DER -out "$W/peer.der"
  printf '{\n  "version": 1,\n  "peers": [{"device_id": "%s", "name": "Desk", "device_type": "desktop", "protocol_version": 8, "paired_at_unix": 1767225600, "cert_der_base64": "%s", "cert_sha256": "%s"}]\n}\n' \
    "$PEER" "$(base64 -w0 "$W/peer.der")" "$(sha256sum "$W/peer.der" | cut -d' ' -f1)" >"$W/trusted_peers.json"
  $ADB push "$W/trusted_peers.json" /data/local/tmp/trusted_peers.json >/dev/null
  $ADB shell "run-as $PKG sh -c 'mkdir -p -m 700 files/kde && cp /data/local/tmp/trusted_peers.json files/kde/trusted_peers.json && chmod 600 files/kde/trusted_peers.json'"
  $ADB shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 4
  upshot home-after-setup
  # A fresh install is not an update: no "What's new" card, and the version is recorded without one
  check android-whatsnew-not-on-fresh-install $([[ -z "$(ui dump 2>/dev/null | cut -f1 | grep '^Updated to')" ]]; echo $?) "no card after a fresh install"
  # add a home-screen widget (debug helper); the launcher asks to confirm the pin
  $ADB shell am start -n "$PKG/.DebugPinWidgetActivity" -e w quick >/dev/null; sleep 3
  for _ in 1 2 3 4 5; do ui dump | grep -q "^Add to home screen" && { tap "Add to home screen"; sleep 3; break; }; sleep 2; done
  upshot widget-pinned
  widgets() { $ADB shell dumpsys appwidget | grep -c "^    provider=.*ComponentInfo{$PKG/" | tr -d '\r'; }
  local widgets_before; widgets_before="$(widgets)"
  check android-widget-added $([[ "$widgets_before" -ge 1 ]]; echo $?) "$widgets_before widget(s) of $PKG registered before the upgrade"
  prefval() { $ADB shell run-as $PKG cat "shared_prefs/$1.xml" 2>/dev/null | tr -d '\r'; }
  # data marker (app-private file + preference) standing in for pairing records; a real pairing is covered by e2e-android.sh
  $ADB shell "run-as $PKG sh -c 'mkdir -p files && echo before-update-marker > files/e2e-marker.txt'"
  $ADB shell am force-stop $PKG
  # Make the data look like it came from a build that predates versioning (no gb_state.xml).
  $ADB shell run-as $PKG rm -f shared_prefs/gb_state.xml
  local before; before="$(appstate)"
  rec android-install-v1 PASS "$(pkgver)"

  start_mock "$A2"
  $ADB reverse "tcp:$MOCK_PORT" "tcp:$MOCK_PORT" >/dev/null
  setprefs() { # last_check_ms auto_check
    printf "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n    <string name=\"debug_base_url\">http://127.0.0.1:%s</string>\n    <boolean name=\"auto_check\" value=\"%s\" />\n    <long name=\"last_check_ms\" value=\"%s\" />\n</map>\n" "$MOCK_PORT" "$2" "$1" >"$W/updates.xml"
    $ADB push "$W/updates.xml" /data/local/tmp/updates.xml >/dev/null
    $ADB shell am force-stop $PKG
    $ADB shell run-as $PKG sh -c "mkdir -p shared_prefs"; $ADB shell run-as $PKG cp /data/local/tmp/updates.xml shared_prefs/updates.xml
  }
  # wait (up to $1 s) until the screen shows text matching the regex $2 (a slow shared host needs polling, not sleeps)
  opendiag() { local a; for a in 1 2 3; do goto_diag >/dev/null 2>&1 && return 0; sleep 3; done; return 1; }
  # The status line is above the buttons; the screen is scrolled, so look at the current view, then one page up.
  wait_any() {
    local n=$1 i
    for ((i = 0; i < n; i += 4)); do
      ui dump 2>/dev/null | cut -f1 | grep -qE "$2" && return 0
      $ADB shell input swipe 540 900 540 1900 250; sleep 1
      ui dump 2>/dev/null | cut -f1 | grep -qE "$2" && return 0
      $ADB shell input swipe 540 1900 540 900 250; sleep 2
    done; return 1
  }
  seen() { wait_any 8 "$1"; }
  # Tap "Download and verify" and, with the replica, make sure the APK was really requested: the card may still show
  # the previous offer's text while the new check is running, and a tap during that check is ignored (busy).
  apk_hits() { jq -s '[.[] | select(.path|test("geekbridge-android.apk")) | select(.status==200)] | length' "$MOCKLOG" 2>/dev/null || echo 0; }
  download_ui() {
    local before attempt; before="$(apk_hits)"
    for attempt in 1 2 3 4; do
      ui scrolltap "Download and verify"
      [[ -z "$REPLICA" ]] && break
      for _ in $(seq 10); do [[ "$(apk_hits)" -gt "$before" ]] && break 2; sleep 1; done
      sleep 3
    done
  }
  checknow() {
    opendiag
    ui scrollto "Check for updates" >/dev/null; tap "Check for updates"
    wait_any 45 "is up to date|is available|Couldn|rate limit|No public release|not understood|no usable|GitHub answered|untrusted|not trusted|no Android package"
  }
  local now; now=$(( $(date +%s) * 1000 ))

  # ---- 12 h throttle (automatic check on app open)
  setprefs "$now" true; : >"$MOCKLOG"; opendiag; sleep 6
  check android-throttle-recent-no-request $([[ "$(api_hits)" == 0 ]]; echo $?) "last check just now: $(api_hits) request(s) to the mock when the app opened"
  setprefs $(( now - 13*3600*1000 )) true; : >"$MOCKLOG"; opendiag; sleep 6
  check android-throttle-13h-requests $([[ "$(api_hits)" -ge 1 ]]; echo $?) "last check 13 h ago: $(api_hits) request(s)"
  setprefs $(( now - 11*3600*1000 )) true; : >"$MOCKLOG"; opendiag; sleep 6
  check android-throttle-11h-no-request $([[ "$(api_hits)" == 0 ]]; echo $?) "last check 11 h ago: $(api_hits) request(s)"
  setprefs 0 false; : >"$MOCKLOG"; opendiag; sleep 6
  check android-auto-check-switch-off $([[ "$(api_hits)" == 0 ]]; echo $?) "auto_check=false: $(api_hits) request(s)"

  # ---- downgrade / same version is not offered
  mode "tag=v0.0.9"; checknow; upshot downgrade
  check android-downgrade-not-offered $(seen "is up to date"; echo $?) "tag v0.0.9 vs installed 0.1.0"
  mode "tag=v$OLDV"; checknow
  check android-same-version-not-offered $(seen "is up to date"; echo $?) "tag v$OLDV"

  # ---- tampered SHA256SUMS
  mode "tamper_sums"; checknow
  download_ui; wait_any 60 "downloaded and verified|does not match|wrong size|interrupted|failed|not acceptable|not usable"; upshot tampered
  check android-tampered-sums-refused $(seen "does not match the published SHA-256"; echo $?) "digest published as 000..0"
  check android-tampered-file-deleted $([[ -z "$($ADB shell run-as $PKG ls cache/updates 2>/dev/null | tr -d '\r')" ]]; echo $?) "cache/updates empty"
  # ---- corrupted APK (same size, one byte flipped), genuine digest
  mode "corrupt"; checknow
  download_ui; wait_any 60 "downloaded and verified|does not match|wrong size|interrupted|failed|not acceptable|not usable"; upshot corrupt
  check android-corrupt-apk-refused $(seen "does not match the published SHA-256"; echo $?) "one flipped byte; screen: $(ui dump 2>/dev/null | cut -f1 | grep -iE 'does not match|wrong size|interrupted|failed|verified' | head -2 | tr '\n' '|')"
  check android-corrupt-file-deleted $([[ -z "$($ADB shell run-as $PKG ls cache/updates 2>/dev/null | tr -d '\r')" ]]; echo $?) "cache/updates empty"
  # ---- no SHA256SUMS
  mode "nosums"; checknow; upshot nosums
  check android-no-sums-no-download $(seen "publishes no SHA-256"; echo $?) "no Download button offered"

  # ---- skip this version: not a feature of the Android app (desktop only), see docs/updates.md
  rec android-skip-version SKIP "Android has no skip control by design; the desktop one is tested above"

  if [[ -n "$REPLICA" ]]; then
    # ---- states a real repository can be in
    mode notfound; checknow; upshot no-public-release
    check android-no-public-release-state $(seen "No public release found"; echo $?) "private repository / no releases: 404 shown as its own state, not 'up to date'"
    mode ratelimit; checknow; upshot rate-limited
    check android-rate-limit-state $(seen "rate limiting"; echo $?) "403 + X-RateLimit-Remaining: 0"
    # ---- signer-certificate pre-check: the newest release carries an APK signed with another key
    mode "only=v0.1.2"; checknow
    ui scrolltap "Download and verify"; wait_any 60 "downloaded and verified|does not match|wrong size|interrupted|failed|signed with a different key|debug-signed|Couldn.t read the signing"; upshot signer-mismatch
    check android-signer-mismatch-refused $(seen "developer \(debug-signed\) build|signed with a different key"; echo $?) "same bytes as the update, different signing key: refused before Install"
    check android-signer-mismatch-file-deleted $([[ -z "$($ADB shell run-as $PKG ls cache/updates 2>/dev/null | tr -d '\r')" ]]; echo $?) "cache/updates empty"
  fi

  # ---- the real upgrade
  mode ""; checknow; upshot offer
  check android-offer-v2 $(seen "Version $NEWV is available"; echo $?) ""
  ui scrolltap "Download and verify"; wait_any 60 "downloaded and verified|does not match|wrong size|interrupted|failed|not acceptable|not usable"; upshot verified
  check android-download-verified $(seen "downloaded and verified against the published SHA-256"; echo $?) ""
  ui scrolltap "Install update"; sleep 5; upshot installer
  log "installer screen: $($ADB shell dumpsys window | grep mCurrentFocus | tr -d '\r')"
  # confirm in the package installer; the button label varies by Android build
  for label in "Update" "Install" "Done"; do
    for _ in 1 2 3 4; do
      ui dump | grep -q "^$label" && { tap "$label"; sleep 4; break; }
      sleep 2
    done
  done
  upshot after-install
  local ok=1
  for _ in $(seq 20); do [[ "$(pkgver)" == *"versionName=$NEWV"* ]] && { ok=0; break; }; sleep 2; done
  check android-upgrade-installed $ok "$(pkgver)"
  check android-data-survived $([[ "$(appstate)" == "$before" && "$($ADB shell run-as $PKG cat files/e2e-marker.txt | tr -d '\r')" == before-update-marker ]]; echo $?) "files/ shared_prefs/ databases/ hashes unchanged, marker intact"
  $ADB shell dumpsys package $PKG | grep -q "versionCode=$NEWCODE" ; check android-version-code $? "versionCode $NEWCODE"
  $ADB shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 6
  # What's new (the AFTER view; the release notes above were the BEFORE view): shown once, dismissable, never again
  $ADB exec-out screencap -p >"$SHOTS/whatsnew-upgrade.png" 2>/dev/null
  check android-whatsnew-shown-after-upgrade $([[ -n "$(ui dump 2>/dev/null | cut -f1 | grep "^Updated to $NEWV")" ]]; echo $?) "card titled Updated to $NEWV on the first launch after the upgrade"
  check android-whatsnew-has-link-and-dismiss $([[ -n "$(ui dump 2>/dev/null | cut -f1 | grep '^Full release notes')" && -n "$(ui dump 2>/dev/null | cut -f1 | grep '^Dismiss')" ]]; echo $?) ""
  tap "Dismiss"; sleep 2
  $ADB exec-out screencap -p >"$SHOTS/whatsnew-dismissed.png" 2>/dev/null
  check android-whatsnew-dismissed $([[ -z "$(ui dump 2>/dev/null | cut -f1 | grep '^Updated to')" ]]; echo $?) "card gone after Dismiss"
  $ADB shell am force-stop $PKG; $ADB shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 5
  check android-whatsnew-not-shown-again $([[ -z "$(ui dump 2>/dev/null | cut -f1 | grep '^Updated to')" ]]; echo $?) "not shown on the next launch"
  check android-app-starts-after-update $([[ -n "$($ADB shell pidof $PKG | tr -d '\r')" ]]; echo $?) ""
  # settings and pairing survived, and the new build stamped the schema version without rewriting anything else
  check android-clipboard-sync-survived $([[ "$(prefval geekbridge_clipboard)" == *'name="enabled" value="true"'* ]]; echo $?) "clipboard sync still on"
  check android-device-name-survived $([[ "$(prefval geekbridge_device_name)" == *"Upgrade Test Phone"* ]]; echo $?) "device still named Upgrade Test Phone"
  check android-pairing-and-optins-survived $([[ "$(prefval geekbridge_state)" == *"paired_count"*"value=\"1\""* && "$(prefval geekbridge_grants)" == *"$PEER"*contacts* && "$(prefval share_policy)" == *"auto_accept:$PEER"* ]]; echo $?) "paired computer, its contacts opt-in and auto-accept kept"
  check android-widget-survived $([[ "$(widgets)" -ge "$widgets_before" && "$widgets_before" -ge 1 ]]; echo $?) "$(widgets) widget(s) after, $widgets_before before"
  check android-schema-version-stamped $([[ "$(prefval gb_state)" == *'name="schema_version" value="1"'* ]]; echo $?) "gb_state.xml schema_version=1 (absent before: data from a build that predates versioning is version 1)"
  # now current: no further offer
  setprefs 0 false; mode ""; checknow
  check android-up-to-date-after-update $(seen "is up to date"; echo $?) ""
  if [[ -n "$REPLICA" ]]; then
    # two checks in a row with an unchanged release: the second carries If-None-Match and is answered 304
    checknow; checknow
    local uas; uas="$(jq -r 'select(.path|endswith("/releases/latest")) | .ua' "$MOCKLOG" | sort -u | tr '\n' ' ')"
    check android-request-user-agent $([[ "$uas" == *"geekbridge-android-updater/$OLDV"* ]]; echo $?) "User-Agent(s) seen: $uas"
    check android-etag-304-seen $([[ "$(jq -s '[.[] | select((.path|endswith("/releases/latest")) and .status==304)] | length' "$MOCKLOG")" -ge 1 ]]; echo $?) "conditional request answered 304 at least once"
    check android-asset-redirect-chain $([[ "$(jq -s '[.[] | select(.path|test("releases/download|release-assets")) | .status] | index(302) != null and index(200) != null' "$MOCKLOG")" == true ]]; echo $?) "download via github.com/…/releases/download -> 302 -> release-assets host"
  fi
  stop_mock
}

case "$WHAT" in
  desktop) desktop ;; android) android ;; all) desktop; android ;; *) echo "usage: $0 [desktop|android|all] [--no-build]"; exit 2 ;;
esac
echo; column -t -s $'\t' "$RESULTS"
! grep -q $'\tFAIL\t' "$RESULTS"
