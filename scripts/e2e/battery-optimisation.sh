#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# "Keep the link up: ask the user to exclude the app from battery saving" end to end on the emulator
# (docs/android-power.md, docs/reports/battery-optimisation-onboarding.md).
#
#   scripts/e2e/battery-optimisation.sh [--apk PATH] [--no-build] [STAGE...]
#
# Stages (default: all):
#   ask         fresh install: the wizard's Battery step asks, the system dialog appears, Deny keeps the prompt, Allow
#               shows "Unrestricted" and puts the app on `dumpsys deviceidle whitelist`; the OEM guide is on the page
#   restricted  `am set-standby-bucket restricted` and `appops RUN_ANY_IN_BACKGROUND ignore` each raise the Home
#               warning and the service notification line; Fix opens the Battery step, whose button opens the
#               Settings app; restoring clears the warning
#   heartbeat   screen off, not in Doze: the phone pings the computer every ~60 s
#   doze        screen off, `deviceidle force-idle`, three maintenance windows: the link stays connected, the service
#               keeps running, and a window makes the phone ping at once (not at the next 60 s tick)
# Everything is private: own AVD clone (E2E_AVD, removed on exit), adb server, desktop ports, state dir and Xvfb.
# Scratch lives under $E2E_SCRATCH (default ~/.cache/gb-battery), never /tmp. Results: dist/android/logs/battery-optimisation-results.tsv
# Honest limit: the emulator does not suspend its CPU in Doze, so this proves the nudge and the link, not the real
# clock stop (docs/reports/battery-optimisation-onboarding.md).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
APKARG="" NOBUILD=() STAGES=()
while (($#)); do case "$1" in
  --apk) APKARG="$2"; shift ;; --no-build) NOBUILD=(--no-build) ;;
  -h|--help) sed -n '3,22p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) STAGES+=("$1") ;; esac; shift; done
[[ ${#STAGES[@]} -eq 0 ]] && STAGES=(ask restricted heartbeat doze)
CACHE="${GB_BATTERY_CACHE:-$HOME/.cache/gb-battery}"; mkdir -p "$CACHE/tmp"
export TMPDIR="$CACHE/tmp" GB_TMP="$CACHE/tmp"
: "${GB_BASE_AVD:=gb36u}" "${GB_MIN_FREE_GB:=10}"
: "${E2E_AVD:=gbbat}" "${E2E_EMU_PORT:=5664}" "${E2E_ADB_PORT:=16130}" "${E2E_TAG:=-bat}"
: "${E2E_RUN:=$CACHE/run}" "${E2E_SCRATCH:=$CACHE/scratch}" "${E2E_TARGET:=$HOME/.cache/gb-scratch/target-shared}"
: "${E2E_XDISP:=:96}" "${E2E_CTRL_PORT:=17791}" "${E2E_API_PORT:=17790}" "${E2E_DISC_PORT:=17746}" "${E2E_LISTEN_PORT:=17841}" "${E2E_REDIR_PORT:=17861}"
: "${E2E_RESULTS:=$ROOT/dist/android/logs/battery-optimisation-results.tsv}"
export GB_BASE_AVD GB_MIN_FREE_GB E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT E2E_RESULTS E2E_SOURCE_ONLY=1 PL_SOURCE_ONLY=1
if [[ -n "$APKARG" ]]; then mkdir -p "$E2E_SCRATCH/dist"; cp "$APKARG" "$E2E_SCRATCH/dist/geekbridge-debug.apk"; fi
# shellcheck disable=SC1091
source scripts/e2e/pair-lifecycle.sh "${NOBUILD[@]}"
export ADBUI_TIMEOUT=60
mkdir -p "$(dirname "$E2E_RESULTS")"; : >"$E2E_RESULTS"

bo_paired() { [[ -n "$PHONE_ID" ]] && is_paired; }
svc_running() { $ADB shell dumpsys activity services $PKG 2>/dev/null | grep -q "ServiceRecord.*LinkService"; }
whitelisted() { $ADB shell dumpsys deviceidle whitelist 2>/dev/null | tr -d '\r' | grep -q ",$PKG,"; }
link_ok() { is_connected && ctl ping "$PHONE_ID" >/dev/null 2>&1; }
pings() { ctl status | jget 'd["links"][0]["pings_received"]' 2>/dev/null || echo -1; }
last_ping_ms() { ctl status | jget 'd["links"][0]["last_ping_unix_ms"] or 0' 2>/dev/null || echo 0; }
now_ms() { echo $(( $(date +%s%N) / 1000000 )); }
focus() { $ADB shell dumpsys window 2>/dev/null | grep -m1 mCurrentFocus | tr -d '\r'; }
power_normal() {
  $ADB shell dumpsys deviceidle unforce >/dev/null 2>&1; $ADB shell dumpsys battery reset >/dev/null 2>&1
  $ADB shell svc power stayon true; wake
}
trap 'power_normal' EXIT

# ---------------------------------------------------------------- stages
stage_ask() {
  $ADB shell cmd deviceidle whitelist -$PKG >/dev/null 2>&1
  stage_battery   # e2e-android.sh: wizard by step title, Deny, Allow, whitelist (records its own result as "battery")
  local guide=FAIL
  # The Battery step also lists the maker's switches; the emulator reports Google, so the Pixel guide must be there.
  goto_home; ui scrolltap "Setup & permissions" >/dev/null 2>&1; sleep 2
  ui wait "Your setup" 10 >/dev/null 2>&1; ui tap Battery --exact >/dev/null 2>&1; sleep 2
  if ui has "Adaptive battery" || ui scrollto "Adaptive battery" >/dev/null 2>&1; then guide=PASS; fi
  shot battery-guide
  [[ $guide == PASS ]] && rec ask-guide PASS "OEM guide (Pixel on the emulator) names Adaptive battery" \
    || rec ask-guide FAIL "no OEM guide text on the Battery step"
  whitelisted && rec ask-whitelist PASS "dumpsys deviceidle whitelist lists $PKG" || rec ask-whitelist FAIL "not on the whitelist"
  goto_home
}

home_warns() { goto_home; sleep 2; ui has "may disconnect in the background"; }
notif_warns() { $ADB shell dumpsys notification --noredact 2>/dev/null | grep -q "Battery optimisation is on"; }
check_restricted() { # LABEL
  local label="$1" ok=1 why=""
  wait_for 20 home_warns || { ok=0; why+=" no Home warning;"; }
  shot "restricted-$label-home"
  # the service notification line is refreshed when the service starts or the link changes
  ui tap Fix --exact >/dev/null 2>&1 || ui tap "Fix" >/dev/null 2>&1; sleep 2
  ui has "Background use is restricted" || ui has "Tap below and choose" || { ok=0; why+=" Fix did not open the Battery step;"; }
  shot "restricted-$label-step"
  if [[ "$label" == appop || "$label" == bucket ]]; then
    ui tap "Open battery settings" >/dev/null 2>&1 && sleep 3
    if [[ "$label" == appop ]]; then
      focus | grep -qi "settings" || { ok=0; why+=" button did not open Settings ($(focus));"; }
    fi
    shot "restricted-$label-settings"
    $ADB shell input keyevent KEYCODE_BACK; sleep 1
  fi
  ((ok)) && rec "restricted-$label" PASS "warning on Home, Fix opens the Battery step, button opens Settings" \
    || rec "restricted-$label" FAIL "$why"
  goto_home
}
stage_restricted() {
  # Needs a paired phone: the warning is for an app that is in use. Pair first if the "ask" stage did not.
  bo_paired || { goto_diag && ensure_link && PHONE_ID="$(phone_id)" && pb_pair2; }
  # 1) Doze exemption removed and the app parked in the restricted standby bucket
  $ADB shell cmd deviceidle whitelist -$PKG >/dev/null 2>&1
  $ADB shell am set-standby-bucket $PKG restricted
  check_restricted bucket
  $ADB shell am set-standby-bucket $PKG active
  # 2) "Restricted" background mode (the appop behind Settings > App battery usage > Restricted)
  $ADB shell cmd appops set $PKG RUN_ANY_IN_BACKGROUND ignore
  check_restricted appop
  # 3) restore: exemption back, appop allowed -> the warning is gone
  $ADB shell cmd appops set $PKG RUN_ANY_IN_BACKGROUND allow
  $ADB shell cmd deviceidle whitelist +$PKG >/dev/null 2>&1
  goto_home; sleep 3
  if ui has "may disconnect in the background"; then rec restricted-clear FAIL "warning still shown after restoring"; else rec restricted-clear PASS "warning gone once the app is unrestricted"; fi
}
pb_pair2() { bo_paired && return 0; TAG=bo qr_pair >/dev/null || return 1; desk_dial; wait_for 30 link_ok; }

relink() { # the restricted stage may have stopped the service: start the link and dial from the computer
  goto_diag && ensure_link; desk_dial >/dev/null 2>&1; wait_for 45 link_ok \
    || { echo "relink: service=$(svc_running && echo up || echo down) phases=[$(link_phases)] paired=[$(paired_ids)] phone=$PHONE_ID" >&2; return 1; }
}
screen_off() { $ADB shell svc power stayon false; $ADB shell dumpsys battery unplug >/dev/null 2>&1; $ADB shell input keyevent KEYCODE_SLEEP; sleep 3; }
stage_heartbeat() {
  bo_paired || { rec heartbeat SKIP "not paired"; return; }
  $ADB shell cmd deviceidle whitelist +$PKG >/dev/null 2>&1
  relink || { rec heartbeat FAIL "no link"; return; }
  screen_off
  local p0 p1 t1 t2 gap
  p0="$(pings)"; sleep 5
  # wait for the next phone ping, then measure the gap to the one after
  local i; for ((i = 0; i < 80; i++)); do [[ "$(pings)" -gt "$p0" ]] && break; sleep 1; done
  t1="$(last_ping_ms)"; p1="$(pings)"
  for ((i = 0; i < 90; i++)); do [[ "$(pings)" -gt "$p1" ]] && break; sleep 1; done
  t2="$(last_ping_ms)"; gap=$(( (t2 - t1) / 1000 ))
  power_normal
  if (( t1 > 0 && t2 > t1 && gap >= 55 && gap <= 65 )); then rec heartbeat PASS "screen off: pings ${gap} s apart (idle cadence 60 s)"
  else rec heartbeat FAIL "screen off: gap between pings ${gap} s (want 55-65), pings $p0 -> $p1"; fi
}

stage_doze() {
  bo_paired || { rec doze SKIP "not paired"; return; }
  $ADB shell cmd deviceidle whitelist +$PKG >/dev/null 2>&1
  relink || { rec doze FAIL "no link before Doze"; return; }
  screen_off
  $ADB shell dumpsys deviceidle force-idle | tail -1 >/dev/null
  local state; state="$($ADB shell dumpsys deviceidle get deep | tr -d '\r')"
  [[ "$state" == IDLE ]] || { rec doze FAIL "device did not enter deep idle (deep=$state)"; power_normal; return; }
  local w ok=1 why="" p before age
  for w in 1 2 3; do
    # open a maintenance window about 15-40 s after the phone's last ping, well before its next 60 s tick
    for ((i = 0; i < 70; i++)); do age=$(( ($(now_ms) - $(last_ping_ms)) / 1000 )); (( age >= 15 && age <= 40 )) && break; sleep 1; done
    before="$(pings)"
    $ADB shell dumpsys deviceidle step deep >/dev/null 2>&1
    for ((i = 0; i < 8; i++)); do p="$(pings)"; [[ "$p" -gt "$before" ]] && break; sleep 1; done
    [[ "$p" -gt "$before" ]] || { ok=0; why+=" window $w: no ping within 8 s (pings $before);"; }
    link_ok || { ok=0; why+=" window $w: link lost;"; }
    # back to idle for the next window
    $ADB shell dumpsys deviceidle step deep >/dev/null 2>&1
    sleep 3
  done
  svc_running || { ok=0; why+=" service not running;"; }
  $ADB shell logcat -d -s GB 2>/dev/null | grep -q "doze idle=false" || { ok=0; why+=" no 'doze idle=false' log line;"; }
  power_normal
  wait_for 20 link_ok || { ok=0; why+=" link not up after Doze;"; }
  ((ok)) && rec doze PASS "3 maintenance windows: link connected, service up, ping within 8 s of each window" || rec doze FAIL "$why"
}
bo_env() { # as pl_env, but the AVD clone may carry a newer build of the app: uninstall first
  [[ -x "$SVC" && -f "$APK" ]] || { BUILD=yes; stage_env || return 1; BUILD=auto; }
  start_xvfb; start_emulator || return 1
  $ADB shell svc power stayon true; $ADB shell settings put global hide_error_dialogs 1; wake
  $ADB uninstall $PKG >/dev/null 2>&1
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || return 1
  start_desktop; desk_id
}
main_bo() {
  bo_env || { rec env FAIL "environment did not come up"; return 1; }
  PHONE_ID=""
  local s
  for s in "${STAGES[@]}"; do
    case "$s" in
      ask) stage_ask ;;
      restricted|heartbeat|doze)
        if ! bo_paired; then
          finish_setup; goto_diag && ensure_link && PHONE_ID="$(phone_id)"; echo "$PHONE_ID" >"$RUN/phone-id"
          pb_pair2 || { rec "$s" FAIL "could not pair"; continue; }
        fi
        "stage_$s" ;;
      *) echo "unknown stage $s" >&2 ;;
    esac
  done
  column -t -s $'\t' "$E2E_RESULTS"
  ! grep -q $'\tFAIL\t' "$E2E_RESULTS"
}
main_bo
