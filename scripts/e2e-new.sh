#!/usr/bin/env bash
# End-to-end for the newer features on the emulator: pairing failure hints, call answer/reject over the link,
# incoming-SMS desktop notification, calendar events in the desktop agenda, Diagnostics "Copy report" privacy.
#
#   scripts/e2e-new.sh [STAGE[,STAGE...]]      stages: restart hints pairdone calls sms agenda report   (default: all)
#
# Reuses scripts/e2e-android.sh helpers. Isolation comes from the E2E_* variables (defaults below are this script's
# own ports/AVD so it can run beside the other e2e scripts; override them to reuse another set). Needs the
# environment `scripts/e2e-android.sh --only env` prepares. The desktop service is restarted with a stub
# `notify-send` first on PATH so desktop notifications are captured in $RUN/notify.log instead of shown.
# Results: dist/android/logs/e2e-new-results.tsv   Screenshots: dist/screenshots/android/new-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-restart,hints,pairdone,calls,sms,agenda,report}"
set --
: "${E2E_AVD:=gb36q}" "${E2E_EMU_PORT:=5614}" "${E2E_ADB_PORT:=16050}" "${E2E_TAG:=-full}"
: "${E2E_RUN:=$ROOT/target/e2e-full-run}" "${E2E_SCRATCH:=$ROOT/target/e2e-full-android}" "${E2E_TARGET:=$ROOT/target/e2e-full}"
: "${E2E_XDISP:=:99}" "${E2E_CTRL_PORT:=17391}" "${E2E_API_PORT:=17390}" "${E2E_DISC_PORT:=17446}" "${E2E_LISTEN_PORT:=1735}" "${E2E_REDIR_PORT:=17361}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-new-results.tsv"
# shellcheck disable=SC1091
source scripts/e2e-android.sh
NEW_SHOTS="$SHOTS"; nshot() { $ADB exec-out screencap -p > "$NEW_SHOTS/new-$1.png" 2>/dev/null; }
STUB="$RUN/stub-bin"; NOTIFY_LOG="$RUN/notify.log"
sh_q() { $ADB shell "$1"; }

stage_restart() {
  mkdir -p "$STUB"; : >"$NOTIFY_LOG"
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s"\nexit 0\n' "$NOTIFY_LOG" >"$STUB/notify-send"; chmod +x "$STUB/notify-send"
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE DISPLAY="$XDISP" PATH="$STUB:$PATH" GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     GEEKBRIDGE_QR_ADDRESSES="10.0.2.2:$LISTEN_PORT" setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" \
     --control-port "$CTRL_PORT" --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback \
     --no-calling-probe --name E2E-Desktop >>"$LOGS/e2e-service.log" 2>&1 &)
  wait_for 15 ctl status >/dev/null 2>&1 && rec restart PASS "service up with stub notify-send" || rec restart FAIL "service did not start"
}

# ---------------------------------------------------------------- failure hints (Test connection)
qr_with_addr() { ctl pair-qr 2>/dev/null | sed -E "s/([?&])a=[^&]*/\1a=$1/"; }
scan() { $ADB shell am start -W -a android.intent.action.VIEW -d "'$1'" $PKG >/dev/null; }
hint_case() { # name address expected-text
  local name="$1" addr="$2" want="$3" p
  p="$(qr_with_addr "$addr")"; [[ "$p" == geekbridge://* ]] || { rec "hint_$name" FAIL "no QR payload"; return; }
  scan "$p"; ui wait "Pair with" 15 >/dev/null; ui tap "Continue" >/dev/null
  if ! ui wait "Test connection" 60; then nshot "hint-$name-nofail"; rec "hint_$name" FAIL "no failure screen with a Test connection button: $(ui dump | cut -f1 | head -6 | tr '\n' '|')"; return; fi
  nshot "hint-$name-failed"
  local head; head="$(ui dump | cut -f1 | grep -iE "did not answer|different network|not accept|could not|did not work|closed" | head -1)"
  ui tap "Test connection" >/dev/null; sleep 12; nshot "hint-$name-report"
  if ui has "$want"; then rec "hint_$name" PASS "failure: '$head'; Test connection said: $want"
  else rec "hint_$name" FAIL "failure: '$head'; Test connection did not say '$want': $(ui dump | cut -f1 | grep -E '✔|✖|^•|reachable|answer|network' | head -5 | tr '\n' '|')"; fi
  ui tap "Close" >/dev/null 2>&1; $ADB shell input keyevent KEYCODE_BACK
}
stage_hints() {
  for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
  $ADB shell pm clear $PKG >/dev/null; wake
  hint_case refused "10.0.2.2:1799" "not running there"
  hint_case noanswer "10.0.2.77:1735" "answer"
}

# ---------------------------------------------------------------- pairing with a fresh QR (leaves the phone paired)
stage_pairdone() {
  for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
  $ADB shell pm clear $PKG >/dev/null; wake
  local p; p="$(ctl pair-qr 2>/dev/null)"; scan "$p"
  ui wait "Pair with E2E-Desktop" 15 >/dev/null; ui tap Continue >/dev/null
  ui wait "Computer identity" 40 >/dev/null; ui tap Pair --exact >/dev/null
  if ui wait "What to do next" 40; then PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    rec pairdone PASS "paired by QR link: $PHONE_ID"
  else rec pairdone FAIL "no next-steps card"; return 1; fi
  # The phone listens for the desktop's dial: finish the setup wizard quickly (Skip all) and go home.
  tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
  goto_home; for _ in $(seq 1 14); do ui has "Step 1 of" || ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  desk_dial; wait_for 20 is_connected && rec link_after_qr PASS "desktop redial connected" || rec link_after_qr FAIL "no link after QR pairing"
}

fresh_link() { load_ids; desk_dial >/dev/null 2>&1; wait_for 20 is_connected; }
write_calls_prefs() {
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
</map>
XML
}
modem_state() { $ADB shell dumpsys telephony.registry | grep -m1 -o "mCallState=[0-9]" | cut -d= -f2; }
calls_json() { ctl calls --reveal 2>/dev/null; }

stage_calls() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec calls SKIP "not paired"; return 1; }
  for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS; do $ADB shell pm grant $PKG android.permission.$p; done
  write_calls_prefs; goto_home; sleep 3; fresh_link || { rec calls FAIL "link not connected"; return 1; }
  ctl consent "$PHONE_ID" telephony on >/dev/null; sleep 3
  ctl status | jget '",".join(d["links"][0]["negotiated"])' | grep -q geekbridge.calls || { rec calls FAIL "calls not negotiated: $(ctl status | jget '",".join(d["links"][0]["negotiated"])')"; return 1; }
  : >"$NOTIFY_LOG"
  $ADB emu gsm call 5558802 >/dev/null; sleep 4; nshot call-ringing
  local seen; seen="$(calls_json | tr -d '\n ' | grep -io 'ringing' | head -1)"
  [[ -n "$seen" ]] && rec call_ring_visible PASS "desktop shows the call ringing (ctl calls)" || rec call_ring_visible FAIL "desktop did not show a ringing call: $(calls_json | head -c 300)"
  grep -qiE "call|ringing|5558802" "$NOTIFY_LOG" && rec call_notification PASS "desktop notification: $(head -c 100 "$NOTIFY_LOG" | tr '\n' ' ' | sed 's/5558802/<number>/')" || rec call_notification FAIL "no desktop notification (stub log empty)"
  local out; out="$(timeout 20 "${CTL[@]}" call reject 2>&1)"; sleep 4
  if [[ "$(modem_state)" == 0 ]]; then rec call_reject PASS "ctl call reject -> phone idle ($out)"; else rec call_reject FAIL "modem state $(modem_state) after reject: $out"; fi
  $ADB emu gsm cancel 5558802 >/dev/null 2>&1; sleep 2
  $ADB emu gsm call 5558803 >/dev/null; sleep 4
  out="$(timeout 20 "${CTL[@]}" call answer 2>&1)"; sleep 4; nshot call-answered
  if [[ "$(modem_state)" == 2 ]]; then rec call_answer PASS "ctl call answer -> phone off-hook ($out)"; else rec call_answer FAIL "modem state $(modem_state) after answer: $out"; fi
  out="$(timeout 20 "${CTL[@]}" call hangup 2>&1)"; sleep 4
  if [[ "$(modem_state)" == 0 ]]; then rec call_hangup PASS "ctl call hangup -> phone idle"; else rec call_hangup FAIL "modem state $(modem_state): $out"; fi
  $ADB emu gsm cancel 5558803 >/dev/null 2>&1
  # Without the opt-in on the desktop the phone must not be controllable.
  ctl consent "$PHONE_ID" telephony off >/dev/null; $ADB emu gsm call 5558804 >/dev/null; sleep 4
  out="$(timeout 20 "${CTL[@]}" call answer 2>&1)"; sleep 2
  if [[ "$(modem_state)" == 1 ]]; then rec call_consent_off PASS "no consent: answer refused, still ringing ($(echo "$out" | head -c 90))"; else rec call_consent_off FAIL "modem state $(modem_state) without consent: $out"; fi
  $ADB emu gsm cancel 5558804 >/dev/null 2>&1; ctl consent "$PHONE_ID" telephony on >/dev/null
}

# ---------------------------------------------------------------- phone data prefs shared by sms + agenda
pd_prefs() { # calendar id
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="sms" value="true" />
    <set name="calendar_ids"><string>$1</string></set>
</map>
XML
}
now_ms() { date +%s%3N; }
sms_ready=0
pd_setup() {
  [[ "$sms_ready" == 1 ]] && return 0
  load_ids
  for p in READ_CONTACTS READ_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS; do $ADB shell pm grant $PKG android.permission.$p; done
  local cal; cal="$(ensure_calendar_new)"; [[ -n "$cal" ]] || return 1
  echo "$cal" >"$RUN/cal-id"
  pd_prefs "$cal"; goto_home; sleep 3; fresh_link || return 1
  for f in contacts calendar sms; do ctl consent "$PHONE_ID" $f on >/dev/null; done; sleep 3
  sms_ready=1
}
ensure_calendar_new() {
  local id; id="$(sh_q "content query --uri content://com.android.calendar/calendars --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  [[ -n "$id" ]] && { echo "$id"; return; }
  sh_q "content insert --uri 'content://com.android.calendar/calendars?caller_is_syncadapter=true&account_name=gbtest&account_type=LOCAL' --bind account_name:s:gbtest --bind account_type:s:LOCAL --bind name:s:Personal --bind calendar_displayName:s:Personal --bind calendar_color:i:-16776961 --bind calendar_access_level:i:700 --bind visible:i:1 --bind sync_events:i:1 --bind ownerAccount:s:gbtest --bind calendar_timezone:s:UTC" >/dev/null
  sh_q "content query --uri content://com.android.calendar/calendars --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1
}

stage_sms() {
  pd_setup || { rec sms FAIL "could not set up phone data / link"; return 1; }
  ctl messages-refresh "$PHONE_ID" >/dev/null 2>&1; sleep 8   # first sync establishes "what is new"
  : >"$NOTIFY_LOG"
  local body="e2e-sms-$(date +%s)"
  $ADB emu sms send 5551234 "$body" >/dev/null; sleep 12; nshot sms-sent
  local view; view="$(ctl messages 2>/dev/null)"
  if grep -qi "New message" "$NOTIFY_LOG"; then rec sms_notification PASS "desktop notification raised: $(head -c 80 "$NOTIFY_LOG" | tr '\n' ' ' | sed 's/5551234/<number>/') (body shown: $(grep -c "$body" "$NOTIFY_LOG"))"
  else rec sms_notification FAIL "no 'New message' notification within 12 s; stub log: $(head -c 120 "$NOTIFY_LOG")"; fi
  ctl messages-refresh "$PHONE_ID" >/dev/null 2>&1; sleep 6; view="$(ctl messages 2>/dev/null)"
  if grep -q "$body" <<<"$view"; then rec sms_in_messages PASS "message text appears in the desktop Messages view"; else rec sms_in_messages FAIL "text not in ctl messages: $(head -c 200 <<<"$view")"; fi
}

stage_agenda() {
  pd_setup || { rec agenda FAIL "could not set up phone data / link"; return 1; }
  local cal base title="E2E Standup $(date +%H%M%S)"; cal="$(cat "$RUN/cal-id")"; base=$(( $(now_ms) / 3600000 * 3600000 ))
  sh_q "content insert --uri content://com.android.calendar/events --bind calendar_id:i:$cal --bind title:s:'$title' --bind dtstart:l:$(( base + 3600000 )) --bind dtend:l:$(( base + 5400000 )) --bind eventTimezone:s:UTC --bind allDay:i:0 --bind eventLocation:s:'Room 4'" >/dev/null
  ctl agenda-refresh "$PHONE_ID" >/dev/null 2>&1; local ok=0
  for _ in $(seq 1 30); do ctl agenda 2>/dev/null | grep -q "$title" && { ok=1; break; }; sleep 2; ctl agenda-refresh "$PHONE_ID" >/dev/null 2>&1; done
  ctl agenda >"$RUN/agenda.json" 2>/dev/null
  if ((ok)); then rec agenda PASS "calendar insert on the phone shows in the desktop agenda ('$title')"; else rec agenda FAIL "event '$title' never reached ctl agenda; negotiated: $(ctl status | jget '",".join(d["links"][0]["negotiated"])')"; fi
  [[ "$(ctl agenda 2>/dev/null | grep -c '"people"')" -ge 1 ]] && rec contacts_agenda PASS "contacts list present in agenda view" || true
}

# ---------------------------------------------------------------- Diagnostics report: no personal data
stage_report() {
  load_ids; $ADB shell am force-stop $PKG; sleep 1
  # Mark the setup wizard as finished so Home opens (the wizard is not what this stage tests).
  $ADB shell "run-as $PKG sh -c \"sed -i 's/<boolean name=\\\"setup_finished\\\" value=\\\"false\\\" \\/>//; s/<\\/map>/<boolean name=\\\"setup_finished\\\" value=\\\"true\\\" \\/><\\/map>/' shared_prefs/geekbridge_state.xml\""
  goto_diag || { rec report FAIL "cannot open Diagnostics"; return 1; }
  ui scrollto "Copy report" >/dev/null && tap "Copy report"; sleep 1
  ui scrollto Paste >/dev/null && tap Paste; sleep 2; nshot report-pasted
  local txt; txt="$(ui dump | cut -f1)"; echo "$txt" >"$RUN/report-screen.txt"
  grep -q "GeekBridge Android support report" <<<"$txt" || { rec report FAIL "report text not found on the phone clipboard view"; return 1; }
  local bad=""
  for needle in "E2E-Desktop" "$PHONE_ID" "$DESK_ID" "10.0.2." "127.0.0.1" "Ada Lovelace" "Grace Hopper" "5558802" "5551234" "e2e-sms" "E2E Standup" "e2e-p2d" "e2e-d2p"; do
    [[ -n "$needle" ]] && grep -qF "$needle" <<<"$txt" && bad="$bad [$needle]"
  done
  grep -qE '[0-9a-f]{32}|[0-9a-f]{40,}' <<<"$txt" && bad="$bad [long hex id]"
  grep -qE '[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}' <<<"$txt" && bad="$bad [ip-like]"
  if [[ -z "$bad" ]]; then rec report PASS "Copy report text holds versions/permissions only; none of: computer name, device ids, addresses, contact/sms/calendar/clipboard text"; else rec report FAIL "personal data in report:$bad"; fi
}

main_new() {
  IFS=, read -r -a st <<<"$STAGES"
  for s in "${st[@]}"; do log "== stage $s"; "stage_$s" || log "stage $s reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 30
}
main_new
