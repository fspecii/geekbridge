#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Manage the dedicated headless Android emulator (AVD "gbcall") that provides the simulated
# cellular modem for the GeekBridge emulated-call checks.
#
#   scripts/emulator-call.sh create-avd [--recreate]
#                                      create AVD "gbcall" (idempotent); --recreate replaces it
#   scripts/emulator-call.sh start     boot the emulator (idempotent; creates the AVD if missing);
#                                      returns when sys.boot_completed=1
#   scripts/emulator-call.sh stop      stop the emulator this script started (idempotent)
#   scripts/emulator-call.sh status    print STATUS; exit 0 only when ready
#   scripts/emulator-call.sh wait      block until ready (exit 0), failed (exit 1) or timeout (exit 2)
#   scripts/emulator-call.sh env       print shell exports (eval "$(scripts/emulator-call.sh env)")
#   scripts/emulator-call.sh state     one-line machine-parseable call state of the emulated modem
#   scripts/emulator-call.sh adb ARGS  adb -s <serial> ARGS on the dedicated adb server
#
# Only simulated cellular call signalling is available. The emulator has no Bluetooth HFP audio
# path; nothing here exercises Bluetooth pairing, SCO audio or a real phone.
#
# STATUS file (.tools/gbcall/STATUS, one line, polled by other tools):
#   booting | ready | failed: <reason>
# `ready` is written only after `adb -s <serial> shell getprop sys.boot_completed` printed 1.
#
# Environment (all optional):
#   GB_EMU_AVD=gbcall            AVD name
#   GB_EMU_SERIAL=emulator-5584  adb serial; the number is the console port (adb port is +1)
#   GB_EMU_ADB_PORT=16037        dedicated adb server port (never the default 5037)
#   GB_EMU_BOOT_TIMEOUT=420      seconds to wait for sys.boot_completed per attempt
#   GB_EMU_ATTEMPTS=3            launch attempts when the emulator process dies during boot
#   GB_EMU_TELEPHONY_WAIT=90     extra seconds to wait for the SIM (non-fatal)
#   GB_EMU_STATE_DIR=.tools/gbcall  where STATUS, emulator.pid, emulator.log and the lock live
#   GB_EMU_UNIT=gbcall-emulator  name of the transient systemd user unit (see below)
#   GB_EMU_EXTRA=""              extra emulator flags, e.g. "-memory 2048 -cores 2"
#
# A clean checkout needs: scripts/bootstrap-android.sh --with-emulator (SDK, emulator and the system
# image $GB_EMULATOR_IMAGE), then `scripts/emulator-call.sh start` (which runs create-avd if needed).
#
# Why a systemd user unit: a process that listens on localhost inside an agent's process tree is
# port-probed by the host tooling (an HTTP GET from a node client). The emulator's modem listens on
# a random localhost port and crashes (SIGSEGV after "Error reading from client fd") when such a
# connection is opened and closed repeatedly. Launching in its own transient unit
# (systemd-run --user) keeps it out of that process tree. Without systemd the script falls back to
# setsid + nohup and prints a warning. See docs/emulated-calls.md.
#
# Safety: `stop` only ever signals the PID recorded in emulator.pid, and only after checking that
# /proc/<pid>/cmdline is this AVD's emulator. It never uses pkill/killall and never touches another
# emulator or adb server. It installs nothing.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
AVD="${GB_EMU_AVD:-gbcall}"
SERIAL="${GB_EMU_SERIAL:-emulator-5584}"
CONSOLE_PORT="${SERIAL#emulator-}"
ADB_PORT="${GB_EMU_ADB_PORT:-16037}"
BOOT_TIMEOUT="${GB_EMU_BOOT_TIMEOUT:-420}"
ATTEMPTS="${GB_EMU_ATTEMPTS:-3}"
TELEPHONY_WAIT="${GB_EMU_TELEPHONY_WAIT:-90}"
STATE_DIR="${GB_EMU_STATE_DIR:-$ROOT/.tools/gbcall}"
UNIT="${GB_EMU_UNIT:-gbcall-emulator}"
STATUS_FILE="$STATE_DIR/STATUS"
PID_FILE="$STATE_DIR/emulator.pid"
LOG_FILE="$STATE_DIR/emulator.log"
LOCK_FILE="$STATE_DIR/emulator.lock"
TMP_DIR="${GB_EMU_TMPDIR:-$ROOT/.tools/gbcall/tmp}"

mkdir -p "$STATE_DIR" "$TMP_DIR"

say() { printf 'emulator-call: %s\n' "$*" >&2; }

# ---- toolchain (project-local, nothing global) -------------------------------------------------
# shellcheck disable=SC1091
source "$ROOT/scripts/android-env.sh" >/dev/null 2>&1 || true
export ANDROID_ADB_SERVER_PORT="$ADB_PORT"
ADB_BIN="$ANDROID_HOME/platform-tools/adb"
[ -x "$ADB_BIN" ] || ADB_BIN="$(command -v adb || true)"
EMU_BIN="$ANDROID_HOME/emulator/emulator"

adbc() { "$ADB_BIN" -s "$SERIAL" "$@"; }
adbt() { timeout "${ADB_TIMEOUT:-10}" "$ADB_BIN" -s "$SERIAL" "$@"; }

set_status() {
  # Atomic one-line write so pollers never see a torn file.
  local tmp="$STATE_DIR/.STATUS.$$"
  printf '%s\n' "$1" > "$tmp" && mv -f "$tmp" "$STATUS_FILE"
}

# ---- process helpers ---------------------------------------------------------------------------
pid_is_ours() {
  # true if $1 is a live emulator process for this AVD and console port
  local pid="${1:-}" cmd
  [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] || return 1
  cmd="$({ tr '\0' ' ' < "/proc/$pid/cmdline"; } 2>/dev/null)" || return 1
  case "$cmd" in
    *"-avd $AVD "*"-port $CONSOLE_PORT "*) return 0 ;;
  esac
  return 1
}

recorded_pid() { [ -r "$PID_FILE" ] && tr -dc '0-9' < "$PID_FILE" || true; }

adopt_unit_pid() {
  # If the pid file is missing or stale but our systemd unit runs this AVD's emulator, record it.
  local pid up
  pid="$(recorded_pid)"
  { [ -n "$pid" ] && pid_is_ours "$pid"; } && return 0
  use_systemd || return 0
  up="$(unit_pid)"
  if [ -n "$up" ] && [ "$up" != 0 ] && pid_is_ours "$up"; then
    printf '%s\n' "$up" > "$PID_FILE"
  fi
}

our_pid_alive() {
  local pid
  adopt_unit_pid
  pid="$(recorded_pid)"
  [ -n "$pid" ] && pid_is_ours "$pid"
}

booted() {
  [ "$(adbt shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n ')" = "1" ]
}

use_systemd() {
  command -v systemd-run >/dev/null 2>&1 && command -v systemctl >/dev/null 2>&1 \
    && systemctl --user show-environment >/dev/null 2>&1
}

unit_pid() { systemctl --user show -p MainPID --value "$UNIT" 2>/dev/null | tr -dc '0-9'; }

# ---- failure diagnosis from the log ------------------------------------------------------------
diagnose_exit() {
  local why=""
  if [ -r "$LOG_FILE" ]; then
    if grep -qiE 'quota exceeded|No space left' "$LOG_FILE"; then
      why="disk quota or no space left (check df of /tmp and $TMP_DIR)"
    elif grep -qE 'no stack mapping|Error reading from client fd' "$LOG_FILE"; then
      why="qemu crashed (SIGSEGV); known emulator bug when the modem port is port-probed; see docs/emulated-calls.md and coredumpctl list"
    elif grep -qE '^unknown option' "$LOG_FILE"; then
      why="$(grep -E '^unknown option' "$LOG_FILE" | tail -n 1)"
    elif grep -qiE 'PANIC|FATAL|ERROR *\|' "$LOG_FILE"; then
      why="$(grep -iE 'PANIC|FATAL|ERROR *\|' "$LOG_FILE" | tail -n 1 | cut -c1-160)"
    fi
  fi
  [ -n "$why" ] || why="emulator process exited during boot; see $LOG_FILE"
  printf '%s' "$why"
}

# ---- launch ------------------------------------------------------------------------------------
launch() {
  local wipe="$1" args=()
  [ "$wipe" = 1 ] && args+=(-wipe-data)
  export GB_EMU_AVD="$AVD" GB_EMU_PORT="$CONSOLE_PORT" GB_EMU_ADB_PORT="$ADB_PORT" GB_EMU_TMPDIR="$TMP_DIR"
  rm -f "$PID_FILE"
  if use_systemd; then
    # A leftover unit with this name that is not our live emulator is stale; replace it.
    if systemctl --user is-active --quiet "$UNIT" 2>/dev/null; then
      systemctl --user stop "$UNIT" 2>/dev/null || true
    fi
    systemctl --user reset-failed "$UNIT" 2>/dev/null || true
    systemd-run --user --quiet --unit="$UNIT" --collect \
      --description="GeekBridge $AVD emulator" \
      -p "StandardOutput=truncate:$LOG_FILE" -p StandardError=inherit \
      -p KillMode=control-group -p TimeoutStopSec=20 \
      --setenv=GB_EMU_AVD --setenv=GB_EMU_PORT --setenv=GB_EMU_ADB_PORT --setenv=GB_EMU_TMPDIR \
      --setenv=GB_EMU_EXTRA="${GB_EMU_EXTRA:-}" \
      "$ROOT/scripts/emulator-call-launch.sh" "${args[@]}" 9>&- || return 1
    local i pid=""
    for i in $(seq 1 50); do
      pid="$(unit_pid)"
      [ -n "$pid" ] && [ "$pid" != 0 ] && break
      sleep 0.2
    done
    [ -n "$pid" ] && [ "$pid" != 0 ] || return 1
    printf '%s\n' "$pid" > "$PID_FILE"
  else
    say "warning: no systemd user session; falling back to setsid/nohup (the emulator may be killed by host port-probing, see docs/emulated-calls.md)"
    : > "$LOG_FILE"
    setsid nohup "$ROOT/scripts/emulator-call-launch.sh" "${args[@]}" >> "$LOG_FILE" 2>&1 < /dev/null 9>&- &
    printf '%s\n' "$!" > "$PID_FILE"
  fi
  return 0
}

# Wait for boot. Echo nothing; return 0 booted, 1 process died, 2 timeout.
wait_boot() {
  local deadline=$(( SECONDS + BOOT_TIMEOUT )) pid
  while [ "$SECONDS" -lt "$deadline" ]; do
    if booted; then return 0; fi
    pid="$(recorded_pid)"
    if [ -z "$pid" ] || ! pid_is_ours "$pid"; then
      # The pid file may lag a moment behind a fallback launch; re-check once.
      sleep 1
      pid="$(recorded_pid)"
      { [ -n "$pid" ] && pid_is_ours "$pid"; } || return 1
    fi
    sleep 3
  done
  return 2
}

wait_telephony() {
  # Non-fatal extra settle: the SIM is READY/LOADED a little after sys.boot_completed.
  local deadline=$(( SECONDS + TELEPHONY_WAIT ))
  while [ "$SECONDS" -lt "$deadline" ]; do
    case "$(adbt shell getprop gsm.sim.state 2>/dev/null | tr -d "\r\n ")" in READY|LOADED) return 0 ;; esac
    sleep 3
  done
  return 1
}

avd_exists() { [ -r "${ANDROID_AVD_HOME:-/nonexistent}/$AVD.ini" ] && [ -d "${ANDROID_AVD_HOME}/$AVD.avd" ]; }

# Create the AVD from the project-local SDK. Idempotent; $1 = recreate (0|1). Returns 0 on success.
create_avd() {
  local recreate="${1:-0}" avdm="$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager"
  local image="${GB_EMULATOR_IMAGE:-system-images;android-36;google_apis;x86_64}"
  local imgdir="$ANDROID_HOME/$(printf '%s' "$image" | tr ';' '/')"
  if avd_exists && [ "$recreate" != 1 ]; then
    say "AVD '$AVD' already exists; nothing to do (use --recreate to replace it)"
    return 0
  fi
  if [ ! -x "$EMU_BIN" ] || [ ! -x "$avdm" ] || [ ! -d "$imgdir" ]; then
    say "cannot create AVD '$AVD': missing project-local emulator, avdmanager or system image '$image'"
    say "run: scripts/bootstrap-android.sh --with-emulator"
    return 1
  fi
  if [ "$recreate" = 1 ] && our_pid_alive; then
    say "refusing to recreate AVD '$AVD' while its emulator is running; run '$0 stop' first"
    return 1
  fi
  local force=()
  [ "$recreate" = 1 ] && force=(--force)
  mkdir -p "$ANDROID_AVD_HOME"
  say "creating AVD '$AVD' (image $image, device pixel_6)"
  # `no` answers avdmanager's "custom hardware profile?" prompt.
  if ! printf 'no\n' | JAVA_HOME="$JAVA_HOME" "$avdm" create avd -n "$AVD" -k "$image" -d pixel_6 "${force[@]}" >"$STATE_DIR/create-avd.log" 2>&1; then
    say "avdmanager failed; see $STATE_DIR/create-avd.log"
    tail -n 5 "$STATE_DIR/create-avd.log" >&2
    return 1
  fi
  # Headless call testing: modest resources, no device frame.
  local cfg="$ANDROID_AVD_HOME/$AVD.avd/config.ini" k v
  if [ -w "$cfg" ]; then
    for kv in hw.ramSize=2G hw.cpu.ncore=4 showDeviceFrame=no; do
      k="${kv%%=*}"; v="${kv#*=}"
      if grep -q "^$k=" "$cfg"; then sed -i "s|^$k=.*|$k=$v|" "$cfg"; else printf '%s=%s\n' "$k" "$v" >> "$cfg"; fi
    done
  fi
  avd_exists || { say "avdmanager reported success but AVD '$AVD' is missing"; return 1; }
  say "AVD '$AVD' created"
}

cmd_create_avd() {
  local recreate=0
  case "${1:-}" in
    "") ;;
    --recreate) recreate=1 ;;
    *) say "usage: $0 create-avd [--recreate]"; return 2 ;;
  esac
  exec 9> "$LOCK_FILE"
  flock -w 120 9 || { say "could not get lock $LOCK_FILE"; return 1; }
  create_avd "$recreate"
}

preflight() {
  [ -x "$EMU_BIN" ] || { set_status "failed: emulator binary missing at $EMU_BIN (run scripts/bootstrap-android.sh)"; return 1; }
  [ -x "$ADB_BIN" ] || { set_status "failed: adb not found"; return 1; }
  if ! avd_exists; then
    say "AVD '$AVD' not found; creating it"
    create_avd 0 || { set_status "failed: AVD '$AVD' missing and could not be created (run scripts/bootstrap-android.sh --with-emulator)"; return 1; }
  fi
  if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    set_status "failed: /dev/kvm not accessible (hardware acceleration required; add user to the kvm group)"; return 1
  fi
  local avail
  avail="$(df -Pk "$TMP_DIR" | awk 'NR==2{print int($4/1024)}')"
  if [ -n "$avail" ] && [ "$avail" -lt 1024 ]; then
    set_status "failed: only ${avail} MiB free for TMPDIR $TMP_DIR (need >= 1024)"; return 1
  fi
  if ss -ltn 2>/dev/null | awk -v p="$CONSOLE_PORT" '$4 ~ (":" p "$") {f=1} END{exit !f}'; then
    if ! our_pid_alive; then
      set_status "failed: console port $CONSOLE_PORT is in use by another process (not started by this script)"; return 1
    fi
  fi
  return 0
}

# ---- subcommands -------------------------------------------------------------------------------
cmd_start() {
  exec 9> "$LOCK_FILE"
  flock -w $(( BOOT_TIMEOUT * ATTEMPTS + 60 )) 9 || { say "could not get lock $LOCK_FILE"; return 1; }

  # Idempotent: already up?
  if our_pid_alive; then
    if booted; then
      set_status ready
      say "already running and booted (pid $(recorded_pid), $SERIAL)"
      return 0
    fi
    say "emulator already starting (pid $(recorded_pid)); waiting for boot"
    set_status booting
    if wait_boot; then
      set_status ready; say "ready ($SERIAL)"; return 0
    fi
    # fall through to a fresh attempt below
  fi

  set_status booting
  preflight || { say "$(cat "$STATUS_FILE")"; return 1; }
  "$ADB_BIN" start-server >/dev/null 2>&1 || true

  local attempt=1 wipe=0 rc why
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    set_status booting
    say "launching AVD '$AVD' on $SERIAL (attempt $attempt/$ATTEMPTS, adb server port $ADB_PORT)"
    if ! launch "$wipe"; then
      set_status "failed: could not start the emulator process (see $LOG_FILE)"
      say "$(cat "$STATUS_FILE")"; return 1
    fi
    wait_boot; rc=$?
    if [ "$rc" = 0 ]; then
      set_status ready
      say "ready after boot_completed=1 ($SERIAL)"
      if ! wait_telephony; then
        say "note: gsm.sim.state is not READY/LOADED yet; telephony may need a few more seconds"
      fi
      return 0
    fi
    if [ "$rc" = 1 ]; then
      why="$(diagnose_exit)"
      say "attempt $attempt: emulator died during boot: $why"
      attempt=$(( attempt + 1 ))
      wipe=1   # a crashed boot can leave a half-written userdata overlay
      [ "$attempt" -le "$ATTEMPTS" ] && continue
      set_status "failed: $why"
      return 1
    fi
    # timeout: process alive but not booted
    set_status "failed: boot timeout after ${BOOT_TIMEOUT}s (emulator pid $(recorded_pid) still running; see $LOG_FILE)"
    say "$(cat "$STATUS_FILE")"
    return 1
  done
  return 1
}

cmd_stop() {
  exec 9> "$LOCK_FILE"
  flock -w 120 9 || { say "could not get lock $LOCK_FILE"; return 1; }
  local pid
  pid="$(recorded_pid)"
  if [ -z "$pid" ]; then
    say "no recorded emulator pid; nothing to stop"
    set_status "failed: stopped (not running)"
    return 0
  fi
  if ! pid_is_ours "$pid"; then
    say "recorded pid $pid is not a live '$AVD' emulator; clearing stale pid file (nothing signalled)"
    rm -f "$PID_FILE"
    set_status "failed: stopped (not running)"
    return 0
  fi
  say "stopping emulator pid $pid ($SERIAL)"
  # 1) graceful, via the console
  ADB_TIMEOUT=10 adbt emu kill >/dev/null 2>&1 || true
  local i
  for i in $(seq 1 20); do pid_is_ours "$pid" || break; sleep 1; done
  # 2) our own systemd unit, if any
  if pid_is_ours "$pid" && use_systemd && [ "$(unit_pid)" = "$pid" ]; then
    systemctl --user stop "$UNIT" 2>/dev/null || true
    for i in $(seq 1 15); do pid_is_ours "$pid" || break; sleep 1; done
  fi
  # 3) the recorded pid only
  if pid_is_ours "$pid"; then
    kill -TERM "$pid" 2>/dev/null || true
    for i in $(seq 1 10); do pid_is_ours "$pid" || break; sleep 1; done
  fi
  if pid_is_ours "$pid"; then
    kill -KILL "$pid" 2>/dev/null || true
    sleep 1
  fi
  if pid_is_ours "$pid"; then
    say "failed to stop pid $pid"
    return 1
  fi
  rm -f "$PID_FILE"
  set_status "failed: stopped (not running)"
  say "stopped"
  return 0
}

refresh_status() {
  # Reconcile STATUS with reality without ever launching anything.
  local cur
  cur="$(cat "$STATUS_FILE" 2>/dev/null || true)"
  if our_pid_alive; then
    if booted; then
      [ "$cur" = ready ] || set_status ready
    else
      case "$cur" in booting) ;; *) set_status booting ;; esac
    fi
  else
    case "$cur" in
      failed:*) ;;
      *) set_status "failed: emulator process not running (run scripts/emulator-call.sh start)" ;;
    esac
  fi
}

cmd_status() {
  refresh_status
  local s
  s="$(cat "$STATUS_FILE" 2>/dev/null || echo 'failed: no status')"
  printf '%s\n' "$s"
  [ "$s" = ready ]
}

cmd_wait() {
  local deadline=$(( SECONDS + ${1:-$BOOT_TIMEOUT} )) s
  while :; do
    refresh_status
    s="$(cat "$STATUS_FILE" 2>/dev/null || true)"
    case "$s" in
      ready) printf 'ready\n'; return 0 ;;
      failed:*) printf '%s\n' "$s"; return 1 ;;
    esac
    [ "$SECONDS" -lt "$deadline" ] || { printf 'timeout waiting for ready (status: %s)\n' "$s"; return 2; }
    sleep 2
  done
}

cmd_env() {
  printf 'export ANDROID_ADB_SERVER_PORT=%q\n' "$ADB_PORT"
  printf 'export GB_EMU_SERIAL=%q\n' "$SERIAL"
  printf 'export GB_EMU_ADB_PORT=%q\n' "$ADB_PORT"
  printf 'export ANDROID_SERIAL=%q\n' "$SERIAL"
  printf 'export ADB=%q\n' "$ADB_BIN"
  printf 'export PATH=%q\n' "$(dirname "$ADB_BIN"):$ANDROID_HOME/emulator:$PATH"
}

# One-line call state of the emulated modem, from `dumpsys telephony.registry`.
# Output:  STATE=<S> NUMBER=<n> CALLSTATE=<0|1|2> RING=<r> FG=<f> BG=<b>
#   S = IDLE | RINGING | WAITING | CONNECTING | DIALING | ALERTING | ACTIVE | HELD | ENDING
#   NUMBER is non-empty only while an inbound call is RINGING (Android clears it at answer).
# Exit 0 on success, 3 if the device is unreachable.
parse_state() {
  awk '
    /^ *mCallState=/ && cs=="" { sub(/^ *mCallState=/, ""); cs=$0 }
    /^ *mCallIncomingNumber=/ && !seenn { seenn=1; sub(/^ *mCallIncomingNumber=/, ""); num=$0 }
    /^ *mPreciseCallState=/ && ring=="" {
      s=$0
      if (match(s, /Ringing call state: -?[0-9]+/))    { t=substr(s,RSTART,RLENGTH); sub(/.*: /,"",t); ring=t }
      if (match(s, /Foreground call state: -?[0-9]+/)) { t=substr(s,RSTART,RLENGTH); sub(/.*: /,"",t); fg=t }
      if (match(s, /Background call state: -?[0-9]+/)) { t=substr(s,RSTART,RLENGTH); sub(/.*: /,"",t); bg=t }
    }
    END {
      if (cs=="") cs=-1; if (ring=="") ring=-1; if (fg=="") fg=-1; if (bg=="") bg=-1
      gsub(/[ \t]/, "", num)
      st="UNKNOWN"
      if (cs==0) st="IDLE"                                  # fg=7 lingers after a hangup; ignore it
      else if (cs==1) { st = (ring==6 || fg==1 || fg==3 || fg==4 || bg==2) ? "WAITING" : (ring==8 ? "ENDING" : "RINGING") }
      else if (cs==2) {
        if (ring==5 && fg<=0)      st="CONNECTING"          # answering: OFFHOOK but nothing active yet
        else if (fg==3)            st="DIALING"
        else if (fg==4)            st="ALERTING"
        else if (fg==1)            st="ACTIVE"
        else if (fg==7 || fg==8)   st="ENDING"
        else if (bg==2)            st="HELD"
        else                       st="CONNECTING"          # OFFHOOK with fg=0: dial/answer in flight
      }
      printf "STATE=%s NUMBER=%s CALLSTATE=%s RING=%s FG=%s BG=%s\n", st, num, cs, ring, fg, bg
    }'
}

cmd_state() {
  local raw
  raw="$(ADB_TIMEOUT=15 adbt shell 'dumpsys telephony.registry | grep -E "^ +(mCallState|mCallIncomingNumber|mPreciseCallState)="' 2>/dev/null | tr -d '\r')" || true
  if [ -z "$raw" ]; then
    echo "STATE=UNKNOWN NUMBER= CALLSTATE=-1 RING=-1 FG=-1 BG=-1"
    return 3
  fi
  printf '%s\n' "$raw" | parse_state
}

cmd_adb() { exec "$ADB_BIN" -s "$SERIAL" "$@"; }

usage() { sed -n '2,/^# Only simulated/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

case "${1:-}" in
  create-avd) shift; cmd_create_avd "$@" ;;
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  status) cmd_status ;;
  wait)   shift; cmd_wait "$@" ;;
  env)    cmd_env ;;
  state)  cmd_state ;;
  adb)    shift; cmd_adb "$@" ;;
  -h|--help|help|"") usage; [ -n "${1:-}" ] || exit 2 ;;
  *) say "unknown subcommand '$1'"; usage; exit 2 ;;
esac
