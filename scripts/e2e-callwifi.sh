#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# EXPERIMENT (debug phone build only): the whole Wi-Fi call audio path, linked. An Android emulator running the debug
# app is PAIRED with a real geekbridge-service whose sound server is a PRIVATE PipeWire (+ pipewire-pulse, two null
# sinks standing in for the headset), then:
#   listener   the app's one-time helper listener: off by default, closes on switch-off and on STOP, has ONE overall
#              90 s deadline even while a local client keeps connecting (security review 8, fix 3)
#   enable     consent on the computer, grant on the phone, switch on, helper started by scripts/callaudio-enable.sh
#   control    the no-call self-test control, then the in-call step on a simulated call (adb emu gsm)
#   listen     Listen from the computer: session live, the far party's tone (the helper's synthetic source) comes out
#              of the private headset, phone shows the indicator; Stop from the computer ends it
#   linkloss   dropping the link ends the session on both sides
#   notif      GeekBridge notifications blocked -> the phone refuses to share (review 8, fix 4)
#
#   scripts/e2e-callwifi.sh [STAGE[,STAGE...]] [--stop]       stages: env battery pair callwifi (default: all)
#   GB_CW_FRESH=1 deletes the AVD clone at the end (default keeps it so a re-run is quick; --stop stops everything)
#
# Isolation: its own AVD clone (gbcw, console port 5662, adb server 16150), service state under $SCRATCH, control port
# 17591, Xvfb :89, a private PipeWire in $SCRATCH/pw. Never the user's config, session, ports 1716/17381/8000, or
# /tmp (TMPDIR below). The emulator has no real call audio: the phone helper's TONE source plays the far party, and
# the self-test verdict is set through a debug hook because the emulator's call sources sound with no call (the real
# self-test says "unsure" there, which is checked and reported). Results: dist/android/logs/e2e-callwifi-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
STAGES="${1:-env,battery,pair,callwifi}"
[[ "$STAGES" == --stop ]] && { STAGES=""; STOP=1; }
set --
SCR="${GB_CW_SCRATCH:-$HOME/.cache/gb-callwifi-linked}"
export TMPDIR="${TMPDIR:-$HOME/.cache/gb-tmp}"
[[ "$TMPDIR" == /tmp* ]] && export TMPDIR="$HOME/.cache/gb-tmp"
mkdir -p "$SCR" "$TMPDIR"
export GB_BASE_AVD="${GB_BASE_AVD:-gbhome}"
export E2E_SOURCE_ONLY=1 E2E_AVD=gbcw E2E_EMU_PORT=5662 E2E_ADB_PORT=16150 E2E_TAG=-cw \
  E2E_RUN="$SCR/run" E2E_SCRATCH="$SCR/android" E2E_TARGET="$SCR/t" \
  E2E_RESULTS="$ROOT/dist/android/logs/e2e-callwifi-results.tsv" E2E_XDISP=:89 \
  E2E_CTRL_PORT=17591 E2E_API_PORT=17590 E2E_DISC_PORT=17526 E2E_LISTEN_PORT=1733 E2E_REDIR_PORT=17181
# shellcheck disable=SC1091
source scripts/e2e-android.sh
BUILD=no
RT="$SCR/pw"          # the private PipeWire runtime dir (short enough for a unix socket path)
PWPIDS="$SCR/pw.pids"
DEBUG_BIN="${GB_CW_BIN:-$ROOT/target/debug}"
CW_APK="${GB_CW_APK:-$ROOT/android/app/build/outputs/apk/debug/app-debug.apk}"

# ---------------------------------------------------------------- private sound server
pw_env() { env XDG_RUNTIME_DIR="$RT" PIPEWIRE_RUNTIME_DIR="$RT" PIPEWIRE_REMOTE=pipewire-0 PULSE_SERVER="unix:$RT/pulse/native" "$@"; }
pw_nid() { pw_env pw-cli ls Node | awk -v n="\"$1\"" '/^\tid/{i=$2} $0 ~ "node.name = "n {print i}' | tr -d ',' | tail -1; }
pw_pick() { # the headset the service plays to: the private null sink
  mkdir -p "$RUN/state"; chmod 700 "$RUN/state"
  [[ -f "$RUN/state/call-audio.json" ]] || printf '{"output":"gb-spk","input":"gb-mic"}\n' >"$RUN/state/call-audio.json"
}
pw_start() {
  [[ -S "$RT/pipewire-0" ]] && pw_env pw-cli info 0 >/dev/null 2>&1 && { pw_pick; return 0; }
  rm -rf "$RT"; mkdir -p "$RT"; chmod 700 "$RT"
  (cd "$SCR" && DBUS_SESSION_BUS_ADDRESS=disabled: XDG_RUNTIME_DIR="$RT" PIPEWIRE_RUNTIME_DIR="$RT" setsid nohup pipewire >"$SCR/pipewire.log" 2>&1 & echo $! >"$PWPIDS")
  wait_for 10 test -S "$RT/pipewire-0" || return 1
  (cd "$SCR" && DBUS_SESSION_BUS_ADDRESS=disabled: XDG_RUNTIME_DIR="$RT" PIPEWIRE_RUNTIME_DIR="$RT" setsid nohup pipewire-pulse >"$SCR/pipewire-pulse.log" 2>&1 & echo $! >>"$PWPIDS")
  wait_for 10 test -S "$RT/pulse/native" || return 1
  local s
  for s in gb-spk gb-mic; do
    pw_env pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$s node.description=$s media.class=Audio/Sink audio.position=[MONO] audio.rate=16000 object.linger=true node.driver=true }" >/dev/null
    for _ in $(seq 1 50); do [[ -n "$(pw_nid $s)" ]] && break; sleep 0.2; done
    [[ -n "$(pw_nid $s)" ]] || return 1
    pw_env pw-cli set-param "$(pw_nid $s)" PortConfig "{ direction: Input, mode: dsp, monitor: true, format: { mediaType: audio, mediaSubtype: raw, format: F32P, rate: 16000, channels: 1, position: [ MONO ] } }" >/dev/null
  done
  # Speak (not exercised here) would record from the monitor of gb-mic; the call-audio pick below names both.
  pw_pick
  pw_env pactl list short sinks 2>/dev/null | grep -q gb-spk
}
# The session manager links the service's playback stream to the chosen sink, as on a real desktop (WirePlumber, private).
pw_linker() {
  [[ -f "$SCR/wp.pid" ]] && kill -0 "$(cat "$SCR/wp.pid")" 2>/dev/null && return 0
  (cd "$SCR" && DBUS_SESSION_BUS_ADDRESS=disabled: XDG_RUNTIME_DIR="$RT" PIPEWIRE_RUNTIME_DIR="$RT" PIPEWIRE_REMOTE=pipewire-0 setsid nohup wireplumber >"$SCR/wireplumber.log" 2>&1 & echo $! >"$SCR/wp.pid")
  sleep 3
}
pw_stop() {
  [[ -f "$SCR/wp.pid" ]] && kill "$(cat "$SCR/wp.pid")" 2>/dev/null; rm -f "$SCR/wp.pid"
  [[ -f "$PWPIDS" ]] && for p in $(cat "$PWPIDS"); do
    [[ "$(tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null)" == pipewire* ]] && kill "$p" 2>/dev/null
  done
  rm -f "$PWPIDS"; rm -rf "$RT"
}
# Dominant frequency (Hz) and RMS of the last seconds played into the private headset (analysed in a pipe, never stored).
listen_headset() { # seconds
  pw_env timeout $(( $1 + 4 )) pw-cat --record --raw --format s16 --rate 16000 --channels 1 -P stream.capture.sink=true --target gb-spk - 2>/dev/null | python3 -I -c '
import math, struct, sys
need = 16000 * 2 * int(sys.argv[1])
buf = b""
while len(buf) < need:
    c = sys.stdin.buffer.read(need - len(buf))
    if not c: break
    buf += c
x = struct.unpack("<%dh" % (len(buf)//2), buf)[-48000:]
if not x: print("0 0"); sys.exit()
def p(f):
    c = 2*math.cos(2*math.pi*f/16000); s1 = s2 = 0.0
    for v in x: s0 = v + c*s1 - s2; s2 = s1; s1 = s0
    return s1*s1 + s2*s2 - c*s1*s2
best = max(range(100, 4000, 10), key=p)
best = max(range(best-10, best+11), key=p)
print("%d %d" % (best, math.sqrt(sum(v*v for v in x)/len(x))))' "$1"
}

# A debug-profile service needs far longer than the release one to publish its control token.
start_service() {
  ctl status >/dev/null 2>&1 && return 0
  mkdir -p "$RUN/state"; chmod 700 "$RUN/state"
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE DISPLAY="$XDISP" GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" --control-port "$CTRL_PORT" \
     --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name E2E-Desktop >>"$LOGS/e2e-service-cw.log" 2>&1 &)
  wait_for 180 ctl status >/dev/null 2>&1
}

# ---------------------------------------------------------------- env (replaces the build/start of e2e-android.sh)
stage_env() {
  [[ "${STOP:-}" == 1 ]] && return 0
  gb_preflight "$ROOT" || { rec env FAIL "not enough free disk"; return 1; }
  local b; for b in geekbridge-service geekbridge-ctl; do [[ -x "$DEBUG_BIN/$b" ]] || { rec env FAIL "missing $DEBUG_BIN/$b (cargo build -p bridge-service --bins)"; return 1; }; done
  [[ -r "$CW_APK" ]] || { rec env FAIL "missing debug APK $CW_APK"; return 1; }
  mkdir -p "$E2E_TARGET/release" "$SCR/android/dist"
  ln -sf "$DEBUG_BIN/geekbridge-ctl" "$E2E_TARGET/release/geekbridge-ctl"
  # The service runs with the private sound server and nothing else changed (no user session variables).
  { echo '#!/usr/bin/env bash'
    printf 'exec env XDG_RUNTIME_DIR=%q PIPEWIRE_RUNTIME_DIR=%q PIPEWIRE_REMOTE=pipewire-0 PULSE_SERVER=%q %q "$@"\n' "$RT" "$RT" "unix:$RT/pulse/native" "$DEBUG_BIN/geekbridge-service"
  } >"$E2E_TARGET/release/geekbridge-service"; chmod 755 "$E2E_TARGET/release/geekbridge-service"
  cp -f "$CW_APK" "$APK"
  pw_start || { rec env FAIL "private PipeWire did not start (see $SCR/pipewire.log)"; return 1; }
  pw_linker
  start_xvfb || { rec env FAIL "Xvfb did not start"; return 1; }
  start_service || { rec env FAIL "desktop service did not start"; return 1; }
  # Keep the emulator up between runs: the clone is made here, not by start_emulator, so no exit hook stops it.
  if gb_avd_ensure "$AVD"; then _gb_avd_clones=(); _gb_avd_hooks=(); fi
  start_emulator || { rec env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r -t "$APK" 2>&1 | tail -1 | grep -q Success || { rec env FAIL "adb install failed"; return 1; }
  printf '' | DISPLAY="$XDISP" xclip -selection clipboard -i
  desk_id
  rec env PASS "debug APK $(sha256sum "$APK" | cut -c1-12) on $SERIAL; service $DESK_ID; private PipeWire sinks: $(pw_env pactl list short sinks | awk '{print $2}' | tr '\n' ' ')"
}

# ---------------------------------------------------------------- the linked checks
CWR=.callaudio.CallWifiReceiver
cw() { $ADB shell "am broadcast -n $PKG/$CWR -a dev.geekbridge.callwifi.$1 ${*:2}" 2>&1 | tr -d '\r'; }
cwstatus() { cw STATUS | sed -n 's/.*data="\(.*\)"$/\1/p' | tail -1; }
cwf() { python3 -I -c "import json,sys; print(json.loads(sys.argv[1]).get(sys.argv[2]))" "$1" "$2" 2>/dev/null; }
view() { ctl callwifi status --json 2>/dev/null; }
vf() { view | python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }
sess_state() { vf "(d.get('session') or {}).get('state','none')"; }
# Hooks run in the app process; STATUS shows the phone's own view.
nc_open() { # PORT -> 0 when something accepts on the phone's loopback
  [[ "$($ADB shell "echo x | nc -w1 127.0.0.1 $1 >/dev/null 2>&1; echo rc=\$?" | tr -d '\r')" == rc=0 ]]
}
ctl_cw() { timeout 30 "${CTL[@]}" callwifi "$@"; }
phone_helper() { cwf "$(cwstatus)" helper; }
computer_ready() { vf "any(p['ready'] and p['in_call'] and p['allowed'] for p in d['phones'])"; }
phone_sharing() { cwf "$(cwstatus)" sharing; }
wait_eq() { # SECONDS EXPECTED CMD...
  local n="$1" want="$2"; shift 2
  local got; for ((i = 0; i < n; i++)); do got="$("$@")"; [[ "$got" == "$want" ]] && return 0; sleep 1; done; return 1
}

cw_listener_checks() {
  local out port token
  # Off by default: no listener while the switch is off.
  cw ENABLE --ez on false >/dev/null
  cw OPEN | grep -q "result=2" && rec cw_listener_off PASS "OPEN refused (result=2) while the switch is off" || rec cw_listener_off FAIL "OPEN answered while off"
  # Closes on switch-off.
  cw ENABLE --ez on true >/dev/null
  out="$(cw OPEN)"; port="$(sed -n 's/.*port=\([0-9]*\).*/\1/p' <<<"$out" | tail -1)"
  if [[ -z "$port" ]]; then rec cw_listener_open FAIL "no port: $out"; return 1; fi
  nc_open "$port" && rec cw_listener_open PASS "listener accepts on the phone's loopback" || rec cw_listener_open FAIL "nothing listening on $port"
  cw ENABLE --ez on false >/dev/null; sleep 1
  nc_open "$port" && rec cw_listener_switch_off FAIL "still listening after the switch went off" || rec cw_listener_switch_off PASS "closed at once when the switch went off"
  # Closes on STOP.
  cw ENABLE --ez on true >/dev/null
  out="$(cw OPEN)"; port="$(sed -n 's/.*port=\([0-9]*\).*/\1/p' <<<"$out" | tail -1)"
  cw STOP >/dev/null; sleep 1
  nc_open "$port" && rec cw_listener_stop FAIL "still listening after STOP" || rec cw_listener_stop PASS "closed by STOP"
  # One overall deadline: a local client that keeps connecting (and failing the token) cannot hold it open.
  cw ENABLE --ez on true >/dev/null
  out="$(cw OPEN)"; port="$(sed -n 's/.*port=\([0-9]*\).*/\1/p' <<<"$out" | tail -1)"
  local t0=$SECONDS closed=""
  $ADB shell "nohup sh -c 'i=0; while [ \$i -lt 40 ]; do echo bad | nc -w1 127.0.0.1 $port >/dev/null 2>&1; sleep 3; i=\$((i+1)); done' >/dev/null 2>&1 &" >/dev/null 2>&1
  while (( SECONDS - t0 < 130 )); do
    if ! nc_open "$port"; then closed=$((SECONDS - t0)); break; fi
    sleep 4
  done
  $ADB shell "pkill -f 'nc -w1 127.0.0.1 $port'" >/dev/null 2>&1
  if [[ -n "$closed" && "$closed" -ge 80 && "$closed" -le 100 ]]; then
    rec cw_listener_deadline PASS "closed after ~${closed} s (one 90 s deadline) while a client kept connecting every 3 s"
  else rec cw_listener_deadline FAIL "closed=${closed:-never within 130 s}"; fi
  cw ENABLE --ez on false >/dev/null
}

stage_callwifi() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec cw SKIP "not paired (run pair first)"; return 1; }
  desk_id
  [[ "${GB_CW_SKIP_LISTENER:-0}" == 1 ]] || cw_listener_checks
  # ---- enable
  ctl consent "$PHONE_ID" call_audio_wifi on >/dev/null 2>&1
  # The computer learns "a call is live" from the phone's call-state link, so the phone's call control is on too
  # (permissions, the "calls" grant, the Answer-from-computer switch, the computer's telephony consent), next to
  # the experiment's own grant for this computer.
  local p; for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_grants.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="grants">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[&quot;calls&quot;,&quot;callaudio&quot;]}}</string>
</map>
XML
  $ADB shell "run-as $PKG sh -c 'cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
</map>
XML
  ctl consent "$PHONE_ID" telephony on >/dev/null 2>&1
  rec cw_phone_grant INFO "phone allows the computer (calls + callaudio grants written like call-ui.sh; the GRANT hook does the same live)"
  cw ENABLE --ez on true | grep -q "result=1" && rec cw_enable PASS ok || rec cw_enable FAIL "ENABLE hook"
  ensure_session
  local neg; neg="$(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  if [[ "$neg" == *geekbridge.callaudio* ]]; then rec cw_negotiated PASS "$(tr ',' '\n' <<<"$neg" | grep -c callaudio) callaudio packet types negotiated"; else rec cw_negotiated FAIL "not negotiated: $neg"; fi
  if env -u ADB ANDROID_ADB_SERVER_PORT="$ANDROID_ADB_SERVER_PORT" "$ROOT/scripts/callaudio-enable.sh" --yes --serial "$SERIAL" >"$LOGS/e2e-cw-enable.log" 2>&1; then rec cw_helper_start PASS "$(tail -n1 "$LOGS/e2e-cw-enable.log" | cut -c1-90)"; else rec cw_helper_start FAIL "$(tail -n2 "$LOGS/e2e-cw-enable.log" | tr '\n' ' ')"; return 1; fi
  local st; wait_eq 20 True phone_helper; st="$(cwstatus)"
  [[ "$(cwf "$st" helper)" == True ]] && rec cw_helper_connected PASS "$st" || { rec cw_helper_connected FAIL "$st"; return 1; }
  # ---- control (no call), then the in-call step on a simulated call
  cw SELFTEST --es step control >/dev/null; sleep 10
  rec cw_control_nocall PASS "ran with no call: $(cwf "$(cwstatus)" progress)"
  $ADB emu gsm cancel 5551230 >/dev/null 2>&1; sleep 1
  $ADB emu gsm call 5551230 >/dev/null 2>&1; sleep 3; $ADB emu gsm accept 5551230 >/dev/null 2>&1; sleep 3
  local cs; cs="$($ADB shell dumpsys telephony.registry | tr -d '\r' | grep -m1 -o 'mCallState=[0-9]')"
  [[ "$cs" == mCallState=2 ]] && rec cw_simulated_call PASS "$cs (off hook)" || rec cw_simulated_call FAIL "${cs:-no call state}"
  for _ in $(seq 1 30); do $ADB shell dumpsys audio | grep -qE 'mode.*(IN_CALL|MODE_IN_CALL)' && break; sleep 1; done
  cw CONFIG --es verdict not_run >/dev/null   # an earlier run's stored verdict must not end the wait below
  cw SELFTEST --es step call >/dev/null
  for _ in $(seq 1 90); do sleep 1; st="$(cwstatus)"; [[ -n "$st" && "$(cwf "$st" verdict)" != not_run ]] && break; done
  sleep 3   # the test thread still pushes its result to the hub; the hook below must come after it
  rec cw_selftest_in_call INFO "verdict=$(cwf "$st" verdict) (the emulator's call sources sound with no call, so unsure is the honest answer)"
  [[ "$(cwf "$st" verdict)" == unsure ]] && rec cw_selftest_unsure_blocks_listen PASS "unsure verdict: the computer must not be offered Listen" || rec cw_selftest_unsure_blocks_listen INFO "verdict $(cwf "$st" verdict)"
  if [[ "$(cwf "$st" verdict)" == unsure ]]; then
    ctl_cw listen "$PHONE_ID" >"$LOGS/e2e-cw-listen-unsure.log" 2>&1; sleep 4
    [[ "$(sess_state)" == none ]] && rec cw_unsure_no_session PASS "Listen with an unsure self-test starts no session: $(tail -n1 "$LOGS/e2e-cw-listen-unsure.log" | cut -c1-110)" || rec cw_unsure_no_session FAIL "session $(sess_state)"
  fi
  # The real self-test cannot say "works" on an emulator: stand in for it (debug hook) and use the helper's tone as the far party.
  cw CONFIG --es verdict works --es source TONE >/dev/null; sleep 3
  rec cw_verdict_forced INFO "verdict forced to works with the TONE source through the debug hook (NOT a real self-test)"
  if wait_eq 30 True computer_ready; then
    rec cw_computer_sees_ready PASS "phone ready, in a call and allowed: $(vf "d['phones'][0]['test']")"
  else rec cw_computer_sees_ready FAIL "$(view | cut -c1-300)"; return 1; fi
  # ---- listen / stop
  ctl_cw listen "$PHONE_ID" >"$LOGS/e2e-cw-listen.log" 2>&1
  if wait_eq 25 live sess_state; then rec cw_session_live PASS "$(vf "(d['session']['name'], d['session']['state'])")"; else rec cw_session_live FAIL "state=$(sess_state) $(tail -n2 "$LOGS/e2e-cw-listen.log" | tr '\n' ' ')"; shot 40-cw-listen-failed; fi
  sleep 3
  local hz rms; read -r hz rms < <(listen_headset 5)
  if [[ "${hz:-0}" -ge 430 && "${hz:-0}" -le 450 && "${rms:-0}" -gt 200 ]]; then rec cw_audio_in_headset PASS "far party's ${hz} Hz tone reached the private headset (rms $rms) through helper, phone hub, TLS link, Opus, jitter buffer, pw-cat"
  else rec cw_audio_in_headset FAIL "hz=$hz rms=$rms"; fi
  rec cw_stats INFO "$(vf "{k: d['session'][k] for k in ('frames','lost','depth_ms','seconds')}")"
  [[ "$(phone_sharing)" == True ]] && rec cw_phone_sharing PASS "phone reports sharing" || rec cw_phone_sharing FAIL "$(cwstatus)"
  $ADB shell dumpsys notification --noredact | grep -q "Call audio is being shared" && rec cw_phone_indicator PASS "ongoing notification posted" || rec cw_phone_indicator FAIL "no notification"
  shot 41-cw-sharing
  ctl_cw stop >/dev/null 2>&1
  wait_eq 10 none sess_state && rec cw_stop_computer PASS "session ended by Stop on the computer" || rec cw_stop_computer FAIL "state=$(sess_state)"
  wait_eq 15 False phone_sharing && rec cw_stop_phone PASS "phone stopped sharing" || rec cw_stop_phone FAIL "$(cwstatus)"
  $ADB shell dumpsys notification --noredact | grep -q "Call audio is being shared" && rec cw_indicator_gone FAIL "notification still posted" || rec cw_indicator_gone PASS "notification removed"
  [[ "$(cwf "$(cwstatus)" helper)" == True ]] && rec cw_helper_survives_stop INFO "helper still connected after Stop" || rec cw_helper_survives_stop INFO "helper disconnected by Stop (re-run the enable script for the next session)"
  # ---- link loss
  helper_back
  ctl_cw listen "$PHONE_ID" >/dev/null 2>&1
  wait_eq 25 live sess_state || rec cw_linkloss_setup FAIL "no live session to cut: $(sess_state)"
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1
  wait_eq 10 none sess_state && rec cw_linkloss_computer PASS "computer dropped the session when the link went down" || rec cw_linkloss_computer FAIL "state=$(sess_state)"
  wait_eq 30 False phone_sharing && rec cw_linkloss_phone PASS "phone stopped sharing after the link was lost" || rec cw_linkloss_phone FAIL "$(cwstatus)"
  $ADB shell dumpsys notification --noredact | grep -q "Call audio is being shared" && rec cw_linkloss_indicator FAIL "notification still posted" || rec cw_linkloss_indicator PASS "notification removed"
  # ---- notifications blocked (security review 8, fix 4)
  # Revoking the notification permission stops the app process: bring it back, reconnect, restart the helper.
  $ADB shell pm revoke "$PKG" android.permission.POST_NOTIFICATIONS >/dev/null 2>&1; sleep 2
  $ADB shell dumpsys package "$PKG" | grep -q 'POST_NOTIFICATIONS: granted=false' && rec cw_notif_blocked_setup INFO "POST_NOTIFICATIONS revoked (the app is told notifications are off)" || rec cw_notif_blocked_setup FAIL "could not revoke the permission"
  cw ENABLE --ez on true >/dev/null; sleep 2
  ensure_session; helper_back
  $ADB emu gsm cancel 5551230 >/dev/null 2>&1; sleep 1; $ADB emu gsm call 5551230 >/dev/null 2>&1; sleep 3; $ADB emu gsm accept 5551230 >/dev/null 2>&1; sleep 5
  ctl_cw listen "$PHONE_ID" >"$LOGS/e2e-cw-listen-blocked.log" 2>&1; sleep 8
  st="$(cwstatus)"
  if [[ "$(cwf "$st" sharing)" == False && "$(sess_state)" != live ]] && grep -q "Notifications for GeekBridge are off" <<<"$st"; then
    rec cw_notif_blocked_refuses PASS "no session while notifications are blocked; phone says: $(cwf "$st" progress)"
  else rec cw_notif_blocked_refuses FAIL "sharing=$(cwf "$st" sharing) session=$(sess_state) status=$st"; fi
  $ADB shell pm grant "$PKG" android.permission.POST_NOTIFICATIONS >/dev/null 2>&1
  # ---- tidy
  $ADB emu gsm cancel 5551230 >/dev/null 2>&1
  cw STOP >/dev/null; cw ENABLE --ez on false >/dev/null
  ctl consent "$PHONE_ID" call_audio_wifi off >/dev/null 2>&1
}
# After a Stop or a lost link the phone may have dropped its helper connection: start it again if so.
helper_back() {
  [[ "$(cwf "$(cwstatus)" helper)" == True ]] && return 0
  cw ENABLE --ez on true >/dev/null
  env -u ADB ANDROID_ADB_SERVER_PORT="$ANDROID_ADB_SERVER_PORT" "$ROOT/scripts/callaudio-enable.sh" --yes --serial "$SERIAL" >"$LOGS/e2e-cw-enable2.log" 2>&1
  wait_eq 20 True phone_helper
  cw CONFIG --es verdict works --es source TONE >/dev/null; sleep 3
}

main_cw() {
  if [[ "${STOP:-}" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    pkill -f "^$DEBUG_BIN/geekbridge-service --state-dir $RUN/state" 2>/dev/null; pw_stop
    pkill -f "Xvfb $XDISP" 2>/dev/null
    adb -P "$ANDROID_ADB_SERVER_PORT" kill-server >/dev/null 2>&1
    if [[ "${GB_CW_FRESH:-0}" == 1 || "${GB_CW_DELETE_AVD:-0}" == 1 ]]; then rm -rf -- "$ANDROID_AVD_HOME/$AVD.avd" "$ANDROID_AVD_HOME/$AVD.ini"; fi
    return 0
  fi
  : >"$LOGS/e2e-run.log"
  IFS=, read -r -a st <<<"$STAGES"
  for s in "${st[@]}"; do log "== stage $s"; "stage_$s" || log "stage $s reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 70
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_cw
