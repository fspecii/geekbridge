#!/usr/bin/env bash
# Pairing lifecycle end to end on the emulator (docs/pairing.md, docs/first-run-audit.md rows 3 and 15):
#
#   scripts/e2e/pair-lifecycle.sh [--repeat N] [--no-build] [SCENARIO...]
#
# Scenarios (each runs N times, default 3, and must pass every time):
#   A_qr / A_code   the PHONE unpairs while the computer still trusts it, then re-pair (QR link / code comparison)
#   B_qr / B_code   the COMPUTER forgets the phone while the phone still trusts it, then re-pair
#   C_qr / C_code   both sides forget, then re-pair
#   rename          rename phone and computer (hostile names too), check the other side, notifications, Diagnostics
# "First attempt" means: one pass through the pairing screens, no retry, no manual unpair.
#
# Topology and helpers come from scripts/e2e-android.sh (sourced). Everything is private: own AVD, adb server, desktop
# ports, state dir, Xvfb and APK build, so it can run beside other e2e scripts. Override with the E2E_* variables
# (see PL_DEFAULTS below). The emulator and the desktop service are started if they are not running yet.
# Results: dist/android/logs/pair-lifecycle-results.tsv   Screenshots: dist/screenshots/pairing/pair-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
REPEAT=3 BUILDARG=() SCEN=()
while (($#)); do
  case "$1" in
    --repeat) REPEAT="$2"; shift ;;
    --no-build) BUILDARG=(--no-build) ;;
    -h|--help) sed -n '2,19p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) SCEN+=("$1") ;;
  esac
  shift
done
[[ ${#SCEN[@]} -eq 0 ]] && SCEN=(A_qr A_code B_qr B_code C_qr C_code rename)
# PL_DEFAULTS: a private set of ports and a free AVD; export E2E_* before calling to change them.
: "${E2E_AVD:=gb36u}" "${E2E_EMU_PORT:=5624}" "${E2E_ADB_PORT:=16090}" "${E2E_TAG:=-pl}"
: "${E2E_RUN:=$ROOT/target/pl-run}" "${E2E_SCRATCH:=$ROOT/target/pl-android}" "${E2E_TARGET:=$ROOT/target/pl}"
: "${E2E_XDISP:=:93}" "${E2E_CTRL_PORT:=17591}" "${E2E_API_PORT:=17590}" "${E2E_DISC_PORT:=17546}" "${E2E_LISTEN_PORT:=17641}" "${E2E_REDIR_PORT:=17661}"
: "${E2E_RESULTS:=$ROOT/dist/android/logs/pair-lifecycle-results.tsv}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT E2E_RESULTS
# shellcheck disable=SC1091
source scripts/e2e-android.sh "${BUILDARG[@]}"
PL_SHOTS="$ROOT/dist/screenshots/pairing"; mkdir -p "$PL_SHOTS"
pshot() { $ADB exec-out screencap -p >"$PL_SHOTS/pair-$1.png" 2>/dev/null; }
STUB="$RUN/stub-bin"; NOTIFY_LOG="$RUN/notify.log"
DESK_NAME="${DESK_NAME:-E2E-Desktop}"

# ---------------------------------------------------------------- environment
start_desktop() { # NAME : (re)start the service with a stub notify-send and a QR address the emulator can reach
  mkdir -p "$STUB"; : >"$NOTIFY_LOG"
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s"\nexit 0\n' "$NOTIFY_LOG" >"$STUB/notify-send"; chmod +x "$STUB/notify-send"
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE DISPLAY="$XDISP" PATH="$STUB:$PATH" GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     GEEKBRIDGE_QR_ADDRESSES="10.0.2.2:$LISTEN_PORT" setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" \
     --control-port "$CTRL_PORT" --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback \
     --no-calling-probe --name "${1:-$DESK_NAME}" >>"$LOGS/e2e-service.log" 2>&1 &)
  wait_for 15 ctl status >/dev/null 2>&1
}
pl_env() {
  [[ -x "$SVC" && -f "$APK" ]] || { BUILD=yes; stage_env || return 1; BUILD=auto; }
  start_xvfb; start_emulator || return 1
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || return 1
  start_desktop; desk_id
}

# ---------------------------------------------------------------- phone helpers
scan() { $ADB shell am start -W -a android.intent.action.VIEW -d "'$1'" $PKG >/dev/null; }
finish_setup() { # mark the setup wizard finished so Home/Diagnostics open directly
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'f=shared_prefs/geekbridge_state.xml; [ -f \$f ] || { mkdir -p shared_prefs; printf \"<?xml version=\\\"1.0\\\" encoding=\\\"utf-8\\\" standalone=\\\"yes\\\" ?>\\n<map>\\n</map>\\n\" > \$f; }; grep -q setup_finished \$f || sed -i \"s#</map>#    <boolean name=\\\"setup_finished\\\" value=\\\"true\\\" />\\n</map>#\" \$f'"
}
phone_unpair() { # Diagnostics > Unpair <computer> > confirm; works with the PC unreachable
  goto_diag || return 1
  ui scrolltap "Unpair " >/dev/null || return 1; sleep 1
  ui tap Unpair >/dev/null; sleep 3; ui has "Unpaired on this phone"
}
phone_knows_computer() { goto_diag && { ui scrollto "Unpair" >/dev/null 2>&1; ui dump | cut -f1 | grep -q '^Unpair '; }; }
phone_paired_with_computer() { # Diagnostics lists the computer as paired
  goto_diag || return 1
  ui dump | cut -f1 | grep -qx "none" && return 1
  ui dump | cut -f1 | grep -qE '^Paired computers' && ! ui dump | awk -F'\t' '$1=="Paired computers"{f=1;next} f{print $1;exit}' | grep -qx none
}
pc_paired() { [[ -n "$(paired_ids)" ]]; }

# First pairing through a QR link (the setup every scenario starts from).
qr_pair() { # -> 0 when the phone reached "What to do next" and the PC lists the phone; prints seconds on stdout
  local t0 p; t0=$(date +%s); p="$(ctl pair-qr 2>/dev/null | head -1)"
  [[ "$p" == geekbridge://pair?* ]] || return 1
  scan "$p"
  ui wait "Connect to a computer" 20 >/dev/null || { pshot "$TAG-qr-no-confirm"; return 2; }
  ui tap Continue >/dev/null
  # The phone asks for approval ("Computer identity") unless it already trusts this exact certificate (B: only the
  # computer forgot), in which case it pairs straight away.
  local i seen=0 dump
  for ((i = 0; i < 40; i++)); do
    dump="$(ui dump | cut -f1)"
    grep -q "^What to do next" <<<"$dump" && break
    if grep -q "^Computer identity" <<<"$dump"; then
      ((seen)) || pshot "$TAG-qr-approve"; seen=1; ui tap Pair --exact >/dev/null
    fi
    sleep 1
  done
  grep -q "^What to do next" <<<"$dump" || { pshot "$TAG-qr-no-done"; return 3; }
  pshot "$TAG-qr-done"
  wait_for 10 pc_paired || return 5
  echo $(( $(date +%s) - t0 ))
}
# Pairing by comparing codes: the computer dials the phone and asks. Needs the phone's link to be running.
code_pair() { # same contract as qr_pair
  local t0 code id dcode; t0=$(date +%s)
  goto_diag; ensure_link || return 1
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1
  desk_dial; wait_for 12 has_unpaired_link || { pshot "$TAG-code-no-link"; return 2; }
  ctl pair "$PHONE_ID" request >/dev/null || return 3
  wait_for 20 ui code >/dev/null || { pshot "$TAG-code-no-prompt"; return 4; }
  pshot "$TAG-code-prompt"
  code="$(ui code)"; id="$(ctl status | jget 'd["challenges"][0]["challenge_id"]')"
  dcode="$(ctl status | jget 'd["challenges"][0]["verification_key"]')"
  [[ -n "$code" && "$code" == "$dcode" ]] || { echo "codes differ phone=$code pc=$dcode" >&2; return 5; }
  ctl approve "$id" "$code" >/dev/null || return 6
  ui tap Pair >/dev/null; sleep 4
  wait_for 15 pc_paired || { pshot "$TAG-code-not-paired"; return 7; }
  pshot "$TAG-code-done"
  echo $(( $(date +%s) - t0 ))
}
# Both sides must agree afterwards: PC lists the phone, the link comes up trusted and a ping gets an answer.
verify_paired() {
  desk_dial; wait_for 20 is_connected || { echo "no trusted link (phases=[$(link_phases)])" >&2; return 1; }
  ctl ping "$PHONE_ID" >/dev/null 2>&1 || { echo "ping failed" >&2; return 1; }
}
fresh_pair() { # wipe the phone, forget on the PC, pair once by QR, finish the wizard
  for id in $(paired_ids); do ctl unpair "$id" >/dev/null 2>&1; done
  $ADB shell pm clear $PKG >/dev/null; wake
  qr_pair >/dev/null || return 1
  PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
  finish_setup; goto_home
}

repeat_scenario() { # name function
  local name="$1" fn="$2" pass=0 fail=0 notes=""
  for ((n = 1; n <= REPEAT; n++)); do
    TAG="$name-$n"; log "== $name run $n/$REPEAT"
    local out; out="$($fn 2>&1)"; local rc=$?
    if ((rc == 0)); then pass=$((pass + 1)); notes+="[$n ok ${out##*$'\n'}s] "; else fail=$((fail + 1)); notes+="[$n FAIL rc=$rc: $(tr '\n' ' ' <<<"$out" | cut -c1-160)] "; fi
  done
  if ((fail == 0)); then rec "$name" PASS "$(date +%F) $pass/$REPEAT first-attempt: $notes"; else rec "$name" FAIL "$(date +%F) $pass/$REPEAT: $notes"; fi
}

# ---------------------------------------------------------------- A: the phone unpairs
repair_with() { # qr|code : final step shared by A, B and C
  local out rc
  if [[ "$1" == qr ]]; then out="$(qr_pair)"; rc=$?; else out="$(code_pair)"; rc=$?; fi
  ((rc == 0)) || { echo "re-pair by $1 failed on the first attempt (step $rc)"; return 1; }
  verify_paired || return 1
  echo "$out"
}
scn_A() { # $1 = qr|code
  fresh_pair || { echo "setup pairing failed"; return 1; }
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  phone_unpair || { echo "phone unpair failed"; return 1; }
  pc_paired || { echo "precondition: the PC should still trust the phone"; return 1; }
  pshot "$TAG-phone-unpaired"
  repair_with "$1"
}
scn_B() {
  fresh_pair || { echo "setup pairing failed"; return 1; }
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  ctl unpair "$PHONE_ID" >/dev/null || return 1
  pc_paired && { echo "precondition: PC should have forgotten the phone"; return 1; }
  repair_with "$1"
}
scn_C() {
  fresh_pair || { echo "setup pairing failed"; return 1; }
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  ctl unpair "$PHONE_ID" >/dev/null; phone_unpair || { echo "phone unpair failed"; return 1; }
  repair_with "$1"
}
s_A_qr() { scn_A qr; }
s_A_code() { scn_A code; }
s_B_qr() { scn_B qr; }
s_B_code() { scn_B code; }
s_C_qr() { scn_C qr; }
s_C_code() { scn_C code; }

# ---------------------------------------------------------------- D: rename (see rename_scenarios below)
# shellcheck disable=SC1091
[[ -f scripts/e2e/pair-rename.sh ]] && source scripts/e2e/pair-rename.sh

main_pl() {
  pl_env || { rec env FAIL "environment did not come up"; return 1; }
  for s in "${SCEN[@]}"; do
    case "$s" in
      rename) declare -F rename_scenario >/dev/null && rename_scenario || rec rename SKIP "pair-rename.sh missing" ;;
      *) repeat_scenario "$s" "s_$s" ;;
    esac
  done
  echo; echo "== results =="; column -t -s $'\t' "$RESULTS" | tail -n 20
  ! tail -n 20 "$RESULTS" | grep -q $'\tFAIL\t'
}
[[ "${PL_SOURCE_ONLY:-}" == 1 ]] && return 0
main_pl
