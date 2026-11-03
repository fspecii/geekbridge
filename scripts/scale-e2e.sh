#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Phone-data scale run: seeds an emulator with a heavy phone's worth of contacts, calendar events and texts,
# pairs it with a desktop service, and measures sync time, bytes, phone CPU / memory and desktop memory
# (docs/reports/scale-report.md). Real providers, real link, real desktop service; made-up data.
#
#   scripts/scale-e2e.sh [STAGE[,STAGE...]] [--stop]
#
# Stages: env seed battery pair measure   (default, in this order; each is idempotent), plus on request:
#   cases    correctness / conflict / consent / log checks on small named data (see stage_cases); needs env,battery,pair
#   hold     keep everything running for hand poking until $RUN/hold is removed
#   env      desktop service, emulator, debug APK (built into target/scale-android)
#   seed     5,000 contacts (accents / CJK / emoji, 1-3 numbers, photos on every 5th), 8 calendars of 3 accounts with
#            20,000 events (daily + weekly series over 5 years, all-day, multi-day, other zones), ~1,600 texts over 800
#            threads. On API 36 only the default SMS app may write the SMS provider, so texts go in through the
#            emulator modem (`adb emu sms send`, ~7/s). SCALE_CONTACTS / SCALE_EVENTS / SCALE_THREADS override sizes.
#   pair     pair the phone with the desktop (shared helper of scripts/e2e-android.sh)
#   measure  turns each feature on, times first usable data and the full sync, one-event delta, CPU / PSS / bytes
#
# Isolation: AVD gb36z (console 5700, adb server 16150), control port 17591, Xvfb :96, cargo target
# target/scale-e2e. Create the AVD once by copying any gb36* AVD (see docs/phone-data-link.md, "Emulator run").
# Results: dist/android/logs/scale-results.tsv and dist/android/logs/scale-measure.txt
# The emulator (a transient systemd unit) and the service keep running after the script ends: stop them with
# `scripts/scale-e2e.sh --stop`. Stages that follow each other need the same session, e.g.
# `scripts/scale-e2e.sh env,battery,pair,cases` (about 10 minutes: the cases stage itself takes about 2 minutes). Start from a
# freshly started emulator (--stop first): a re-seeded calendar provider is not a clean one.
# Every name and port can be overridden (SCALE_AVD, SCALE_EMU_PORT, SCALE_ADB_PORT, SCALE_CTRL_PORT, SCALE_TAG, ...), and
# so can the desktop cargo dir (SCALE_TARGET) and the APK scratch dir (SCALE_SCRATCH); SCALE_PREBUILT=1 skips the desktop
# build (CI). The desktop service is rebuilt on every `env` otherwise, so a run never measures a stale binary.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-env,seed,battery,pair,measure}"
[[ "$STAGES" == --stop ]] && { STAGES=""; STOP=1; }
set --
# Every name and port can be overridden (SCALE_AVD, SCALE_EMU_PORT, SCALE_ADB_PORT, SCALE_CTRL_PORT, ...), so a second
# run can sit next to another emulator job without sharing a thing.
export E2E_SOURCE_ONLY=1 E2E_AVD="${SCALE_AVD:-gb36z}" E2E_EMU_PORT="${SCALE_EMU_PORT:-5700}" E2E_ADB_PORT="${SCALE_ADB_PORT:-16150}" \
  E2E_TAG="${SCALE_TAG:--scale}" \
  E2E_RUN="$ROOT/target/scale-run${SCALE_TAG:-}" E2E_SCRATCH="${SCALE_SCRATCH:-$ROOT/target/scale-android}" E2E_TARGET="${SCALE_TARGET:-$ROOT/target/scale-e2e}" \
  E2E_RESULTS="$ROOT/dist/android/logs/scale-results${SCALE_TAG:-}.tsv" E2E_XDISP="${SCALE_XDISP:-:96}" \
  E2E_CTRL_PORT="${SCALE_CTRL_PORT:-17591}" E2E_API_PORT="${SCALE_API_PORT:-17590}" E2E_DISC_PORT="${SCALE_DISC_PORT:-17526}" \
  E2E_LISTEN_PORT="${SCALE_LISTEN_PORT:-1732}" E2E_REDIR_PORT="${SCALE_REDIR_PORT:-17271}"
# shellcheck disable=SC1091
source scripts/e2e-android.sh
# e2e-android.sh only builds the desktop service when the binary is missing, and E2E_TARGET under target/ is redirected to a
# cargo dir shared by every e2e job: a stale service from another checkout would be measured instead of this one. Always
# run the (incremental) cargo build, so the run tests the checkout it is run from.
build_desktop() {
  [[ "${SCALE_PREBUILT:-}" == 1 ]] && return 0   # CI hands over the binaries and the APK it built once (docs/emulator-ci.md)
  log "building desktop service/ctl ($E2E_TARGET)"
  CARGO_TARGET_DIR="$E2E_TARGET" cargo build --release -p bridge-service --bins >"$LOGS/e2e-build-desktop.log" 2>&1
}
OUT="$ROOT/dist/android/logs/scale-measure${SCALE_TAG:-}.txt"
CONTACTS="${SCALE_CONTACTS:-5000}" EVENTS="${SCALE_EVENTS:-20000}" THREADS="${SCALE_THREADS:-800}"
now_ms() { date +%s%3N; }
say() { printf '%s\n' "$*" | tee -a "$OUT"; }

grant_all() { for p in READ_CONTACTS READ_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS WRITE_CONTACTS WRITE_CALENDAR; do $ADB shell pm grant $PKG android.permission.$p 2>/dev/null; done; }
seed_wait() { # tag text, seconds
  wait_for "$2" bash -c "$ADB logcat -d -s GBScale:I | grep -q 'done $1'"
}
stage_seed() {
  grant_all
  $ADB logcat -c
  local t0; t0=$(now_ms)
  $ADB shell am broadcast -n $PKG/.ScaleSeedReceiver -a dev.geekbridge.debug.SEED --es what clear >/dev/null
  seed_wait clear 120
  $ADB shell am broadcast -n $PKG/.ScaleSeedReceiver -a dev.geekbridge.debug.SEED --es what contacts --ei n "$CONTACTS" >/dev/null
  seed_wait contacts 900 || { rec seed_contacts FAIL "seeder did not finish"; return 1; }
  say "seed contacts: $CONTACTS in $(( $(now_ms) - t0 )) ms"
  t0=$(now_ms)
  $ADB shell am broadcast -n $PKG/.ScaleSeedReceiver -a dev.geekbridge.debug.SEED --es what calendar --ei n "$EVENTS" >/dev/null
  seed_wait calendar 1800 || { rec seed_calendar FAIL "seeder did not finish"; return 1; }
  say "seed calendar: $EVENTS events in $(( $(now_ms) - t0 )) ms"
  local have; have=$($ADB shell content query --uri content://sms --projection _id | wc -l)
  if (( have < THREADS * 2 )); then
    t0=$(now_ms)
    for n in $(seq 1 "$THREADS"); do
      for k in 1 2; do
        $ADB emu sms send "$(printf '+4670%07d' $((1000000 + n)))" "Thread $n message $k - lunch tomorrow? the quick brown fox" >/dev/null
        sleep 0.15
      done
    done
    say "seed sms: $((THREADS * 2)) texts over $THREADS threads sent in $(( $(now_ms) - t0 )) ms"
    sleep 10
  fi
  rec seed PASS "contacts=$CONTACTS events=$EVENTS sms_rows=$($ADB shell content query --uri content://sms --projection _id | wc -l)"
}

pid_of() { $ADB shell pidof $PKG | awk '{print $1}'; }
cpu_ticks() { $ADB shell cat /proc/"$(pid_of)"/stat 2>/dev/null | awk '{print $14+$15}'; } # utime+stime, 10 ms ticks
pss_kb() { $ADB shell dumpsys meminfo $PKG 2>/dev/null | awk '/TOTAL PSS:/ {print $3; exit} /^ +TOTAL +[0-9]/ {print $2; exit}'; }
dev_bytes() { $ADB shell cat /proc/net/dev | awk '/eth0|wlan0/ {rx+=$2; tx+=$10} END {print rx+tx}'; }
desk_rss_kb() { ps -o rss= -p "$(pgrep -f "^$SVC --state-dir $RUN/state" | head -1)" 2>/dev/null | tr -d ' '; }
count_of() { ctl agenda 2>/dev/null | jget "$1"; }
wait_count() { # description python-expr target timeout_s -> prints elapsed ms or -1
  local t0 target="$3" v=0; t0=$(now_ms)
  for ((i = 0; i < $4 * 5; i++)); do
    v="$(count_of "$2")"; [[ "${v:-0}" =~ ^[0-9]+$ ]] && (( v >= target )) && { echo $(( $(now_ms) - t0 )); return 0; }
    sleep 0.2
  done
  echo -1
}
msgs_of() { ctl messages 2>/dev/null | jget "$1"; }

stage_measure() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec measure SKIP "not paired (run pair first)"; return 1; }
  : > "$OUT"
  wake; grant_all
  local cal_ids; cal_ids="$($ADB shell content query --uri content://com.android.calendar/calendars --projection _id | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | paste -sd, -)"
  local setlist; setlist="$(for i in ${cal_ids//,/ }; do printf '<string>%s</string>' "$i"; done)"
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="sms" value="true" />
    <set name="calendar_ids">$setlist</set>
</map>
XML
  # Opt-ins are granted per paired computer (ComputerGrants): allow this desktop for the three features.
  $ADB shell "run-as $PKG sh -c 'cat > shared_prefs/geekbridge_grants.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="grants">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[&quot;contacts&quot;,&quot;calendar&quot;,&quot;sms&quot;]}}</string>
</map>
XML
  goto_home; sleep 2; ensure_link || true
  desk_dial; wait_for 30 is_connected || { rec measure FAIL "link did not come up"; return 1; }
  # The transport restarts once to declare the new capabilities; make sure the link is up again and stays up.
  sleep 8; for _ in 1 2 3; do is_connected && break; desk_dial; wait_for 30 is_connected; sleep 5; done
  is_connected || { rec measure FAIL "link is down"; return 1; }
  say "phone: $($ADB shell getprop ro.product.model | tr -d '\r') API $($ADB shell getprop ro.build.version.sdk | tr -d '\r'); calendars=$cal_ids"
  say "baseline: phone PSS $(pss_kb) kB, desktop service RSS $(desk_rss_kb) kB"

  for f in contacts calendar sms; do ctl consent "$PHONE_ID" $f off >/dev/null 2>&1; done
  sleep 2
  local c0 b0 t0 first full
  # ---- contacts
  c0=$(cpu_ticks); b0=$(dev_bytes); t0=$(now_ms)
  ctl consent "$PHONE_ID" contacts on >/dev/null
  first=$(wait_count contacts 'd["devices"][0]["contacts_count"]' 1 120)
  full=$(wait_count contacts 'd["devices"][0]["contacts_count"]' "$CONTACTS" 900)
  say "contacts: first usable ${first} ms, full ($(count_of 'd["devices"][0]["contacts_count"]') of $CONTACTS) ${full} ms, phone CPU $(( ($(cpu_ticks) - c0) * 10 )) ms, device net $(( $(dev_bytes) - b0 )) B, PSS $(pss_kb) kB"
  rec contacts_sync "$([[ $full -ge 0 ]] && echo 1 || echo 0)" "first=${first}ms full=${full}ms"
  # ---- calendar
  c0=$(cpu_ticks); b0=$(dev_bytes)
  ctl consent "$PHONE_ID" calendar on >/dev/null
  first=$(wait_count cal 'd["devices"][0]["events_count"]' 1 180)
  sleep 3
  say "calendar: first usable ${first} ms, events on desktop $(count_of 'd["devices"][0]["events_count"]') (truncated=$(count_of 'd["devices"][0]["calendar_truncated"]')), phone CPU $(( ($(cpu_ticks) - c0) * 10 )) ms, device net $(( $(dev_bytes) - b0 )) B, PSS $(pss_kb) kB"
  rec calendar_sync "$([[ $first -ge 0 ]] && echo 1 || echo 0)" "first=${first}ms"
  # ---- one event changes: the delta must be tiny and quick (the phone pushes at most every 30 s)
  # An instance that starts within the next day, so it shows in the desktop's today view.
  local from to ev; from=$(now_ms); to=$(( from + 86400000 ))
  ev="$($ADB shell content query --uri "content://com.android.calendar/instances/when/$from/$to" --projection event_id:title | grep -v '^$' | sed -n 's/.*event_id=\([0-9]*\).*/\1/p' | head -1)"
  b0=$(dev_bytes); t0=$(now_ms)
  $ADB shell content update --uri "content://com.android.calendar/events/$ev" --bind title:s:ScaleChanged >/dev/null 2>&1
  local seen=-1
  for _ in $(seq 120); do
    ctl agenda 2>/dev/null | grep -q ScaleChanged && { seen=$(( $(now_ms) - t0 )); break; }; sleep 0.5
  done
  say "one event changed: visible on desktop after ${seen} ms, device net $(( $(dev_bytes) - b0 )) B (full sync was far larger)"
  rec calendar_delta "$([[ $seen -ge 0 ]] && echo 1 || echo 0)" "${seen}ms"
  # ---- sms
  c0=$(cpu_ticks); b0=$(dev_bytes); t0=$(now_ms)
  ctl consent "$PHONE_ID" sms on >/dev/null
  for _ in $(seq 300); do [[ "$(msgs_of 'len(d["conversations"])')" -ge 1 ]] 2>/dev/null && break; sleep 0.2; done
  first=$(( $(now_ms) - t0 ))
  sleep 5
  say "sms: first conversations ${first} ms, listed $(msgs_of 'len(d["conversations"])') of $THREADS threads (total_held=$(msgs_of 'd.get("conversations_total")'), on_phone=$(msgs_of 'd["devices"][0].get("threads_on_phone")')), phone CPU $(( ($(cpu_ticks) - c0) * 10 )) ms, device net $(( $(dev_bytes) - b0 )) B, PSS $(pss_kb) kB"
  rec sms_list 1 "first=${first}ms"
  # ---- end state
  say "end: phone PSS $(pss_kb) kB, desktop service RSS $(desk_rss_kb) kB"
  say "battery (emulator numbers are indicative only): $($ADB shell dumpsys batterystats --charged $PKG 2>/dev/null | grep -m1 -i 'Uid u0a.*:' | tr -s ' ')"
  $ADB shell am start -n $PKG/.HomeActivity >/dev/null 2>&1
  $ADB exec-out screencap -p > "$ROOT/dist/screenshots/android/scale-home.png" 2>/dev/null
  say "frames: $($ADB shell dumpsys gfxinfo $PKG 2>/dev/null | grep -E 'Janky frames|Total frames' | tr -s ' ' | paste -sd';' -)"
  column -t -s $'\t' "$RESULTS" | tail -n 12
}

# ------------------------------------------------------------------ cases: correctness, conflicts, consent, logs
# Small named scenarios (ScaleSeedReceiver `cases`): series across a DST change with an exception, EXDATE and a cancelled
# occurrence, all-day events, a zone across the date line, two accounts with one title, a read-only calendar, an
# invitation, contacts with RTL / override / long-note data; then edit conflicts, deletions both ways, consent
# withdrawal on both ends and a grep of every log for the made-up content. Writes dist/android/logs/cases-*.json.
CASES_OUT="$ROOT/dist/android/logs/cases${SCALE_TAG:-}"
agenda_to() { ctl agenda "$1" "$2" "$3" >"$4" 2>/dev/null; } # query day days file
chk() { # name, python expression over d (and E for events by title), file
  local name="$1" expr="$2" f="$3" r
  r="$(python3 -c "
import json,sys
d=json.load(open('$f'))
E={}
for e in d.get('events',[]): E.setdefault(e['title'],[]).append(e)
P={p['name']:p for p in d.get('people',[])}
ok=bool($expr)
print('1' if ok else '0')" 2>&1 | tail -1)"
  if [[ "$r" == 1 ]]; then rec "case_$name" PASS ""; else rec "case_$name" FAIL "$expr"; fi
}
stage_cases() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec cases SKIP "not paired (run pair first)"; return 1; }
  mkdir -p "$CASES_OUT"; wake; grant_all
  $ADB logcat -c
  $ADB shell am broadcast -n $PKG/.ScaleSeedReceiver -a dev.geekbridge.debug.SEED --es what cases >/dev/null
  seed_wait cases 180 || { rec cases FAIL "seeder did not finish"; $ADB logcat -d -s GBScale | tail -5; return 1; }
  local q='content://com.android.calendar/calendars'
  local OWN RO
  OWN="$($ADB shell content query --uri $q --projection _id:account_name | sed -n 's/.*_id=\([0-9]*\), account_name=cc1.*/\1/p' | head -1)"
  RO="$($ADB shell content query --uri $q --projection _id:account_name | sed -n 's/.*_id=\([0-9]*\), account_name=cc2.*/\1/p' | head -1)"
  [[ -n "$OWN" && -n "$RO" ]] || { rec cases FAIL "calendars not found (own=$OWN ro=$RO)"; return 1; }
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="calendar_edit" value="true" />
    <set name="calendar_ids"><string>$OWN</string><string>$RO</string></set>
</map>
XML
  $ADB shell "run-as $PKG sh -c 'cat > shared_prefs/geekbridge_grants.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="grants">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[&quot;contacts&quot;,&quot;calendar&quot;,&quot;calendar_edit&quot;]}}</string>
</map>
XML
  goto_home; sleep 2; ensure_link || true
  desk_dial; wait_for 30 is_connected || { rec cases FAIL "link did not come up"; return 1; }
  sleep 8; for _ in 1 2 3; do is_connected && break; desk_dial; wait_for 30 is_connected; sleep 5; done
  for f in contacts calendar calendar_edit; do ctl consent "$PHONE_ID" $f on >/dev/null; done
  local n=0
  for _ in $(seq 60); do n="$(count_of 'd["devices"][0]["events_count"]')"; [[ "${n:-0}" -ge 10 ]] 2>/dev/null && break; sleep 1; done
  rec cases_sync "$([[ "${n:-0}" -ge 10 ]] && echo PASS || echo FAIL)" "events on desktop: $n"
  for _ in $(seq 60); do c="$(count_of 'd["devices"][0]["contacts_count"]')"; [[ "${c:-0}" -ge 5 ]] 2>/dev/null && break; sleep 1; done

  local ms; ms() { python3 -c "import datetime as D,zoneinfo as Z,sys;y,m,d,h,tz=sys.argv[1:6];print(int(D.datetime(int(y),int(m),int(d),int(h),tzinfo=Z.ZoneInfo(tz)).timestamp()*1000))" "$@"; }
  agenda_to "" 2027-03-14 28 "$CASES_OUT/mar.json"
  # Series across the DST change (28 Mar 2027): 09:00 Stockholm is 08:00Z before, 07:00Z after. Moved one stays moved.
  chk series_dst "sorted(e['start_ms'] for e in E.get('CC Weekly',[]))==[$(ms 2027 3 15 9 Europe/Stockholm),$(ms 2027 4 5 9 Europe/Stockholm)]" "$CASES_OUT/mar.json"
  chk exception_moved "[e['start_ms'] for e in E.get('CC Weekly moved',[])]==[$(ms 2027 3 22 10 Europe/Stockholm)]" "$CASES_OUT/mar.json"
  chk cancelled_occurrence_absent "not any(e['start_ms']==$(ms 2027 3 29 9 Europe/Stockholm) for t in E for e in E[t])" "$CASES_OUT/mar.json"
  chk series_marked_recurring "all(e['recurring'] for e in E['CC Weekly'])" "$CASES_OUT/mar.json"
  agenda_to "" 2027-05-01 56 "$CASES_OUT/may.json"
  chk exdate_removed "sorted(e['start_ms'] for e in E.get('CC Exdate',[]))==[$(ms 2027 5 3 14 Europe/Stockholm),$(ms 2027 5 17 14 Europe/Stockholm)]" "$CASES_OUT/may.json"
  chk allday_single "[(e['all_day'],e['end_ms']-e['start_ms']) for e in E.get('CC Allday',[])]==[(True,86400000)]" "$CASES_OUT/may.json"
  chk allday_multiday_over_month_end "[(e['all_day'],e['end_ms']-e['start_ms']) for e in E.get('CC Allday trip',[])]==[(True,3*86400000)]" "$CASES_OUT/may.json"
  agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"
  chk zone_east_of_utc "[(e['start_ms'],e.get('tz')) for e in E.get('CC Tokyo',[])]==[($(ms 2027 7 1 8 Asia/Tokyo),'Asia/Tokyo')]" "$CASES_OUT/jul.json"
  chk two_accounts_same_title "len(E.get('CC Shared title',[]))==2 and len({e['calendar_id'] for e in E['CC Shared title']})==2" "$CASES_OUT/jul.json"
  chk readonly_calendar_flagged "{c['id']:c['writable'] for c in d['calendars']}.get('$RO')==False and {c['id']:c['writable'] for c in d['calendars']}.get('$OWN')==True" "$CASES_OUT/jul.json"
  # Contacts
  agenda_to "CC" 2027-03-14 1 "$CASES_OUT/people.json"
  chk contacts_unicode_rtl "'CC محمد علي' in P and 'CC Ünïcödé 山田 🎉' in P" "$CASES_OUT/people.json"
  chk contacts_no_bidi_override "not any('‮' in p['name'] for p in d['people'])" "$CASES_OUT/people.json"
  chk contacts_numbers_deduped "len(P['CC Multi']['phones'])<=2 and len(P['CC Multi']['emails'])==1" "$CASES_OUT/people.json"
  chk contacts_long_note_kept "'CC Longnote' in P and P['CC Longnote']['phones']" "$CASES_OUT/people.json"
  say "Multi phones: $(python3 -c "import json;d=json.load(open('$CASES_OUT/people.json'));print([p['phones'] for p in d['people'] if p['name']=='CC Multi'])")"

  # ---- edit conflict: the phone's copy moves on twice (the second push waits for the 30 s spacing); the desktop edits
  local EID; EID="$(python3 -c "import json;d=json.load(open('$CASES_OUT/jul.json'));print([e['event_id'] for e in d['events'] if e['title']=='CC Conflict'][0])")"
  local sjul; sjul="$(ms 2027 7 7 10 Europe/Stockholm)"
  $ADB shell content update --uri "content://com.android.calendar/events/$EID" --bind title:s:CCv1 >/dev/null
  for _ in $(seq 40); do agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"; grep -q CCv1 "$CASES_OUT/jul.json" && break; sleep 1; done
  $ADB shell content update --uri "content://com.android.calendar/events/$EID" --bind title:s:CCv2 >/dev/null
  sleep 1
  ctl calendar-update "$PHONE_ID" "$EID" "Mine" "$sjul" 60 >/dev/null 2>&1
  local seen=""; for _ in $(seq 20); do agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"; seen="$(python3 -c "import json;d=json.load(open('$CASES_OUT/jul.json'));print(','.join(str(e.get('reason')) for e in d['edits']))")"; [[ "$seen" == *changed* ]] && break; sleep 1; done
  local title; title="$($ADB shell content query --uri content://com.android.calendar/events/$EID --projection title | sed -n 's/.*title=//p')"
  rec case_conflict_refused "$([[ "$seen" == *changed* && "$title" == CCv2 ]] && echo PASS || echo FAIL)" "edits=[$seen] phone title=$title"
  for _ in $(seq 60); do agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"; grep -q CCv2 "$CASES_OUT/jul.json" && break; sleep 1; done
  ctl calendar-update "$PHONE_ID" "$EID" "Mine" "$sjul" 60 >/dev/null 2>&1; sleep 4
  title="$($ADB shell content query --uri content://com.android.calendar/events/$EID --projection title | sed -n 's/.*title=//p')"
  rec case_edit_from_current_version_applies "$([[ "$title" == Mine ]] && echo PASS || echo FAIL)" "phone title=$title"
  # ---- invitation (not organiser): refused; read-only calendar: refused before anything is sent
  local INV; INV="$(python3 -c "import json;d=json.load(open('$CASES_OUT/jul.json'));print([e['event_id'] for e in d['events'] if e['title']=='CC Invitation'][0])")"
  local inv_out; inv_out="$(ctl calendar-update "$PHONE_ID" "$INV" "Hijack" "$(ms 2027 7 6 10 Europe/Stockholm)" 60 2>&1 | tr '\n' ' ')"
  for _ in $(seq 10); do agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"; grep -q not_organizer "$CASES_OUT/jul.json" && break; sleep 1; done
  chk invitation_not_organiser_refused "any(x.get('reason')=='not_organizer' for x in d['edits'])" "$CASES_OUT/jul.json"
  say "invitation update: $inv_out"
  RIDE="$(python3 -c "import json;d=json.load(open('$CASES_OUT/jul.json'));print([e['event_id'] for e in d['events'] if e['title']=='CC Shared title' and e['calendar_id']=='$RO'][0])")"
  if ctl calendar-update "$PHONE_ID" "$RIDE" "Hijack" "$sjul" 60 >/dev/null 2>&1; then rec case_readonly_refused FAIL "accepted"; else rec case_readonly_refused PASS ""; fi
  # ---- deletion both ways
  local TOK; TOK="$(python3 -c "import json;d=json.load(open('$CASES_OUT/jul.json'));print([e['event_id'] for e in d['events'] if e['title']=='CC Tokyo'][0])")"
  $ADB shell content delete --uri "content://com.android.calendar/events/$TOK" >/dev/null
  local gone=0; for _ in $(seq 70); do agenda_to "" 2027-06-28 14 "$CASES_OUT/jul.json"; grep -q "CC Tokyo" "$CASES_OUT/jul.json" || { gone=1; break; }; sleep 1; done
  rec case_phone_delete_reaches_desktop "$([[ $gone == 1 ]] && echo PASS || echo FAIL)" ""
  local AD; AD="$(python3 -c "import json;d=json.load(open('$CASES_OUT/may.json'));print([e['event_id'] for e in d['events'] if e['title']=='CC Allday'][0])")"
  ctl calendar-delete "$PHONE_ID" "$AD" >/dev/null 2>&1; sleep 4
  local left; left="$($ADB shell content query --uri content://com.android.calendar/events --projection title --where "title='CC Allday' AND deleted=0" | grep -c 'title=')"
  rec case_desktop_delete_reaches_phone "$([[ "$left" == 0 ]] && echo PASS || echo FAIL)" "rows left: $left"

  # ---- consent withdrawn on the desktop: the data is gone from memory at once
  ctl consent "$PHONE_ID" contacts off >/dev/null; ctl consent "$PHONE_ID" calendar off >/dev/null
  agenda_to "" 2027-03-14 28 "$CASES_OUT/off.json"
  chk desktop_consent_off_erases "d['people_total']==0 and d['events']==[] and all(x['contacts_count']==0 and x['events_count']==0 for x in d['devices'])" "$CASES_OUT/off.json"
  # ---- consent withdrawn on the phone (feature switched off there): the next link must leave nothing on the desktop
  for f in contacts calendar; do ctl consent "$PHONE_ID" $f on >/dev/null; done
  for _ in $(seq 40); do n="$(count_of 'd["devices"][0]["events_count"]')"; [[ "${n:-0}" -ge 5 ]] 2>/dev/null && break; sleep 1; done
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
</map>
XML
  goto_home; sleep 2; ensure_link || true; desk_dial; wait_for 30 is_connected; sleep 6
  agenda_to "" 2027-03-14 28 "$CASES_OUT/withdrawn.json"
  chk phone_withdrawal_erases_desktop_copy "d['people_total']==0 and d['events']==[] and all(x['contacts_count']==0 and x['events_count']==0 for x in d['devices'])" "$CASES_OUT/withdrawn.json"
  # ---- logs carry no contact or event content
  local leaks; leaks=$( { $ADB logcat -d; cat "$LOGS/e2e-service.log" 2>/dev/null; } | grep -cE "CC Weekly|CC Allday|CC Conflict|CC Invitation|CC Multi|CC Longnote|Mine|CCv[12]|محمد|boss@example|multi@example|m@example|\+46 70|070-123" )
  rec case_logs_free_of_content "$([[ "$leaks" == 0 ]] && echo PASS || echo FAIL)" "matching log lines: $leaks"
}

# hold: keep the emulator and service up for hand poking until the file $RUN/hold is removed.
stage_hold() { touch "$RUN/hold"; log "holding; remove $RUN/hold to stop"; while [[ -e "$RUN/hold" ]]; do sleep 2; done; }
main_scale() {
  if [[ "${STOP:-}" == 1 ]]; then
    pkill -f "$E2E_TARGET/release/geekbridge" 2>/dev/null; $ADB emu kill >/dev/null 2>&1
    systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; pkill -f "Xvfb $XDISP" 2>/dev/null; return 0
  fi
  IFS=, read -r -a st <<<"$STAGES"
  local before; before="$(wc -l <"$RESULTS" 2>/dev/null || echo 0)"
  for s in "${st[@]}"; do
    log "== stage $s"
    [[ "$s" == env ]] && { build_desktop || { rec env FAIL "desktop build failed, see dist/android/logs/e2e-build-desktop.log"; continue; }; }
    "stage_$s" || log "stage $s reported a failure"
  done
  # Exit status for CI: non-zero if any result line written by this run says FAIL.
  ! tail -n +"$((before + 1))" "$RESULTS" 2>/dev/null | awk -F'\t' '$2 == "FAIL" || $2 == 0 {f = 1} END {exit !f}'
}
main_scale
