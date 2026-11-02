#!/usr/bin/env bash
# Real end-to-end for contacts, calendar and SMS: Android emulator app <-> desktop geekbridge-service
# <-> the real Agenda and Messages windows (under a private Xvfb), with data injected into the
# emulator's own providers.
#
#   scripts/e2e-phonedata.sh [STAGE[,STAGE...]] [--stop]
#
# Stages: env battery pair phonedata messages   (default: all, in this order; each is idempotent)
#
# It reuses the helpers of scripts/e2e-android.sh but shares nothing live with it: its own AVD
# (gb36d, console port 5598, adb server 16039), desktop state (target/e2e-pd-run), control port 17491,
# Xvfb :98 and cargo target (target/e2e-pd), so it can run next to that script's emulator.
# Create the AVD once by copying gb36 (see docs/phone-data-link.md, "Emulator run").
# Screenshots: dist/screenshots/phonedata/*.png   Results: dist/android/logs/e2e-phonedata-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-env,battery,pair,phonedata,messages}"
[[ "$STAGES" == --stop ]] && { STAGES=""; STOP=1; }
set --
export E2E_SOURCE_ONLY=1 E2E_AVD=gb36d E2E_EMU_PORT=5598 E2E_ADB_PORT=16039 E2E_TAG=-pd \
  E2E_RUN="$ROOT/target/e2e-pd-run" E2E_SCRATCH="$ROOT/target/e2e-pd-android" E2E_TARGET="$ROOT/target/e2e-pd" \
  E2E_RESULTS="$ROOT/dist/android/logs/e2e-phonedata-results.tsv" E2E_XDISP=:98 \
  E2E_CTRL_PORT=17491 E2E_API_PORT=17490 E2E_DISC_PORT=17426 E2E_LISTEN_PORT=1731 E2E_REDIR_PORT=17171
# shellcheck disable=SC1091
source scripts/e2e-android.sh
PD_SHOTS="$ROOT/dist/screenshots/phonedata"; mkdir -p "$PD_SHOTS"
BIN_APPS="$E2E_TARGET/release"

pdshot() { $ADB exec-out screencap -p > "$PD_SHOTS/android-$1.png" 2>/dev/null; }
now_ms() { date +%s%3N; }
sh_q() { $ADB shell "$1"; }   # one string, interpreted by the device shell

# Injected, made-up data (no real person): contacts, a calendar with events, SMS history.
inject_contacts() {
  local -a people=("Ada Lovelace|+44 20 7946 0001" "Grace Hopper|+1 555 010 0002" "Alan Turing|+44 20 7946 0003")
  for p in "${people[@]}"; do
    local name="${p%%|*}" num="${p##*|}"
    sh_q "content insert --uri content://com.android.contacts/raw_contacts --bind account_type:s:LOCAL --bind account_name:s:gbtest" >/dev/null
    local rid; rid="$(sh_q "content query --uri content://com.android.contacts/raw_contacts --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | sort -n | tail -1)"
    sh_q "content insert --uri content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/name --bind data1:s:'$name'" >/dev/null
    sh_q "content insert --uri content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/phone_v2 --bind data1:s:'$num' --bind data2:i:2" >/dev/null
  done
}
calendar_id() { sh_q "content query --uri content://com.android.calendar/calendars --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1; }
ensure_calendar() {
  local id; id="$(calendar_id)"; [[ -n "$id" ]] && { echo "$id"; return 0; }
  sh_q "content insert --uri 'content://com.android.calendar/calendars?caller_is_syncadapter=true&account_name=gbtest&account_type=LOCAL' --bind account_name:s:gbtest --bind account_type:s:LOCAL --bind name:s:Personal --bind calendar_displayName:s:Personal --bind calendar_color:i:-16776961 --bind calendar_access_level:i:700 --bind visible:i:1 --bind sync_events:i:1 --bind ownerAccount:s:gbtest --bind calendar_timezone:s:UTC" >/dev/null
  calendar_id
}
inject_events() {
  local cal="$1" base; base=$(( $(now_ms) / 3600000 * 3600000 ))
  local day=$(( base / 86400000 * 86400000 ))
  add_event() { sh_q "content insert --uri content://com.android.calendar/events --bind calendar_id:i:$cal --bind title:s:'$1' --bind dtstart:l:$2 --bind dtend:l:$3 --bind eventTimezone:s:UTC --bind allDay:i:${4:-0} ${5:+--bind eventLocation:s:'$5'}" >/dev/null; }
  add_event "Standup" $(( base + 3600000 )) $(( base + 5400000 )) 0 "Room 4"
  add_event "Design review" $(( base + 3 * 3600000 )) $(( base + 4 * 3600000 )) 0
  add_event "Team offsite" "$day" $(( day + 86400000 )) 1
  add_event "Dentist" $(( base + 26 * 3600000 )) $(( base + 27 * 3600000 )) 0 "Main St 4"
}
write_prefs() { # enable the three features in the app (the switches live in app preferences)
  local ids="$1"
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="sms" value="true" />
    <set name="calendar_ids"><string>$ids</string></set>
</map>
XML
}
grant_all() { for p in READ_CONTACTS READ_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS; do $ADB shell pm grant $PKG android.permission.$p; done; }

stage_phonedata() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec phonedata SKIP "not paired (run pair first)"; return 1; }
  is_connected || { desk_dial; wait_for 15 is_connected; } || { rec phonedata FAIL "link not connected"; return 1; }
  wake
  grant_all
  inject_contacts
  local cal; cal="$(ensure_calendar)"; [[ -n "$cal" ]] || { rec phonedata FAIL "could not create a calendar on the emulator"; return 1; }
  inject_events "$cal"
  write_prefs "$cal"
  # The transport restarts with the new capabilities: start the app, bring the link up and redial.
  goto_home; sleep 2; ensure_link || true
  desk_dial; wait_for 20 is_connected || { rec phonedata FAIL "link did not come back"; return 1; }
  pdshot 01-home
  local caps; caps="$(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  log "negotiated: $caps"
  for f in contacts calendar sms; do ctl consent "$PHONE_ID" $f on >/dev/null; done
  local ok_c=0 ok_e=0
  wait_for 30 bash -c "[[ \$(\"${CTL[0]}\" --state-dir \"$RUN/state\" --control-port $CTRL_PORT agenda | python3 -c 'import json,sys;print(len(json.load(sys.stdin)[\"people\"]))') -ge 3 ]]" && ok_c=1
  wait_for 30 bash -c "[[ \$(\"${CTL[0]}\" --state-dir \"$RUN/state\" --control-port $CTRL_PORT agenda | python3 -c 'import json,sys;print(len(json.load(sys.stdin)[\"events\"]))') -ge 2 ]]" && ok_e=1
  ctl agenda > "$RUN/agenda.json"
  rec phonedata_agenda "$((ok_c && ok_e ? 1 : 0))" "contacts=$ok_c events=$ok_e negotiated=$caps"
}

# Messages end to end: an SMS injected with `adb emu sms send` must reach the desktop conversation
# list (unread, sender), be searchable and be marked read locally; a send from the desktop must be
# accepted by the phone (its own status packets, never "delivered" on a guess).
stage_messages() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec messages SKIP "not paired (run pair first)"; return 1; }
  is_connected || { desk_dial; wait_for 15 is_connected; } || { rec messages FAIL "link not connected"; return 1; }
  wake; grant_all
  ctl consent "$PHONE_ID" sms on >/dev/null
  local tag="gbmsg$RANDOM"
  $ADB emu sms send 5550142 "Hello from the emulator $tag" >/dev/null
  local seen=0
  for _ in $(seq 40); do
    ctl messages "$PHONE_ID" > "$RUN/messages.json" 2>/dev/null
    jget 'any("'"$tag"'" in c["last_body"] and c["unread"]>=1 for c in d["conversations"])' < "$RUN/messages.json" | grep -q True && { seen=1; break; }
    sleep 2
  done
  rec messages_incoming "$seen" "injected text listed with unread count"
  [[ $seen == 1 ]] || return 1
  local tid; tid="$(jget '[c["thread_id"] for c in d["conversations"] if "'"$tag"'" in c["last_body"]][0]' < "$RUN/messages.json")"
  ctl messages "$PHONE_ID" "$tid" | jget 'any("'"$tag"'" in m["body"] for m in d["selected"]["messages"])' | grep -q True \
    && rec messages_thread 1 "thread $tid shows the message" || rec messages_thread 0 "thread $tid missing the message"
  ctl messages-send "$PHONE_ID" 5550142 "Reply from the desktop $tag" >/dev/null
  local phase=""
  for _ in $(seq 30); do
    phase="$(ctl messages "$PHONE_ID" | jget '[s["phase"] for s in d["sends"] if "'"$tag"'" in s["text"]][0]')"
    case "$phase" in accepted_by_phone|sent|delivered|failed) break;; esac; sleep 2
  done
  case "$phase" in accepted_by_phone|sent|delivered) rec messages_send 1 "phone reported: $phase";; *) rec messages_send 0 "phase=$phase";; esac
  [[ "$phase" == queued || "$phase" == handed_to_phone ]] && rec messages_honest 0 "stuck at $phase" || rec messages_honest 1 "never claimed more than the phone said"
  $ADB exec-out screencap -p > "$PD_SHOTS/android-messages.png" 2>/dev/null
  ( cd "$ROOT" && DISPLAY=$XDISP GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CTRL_PORT \
      GEEKBRIDGE_MESSAGE_TO=5550142 timeout 8 "$BIN_APPS/geekbridge-messages" --api-port $API_PORT --theme light >/dev/null 2>&1 & sleep 5; \
      DISPLAY=$XDISP import -window root "$PD_SHOTS/desktop-messages.png" 2>/dev/null; wait ) || true
}

main_pd() {
  if [[ "${STOP:-}" == 1 ]]; then
    pkill -f "$E2E_TARGET/release/geekbridge" 2>/dev/null; $ADB emu kill >/dev/null 2>&1
    systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; pkill -f "Xvfb $XDISP" 2>/dev/null; return 0
  fi
  IFS=, read -r -a st <<<"$STAGES"
  for s in "${st[@]}"; do log "== stage $s"; "stage_$s" || log "stage $s reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 20
}
main_pd
