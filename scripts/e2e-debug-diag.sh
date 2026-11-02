#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator proof of the debug diagnostics (docs/debug-diagnostics.md): the debug APK's own log reaches the
# desktop, and `geekbridge-ctl phone-state` explains why each capability is (not) offered, before and after the
# switches and permissions change from adb.
#
#   scripts/e2e-debug-diag.sh [--stop]
#
# Shares nothing live with the other e2e scripts: its own AVD (gb36x, cloned from gb36 and removed on exit unless
# GB_KEEP_AVD=1), console port 5654, adb server 16043, control port 17501, state target/e2e-dbg-run.
# Needs the debug APK in $E2E_SCRATCH/dist/geekbridge-debug.apk (cd android && gradle --offline :app:assembleDebug,
# then copy) and the desktop binaries in target/e2e-dbg (CARGO_TARGET_DIR=target/e2e-dbg cargo build --release -p bridge-service --bins).
# Results: dist/android/logs/e2e-debug-diag-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
[[ "${1:-}" == --stop ]] && STOP_ONLY=1
set --
export GB_LEGACY_TARGET=1 GB_BASE_AVD="${GB_BASE_AVD:-gb36}"
export E2E_SOURCE_ONLY=1 E2E_AVD=gb36x E2E_EMU_PORT=5654 E2E_ADB_PORT=16043 E2E_TAG=-dbg \
  E2E_RUN="$ROOT/target/e2e-dbg-run" E2E_SCRATCH="$ROOT/target/e2e-dbg-android" E2E_TARGET="$ROOT/target/e2e-dbg" \
  E2E_RESULTS="$ROOT/dist/android/logs/e2e-debug-diag-results.tsv" E2E_XDISP=:96 \
  E2E_CTRL_PORT=17501 E2E_API_PORT=17500 E2E_DISC_PORT=17436 E2E_LISTEN_PORT=1735 E2E_REDIR_PORT=17181
# shellcheck disable=SC1091
source scripts/e2e-android.sh --no-build
if [[ "${STOP_ONLY:-}" == 1 ]]; then STOP=1; main; exit 0; fi
mkdir -p "$(dirname "$APK")"
GRADLE_APK="$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
[[ -f "$GRADLE_APK" ]] && cp -f "$GRADLE_APK" "$APK"

BC="dev.geekbridge.android/.DebugControlReceiver"
bcast() { $ADB shell am broadcast -n "$BC" -a dev.geekbridge.debug.SET "$@" >/dev/null; }
state() { ctl phone-state "$PHONE_ID" 2>&1; }
expect_state() { # label grep-pattern  -> PASS/FAIL line in the results
  local label="$1" pat="$2" out; out="$(state)"
  if grep -qE -- "$pat" <<<"$out"; then rec debug PASS "$label"; else rec debug FAIL "$label: no match for '$pat'"; printf '%s\n' "$out" | head -60 >&2; return 1; fi
}
reconnect() { wake; desk_dial; wait_for 25 is_connected; }
log_file() { echo "$RUN/state/phone-logs/$PHONE_ID.log"; }

stage_debug() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec debug SKIP "not paired (run env,battery,pair first)"; return 1; }
  is_connected || reconnect || { rec debug FAIL "link not connected"; return 1; }
  # 1. consent: default ON for a debug phone, visible in the consent list
  local on; on="$(ctl status | jget 'next(c["enabled"] for p in d["paired"] if p["device_id"]=="'"$PHONE_ID"'" for c in p["consents"] if c["feature"]=="debug")')"
  [[ "$on" == True ]] && rec debug PASS "debug consent is on by default for a debug phone" || rec debug FAIL "debug consent not on ($on)"
  # 2. logs flow phone -> PC
  wait_for 30 bash -c "[[ -s '$(log_file)' ]]" && rec debug PASS "phone log lines arrive in phone-logs/<id>.log" || { rec debug FAIL "no log file"; return 1; }
  $ADB shell log -t GB "e2e-marker-$$"; sleep 4
  ctl phone-log "$PHONE_ID" -n 400 | grep -q "e2e-marker-$$" && rec debug PASS "a fresh log line reaches 'phone-log' within seconds" || rec debug FAIL "marker line not seen"
  [[ "$(stat -c %a "$(log_file)")" == 600 ]] && rec debug PASS "log file is 0600" || rec debug FAIL "log file mode $(stat -c %a "$(log_file)")"
  ctl phone-log "$PHONE_ID" -n 400 | grep -qE 'GB: *(link|capability|debug)' && rec debug PASS "GB tag lines present (capability/link reasons)" || rec debug FAIL "no GB capability/link lines"
  # 3. state before: switches off, permissions not granted
  for p in READ_SMS SEND_SMS RECEIVE_SMS; do $ADB shell pm revoke $PKG android.permission.$p >/dev/null 2>&1; done
  bcast --es what sms --ez on false
  sleep 2
  expect_state "sms: switch OFF is explained" 'sms: not offered: Messages is switched OFF in the app'
  # 4. switch on, permissions missing
  bcast --es what sms --ez on true; sleep 3
  expect_state "sms: switch ON but permission missing is explained" 'sms: not offered: READ_SMS not granted'
  # 5. permissions granted, per-computer switch still OFF
  for p in READ_SMS SEND_SMS RECEIVE_SMS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  sleep 4; is_connected || reconnect
  expect_state "sms: permissions granted, per-computer switch OFF" 'sms: not offered: per-computer Messages switch is OFF'
  # 6. per-computer switch on -> offered (the phone re-declares and reconnects)
  bcast --es what grant --es computer "$PHONE_ID_ON_PHONE" --es feature sms --ez on true
  sleep 6; is_connected || reconnect
  expect_state "sms: offered after the per-computer switch" '^sms: offered'
  # 7. nothing personal in the dump or the log
  local dump; dump="$(state)"
  if grep -qE '[0-9]{7,}|@[A-Za-z0-9.-]+\.[a-z]{2,}' <<<"$dump$(ctl phone-log "$PHONE_ID" -n 2000)"; then rec debug FAIL "digits/e-mail found in dump or log"; else rec debug PASS "no long digit runs or e-mail addresses in the dump or log"; fi
  # 8. consent off stops the stream
  ctl consent "$PHONE_ID" debug off >/dev/null; local before; before="$(wc -c <"$(log_file)")"
  $ADB shell log -t GB "after-off-$$"; sleep 4
  [[ "$(wc -c <"$(log_file)")" == "$before" ]] && rec debug PASS "debug consent off: nothing more is stored" || rec debug FAIL "file grew after consent off"
  state | grep -qi "debug diagnostics are off" && rec debug PASS "phone-state explains consent off" || rec debug FAIL "phone-state did not explain consent off"
  ctl consent "$PHONE_ID" debug on >/dev/null
  shot dbg-final
}

stage_pairedid() { :; }
main_debug() {
  : >"$RESULTS"
  for st in env battery pair; do log "== stage $st"; "stage_$st" || { log "stage $st failed"; break; }; done
  load_ids
  # The phone's own id as the phone sees it (the per-computer grants are keyed by the computer's id).
  PHONE_ID_ON_PHONE="$DESK_ID"
  log "== stage debug"; stage_debug
  echo; echo "== results =="; column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_debug
