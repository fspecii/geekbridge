#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Phone-data scale run: seeds an emulator with a heavy phone's worth of contacts, calendar events and texts,
# pairs it with a desktop service, and measures sync time, bytes, phone CPU / memory and desktop memory
# (docs/scale-report.md). Real providers, real link, real desktop service; made-up data.
#
#   scripts/scale-e2e.sh [STAGE[,STAGE...]] [--stop]
#
# Stages: env seed battery pair measure   (default: all, in this order; each is idempotent)
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
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-env,seed,battery,pair,measure}"
[[ "$STAGES" == --stop ]] && { STAGES=""; STOP=1; }
set --
export E2E_SOURCE_ONLY=1 E2E_AVD=gb36z E2E_EMU_PORT=5700 E2E_ADB_PORT=16150 E2E_TAG=-scale \
  E2E_RUN="$ROOT/target/scale-run" E2E_SCRATCH="$ROOT/target/scale-android" E2E_TARGET="$ROOT/target/scale-e2e" \
  E2E_RESULTS="$ROOT/dist/android/logs/scale-results.tsv" E2E_XDISP=:96 \
  E2E_CTRL_PORT=17591 E2E_API_PORT=17590 E2E_DISC_PORT=17526 E2E_LISTEN_PORT=1732 E2E_REDIR_PORT=17271
# shellcheck disable=SC1091
source scripts/e2e-android.sh
OUT="$ROOT/dist/android/logs/scale-measure.txt"
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

main_scale() {
  if [[ "${STOP:-}" == 1 ]]; then
    pkill -f "$E2E_TARGET/release/geekbridge" 2>/dev/null; $ADB emu kill >/dev/null 2>&1
    systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; pkill -f "Xvfb $XDISP" 2>/dev/null; return 0
  fi
  IFS=, read -r -a st <<<"$STAGES"
  for s in "${st[@]}"; do log "== stage $s"; "stage_$s" || log "stage $s reported a failure"; done
}
main_scale
