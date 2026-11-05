#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator proof of the phone side of calls (docs/reports/call-chain-review.md): the debug APK's call-state
# listener, the reports to the paired computer and the computer's answer / decline / hang up / dial, driven by the
# emulator modem (`adb emu gsm call|accept|cancel`) and `geekbridge-ctl call ...`.
#
#   scripts/e2e-call-chain.sh [--stop]      (CALLCHAIN_ONLY=stage1,stage2 to skip the pairing stages)
#
# Own AVD (gbacall, cloned from gb36, removed on exit unless GB_KEEP_AVD=1), console port 5722, adb server 16722,
# control port 17722, state target/e2e-cc-run. Needs the debug APK in $E2E_SCRATCH/dist/geekbridge-debug.apk and the
# desktop binaries in $E2E_TARGET/release (see scripts/e2e-debug-diag.sh).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
[[ "${1:-}" == --stop ]] && STOP_ONLY=1
set --
export GB_LEGACY_TARGET=1 GB_BASE_AVD="${GB_BASE_AVD:-gb36}"
export E2E_SOURCE_ONLY=1 E2E_AVD=gbacall E2E_EMU_PORT=5722 E2E_ADB_PORT=16722 E2E_TAG=-cc \
  E2E_RUN="$ROOT/target/e2e-cc-run" E2E_SCRATCH="${E2E_SCRATCH:-$ROOT/target/e2e-cc-android}" E2E_TARGET="${E2E_TARGET:-$ROOT/target/e2e-cc}" \
  E2E_RESULTS="$ROOT/dist/android/logs/e2e-call-chain-results.tsv" E2E_XDISP=:89 \
  E2E_CTRL_PORT=17722 E2E_API_PORT=17721 E2E_DISC_PORT=17622 E2E_LISTEN_PORT=1722 E2E_REDIR_PORT=17222
# shellcheck disable=SC1091
source scripts/e2e-android.sh --no-build
if [[ "${STOP_ONLY:-}" == 1 ]]; then STOP=1; main; exit 0; fi
mkdir -p "$(dirname "$APK")"
GRADLE_APK="$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
[[ -f "$GRADLE_APK" ]] && cp -f "$GRADLE_APK" "$APK"

BC="dev.geekbridge.android/.DebugControlReceiver"
bcast() { $ADB shell am broadcast -n "$BC" -a dev.geekbridge.debug.SET "$@" >/dev/null; }
state() { ctl phone-state "$PHONE_ID" 2>&1; }
gsm() { $ADB emu gsm "$@" >/dev/null 2>&1; }
home() { $ADB shell am start -n dev.geekbridge.android/.HomeActivity >/dev/null 2>&1; }
calls() { ctl calls "$@" 2>&1; }
ctlcall() { ctl call "$@" 2>&1; }
# check LABEL CMD PATTERN [TRIES]: poll CMD's output for PATTERN
check() {
  local label="$1" cmd="$2" pat="$3" n="${4:-12}" out=""
  for ((k = 0; k < n; k++)); do out="$($cmd)"; grep -qiE -- "$pat" <<<"$out" && { rec callchain PASS "$label"; return 0; }; sleep 1; done
  rec callchain FAIL "$label: no match for '$pat'"; printf '%s\n' "$out" | head -30 >&2; return 1
}
absent() { # label cmd pattern
  local out; out="$($2)"
  if grep -qiE -- "$3" <<<"$out"; then rec callchain FAIL "$1: found '$3'"; printf '%s\n' "$out" | head -20 >&2; else rec callchain PASS "$1"; fi
}
idle() { for n in 5550100 5550101 5550102 5550103; do gsm cancel $n; done; sleep 2; }
NOCALL='^no calls'; RING='^ringing'; LIVE='^active'
reconnect() { wake; desk_dial >/dev/null 2>&1; wait_for 60 is_connected; }

stage_callchain() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec callchain SKIP "not paired"; return 1; }
  is_connected || reconnect || { rec callchain FAIL "link not connected"; return 1; }
  PKGP="android.permission"
  ctl consent "$PHONE_ID" telephony on >/dev/null 2>&1
  bcast --es what calls --ez on true
  bcast --es what grant --es computer "$DESK_ID" --es feature calls --ez on true
  for p in ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS CALL_PHONE POST_NOTIFICATIONS; do $ADB shell pm grant $PKG $PKGP.$p >/dev/null 2>&1; done
  # 1. a permission granted mid-session is picked up without a restart. The revoke kills the app; the sticky service
  # restarts it (process death) and must find the permission missing, then see the grant when the screen comes on.
  $ADB shell pm revoke $PKG $PKGP.READ_PHONE_STATE >/dev/null 2>&1; sleep 8
  reconnect || { rec callchain FAIL "link not back after the process was killed"; return 1; }
  check "after process death the service restarts and explains the missing permission" state 'not registered \(no_read_phone_state\)' 20
  $ADB shell pm grant $PKG $PKGP.READ_PHONE_STATE >/dev/null 2>&1
  $ADB shell input keyevent KEYCODE_SLEEP; sleep 2; wake
  check "a grant made in Settings registers the listener when the screen comes on, no restart" state 'call state listener: registered via' 15
  bcast --es what calls --ez on true   # switching on twice must not wipe the known line state
  sleep 2; idle
  check "idle reported to the computer" "calls" "$NOCALL" 15
  # 2. incoming call: the computer sees it masked unless revealed
  gsm call 5550100
  check "ringing reaches the computer" "calls" "$RING" 15
  check "number reaches the computer when revealed (READ_CALL_LOG granted)" "calls --reveal" '5550100' 10
  absent "number is masked without --reveal" "calls" '5550100'
  # 3. answer then hang up from the computer
  ctlcall answer >&2
  check "answer from the computer puts the call off hook" "calls" "$LIVE" 15
  ctlcall hangup >&2
  check "hang up from the computer ends the call" "calls" "$NOCALL" 15
  # 4. decline from the computer
  gsm call 5550100; check "ringing again" "calls" "$RING" 15
  ctlcall reject >&2
  check "decline from the computer ends the ringing call" "calls" "$NOCALL" 15
  # 5. remote hang up
  gsm call 5550100; check "ringing (3)" "calls" "$RING" 15
  ctlcall answer >&2; check "answered (3)" "calls" "$LIVE" 15
  gsm cancel 5550100
  check "remote hang up is reported as idle" "calls" "$NOCALL" 15
  # 6. missed call
  gsm call 5550100; check "ringing (4)" "calls" "$RING" 15
  gsm cancel 5550100
  check "a missed call ends as idle" "calls" "$NOCALL" 15
  # 7. a call placed on the phone itself (off hook without ringing)
  $ADB shell am start -a android.intent.action.CALL -d tel:5550102 >/dev/null 2>&1
  check "a call placed on the phone itself is reported" "calls" "$LIVE" 20
  ctlcall hangup >&2
  check "outgoing call ended from the computer" "calls" "$NOCALL" 15
  # 8. dial from the computer with the app in the background (TelecomManager.placeCall from the service)
  $ADB shell input keyevent KEYCODE_HOME; sleep 1
  ctlcall dial 5550103 --yes >&2
  check "dial from the computer starts a call while the app is in the background" "calls" "$LIVE" 20
  ctlcall hangup >&2
  check "dialed call ended" "calls" "$NOCALL" 15
  # 9. screen off
  $ADB shell input keyevent KEYCODE_SLEEP; sleep 2
  gsm call 5550100; check "ringing with the screen off" "calls" "$RING" 15
  ctlcall answer >&2; check "answer works with the screen off" "calls" "$LIVE" 15
  ctlcall hangup >&2; check "hang up works with the screen off" "calls" "$NOCALL" 15
  wake
  # 10. confirm on the phone: the question is a notification; Call places the call
  bcast --es what confirm_dial --ez on true; sleep 1
  ctlcall dial 5550103 --yes >&2; sleep 3
  $ADB shell dumpsys notification --noredact | grep -q "channel=geekbridge_dial" && rec callchain PASS "the confirm question is shown as a notification" || rec callchain FAIL "no confirm notification"
  $ADB shell cmd statusbar expand-notifications; sleep 2; tap "Call" >/dev/null 2>&1
  check "tapping Call on the phone places the call" "calls" "$LIVE" 15
  ctlcall hangup >&2; sleep 2
  # without notification permission the question cannot be seen: refused at once, nothing dialed
  $ADB shell pm revoke $PKG $PKGP.POST_NOTIFICATIONS >/dev/null 2>&1; sleep 8
  reconnect; sleep 3; bcast --es what confirm_dial --ez on true; $ADB shell input keyevent KEYCODE_HOME; sleep 1
  ctlcall dial 5550103 --yes >&2
  check "no notification permission: the dial is declined at once" "calls" 'did not confirm' 8
  absent "and no call was placed" "calls" "$LIVE"
  $ADB shell pm grant $PKG $PKGP.POST_NOTIFICATIONS >/dev/null 2>&1
  # 11. nothing personal in the phone dump or log
  local dump; dump="$(state)$(ctl phone-log "$PHONE_ID" -n 2000 2>&1)"
  if grep -q '5550' <<<"$dump"; then rec callchain FAIL "a phone number appears in the phone dump or log"; else rec callchain PASS "no number in the phone dump or log"; fi
  idle
}

main_cc() {
  : >"$RESULTS"
  for st in ${CALLCHAIN_PRE:-env battery pair}; do log "== stage $st"; "stage_$st" || { log "stage $st failed"; break; }; done
  load_ids
  log "== stage callchain"; stage_callchain
  echo; echo "== results =="; column -t -s $'\t' "$RESULTS" | tail -n 60
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
[[ "${CALLCHAIN_SOURCE_ONLY:-}" == 1 ]] || main_cc
