#!/usr/bin/env bash
# Real end-to-end for the Contacts and Calendar apps: Android emulator <-> desktop geekbridge-service <->
# the real desktop windows (private Xvfb). Contacts and calendar events are injected into the emulator's
# own providers with `content insert`; an event is then CREATED from the desktop and looked up again in the
# emulator's Calendar Provider with `content query` (the check that the write really landed), then deleted.
#
#   scripts/e2e-calendar.sh [STAGE[,STAGE...]] [--stop]
#
# Stages: env battery pair calendar   (default: all; each is idempotent)
# Own AVD (gb36c, a copy of gb36d), console port 5600, adb server 16043, desktop state target/e2e-cal-run, Xvfb :98/:96.
# Screenshots: dist/screenshots/desktop/calendar-*.png, contacts-*.png; dist/screenshots/android/calendar-*.png
# Results: dist/android/logs/e2e-calendar-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-env,battery,pair,calendar}"
[[ "$STAGES" == --stop ]] && { STAGES=""; STOP=1; }
set --
export E2E_SOURCE_ONLY=1 E2E_AVD=gb36c E2E_EMU_PORT=5600 E2E_ADB_PORT=16043 E2E_TAG=-cal \
  E2E_RUN="$ROOT/target/e2e-cal-run" E2E_SCRATCH="$ROOT/target/e2e-cal-android" E2E_TARGET="$ROOT/target/e2e-pd" \
  E2E_RESULTS="$ROOT/dist/android/logs/e2e-calendar-results.tsv" E2E_XDISP=:94 \
  E2E_CTRL_PORT=17691 E2E_API_PORT=17690 E2E_DISC_PORT=17636 E2E_LISTEN_PORT=1757 E2E_REDIR_PORT=17371
# shellcheck disable=SC1091
source scripts/e2e-android.sh
BUILD="${E2E_BUILD:-yes}"   # always rebuild the desktop service/ctl and the APK: they are what is under test
DESK_SHOTS="$ROOT/dist/screenshots/desktop"; AND_SHOTS="$ROOT/dist/screenshots/android"
mkdir -p "$DESK_SHOTS" "$AND_SHOTS"
BIN="$E2E_TARGET/release"
sh_q() { $ADB shell "$1"; }
andshot() { $ADB exec-out screencap -p > "$AND_SHOTS/calendar-$1.png" 2>/dev/null; }
now_ms() { date +%s%3N; }
pcheck() { if [[ "$2" == 1 ]]; then rec "$1" PASS "${3:-}"; else rec "$1" FAIL "${3:-}"; fi; }

provider_insert() { sh_q "content insert --uri $1 ${*:2}" >/dev/null; }
add_calendar() { # name account type access color -> id
  provider_insert "'content://com.android.calendar/calendars?caller_is_syncadapter=true&account_name=$2&account_type=$3'" \
    --bind account_name:s:$2 --bind account_type:s:$3 --bind name:s:"$1" --bind calendar_displayName:s:"$1" \
    --bind calendar_color:i:$5 --bind calendar_access_level:i:$4 --bind visible:i:1 --bind sync_events:i:1 \
    --bind ownerAccount:s:$2 --bind calendar_timezone:s:UTC
  sh_q "content query --uri content://com.android.calendar/calendars --projection _id:calendar_displayName" \
    | sed -n "s/.*_id=\([0-9]*\), calendar_displayName=$1\$/\1/p" | head -1
}
add_event() { # cal title start end allday [location] [rrule]
  provider_insert content://com.android.calendar/events --bind calendar_id:i:$1 --bind title:s:"'$2'" \
    --bind dtstart:l:$3 --bind dtend:l:$4 --bind eventTimezone:s:UTC --bind allDay:i:$5 ${6:+--bind eventLocation:s:"'$6'"}
}
last_event_id() { sh_q "content query --uri content://com.android.calendar/events --projection _id --sort '_id DESC'" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1; }
grant() { for p in READ_CONTACTS READ_CALENDAR WRITE_CALENDAR; do $ADB shell pm grant $PKG android.permission.$p; done; }
write_prefs() { # calendar ids (space separated), edit on
  $ADB shell am force-stop $PKG; sleep 1
  local ids=""; for i in $1; do ids+="<string>$i</string>"; done
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="calendar_edit" value="true" />
    <boolean name="sms" value="false" />
    <set name="calendar_ids">$ids</set>
</map>
XML
}
# Seeding must be repeatable: every run starts from an empty provider for the test accounts, otherwise each re-run
# stacks another copy of every event/contact and the screenshots show the same entry several times.
reset_seed() {
  provider_insert_del() { sh_q "content delete --uri $1 ${*:2}" >/dev/null 2>&1; }
  provider_insert_del "'content://com.android.calendar/calendars?caller_is_syncadapter=true&account_name=demo@gmail.com&account_type=com.google'"
  provider_insert_del "'content://com.android.contacts/raw_contacts?caller_is_syncadapter=true'" --where "\"account_name='gbtest'\""
}
# Prints the number of provider events that repeat another one's (calendar, title, start, end): 0 for a clean seed.
seed_duplicates() {
  sh_q "content query --uri content://com.android.calendar/events --projection calendar_id:title:dtstart:dtend:original_id" \
    | tr -d '\r' | sed 's/^Row: [0-9]* //; s/_id=[0-9]*, //' | grep -v 'original_id=[0-9]' | sort | uniq -d | wc -l
}
inject_contacts() {
  local -a people=("Ada Lovelace|+44 20 7946 0001|ada@example.org" "Grace Hopper|+1 555 010 0002|grace@example.org" "Alan Turing|+44 20 7946 0003|" "Katherine Johnson|+1 555 010 0004|kj@example.org" "Margaret Hamilton|+1 555 010 0005|")
  for p in "${people[@]}"; do
    IFS='|' read -r name num mail <<<"$p"
    provider_insert content://com.android.contacts/raw_contacts --bind account_type:s:LOCAL --bind account_name:s:gbtest
    local rid; rid="$(sh_q "content query --uri content://com.android.contacts/raw_contacts --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | sort -n | tail -1)"
    provider_insert content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/name --bind data1:s:"'$name'"
    provider_insert content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/phone_v2 --bind data1:s:"'$num'" --bind data2:i:2
    [[ -n "$mail" ]] && provider_insert content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/email_v2 --bind data1:s:"$mail" --bind data2:i:1
  done
}
desk_shot() { # name theme app
  local theme="$1" app="$2" name="$3"
  env -u WAYLAND_DISPLAY DISPLAY=:96 GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT="$CTRL_PORT" \
    "$BIN/$app" --api-port "$API_PORT" --theme "$theme" >/dev/null 2>&1 &
  local pid=$!; sleep 6
  DISPLAY=:96 import -window root "$DESK_SHOTS/$name-$theme.png" 2>/dev/null
  kill $pid 2>/dev/null; wait $pid 2>/dev/null; true
}

stage_calendar() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec calendar SKIP "not paired (run pair first)"; return 1; }
  CARGO_TARGET_DIR="$E2E_TARGET" cargo build --release -p bridge-apps --bins >>"$LOGS/e2e-build-desktop.log" 2>&1
  wake; grant
  reset_seed
  inject_contacts
  local personal holidays base day
  personal="$(add_calendar Personal demo@gmail.com com.google 700 -14575885)"
  holidays="$(add_calendar Holidays demo@gmail.com com.google 200 -12627531)"
  [[ -n "$personal" && -n "$holidays" ]] || { rec calendar FAIL "could not create calendars ($personal/$holidays)"; return 1; }
  base=$(( $(now_ms) / 3600000 * 3600000 )); day=$(( base / 86400000 * 86400000 ))
  add_event "$personal" Standup $(( base + 3600000 )) $(( base + 5400000 )) 0 "Room 4"
  add_event "$personal" "Design review" $(( base + 3 * 3600000 )) $(( base + 4 * 3600000 )) 0
  add_event "$personal" "Team offsite" "$day" $(( day + 86400000 )) 1
  add_event "$personal" Dentist $(( base + 26 * 3600000 )) $(( base + 27 * 3600000 )) 0 "Main St 4"
  local rid; rid="$(last_event_id)"
  provider_insert content://com.android.calendar/reminders --bind event_id:i:$rid --bind minutes:i:30 --bind method:i:1
  add_event "$holidays" "Public holiday" $(( day + 2 * 86400000 )) $(( day + 3 * 86400000 )) 1
  # A weekly series (RRULE + duration, no dtend) to prove recurring instances are expanded and not editable.
  provider_insert content://com.android.calendar/events --bind calendar_id:i:$personal --bind title:s:"'Weekly sync'" \
    --bind dtstart:l:$(( base + 2 * 3600000 )) --bind duration:s:PT1H --bind "rrule:s:'FREQ=DAILY;COUNT=5'" --bind eventTimezone:s:UTC --bind allDay:i:0
  pcheck calendar_seed_unique "$(( $(seed_duplicates) == 0 ))" "no event is stored twice in the provider after seeding"
  write_prefs "$personal $holidays"
  goto_home; sleep 2; ensure_link || true
  desk_dial; wait_for 25 is_connected || { rec calendar FAIL "link did not come back"; return 1; }
  local caps; caps="$(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  [[ "$caps" == *geekbridge.calendar.write* ]]; pcheck calendar_caps $(( $? == 0 )) "write capability negotiated=$([[ "$caps" == *geekbridge.calendar.write* ]] && echo yes || echo no)"
  for f in contacts calendar calendar_edit calendar_alerts; do ctl consent "$PHONE_ID" $f on >/dev/null; done
  local n=0
  wait_for 40 bash -c "[[ \$(\"${CTL[0]}\" --state-dir \"$RUN/state\" --control-port $CTRL_PORT agenda '' $(date -u +%F) 14 | python3 -c 'import json,sys;d=json.load(sys.stdin);print(len(d[\"events\"]) if len(d[\"calendars\"])==2 else 0)') -ge 8 ]]" && n=1
  ctl agenda "" "$(date -u +%F)" 14 > "$RUN/agenda.json"
  pcheck calendar_read "$n" "$(jget 'str(len(d["calendars"]))+" calendars, "+str(len(d["events"]))+" events, "+str(len(d["people"]))+" people"' < "$RUN/agenda.json")"
  local meta; meta="$(python3 - "$RUN/agenda.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
ev={e["title"]:e for e in d["events"]}
ok = ev["Dentist"]["reminders_min"]==[30] and any(e["recurring"] for e in d["events"] if e["title"]=="Weekly sync") \
  and sum(1 for e in d["events"] if e["title"]=="Weekly sync")>=2 and any(c["account"]=="demo@gmail.com" and c["account_type"]=="com.google" for c in d["calendars"])
print(1 if ok else 0)
PY
)"
  pcheck calendar_meta "$meta" "reminders, recurring instances expanded, Google account name"
  andshot 01-share-settings
  # --- desktop creates an event; it must land in the emulator's provider
  local start=$(( $(now_ms) + 3 * 86400000 )); start=$(( start / 60000 * 60000 ))
  ctl calendar-add "$PHONE_ID" "$personal" "Desktop lunch" "$start" 60 "Cafe Lovelace" 2>"$RUN/op.txt" >/dev/null
  local landed=0
  wait_for 30 bash -c "$ADB shell \"content query --uri content://com.android.calendar/events --projection title:calendar_id:dirty:deleted --where \\\"title='Desktop lunch'\\\"\" | grep -q 'calendar_id=$personal'" && landed=1
  pcheck calendar_create_lands "$landed" "$(sh_q "content query --uri content://com.android.calendar/events --projection title:calendar_id:dtstart:eventLocation:dirty --where \"title='Desktop lunch'\"" | tr -d '\r' | head -1)"
  local applied=0
  wait_for 30 bash -c "[[ \$(\"${CTL[0]}\" --state-dir \"$RUN/state\" --control-port $CTRL_PORT agenda '' $(date -u +%F) 14 | python3 -c 'import json,sys;d=json.load(sys.stdin);print(int(any(e[\"phase\"]==\"applied\" for e in d[\"edits\"]) and any(e[\"title\"]==\"Desktop lunch\" for e in d[\"events\"])))') == 1 ]]" && applied=1
  pcheck calendar_create_confirmed "$applied" "phone confirmed and the event is back in the desktop view"
  # read-only calendar must be refused
  ctl calendar-add "$PHONE_ID" "$holidays" "Nope" "$start" 30 >/dev/null 2>&1; local refused=$?
  local leaked; leaked="$(sh_q "content query --uri content://com.android.calendar/events --projection title --where \"title='Nope'\"" | grep -c title=Nope)"
  pcheck calendar_readonly_refused $(( refused != 0 && leaked == 0 )) "exit=$refused leaked=$leaked"
  # --- screenshots of the real windows
  pgrep -f "Xvfb :96" >/dev/null || { (setsid nohup Xvfb :96 -screen 0 1280x800x24 >"$RUN/xvfb96.log" 2>&1 &); sleep 1; }
  for t in light dark; do desk_shot "$t" geekbridge-agenda calendar; desk_shot "$t" geekbridge-contacts contacts; done
  andshot 02-after-edit
  # --- delete the event again, from the desktop
  local eid; eid="$(sh_q "content query --uri content://com.android.calendar/events --projection _id --where \"title='Desktop lunch'\"" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  ctl calendar-delete "$PHONE_ID" "$eid" >/dev/null 2>&1
  local gone=0
  wait_for 30 bash -c "[[ \$($ADB shell \"content query --uri content://com.android.calendar/events --projection _id --where \\\"_id=$eid\\\"\" | grep -c _id=) == 0 ]]" && gone=1
  pcheck calendar_delete_lands "$gone" "event $eid removed from the provider"
}

main_cal() {
  if [[ "${STOP:-}" == 1 ]]; then
    pkill -f "$E2E_TARGET/release/geekbridge" 2>/dev/null; $ADB emu kill >/dev/null 2>&1
    systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; pkill -f "Xvfb :9[68]" 2>/dev/null; return 0
  fi
  IFS=, read -r -a st <<<"$STAGES"
  for s in "${st[@]}"; do log "== stage $s"; "stage_$s" || log "stage $s reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 20
}
main_cal
