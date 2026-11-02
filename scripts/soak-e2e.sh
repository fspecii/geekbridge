#!/usr/bin/env bash
# Multi-hour stability soak: release desktop service + release (R8) Android APK on an emulator, a realistic
# loop of clipboard / file / link-flap / call / phone-data / app-restart / screen-off events, and a sample of
# memory, threads, descriptors, wake locks and alarms on both sides every SAMPLE_EVERY seconds.
#
#   DURATION=7200 scripts/soak-e2e.sh [--no-build] [--report-only DIR] [--stop]
#
#   DURATION       wall-clock seconds of the loop (default 7200 = 2 h; the loop is bounded, setup/teardown are extra)
#   PROFILE=short  intervals suited to a 15-30 min confirmation run (default when DURATION <= 2400)
#   *_EVERY        CLIP_EVERY=60 FILE_EVERY=300 FLAP_EVERY=600 CALL_EVERY=1200 DATA_EVERY=1200
#                  RESTART_EVERY=1800 IDLE_EVERY=1500 (seconds); IDLE_SECS=300 is the screen-off length
#   FILE_MB_MIN/FILE_MB_MAX  transfer sizes (5..20 MB); SAMPLE_EVERY=60
#   SOAK_TS        output directory name under dist/soak/ (default: timestamp); SOAK_OUT overrides the path
#
# Stages: build (release APK via scripts/android-perf.sh size + release desktop binaries) -> setup (private Xvfb,
# desktop service with a scratch HOME, own emulator, pairing via scripts/android-perf.sh setup, phone data and call
# prefs) -> loop -> teardown (everything this script started is killed, also on Ctrl-C) -> report
# (scripts/soak-report.py writes report.md + PNG charts next to the logs).
#
# Isolation: AVD gb36s on console port 5694 / adb server 16067 (never 5037), desktop control port 17881, Xvfb :95,
# state under target/soak-run, scratch HOME so received files and ~/.config of the real user are never touched.
# Override with SOAK_AVD, SOAK_EMU_PORT, SOAK_ADB_PORT, SOAK_CTRL_PORT, SOAK_API_PORT, SOAK_DISC_PORT,
# SOAK_LISTEN_PORT, SOAK_XDISP, SOAK_REDIR_PORT if another agent already owns one of them (checked at start).
# Output (dist/soak/<ts>/): samples.csv events.tsv failures.log desktop-service.log phone-logcat.log run.log report.md *.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1

SOAK_BUILD=yes REPORT_ONLY="" STOP=0
while (($#)); do
  case "$1" in
    --no-build) SOAK_BUILD=no ;;
    --report-only) REPORT_ONLY="$2"; shift ;;
    --stop) STOP=1 ;;
    -h|--help) sed -n '2,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done
if [[ -n "$REPORT_ONLY" ]]; then exec python3 -I "$ROOT/scripts/soak-report.py" "$REPORT_ONLY"; fi

DURATION="${DURATION:-7200}"
if [[ "${PROFILE:-}" == short || ( -z "${PROFILE:-}" && "$DURATION" -le 2400 ) ]]; then
  : "${CLIP_EVERY:=75}" "${FILE_EVERY:=150}" "${FLAP_EVERY:=210}" "${CALL_EVERY:=300}" "${DATA_EVERY:=300}" "${RESTART_EVERY:=480}" "${IDLE_EVERY:=420}" "${IDLE_SECS:=90}"
fi
: "${CLIP_EVERY:=60}" "${FILE_EVERY:=300}" "${FLAP_EVERY:=600}" "${CALL_EVERY:=1200}" "${DATA_EVERY:=1200}" "${RESTART_EVERY:=1800}" "${IDLE_EVERY:=1500}" "${IDLE_SECS:=300}"
: "${SAMPLE_EVERY:=60}" "${FILE_MB_MIN:=5}" "${FILE_MB_MAX:=20}"

SOAK_TS="${SOAK_TS:-$(date +%Y%m%d-%H%M%S)}"
OUT="${SOAK_OUT:-$ROOT/dist/soak/$SOAK_TS}"
mkdir -p "$OUT"
P="${SOAK_RUN:-$ROOT/target/soak-run}"
HOME_SCRATCH="$P/home"                     # HOME of the desktop service: files land in $HOME_SCRATCH/Downloads/GeekBridge
PKG=dev.geekbridge.android

# Dedicated ports/names, shared with scripts/android-perf.sh through its PERF_* knobs.
export PERF_DIR="$P" PERF_SCRATCH="${SOAK_SCRATCH:-$ROOT/target/soak-android}" PERF_DESKTOP="${SOAK_DESKTOP:-$ROOT/target/soak-desktop}"
export PERF_AVD="${SOAK_AVD:-gb36s}" PERF_EMU_PORT="${SOAK_EMU_PORT:-5694}" PERF_ADB_PORT="${SOAK_ADB_PORT:-16067}"
export PERF_CTRL_PORT="${SOAK_CTRL_PORT:-17881}" PERF_API_PORT="${SOAK_API_PORT:-17880}" PERF_DISC_PORT="${SOAK_DISC_PORT:-17816}"
export PERF_LISTEN_PORT="${SOAK_LISTEN_PORT:-1735}" PERF_XDISP="${SOAK_XDISP:-:95}" PERF_REDIR_PORT="${SOAK_REDIR_PORT:-17461}" PERF_TAG=soak
export VARIANT=release

# The same variables scripts/e2e-android.sh reads (sourced below for its UI helpers).
export E2E_AVD="$PERF_AVD" E2E_EMU_PORT="$PERF_EMU_PORT" E2E_ADB_PORT="$PERF_ADB_PORT" E2E_RUN="$P/desk" E2E_SCRATCH="$PERF_SCRATCH"
export E2E_CTRL_PORT="$PERF_CTRL_PORT" E2E_API_PORT="$PERF_API_PORT" E2E_DISC_PORT="$PERF_DISC_PORT" E2E_LISTEN_PORT="$PERF_LISTEN_PORT"
export E2E_XDISP="$PERF_XDISP" E2E_REDIR_PORT="$PERF_REDIR_PORT" E2E_TAG=soak E2E_RESULTS="$OUT/e2e-results.tsv" E2E_TARGET="$PERF_DESKTOP"
export ANDROID_ADB_SERVER_PORT="$PERF_ADB_PORT"
SERIAL="emulator-$PERF_EMU_PORT"
BIN="$PERF_DESKTOP/release"
SVC_BIN="$BIN/geekbridge-service"

log() { printf '[soak %s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log" >&2; }

# ---------------------------------------------------------------- teardown (also on Ctrl-C / error)
BG_PIDS=()
teardown() {
  trap - EXIT INT TERM
  log "teardown: stopping everything this run started"
  for p in "${BG_PIDS[@]}"; do kill "$p" 2>/dev/null; done
  A shell "iptables -D INPUT -j DROP; iptables -D OUTPUT -j DROP" >/dev/null 2>&1
  A emu kill >/dev/null 2>&1
  systemctl --user stop "gbe2e-emulatorsoak" >/dev/null 2>&1
  pkill -f "^$SVC_BIN --state-dir $P/desk/state" 2>/dev/null
  pkill -f "Xvfb $PERF_XDISP" 2>/dev/null
  sleep 3
  pkill -f "qemu-system-x86_64.*-avd $PERF_AVD " 2>/dev/null
  adb kill-server >/dev/null 2>&1
  rm -f "/tmp/.X11-unix/X${PERF_XDISP#:}" "/tmp/.X${PERF_XDISP#:}-lock"
  # Nothing may be left behind.
  local left; left="$(pgrep -af "(^$SVC_BIN|-avd $PERF_AVD |Xvfb $PERF_XDISP|adb -L tcp:$PERF_ADB_PORT)" | grep -v pgrep | cut -c1-120)"
  [[ -z "$left" ]] && log "teardown: no processes left" || log "teardown: LEFTOVER: $left"
}
A() { adb -s "$SERIAL" "$@"; }

if ((STOP)); then trap - EXIT; teardown; exit 0; fi

# ---------------------------------------------------------------- pre-flight: nothing of ours may collide with other agents
preflight() {
  local port busy=0
  for port in "$PERF_EMU_PORT" $((PERF_EMU_PORT + 1)) "$PERF_ADB_PORT" "$PERF_CTRL_PORT" "$PERF_API_PORT" "$PERF_LISTEN_PORT" "$PERF_REDIR_PORT"; do
    if ss -ltn "sport = :$port" 2>/dev/null | grep -q LISTEN; then log "port $port is already in use"; busy=1; fi
  done
  [[ -e "/tmp/.X11-unix/X${PERF_XDISP#:}" ]] && { log "X display $PERF_XDISP is in use"; busy=1; }
  pgrep -af "qemu-system.*-avd $PERF_AVD " >/dev/null && { log "AVD $PERF_AVD is already running"; busy=1; }
  [[ -d "$ANDROID_AVD_HOME/$PERF_AVD.avd" ]] || { log "AVD $PERF_AVD does not exist (avdmanager create avd -n $PERF_AVD -k '$GB_EMULATOR_IMAGE' -d pixel_6)"; busy=1; }
  return $busy
}

# ---------------------------------------------------------------- build
stage_build() {
  if [[ "$SOAK_BUILD" == no && -f "$P/geekbridge-release.apk" && -x "$SVC_BIN" ]]; then log "build skipped (--no-build)"; return 0; fi
  mkdir -p "$P"
  log "release APK (R8) + desktop service via scripts/android-perf.sh size"
  scripts/android-perf.sh size >"$OUT/build-size.log" 2>&1 || { log "size stage failed, see $OUT/build-size.log"; return 1; }
  log "cargo build --release --locked -p bridge-service -p bridge-apps"
  CARGO_TARGET_DIR="$PERF_DESKTOP" cargo build --release --locked -p bridge-service -p bridge-apps >"$OUT/build-desktop.log" 2>&1 \
    || { log "desktop build failed, see $OUT/build-desktop.log"; return 1; }
  cp "$P/results.tsv" "$OUT/build-sizes.tsv" 2>/dev/null
  { echo "git: $(git rev-parse --short HEAD) + $(git status --short | wc -l) changed paths"; sha256sum "$P/geekbridge-release.apk" "$SVC_BIN"; } >"$OUT/build-info.txt"
}

# ---------------------------------------------------------------- helpers shared with the e2e scripts
set --
export E2E_SOURCE_ONLY=1
# shellcheck disable=SC1091
source scripts/e2e-android.sh
# e2e-android.sh logs into dist/android/logs; this run keeps its own.
log() { printf '[soak %s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log" >&2; }
rec() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:-}" >>"$OUT/e2e-results.tsv"; }
export ADB="adb -s $SERIAL"
PHONE_ID=""
T0=0
# A loaded host can make the app miss an input deadline: dismiss Android's "isn't responding" dialog (and count it).
wake() {
  $ADB shell input keyevent KEYCODE_WAKEUP; $ADB shell wm dismiss-keyguard >/dev/null 2>&1
  if timeout 40 "$UI" has "isn't responding" >/dev/null 2>&1; then
    timeout 40 "$UI" tap Wait >/dev/null 2>&1; echo "$(elapsed)" >>"$OUT/anr.log"; log "dismissed an ANR dialog"
  fi
}
msnow() { date +%s%3N; }
elapsed() { echo $((SECONDS - T0)); }
phone_id_now() { PHONE_ID="$(cat "$P/desk/phone-id" 2>/dev/null)"; }
desk_pid() { pgrep -f "^$SVC_BIN --state-dir $P/desk/state" | head -1; }
SCREEN_OFF_UNTIL=0

# ---------------------------------------------------------------- event + failure records
EV="$OUT/events.tsv" FAILS="$OUT/failures.log"
[[ -f "$EV" ]] || printf 'elapsed_s\tkind\tok\tlatency_ms\tdetail\n' >"$EV"
ev() { printf '%s\t%s\t%s\t%s\t%s\n' "$(elapsed)" "$1" "$2" "$3" "${4:-}" >>"$EV"; }
fail() { # kind detail  -> failures.log with the log tails from both sides
  local kind="$1" detail="$2"
  {
    echo "=== t+$(elapsed)s $(date +%T) $kind: $detail"
    echo "--- desktop service log (last 25 lines)"; tail -n 25 "$OUT/desktop-service.log" 2>/dev/null
    echo "--- phone logcat, app lines (last 25)"; grep -E 'geekbridge|GeekBridge|AndroidRuntime|FATAL' "$OUT/phone-logcat.log" 2>/dev/null | tail -n 25
    echo "--- link: $(ctl status 2>/dev/null | jget '[(l["phase"],l.get("peer","")) for l in d["links"]]')"
    echo
  } >>"$FAILS"
  log "FAIL $kind: $detail"
}

# ---------------------------------------------------------------- setup
start_xvfb_soak() {
  (setsid nohup Xvfb "$PERF_XDISP" -screen 0 800x600x24 -nolisten tcp >"$OUT/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${PERF_XDISP#:}"
}
start_service_soak() {
  mkdir -p "$P/desk/state" "$HOME_SCRATCH/Downloads"; chmod 700 "$P/desk/state"
  # No WAYLAND_DISPLAY / Hyprland: clipboard code uses xclip on the private Xvfb. HOME is a scratch dir.
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE HOME="$HOME_SCRATCH" DISPLAY="$PERF_XDISP" GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     setsid nohup "$SVC_BIN" --state-dir "$P/desk/state" --api-port "$PERF_API_PORT" --control-port "$PERF_CTRL_PORT" \
     --discovery-port "$PERF_DISC_PORT" --listen-inbound "127.0.0.1:$PERF_LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name Soak-Desktop >>"$OUT/desktop-service.log" 2>&1 &)
  wait_for 20 ctl status
}
# Write an Android SharedPreferences file as root (the release build is not debuggable, so no run-as).
write_pref_root() { # name  (xml on stdin)
  local uid; uid="$(A shell stat -c %u "/data/data/$PKG" | tr -d '\r')"
  A shell "mkdir -p /data/data/$PKG/shared_prefs && cat > /data/data/$PKG/shared_prefs/$1.xml && chown $uid:$uid /data/data/$PKG/shared_prefs/$1.xml && chmod 660 /data/data/$PKG/shared_prefs/$1.xml && restorecon /data/data/$PKG/shared_prefs/$1.xml"
}
inject_phone_data() {
  local people=("Ada Lovelace|+44 20 7946 0001" "Grace Hopper|+1 555 010 0002" "Alan Turing|+44 20 7946 0003") p
  for p in "${people[@]}"; do
    A shell "content insert --uri content://com.android.contacts/raw_contacts --bind account_type:s:LOCAL --bind account_name:s:gbtest" >/dev/null
    local rid; rid="$(A shell "content query --uri content://com.android.contacts/raw_contacts --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | sort -n | tail -1)"
    A shell "content insert --uri content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/name --bind data1:s:'${p%%|*}'" >/dev/null
    A shell "content insert --uri content://com.android.contacts/data --bind raw_contact_id:i:$rid --bind mimetype:s:vnd.android.cursor.item/phone_v2 --bind data1:s:'${p##*|}' --bind data2:i:2" >/dev/null
  done
  local cal; cal="$(A shell "content query --uri content://com.android.calendar/calendars --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  if [[ -z "$cal" ]]; then
    A shell "content insert --uri 'content://com.android.calendar/calendars?caller_is_syncadapter=true&account_name=gbtest&account_type=LOCAL' --bind account_name:s:gbtest --bind account_type:s:LOCAL --bind name:s:Personal --bind calendar_displayName:s:Personal --bind calendar_color:i:-16776961 --bind calendar_access_level:i:700 --bind visible:i:1 --bind sync_events:i:1 --bind ownerAccount:s:gbtest --bind calendar_timezone:s:UTC" >/dev/null
    cal="$(A shell "content query --uri content://com.android.calendar/calendars --projection _id" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  fi
  CAL_ID="$cal"
  local base i; base=$(( $(msnow) / 3600000 * 3600000 ))
  for i in 1 2 3; do
    A shell "content insert --uri content://com.android.calendar/events --bind calendar_id:i:$cal --bind title:s:'Soak event $i' --bind dtstart:l:$((base + i * 3600000)) --bind dtend:l:$((base + i * 3600000 + 1800000)) --bind eventTimezone:s:UTC --bind allDay:i:0" >/dev/null
  done
}
stage_setup_soak() {
  mkdir -p "$P"
  start_xvfb_soak || { log "Xvfb did not start"; return 1; }
  start_service_soak || { log "desktop service did not start"; return 1; }
  log "emulator $PERF_AVD ($SERIAL): boot + install (e2e env stage), release APK, setup wizard, pairing (e2e pair stage)"
  BUILD=no; mkdir -p "$P/desk"
  stage_env >"$OUT/setup.log" 2>&1 || { tail -n 5 "$OUT/setup.log" >&2; return 1; }
  A uninstall "$PKG" >/dev/null 2>&1; A install -r "$P/geekbridge-release.apk" 2>&1 | tail -1 | grep -q Success || { log "release APK install failed"; return 1; }
  A root >/dev/null 2>&1; sleep 2; wait_for 30 bash -c "adb -s $SERIAL shell id 2>/dev/null | grep -q uid=0"
  # The wizard's battery step is a system dialog that is slow on a loaded host: grant it with the shell instead and
  # walk the wizard with Skip/Next/Finish (scripts/e2e-android.sh covers the dialog itself).
  A shell cmd deviceidle whitelist +"$PKG" >/dev/null 2>&1
  for perm in POST_NOTIFICATIONS READ_CONTACTS READ_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG; do
    A shell pm grant "$PKG" "android.permission.$perm" >/dev/null 2>&1
  done
  wake; goto_home
  local k d
  for k in $(seq 1 30); do   # walk the wizard; an empty dump just means the app is still starting on a loaded host
    wake; d="$(ui dump 2>/dev/null | cut -f1)"
    if grep -q '^Step \|^Overview$' <<<"$d"; then
      tap Finish >/dev/null 2>&1 || tap Skip >/dev/null 2>&1 || tap Overview >/dev/null 2>&1 || tap Next >/dev/null 2>&1
    elif grep -q 'QUICK ACTIONS' <<<"$d"; then break
    else sleep 3; fi
  done
  for k in 1 2 3; do wake; goto_home; stage_pair >>"$OUT/setup.log" 2>&1 && break; $ADB shell am force-stop "$PKG"; sleep 3; done; tail -n 3 "$OUT/e2e-results.tsv" | tee -a "$OUT/run.log" >&2
  phone_id_now; [[ -n "$PHONE_ID" ]] || { log "no phone id after setup"; return 1; }
  desk_id
  A shell am force-stop "$PKG"; wake
  A root >/dev/null 2>&1; sleep 2
  A shell "id" | grep -q uid=0 || { log "adb root is unavailable"; return 1; }
  A shell svc power stayon true
  # Features the loop exercises: permissions, app switches, consents, share permissions.
  local perm
  for perm in READ_CONTACTS READ_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG POST_NOTIFICATIONS; do
    A shell pm grant "$PKG" "android.permission.$perm" >/dev/null 2>&1
  done
  inject_phone_data
  A shell am force-stop "$PKG"; sleep 1
  write_pref_root geekbridge_phonedata <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="contacts" value="true" />
    <boolean name="calendar" value="true" />
    <boolean name="sms" value="true" />
    <set name="calendar_ids"><string>${CAL_ID:-1}</string></set>
</map>
XML
  write_pref_root geekbridge_calls <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
</map>
XML
  wake; goto_home; sleep 3; ensure_link >/dev/null 2>&1 || true
  goto_home; sleep 2
  desk_dial; wait_for 25 is_connected || { log "link not connected after setup"; return 1; }
  local f; for f in clipboard contacts calendar sms telephony; do ctl consent "$PHONE_ID" "$f" on >/dev/null 2>&1; done
  ctl share allow "$PHONE_ID" on >/dev/null; ctl share auto-accept "$PHONE_ID" on >/dev/null
  phone_clip_sync_on || log "WARN: could not confirm Clipboard sync on the phone"
  log "negotiated: $(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  printf '' | DISPLAY="$PERF_XDISP" xclip -selection clipboard -i
}

# ---------------------------------------------------------------- measurement
phone_pid() { A shell pidof "$PKG" 2>/dev/null | tr -d '\r' | awk '{print $1}'; }
link_up() { [[ "$(ctl status 2>/dev/null | jget '" ".join(l["phase"] for l in d["links"])')" == *connected* ]]; }
APP_UID=""
sample() {
  local t pid dpid mi
  t=$(( $(date +%s) - EPOCH0 ))
  dpid="$(desk_pid)"
  local d_rss="" d_thr="" d_fd="" d_cpu=""
  if [[ -n "$dpid" && -r "/proc/$dpid/status" ]]; then
    d_rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$dpid/status")"; d_thr="$(awk '/^Threads:/ {print $2}' "/proc/$dpid/status")"
    d_fd="$(find "/proc/$dpid/fd" -mindepth 1 2>/dev/null | wc -l)"
    d_cpu="$(awk '{print $14+$15}' "/proc/$dpid/stat")"
  fi
  local p_pss="" p_java="" p_nat="" p_thr="" p_fd="" p_sock="" p_wl="" p_alarm="" p_screen="" p_rss=""
  pid="$(phone_pid)"
  [[ -n "$APP_UID" ]] || APP_UID="$(A shell stat -c %u "/data/data/$PKG" 2>/dev/null | tr -d '\r')"
  if [[ -n "$pid" ]]; then
    mi="$(timeout 20 adb -s "$SERIAL" shell dumpsys meminfo "$pid" 2>/dev/null)"
    p_pss="$(awk '/TOTAL PSS:/ {print $3}' <<<"$mi")"; p_rss="$(awk '/TOTAL PSS:/ {print $6}' <<<"$mi")"
    p_java="$(awk '/Java Heap:/ {print $3; exit}' <<<"$mi")"; p_nat="$(awk '/Native Heap:/ {print $3; exit}' <<<"$mi")"
    read -r p_thr p_fd p_sock < <(timeout 15 adb -s "$SERIAL" shell "echo \$(grep -m1 Threads /proc/$pid/status | cut -f2) \$(ls /proc/$pid/fd | wc -l) \$(ls -l /proc/$pid/fd | grep -c socket:)" 2>/dev/null | tr -d '\r')
  fi
  p_wl="$(timeout 20 adb -s "$SERIAL" shell dumpsys power 2>/dev/null | tr -d '\r' | grep -c "PARTIAL_WAKE_LOCK.*uid=${APP_UID:-x}")"
  p_alarm="$(timeout 20 adb -s "$SERIAL" shell dumpsys alarm 2>/dev/null | tr -d '\r' | grep -cE "^ +(RTC|ELAPSED)[_A-Z]* #[0-9]+: Alarm\{.*$PKG")"
  p_screen="$(timeout 10 adb -s "$SERIAL" shell dumpsys power 2>/dev/null | grep -m1 -o 'mWakefulness=[A-Za-z]*' | cut -d= -f2)"
  local up=0; link_up && up=1
  local derr dwarn perr
  derr="$(grep -ciE '(^|[ \[])ERROR([ \]:]|$)' "$OUT/desktop-service.log" 2>/dev/null)"
  dwarn="$(grep -ciE '(^|[ \[])WARN(ING)?([ \]:]|$)' "$OUT/desktop-service.log" 2>/dev/null)"
  perr="$(grep -cE '( E .*(geekbridge|GeekBridge)|FATAL EXCEPTION|AndroidRuntime|ANR in dev.geekbridge)' "$OUT/phone-logcat.log" 2>/dev/null)"
  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' "$t" "$dpid" "$d_rss" "$d_thr" "$d_fd" "$d_cpu" \
    "$pid" "$p_pss" "$p_rss" "$p_java" "$p_nat" "$p_thr" "$p_fd" "$p_sock" "$p_wl" "$p_alarm" "$up" "${p_screen:-}" "${derr:-0}/${dwarn:-0}" "${perr:-0}" >>"$OUT/samples.csv"
}
sampler_loop() {
  local next=0
  while true; do
    sample
    next=$((next + SAMPLE_EVERY)); local now=$(( $(date +%s) - EPOCH0 ))
    ((next > now)) && sleep $((next - now)) || next=$now
  done
}

# ---------------------------------------------------------------- loop tasks
TASK_N=0 TASK_MAX="${TASK_MAX:-420}"
declare -A RUNS
ensure_up() { # link connected, phone awake; redial otherwise. Prints nothing; returns 1 if the link cannot be restored
  link_up && return 0
  local t0; t0=$(msnow)
  wake; A shell am start -n "$PKG/.HomeActivity" >/dev/null 2>&1; sleep 2
  desk_dial; wait_for 25 is_connected && { ev recover 1 $(( $(msnow) - t0 )) "redial after unexpected loss"; return 0; }
  ev recover 0 $(( $(msnow) - t0 )) "redial failed"; fail recover "link could not be restored by a redial"; return 1
}
phone_clip_text() { # the phone clipboard through the app's own Paste button in Diagnostics (the only reader allowed in the foreground)
  goto_diag >/dev/null 2>&1; ui scrollto Paste >/dev/null 2>&1; tap Paste >/dev/null 2>&1; sleep 2; ui dump 2>/dev/null
}
t_clip() {
  local n=$TASK_N; ensure_up || { ev clip_d2p 0 0 "no link"; ev clip_p2d 0 0 "no link"; return; }
  wake
  # desktop -> phone
  local tok="soak-d2p-$n-$RANDOM" t0; t0=$(msnow)
  printf '%s' "$tok" | DISPLAY="$PERF_XDISP" xclip -selection clipboard -i
  local ok=0 i
  for i in 1 2 3 4; do
    sleep 4
    if phone_clip_text | grep -qF "$tok"; then ok=1; break; fi
  done
  ev clip_d2p $ok $(( $(msnow) - t0 )) "$tok"
  # The Diagnostics screen (where the phone clipboard is read) restarts the phone link without clipboard: redial from Home.
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1; goto_home; sleep 2; desk_dial; wait_for 20 is_connected
  ((ok)) || fail clip_d2p "phone clipboard never showed $tok"
  # phone -> desktop
  tok="soak-p2d-$n-$RANDOM"; t0=$(msnow); ok=0
  goto_home; phone_copy_nonsensitive "$tok" >/dev/null 2>&1; goto_home; sleep 2
  printf 'stale' | DISPLAY="$PERF_XDISP" xclip -selection clipboard -i
  pull_with_tap "$OUT/pull.out"
  [[ "$(desk_clip)" == "$tok" ]] && ok=1
  ev clip_p2d $ok $(( $(msnow) - t0 )) "$tok"
  ((ok)) || fail clip_p2d "desktop clipboard is '$(desk_clip)', wanted $tok; pull said: $(head -c 120 "$OUT/pull.out")"
}
rand_mb() { echo $(( FILE_MB_MIN + RANDOM % (FILE_MB_MAX - FILE_MB_MIN + 1) )); }
t_file() {
  local n=$TASK_N mb; mb="$(rand_mb)"; ensure_up || { ev file 0 0 "no link"; return; }
  wake; mkdir -p "$P/xfer"
  local name="soak-$n-${mb}mb"
  if ((n % 2)); then   # desktop -> phone, accepted on the phone
    local f="$P/xfer/$name.bin" dev="/sdcard/Download/GeekBridge/$name.bin"
    head -c $((mb * 1048576)) /dev/urandom >"$f"; local want; want="$(sha256sum "$f" | cut -d' ' -f1)"
    A shell "rm -f '$dev'" >/dev/null 2>&1; goto_home
    clear_dialogs
    local t0; t0=$(msnow)
    ctl send-file "$PHONE_ID" "$f" >/dev/null 2>&1
    local acc=0 k
    for k in 1 2 3 4 5 6; do
      if timeout 60 "$UI" has "wants to send a file"; then timeout 60 "$UI" tap Accept >/dev/null 2>&1 && { acc=1; break; }; fi; sleep 2
    done
    local have="" size=0 deadline=$((SECONDS + 240))
    while ((SECONDS < deadline)); do
      size="$(A shell stat -c %s "$dev" 2>/dev/null | tr -d '\r')"; size="${size:-0}"
      ((size >= mb * 1048576)) && break; sleep 3
    done
    local ms=$(( $(msnow) - t0 ))
    have="$(A shell sha256sum "$dev" 2>/dev/null | cut -d' ' -f1)"
    if [[ -n "$have" && "$have" == "$want" ]]; then ev file_d2p 1 "$ms" "${mb}MB accepted=$acc"
    else ev file_d2p 0 "$ms" "${mb}MB accepted=$acc size=$size"; fail file_d2p "$name: sha mismatch/missing (accepted=$acc size=$size/$((mb * 1048576)); transfer: $(ctl share 2>/dev/null | grep "$name" | tail -1))"; fi
    A shell "rm -f '$dev'" >/dev/null 2>&1; rm -f "$f"
  else                  # phone -> desktop through the Share sheet (the desktop auto-accepts)
    local f="$P/xfer/$name.png"
    { printf '\x89PNG\r\n\x1a\n'; head -c $((mb * 1048576)) /dev/urandom; } >"$f"
    local want; want="$(sha256sum "$f" | cut -d' ' -f1)"
    A shell "rm -f /sdcard/Download/$name.png" >/dev/null 2>&1
    A push "$f" "/sdcard/Download/$name.png" >/dev/null 2>&1
    A shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d "file:///sdcard/Download/$name.png" >/dev/null 2>&1; sleep 3
    local id; id="$(A shell "content query --uri content://media/external/images/media --projection _id --where \"_display_name='$name.png'\"" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
    if [[ -z "$id" ]]; then ev file_p2d 0 0 "MediaStore did not index $name"; fail file_p2d "MediaStore did not index $name.png"; rm -f "$f"; return; fi
    goto_home; clear_dialogs
    local t0; t0=$(msnow)
    A shell su 2000 am start -a android.intent.action.SEND -t image/png --eu android.intent.extra.STREAM "content://media/external/images/media/$id" --grant-read-uri-permission >/dev/null 2>&1   # as shell, not root: MediaProvider refuses a grant from uid 0
    sleep 3; tap "Send to computer" >/dev/null 2>&1; tap "Just once" >/dev/null 2>&1
    local got="" deadline=$((SECONDS + 240))
    while ((SECONDS < deadline)); do
      got="$(ls -t "$HOME_SCRATCH"/Downloads/GeekBridge/"$name"*.png 2>/dev/null | head -1)"
      [[ -n "$got" && "$(stat -c %s "$got")" -ge $((mb * 1048576)) ]] && break; sleep 3
    done
    local ms=$(( $(msnow) - t0 ))
    if [[ -n "$got" && "$(sha256sum "$got" | cut -d' ' -f1)" == "$want" ]]; then ev file_p2d 1 "$ms" "${mb}MB"
    else ev file_p2d 0 "$ms" "${mb}MB"; fail file_p2d "$name: not received/sha mismatch (transfer: $(ctl share 2>/dev/null | grep "$name" | tail -1))"; tap Cancel >/dev/null 2>&1; fi
    rm -f "$f" "$HOME_SCRATCH"/Downloads/GeekBridge/"$name"* ; A shell "rm -f /sdcard/Download/$name.png" >/dev/null 2>&1
    A shell am force-stop com.android.intentresolver >/dev/null 2>&1
  fi
}
t_flap() {
  local n=$TASK_N; ensure_up || { ev flap 0 0 "no link"; return; }
  local t0 t1 det
  if ((n % 2)); then   # clean: the desktop closes the link
    t0=$(msnow); ctl disconnect "$PHONE_ID" >/dev/null 2>&1
    wait_for 10 bash -c "! \"${CTL[0]}\" --state-dir \"$P/desk/state\" --control-port $PERF_CTRL_PORT status | grep -q connected"
    ev flap_close 1 $(( $(msnow) - t0 )) "ctl disconnect noticed"
    sleep 3
  else                 # silent: every packet to/from the guest is dropped (no FIN, no RST); measure the dead-link detection
    A shell "iptables -I INPUT -j DROP; iptables -I OUTPUT -j DROP" >/dev/null 2>&1
    t0=$(msnow); det=0
    for ((i = 0; i < 150; i++)); do link_up || { det=1; break; }; sleep 1; done
    if ((det)); then ev dead_link_detect 1 $(( $(msnow) - t0 )) "desktop dropped the black-holed link"
    else ev dead_link_detect 0 $(( $(msnow) - t0 )) "desktop still thought the link was up after 150 s"; fail dead_link "desktop still showed the black-holed link as connected after 150 s"; fi
    A shell "iptables -D INPUT -j DROP; iptables -D OUTPUT -j DROP" >/dev/null 2>&1
    sleep 2
  fi
  wake; A shell am start -n "$PKG/.HomeActivity" >/dev/null 2>&1; sleep 2
  t1=$(msnow); desk_dial
  if wait_for 30 is_connected; then ev reconnect 1 $(( $(msnow) - t1 )) "flap $n"
  else ev reconnect 0 $(( $(msnow) - t1 )) "flap $n"; fail reconnect "no link 30 s after the redial (flap $n)"; fi
}
modem_state() { A shell dumpsys telephony.registry 2>/dev/null | grep -m1 -o "mCallState=[0-9]" | cut -d= -f2; }
call_state_is() { ctl calls --reveal 2>/dev/null | tr -d '\n ' | grep -qi "$1"; }
t_call() {
  local n=$TASK_N num=5558801; ensure_up || { ev call 0 0 "no link"; return; }
  wake; A emu gsm cancel $num >/dev/null 2>&1
  local t0; t0=$(msnow)
  A emu gsm call $num >/dev/null
  if ! wait_for 25 call_state_is ringing; then ev call_ring 0 $(( $(msnow) - t0 )) "desktop never saw it ringing"; fail call_ring "no ringing call on the desktop: $(ctl calls --reveal 2>&1 | head -c 200)"; A emu gsm cancel $num >/dev/null 2>&1; return; fi
  ev call_ring 1 $(( $(msnow) - t0 )) "ringing on desktop"
  t0=$(msnow); ctl call answer >/dev/null 2>&1
  local ok=0 i; for ((i = 0; i < 24; i++)); do [[ "$(modem_state)" == 2 ]] && { ok=1; break; }; sleep 0.5; done
  ev call_answer $ok $(( $(msnow) - t0 )) "modem off-hook"
  ((ok)) || fail call_answer "answer did not take the modem off-hook (state $(modem_state))"
  sleep 5; t0=$(msnow); ctl call hangup >/dev/null 2>&1; ok=0
  for ((i = 0; i < 24; i++)); do [[ "$(modem_state)" == 0 ]] && { ok=1; break; }; sleep 0.5; done
  ev call_hangup $ok $(( $(msnow) - t0 )) "modem idle"
  ((ok)) || { fail call_hangup "hangup left the modem in state $(modem_state)"; A emu gsm cancel $num >/dev/null 2>&1; }
}
t_data() {
  local n=$TASK_N; ensure_up || { ev data 0 0 "no link"; return; }
  wake; local t0; t0=$(msnow)
  ctl agenda-refresh "$PHONE_ID" >/dev/null 2>&1
  local people=0 events=0 i
  for ((i = 0; i < 30; i++)); do
    ctl agenda >"$OUT/agenda.json" 2>/dev/null
    people="$(jget 'len(d["people"])' <"$OUT/agenda.json")"; events="$(jget 'len(d["events"])' <"$OUT/agenda.json")"
    ((${people:-0} >= 3 && ${events:-0} >= 1)) && break; sleep 2
  done
  if ((${people:-0} >= 3 && ${events:-0} >= 1)); then ev agenda_refresh 1 $(( $(msnow) - t0 )) "people=$people events=$events"
  else ev agenda_refresh 0 $(( $(msnow) - t0 )) "people=$people events=$events"; fail agenda_refresh "agenda after refresh: people=${people:-?} events=${events:-?}"; fi
  local tag="soaksms$n$RANDOM"; t0=$(msnow)
  A emu sms send 5550142 "Soak message $tag" >/dev/null
  local seen=0
  for ((i = 0; i < 30; i++)); do
    ctl messages "$PHONE_ID" 2>/dev/null | jget 'any("'"$tag"'" in c["last_body"] for c in d["conversations"])' | grep -q True && { seen=1; break; }; sleep 2
  done
  ev sms_inject $seen $(( $(msnow) - t0 )) "$tag"
  ((seen)) || fail sms_inject "SMS $tag never reached the desktop conversation list"
}
t_restart() {
  local n=$TASK_N; local t0; t0=$(msnow)
  A shell am force-stop "$PKG"; sleep 3
  wake; A shell am start -n "$PKG/.HomeActivity" >/dev/null 2>&1; sleep 4
  desk_dial
  if wait_for 40 is_connected; then ev app_restart 1 $(( $(msnow) - t0 )) "force-stop $n, trusted redial without a code"
  else ev app_restart 0 $(( $(msnow) - t0 )) "force-stop $n"; fail app_restart "link did not come back after a force-stop (phases: $(link_phases))"; fi
  for f in clipboard; do ctl consent "$PHONE_ID" $f on >/dev/null 2>&1; done
  phone_clip_sync_on >/dev/null 2>&1
}
t_idle() {
  local n=$TASK_N; local t0; t0=$(msnow)
  A shell input keyevent KEYCODE_SLEEP; sleep 2
  local up0=0 total=0 end=$((SECONDS + IDLE_SECS))
  while ((SECONDS < end)); do total=$((total + 1)); link_up && up0=$((up0 + 1)); sleep 5; done
  local alive=0; link_up && alive=1
  ev idle_screen_off $alive $(( $(msnow) - t0 )) "screen off ${IDLE_SECS}s, link up in $up0/$total probes"
  ((alive)) || fail idle_screen_off "link lost while the screen was off ($up0/$total probes up)"
  wake; sleep 2
}

# ---------------------------------------------------------------- scheduler
declare -A NEXT EVERY
schedule() { NEXT[$1]=$2; EVERY[$1]=$3; }
run_loop() {
  schedule clip 15 "$CLIP_EVERY"; schedule file 60 "$FILE_EVERY"; schedule flap 150 "$FLAP_EVERY"
  schedule data 240 "$DATA_EVERY"; schedule call 420 "$CALL_EVERY"; schedule restart $((RESTART_EVERY / 2)) "$RESTART_EVERY"
  schedule idle "$IDLE_EVERY" "$IDLE_EVERY"
  local order=(idle restart call flap data file clip) name
  while (($(elapsed) < DURATION)); do
    local ran=0
    for name in "${order[@]}"; do
      (($(elapsed) >= DURATION)) && break
      if (($(elapsed) >= NEXT[$name])); then
        log "task $name at t+$(elapsed)s"
        RUNS[$name]=$(( ${RUNS[$name]:-0} + 1 )); TASK_N=${RUNS[$name]}
        ( "t_$name" ) & local tp=$! tstart=$SECONDS
        while kill -0 "$tp" 2>/dev/null; do
          if ((SECONDS - tstart > TASK_MAX)); then
            pkill -KILL -P "$tp" 2>/dev/null; kill -KILL "$tp" 2>/dev/null
            ev "$name" 0 $(( (SECONDS - tstart) * 1000 )) "task exceeded the ${TASK_MAX}s watchdog"; fail "$name" "stuck: killed by the ${TASK_MAX}s task watchdog"
            A shell "iptables -D INPUT -j DROP; iptables -D OUTPUT -j DROP" >/dev/null 2>&1; wake; A shell input keyevent KEYCODE_HOME >/dev/null 2>&1; break
          fi
          sleep 1
        done
        wait "$tp" 2>/dev/null
        local now; now=$(elapsed)
        NEXT[$name]=$(( NEXT[$name] + EVERY[$name] ))
        ((NEXT[$name] <= now)) && NEXT[$name]=$((now + EVERY[$name] / 2))   # fell behind: skip the missed runs
        ran=1
      fi
    done
    ((ran)) || sleep 2
  done
}

# ---------------------------------------------------------------- main
main() {
  trap teardown EXIT
  trap 'exit 130' INT TERM
  preflight || { trap - EXIT; return 2; }
  log "soak start: DURATION=${DURATION}s out=$OUT avd=$PERF_AVD serial=$SERIAL ctrl=$PERF_CTRL_PORT xdisp=$PERF_XDISP"
  stage_build || return 1
  stage_setup_soak || { log "setup failed, see $OUT/setup.log"; return 1; }
  printf 'elapsed_s,desk_pid,desk_rss_kb,desk_threads,desk_fds,desk_cpu_ticks,ph_pid,ph_pss_kb,ph_rss_kb,ph_java_kb,ph_native_kb,ph_threads,ph_fds,ph_sockets,ph_wakelocks,ph_alarms,link_up,screen,desk_err_warn,ph_log_err\n' >"$OUT/samples.csv"
  (adb -s "$SERIAL" logcat -c; exec adb -s "$SERIAL" logcat -v threadtime '*:W') >"$OUT/phone-logcat.log" 2>&1 &
  BG_PIDS+=($!)
  EPOCH0=$(date +%s); T0=$SECONDS
  sampler_loop & BG_PIDS+=($!)
  log "loop: ${DURATION}s; clip/${CLIP_EVERY} file/${FILE_EVERY} flap/${FLAP_EVERY} call/${CALL_EVERY} data/${DATA_EVERY} restart/${RESTART_EVERY} idle/${IDLE_EVERY}x${IDLE_SECS}"
  run_loop
  log "loop finished after $(elapsed)s"
  sleep 3
  sample   # a final sample after the last task
  kill "${BG_PIDS[@]}" 2>/dev/null; BG_PIDS=()
  python3 -I "$ROOT/scripts/soak-report.py" "$OUT" >>"$OUT/run.log" 2>&1 || log "report generation failed"
  log "report: $OUT/report.md"
}
main
