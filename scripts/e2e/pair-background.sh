#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# "The app goes to the background right after pairing" on the emulator (docs/reports/real-device-first-pairing.md).
#
#   scripts/e2e/pair-background.sh [--apk PATH]    (default: the APK in $E2E_SCRATCH/dist)
#
# Pairs through the QR link (the phone pairs while visible), then, with "Stay connected" on:
#   home     press HOME right after pairing completes; the link must still be connected 25 s later
#   swipe    swipe the app away from Recents; the link must be (or become) connected again
#   doze     force the device idle; after leaving idle the link must be connected again
# Private ports, AVD, adb server, desktop state: see the E2E_* defaults below. Results: dist/android/logs/pair-background-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
APKARG=""
while (($#)); do case "$1" in --apk) APKARG="$2"; shift ;; *) echo "unknown $1" >&2; exit 2 ;; esac; shift; done
: "${E2E_AVD:=gb36}" "${E2E_EMU_PORT:=5644}" "${E2E_ADB_PORT:=16110}" "${E2E_TAG:=-pb}"
: "${E2E_RUN:=$ROOT/target/pb-run}" "${E2E_SCRATCH:=${TMPDIR:-/tmp}/pb-android}" "${E2E_TARGET:=$ROOT/target/rc}"
: "${E2E_XDISP:=:94}" "${E2E_CTRL_PORT:=17691}" "${E2E_API_PORT:=17690}" "${E2E_DISC_PORT:=17646}" "${E2E_LISTEN_PORT:=17741}" "${E2E_REDIR_PORT:=17761}"
: "${E2E_RESULTS:=$ROOT/dist/android/logs/pair-background-results.tsv}"
export GB_LEGACY_TARGET=1 E2E_SOURCE_ONLY=1 PL_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT E2E_RESULTS
if [[ -n "$APKARG" ]]; then mkdir -p "$E2E_SCRATCH/dist"; cp "$APKARG" "$E2E_SCRATCH/dist/geekbridge-debug.apk"; fi
# shellcheck disable=SC1091
source scripts/e2e/pair-lifecycle.sh --no-build
export ADBUI_TIMEOUT=60
: >"$E2E_RESULTS"

svc_running() { $ADB shell dumpsys activity services $PKG 2>/dev/null | grep -q "ServiceRecord.*LinkService"; }
link_ok() { is_connected && ctl ping "$PHONE_ID" >/dev/null 2>&1; }
settle_connected() { wait_for "${1:-40}" link_ok; }

pb_env() {
  pl_env || return 1
  finish_setup
  PHONE_ID=""
  goto_diag && ensure_link || return 1
  PHONE_ID="$(phone_id)"; echo "$PHONE_ID" >"$RUN/phone-id"
  [[ -n "$PHONE_ID" ]]
}
pb_pair() {
  is_paired && { ctl unpair "$PHONE_ID" >/dev/null 2>&1; sleep 3; }
  TAG=pb; qr_pair >/dev/null || return 1
  desk_dial  # the PC dials the phone back through the emulator redirection once the phone listens
  return 0
}

# The real-device story: the code is on the phone, the owner goes to the computer (the phone leaves the foreground),
# compares and approves there, then confirms on the phone. The link must still be there.
pb_prehome() {
  is_paired && { ctl unpair "$PHONE_ID" >/dev/null 2>&1; sleep 3; ensure_link || return 1; }
  desk_dial; wait_for 10 has_unpaired_link || { rec prehome FAIL "no link to the phone"; return 1; }
  ctl pair "$PHONE_ID" request >/dev/null
  wait_for 15 ui code >/dev/null || { rec prehome FAIL "no pairing dialog on the phone"; return 1; }
  local code id; code="$(ui code)"; id="$(ctl status | jget 'd["challenges"][0]["challenge_id"]')"
  $ADB shell input keyevent KEYCODE_HOME; sleep 8
  if [[ -z "$(link_phases)" ]]; then rec prehome FAIL "the phone dropped the link when it left the foreground during pairing (service=$(svc_running && echo up || echo down))"; return 1; fi
  ctl approve "$id" "$code" >/dev/null || { rec prehome FAIL "desktop approve refused"; return 1; }
  goto_home; ui wait Pair 20 >/dev/null; ui tap Pair >/dev/null; sleep 4
  if wait_for 20 is_paired; then rec prehome PASS "link survived 8 s in the background during pairing; paired afterwards; service $(svc_running && echo up || echo down)"
  else rec prehome FAIL "not paired after returning to the app"; return 1; fi
}

main_pb() {
  pb_env || { rec env FAIL "environment did not come up"; return 1; }
  pb_prehome || pb_pair || { rec pair FAIL "pairing failed"; return 1; }
  is_paired || { rec pair FAIL "not paired"; return 1; }
  rec pair PASS "paired; service running: $(svc_running && echo yes || echo no)"
  # --- home: leave the app the moment pairing is done
  $ADB shell input keyevent KEYCODE_HOME
  sleep 25
  if svc_running && settle_connected 40; then rec home PASS "service running, link connected 25 s after HOME"
  else rec home FAIL "after HOME: service=$(svc_running && echo up || echo down) link=$(link_phases)"; fi
  # --- swipe the app away
  $ADB shell input keyevent KEYCODE_APP_SWITCH; sleep 2
  $ADB shell input swipe 540 1500 540 100 150; sleep 3
  $ADB shell input keyevent KEYCODE_HOME; sleep 5
  desk_dial >/dev/null 2>&1
  if svc_running && settle_connected 40; then rec swipe PASS "service survived the swipe; link connected"
  else rec swipe FAIL "after swipe: service=$(svc_running && echo up || echo down) link=$(link_phases)"; fi
  # --- doze
  $ADB shell dumpsys battery unplug >/dev/null 2>&1
  $ADB shell dumpsys deviceidle force-idle >/dev/null 2>&1
  sleep 20
  $ADB shell dumpsys deviceidle unforce >/dev/null 2>&1; $ADB shell dumpsys battery reset >/dev/null 2>&1
  desk_dial >/dev/null 2>&1
  if svc_running && settle_connected 60; then rec doze PASS "service up and link connected after forced idle"
  else rec doze FAIL "after doze: service=$(svc_running && echo up || echo down) link=$(link_phases)"; fi
  echo; column -t -s $'\t' "$RESULTS"
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
[[ "${PB_SOURCE_ONLY:-}" == 1 ]] && return 0
main_pb
