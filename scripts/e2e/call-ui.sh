#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Incoming-call answering through the REAL desktop UI (not just `geekbridge-ctl call answer`).
#
#   scripts/e2e/call-ui.sh [--stop] [STAGE[,STAGE...]]     stages: env pair notify popup   (default: all)
#
# What runs, all private (nothing touches the real session bus, ~/.config, display or clipboard):
#   * `dbus-run-session`      a private session bus, so the notification daemon and the service never see the user's bus
#   * headless `sway` + `mako`  a real Wayland notification daemon on that bus (WLR_BACKENDS=headless), grim for pictures
#   * Xvfb                    a private X display for the egui Calls window (clicked with xdotool, captured with import)
#   * emulator `gb36k`        own console port / adb server; the desktop service is paired with it (e2e topology)
#   * geekbridge-service      real `notify-send` -> mako, telephony consent on, "Answer from computer" on in the phone app
# Stages:
#   notify  ring the emulator; the notification must arrive with Answer + Decline (captured on the bus with dbus-monitor),
#           then the action is invoked through the daemon (`makoctl invoke`): Answer -> modem off-hook, Decline -> idle
#   popup   the Calls window (geekbridge-calls): screenshot ringing, click Answer (off-hook), in-call panel + the
#           "Answered on the phone" audio hint, click Hang up (idle), ring again and click Decline (idle)
# Results: dist/android/logs/e2e-callui-results.tsv   Pictures: dist/screenshots/desktop/call-popup-*.png, call-notification-*.png
# Needs: sway mako makoctl grim Xvfb xdotool import dbus-run-session dbus-monitor tesseract, the .tools Android toolchain,
# release binaries in target/call-ui (`CARGO_TARGET_DIR=target/call-ui cargo build --release -p bridge-service -p bridge-apps --bins`)
# and the debug APK from scripts/build-android.sh (E2E_SCRATCH).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

# Everything below runs on a private session bus. The service, mako and the apps inherit it.
if [[ -z "${CALLUI_INNER:-}" ]]; then
  exec dbus-run-session -- env CALLUI_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

STAGES="env,pair,notify,popup"
STOPPING=0
for a in "$@"; do
  case "$a" in
    --stop) STOPPING=1 ;;
    -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) STAGES="$a" ;;
  esac
done
set --

: "${E2E_AVD:=gb36k}" "${E2E_EMU_PORT:=5634}" "${E2E_ADB_PORT:=16080}" "${E2E_TAG:=-callui}"
: "${E2E_RUN:=$ROOT/target/call-ui-run}" "${E2E_SCRATCH:=$ROOT/target/e2e-full-android}" "${E2E_TARGET:=$ROOT/target/call-ui}"
: "${E2E_XDISP:=:97}" "${E2E_CTRL_PORT:=17511}" "${E2E_API_PORT:=17510}" "${E2E_DISC_PORT:=17546}" "${E2E_LISTEN_PORT:=1737}" "${E2E_REDIR_PORT:=17561}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-callui-results.tsv"
set -- --no-build
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set --

DESK_SHOTS="$ROOT/dist/screenshots/desktop"; mkdir -p "$DESK_SHOTS"
# Private XDG tree: nothing is read from or written to the real ~/.config, ~/.local or the user's runtime dir.
PRIV="$RUN/priv"; mkdir -p "$PRIV"/{config,data,state,cache}
XRT="/tmp/gb-callui-xdg-$UID"; mkdir -p "$XRT"; chmod 700 "$XRT"
APP_ENV=(env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u SWAYSOCK -u NIRI_SOCKET
  XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" XDG_DATA_HOME="$PRIV/data" XDG_STATE_HOME="$PRIV/state"
  XDG_CACHE_HOME="$PRIV/cache" XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=GeekBridgeE2E)
SWAY_SOCKET_NAME="gb-callui-0"
NOTIFY_BUS_LOG="$RUN/notify-bus.log"
CALLS_BIN="$E2E_TARGET/release/geekbridge-calls"
SERVICE_LOG="$LOGS/e2e-callui-service.log"
NUMBER_A=5558801 NUMBER_B=5558802 NUMBER_C=5558803 NUMBER_D=5558804 NUMBER_E=5558805

modem_state() { $ADB shell dumpsys telephony.registry 2>/dev/null | grep -m1 -o "mCallState=[0-9]" | cut -d= -f2; }
wait_modem() { # state seconds
  local want="$1" n="$2" i
  for ((i = 0; i < n * 2; i++)); do [[ "$(modem_state)" == "$want" ]] && return 0; sleep 0.5; done
  return 1
}
calls_json() { ctl calls --reveal 2>/dev/null; }
call_ringing() { calls_json | tr -d '\n ' | grep -qi '"phase":"ringing"\|ringing'; }
sway_env() { env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR="$XRT" WAYLAND_DISPLAY="$(cat "$RUN/wayland-display" 2>/dev/null)" SWAYSOCK="$XRT/sway.sock" "$@"; }
hang_up_all() { $ADB emu gsm cancel "$NUMBER_A" >/dev/null 2>&1; for n in $NUMBER_B $NUMBER_C $NUMBER_D $NUMBER_E; do $ADB emu gsm cancel "$n" >/dev/null 2>&1; done; wait_modem 0 8; }
ring() { # number -> waits until the desktop sees the call ringing
  $ADB emu gsm call "$1" >/dev/null
  wait_for 25 call_ringing
}

stop_desktop() {
  pkill -f "^$CALLS_BIN" 2>/dev/null
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null
  pkill -f "dbus-monitor.*$XRT" 2>/dev/null
  [[ -f "$RUN/mako.pid" ]] && kill "$(cat "$RUN/mako.pid")" 2>/dev/null
  [[ -f "$RUN/sway.pid" ]] && kill "$(cat "$RUN/sway.pid")" 2>/dev/null
  [[ -f "$RUN/monitor.pid" ]] && kill "$(cat "$RUN/monitor.pid")" 2>/dev/null
  pkill -f "Xvfb $XDISP" 2>/dev/null
  true
}
trap stop_desktop EXIT

# ---------------------------------------------------------------- env
start_wayland_and_mako() {
  [[ -f "$RUN/sway.pid" ]] && kill "$(cat "$RUN/sway.pid")" 2>/dev/null
  rm -f "$XRT"/wayland-* "$XRT/sway.sock"
  : >"$RUN/sway.conf"
  (env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE XDG_RUNTIME_DIR="$XRT" \
     XDG_CONFIG_HOME="$PRIV/config" WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
     SWAYSOCK="$XRT/sway.sock" setsid sway -c "$RUN/sway.conf" >"$RUN/sway.log" 2>&1 & echo $! >"$RUN/sway.pid")
  wait_for 15 bash -c "ls '$XRT'/wayland-* >/dev/null 2>&1" || return 1
  basename "$(ls "$XRT"/wayland-* | grep -v lock | head -1)" >"$RUN/wayland-display"
  # A real notification daemon on the private bus. Timeouts off so a notification stays until it is acted on.
  printf 'default-timeout=0\nignore-timeout=1\nmax-visible=5\nlayer=overlay\n' >"$RUN/mako.conf"
  (env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" WAYLAND_DISPLAY="$(cat "$RUN/wayland-display")" \
     setsid mako --config "$RUN/mako.conf" >"$RUN/mako.log" 2>&1 & echo $! >"$RUN/mako.pid")
  # Wait for the name without asking mako anything: a client's request would D-Bus-activate a second mako first.
  wait_for 15 bash -c "busctl --user list --no-pager 2>/dev/null | grep -q 'org.freedesktop.Notifications.*mako'"
}
start_bus_monitor() {
  [[ -f "$RUN/monitor.pid" ]] && kill "$(cat "$RUN/monitor.pid")" 2>/dev/null
  : >"$NOTIFY_BUS_LOG"
  (setsid dbus-monitor "interface='org.freedesktop.Notifications',member='Notify'" >"$NOTIFY_BUS_LOG" 2>&1 & echo $! >"$RUN/monitor.pid")
}
start_desktop_service() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
  mkdir -p "$RUN/state"; chmod 700 "$RUN/state"
  # The real notify-send is used on purpose (libnotify <-> mako). No WAYLAND_DISPLAY: clipboard code uses the private Xvfb.
  ("${APP_ENV[@]}" DISPLAY="$XDISP" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_QR_ADDRESSES="10.0.2.2:$LISTEN_PORT" \
     setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" --control-port "$CTRL_PORT" \
     --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name E2E-Desktop >>"$SERVICE_LOG" 2>&1 &)
  wait_for 15 ctl status >/dev/null 2>&1
}
stage_env() {
  [[ "$STOPPING" == 1 ]] && return 0
  [[ -x "$SVC" && -x "$CALLS_BIN" && -f "$APK" ]] || { rec callui_env FAIL "missing binaries/APK ($SVC, $CALLS_BIN, $APK)"; return 1; }
  pkill -f "Xvfb $XDISP" 2>/dev/null
  wait_for 10 bash -c "! pgrep -f 'Xvfb $XDISP' >/dev/null"
  rm -f "/tmp/.X11-unix/X${XDISP#:}" "/tmp/.X${XDISP#:}-lock"
  (setsid nohup Xvfb "$XDISP" -screen 0 1280x900x24 -nolisten tcp >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${XDISP#:}" || { rec callui_env FAIL "Xvfb did not start"; return 1; }
  start_wayland_and_mako || { rec callui_env FAIL "sway/mako did not start: $(tail -3 "$RUN/sway.log" "$RUN/mako.log" | tr '\n' '|')"; return 1; }
  start_bus_monitor
  start_desktop_service || { rec callui_env FAIL "desktop service did not start: $(tail -3 "$SERVICE_LOG" | tr '\n' '|')"; return 1; }
  start_emulator || { rec callui_env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || { rec callui_env FAIL "adb install failed"; return 1; }
  desk_id
  local owner; owner="$(env XDG_RUNTIME_DIR="$XRT" busctl --user --no-pager status org.freedesktop.Notifications 2>/dev/null | grep -m1 '^Comm=')"
  rec callui_env PASS "private bus $DBUS_SESSION_BUS_ADDRESS; notification daemon: ${owner:-mako}; desktop $DESK_ID; emulator $SERIAL"
}

# ---------------------------------------------------------------- pairing (QR link opened as the camera app would) and phone prefs
scan() { $ADB shell am start -W -a android.intent.action.VIEW -d "'$1'" $PKG >/dev/null; }
# A newly paired computer holds no phone-side grants (android ComputerGrants): allow calls for the desktop's id.
write_grants_calls() {
  desk_id
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_grants.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="grants">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[&quot;calls&quot;]}}</string>
</map>
XML
}
write_calls_prefs() {
  $ADB shell am force-stop $PKG; sleep 1
  write_grants_calls
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
</map>
XML
}
stage_pair() {
  desk_id; PHONE_ID="$(cat "$RUN/phone-id" 2>/dev/null)"
  wake
  for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  if [[ -n "$PHONE_ID" ]] && is_paired; then
    write_calls_prefs; goto_home; sleep 3
  else
    for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
    $ADB shell pm clear $PKG >/dev/null; wake
    for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
    local qr; qr="$(ctl pair-qr 2>/dev/null | grep -m1 "^geekbridge://")"; echo "${qr:0:60}..." >"$RUN/qr-prefix.txt"
    [[ "$qr" == geekbridge://* ]] || { rec callui_pair FAIL "pair-qr gave: ${qr:0:120}"; return 1; }
    scan "$qr"
    ui wait "Connect to a computer" 20 >/dev/null || { rec callui_pair FAIL "no pairing page after the link: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    ui tap Continue >/dev/null
    ui wait "Computer identity" 40 >/dev/null || { rec callui_pair FAIL "no identity step: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    ui tap Pair --exact >/dev/null
    ui wait "What to do next" 40 >/dev/null || { rec callui_pair FAIL "QR pairing did not finish: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
    write_calls_prefs; goto_home
    for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  fi
  sleep 2; desk_dial >/dev/null 2>&1
  wait_for 25 is_connected || { rec callui_pair FAIL "link not connected"; return 1; }
  ctl consent "$PHONE_ID" telephony on >/dev/null; sleep 3
  ctl status | jget '",".join(d["links"][0]["negotiated"])' | grep -q geekbridge.calls \
    || { rec callui_pair FAIL "calls not negotiated: $(ctl status | jget '",".join(d["links"][0]["negotiated"])')"; return 1; }
  rec callui_pair PASS "paired $PHONE_ID, connected, telephony consent on, 'Answer from computer' on, calls negotiated"
}

# ---------------------------------------------------------------- notification stage
mako_ids() { sway_env makoctl list 2>/dev/null | sed -n 's/^Notification \([0-9]*\):.*/\1/p'; }
mako_actions_of() { sway_env makoctl list 2>/dev/null; }
notification_up() { sway_env makoctl list 2>/dev/null | grep -q "Incoming call"; }
grim_shot() { sway_env grim "$DESK_SHOTS/$1" 2>/dev/null; }
notification_case() { # name number action expected-modem-state
  local name="$1" number="$2" action="$3" want="$4" id
  hang_up_all; sway_env makoctl dismiss -a >/dev/null 2>&1; : >"$NOTIFY_BUS_LOG"
  ring "$number" || { rec "callui_notify_$name" FAIL "desktop never saw the call ringing: $(calls_json | head -c 200)"; return 1; }
  wait_for 15 notification_up || { rec "callui_notify_$name" FAIL "no 'Incoming call' notification in mako; bus log: $(tr '\n' ' ' <"$NOTIFY_BUS_LOG" | head -c 300)"; return 1; }
  id="$(mako_ids | tail -1)"
  if [[ "$name" == answer ]]; then
    local bus; bus="$(tr -d '\n' <"$NOTIFY_BUS_LOG")"
    if grep -q 'string "answer"' <<<"$bus" && grep -q 'string "Answer"' <<<"$bus" && grep -q 'string "decline"' <<<"$bus" && grep -q 'string "Decline"' <<<"$bus"; then
      rec callui_notify_actions PASS "Notify on the bus carries actions answer/Answer and decline/Decline (category call.incoming)"
    else rec callui_notify_actions FAIL "actions missing in the Notify call: $(echo "$bus" | tr -s ' ' | head -c 400)"; fi
    grim_shot call-notification-ringing.png
  fi
  sway_env makoctl invoke -n "$id" "$action" >/dev/null 2>&1 || { rec "callui_notify_$name" FAIL "makoctl invoke $action failed (id $id): $(sway_env makoctl list | head -8 | tr '\n' '|')"; return 1; }
  if wait_modem "$want" 12; then rec "callui_notify_$name" PASS "makoctl invoke $action -> modem state $want ($([[ $want == 2 ]] && echo off-hook || echo idle))"
  else rec "callui_notify_$name" FAIL "after '$action' the modem state is $(modem_state), expected $want"; fi
}
stage_notify() {
  [[ -n "${CI:-}" ]] && { notify-send --version; notify-send --help 2>&1 | grep -i action
    gdbus call --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
      --method org.freedesktop.Notifications.GetCapabilities
    : >"$NOTIFY_BUS_LOG"; timeout 4 notify-send --app-name=Diag --wait --action=a=Alpha --action=b=Beta -- "diag title" "diag body"
    echo "notify-send rc=$?"; sleep 1; tr -s ' \n' ' ' <"$NOTIFY_BUS_LOG" | head -c 900; echo; sway_env makoctl dismiss -a
    echo "pids: $(pgrep -a 'mako|dunst' | tr '\n' ';')"; } >"$RUN/notify-diag.log" 2>&1
  # Answer/Decline buttons need libnotify's notify-send --action (>= 0.8.4 in practice); Ubuntu 24.04's build (the CI runner) sends the
  # notification without actions, so the action round trip cannot be tested there. Say so instead of failing.
  if [[ -n "${CI:-}" ]] && ! notify-send --help 2>&1 | grep -q -- '--action'; then
    rec callui_notify_answer SKIP "this notify-send ($(notify-send --version 2>&1 | head -1)) has no --action: nothing to invoke"
    rec callui_notify_decline SKIP "same reason"; return 0
  fi
  notification_case answer "$NUMBER_A" answer 2
  # Hang up from the desktop so the next ring starts clean.
  ctl call hangup >/dev/null 2>&1; wait_modem 0 8 || hang_up_all
  notification_case decline "$NUMBER_B" decline 0
  hang_up_all
}

# ---------------------------------------------------------------- the Calls window
APPWID=""
app_start() {
  pkill -f "^$CALLS_BIN" 2>/dev/null
  pgrep -af "$E2E_TARGET/release/geekbridge" | grep -v "geekbridge-service\|geekbridge-calls" >"$RUN/other-windows.txt"
  pkill -f "^$E2E_TARGET/release/geekbridge-(home|agenda|clipboard|contacts|messages|share)( |$)" 2>/dev/null
  pkill -f "^$E2E_TARGET/release/geekbridge( |$)" 2>/dev/null; sleep 0.5
  ("${APP_ENV[@]}" DISPLAY="$XDISP" GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT="$CTRL_PORT" \
     setsid nohup "$CALLS_BIN" --api-port "$API_PORT" --theme dark >"$RUN/calls-app.log" 2>&1 &)
  local i
  for ((i = 0; i < 40; i++)); do
    # By pid: the pairing flow can open other GeekBridge windows (home, calendar) on the same display.
    APPWID="$(DISPLAY=$XDISP xdotool search --onlyvisible --pid "$(pgrep -nf "^$CALLS_BIN")" 2>/dev/null | head -1)"
    [[ -n "$APPWID" ]] && break; sleep 0.5
  done
  [[ -n "$APPWID" ]] && DISPLAY=$XDISP xdotool windowfocus "$APPWID" 2>/dev/null
  [[ -n "$APPWID" ]]
}
win_geometry() { DISPLAY=$XDISP xdotool getwindowgeometry --shell "$APPWID" 2>/dev/null; } # sets X Y WIDTH HEIGHT
click_at() { # x y (relative to the window)
  eval "$(win_geometry)"
  DISPLAY=$XDISP xdotool mousemove $((X + $1)) $((Y + $2)); sleep 0.4
  DISPLAY=$XDISP xdotool click 1
}
app_shot() { # file
  eval "$(win_geometry)"
  DISPLAY=$XDISP import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$DESK_SHOTS/$1" 2>/dev/null
}
# The window is dark: invert and enlarge it first or tesseract reads little.
ocr() { { convert "$DESK_SHOTS/$1" -colorspace Gray -negate -resize 250% png:- 2>/dev/null || cat "$DESK_SHOTS/$1"; } | tesseract stdin - --psm 6 2>/dev/null | tr -s ' \n' ' '; }
px() { convert "$DESK_SHOTS/$1" -format "%[fx:int(255*p{$2,$3}.r)] %[fx:int(255*p{$2,$3}.g)] %[fx:int(255*p{$2,$3}.b)]" info: 2>/dev/null; }
# Button positions inside the 380x680 window (see the pictures): the popup is centred, Decline (red) left, Answer (off-white pill) right.
# The headline is dot-matrix ("INCOMING"), which OCR cannot read, and the button labels are white on colour, so the popup is recognised by its explanatory line.
POPUP_DECLINE_X=${POPUP_DECLINE_X:-100} POPUP_ANSWER_X=${POPUP_ANSWER_X:-278} POPUP_BUTTON_Y=${POPUP_BUTTON_Y:-377}
HANGUP_X=${HANGUP_X:-301} HANGUP_Y=${HANGUP_Y:-102}
popup_visible() { app_shot call-popup-probe.png; ocr call-popup-probe.png | grep -qiE "Answers on the phone|Bluetooth headset|Answers here"; }
stage_popup() {
  hang_up_all; sway_env makoctl dismiss -a >/dev/null 2>&1
  app_start || { rec callui_popup FAIL "the Calls window did not open: $(tail -3 "$RUN/calls-app.log" | tr '\n' '|')"; return 1; }
  sleep 2; app_shot call-popup-idle.png
  # --- ringing
  ring "$NUMBER_C" || { rec callui_popup_ring FAIL "desktop never saw the call ringing"; return 1; }
  wait_for 15 popup_visible && rec callui_popup_ring PASS "popup shown with its explanatory line (OCR of call-popup-ringing.png)" \
    || rec callui_popup_ring FAIL "no incoming-call popup: $(ocr call-popup-probe.png | head -c 200)"
  app_shot call-popup-ringing.png
  # White-on-colour button labels defeat OCR, so the buttons are checked by colour at their known positions.
  local dec ans
  dec="$(px call-popup-ringing.png 60 "$POPUP_BUTTON_Y")"; ans="$(px call-popup-ringing.png 215 "$POPUP_BUTTON_Y")"
  read -r dr dg db <<<"$dec"; read -r ar ag ab <<<"$ans"
  if ((dr > 180 && dg < 90)) && ((ar > 220 && ag > 220 && ab > 220)); then rec callui_popup_buttons PASS "red Decline button rgb=$dec, off-white Answer button rgb=$ans (sampled off the labels)"
  else rec callui_popup_buttons FAIL "no red/off-white buttons where expected: decline rgb=$dec answer rgb=$ans"; fi
  # --- Answer
  click_at "$POPUP_ANSWER_X" "$POPUP_BUTTON_Y"
  if wait_modem 2 12; then rec callui_popup_answer PASS "click on Answer -> modem off-hook"; else rec callui_popup_answer FAIL "modem $(modem_state) after clicking Answer"; fi
  sleep 2; app_shot call-popup-answered.png
  local ocr_live; ocr_live="$(ocr call-popup-answered.png)"
  # The Hang up label is white on red and some tesseract builds (CI) do not read it: accept a red pill where it is.
  local hu hr hg hb; hu="$(px call-popup-answered.png $((HANGUP_X - 40)) "$HANGUP_Y")"; read -r hr hg hb <<<"$hu"
  grep -qiE "In ?call" <<<"$ocr_live" && { grep -qiE "Hang ?up" <<<"$ocr_live" || { ((hr > 170 && hg < 90)); }; } \
    && rec callui_popup_incall PASS "in-call panel with timer and Hang up (OCR: $(echo "$ocr_live" | head -c 120))" \
    || rec callui_popup_incall FAIL "in-call panel not read: $ocr_live"
  # --- the audio hint: nothing routed the voice to this computer (no Bluetooth HFP here)
  local hint; hint="$(calls_json | tr '\n' ' ' | grep -io 'answered on the phone[^|]*' | head -1 | head -c 120)"
  if grep -qi "answered on the phone" <<<"$hint" && grep -qi "Audio on the phone" <<<"$ocr_live"; then
    rec callui_popup_hint PASS "no HFP route: window says 'Audio on the phone', service view says: $hint"
  else rec callui_popup_hint FAIL "hint missing; service: ${hint:-none}; window OCR: $ocr_live"; fi
  # --- Hang up from the panel
  click_at "$HANGUP_X" "$HANGUP_Y"
  if wait_modem 0 12; then rec callui_popup_hangup PASS "click on Hang up -> modem idle"; else rec callui_popup_hangup FAIL "modem $(modem_state) after clicking Hang up"; fi
  hang_up_all; sleep 2
  # --- Decline
  ring "$NUMBER_D" || { rec callui_popup_decline FAIL "second call never rang on the desktop"; return 1; }
  wait_for 15 popup_visible
  click_at "$POPUP_DECLINE_X" "$POPUP_BUTTON_Y"
  if wait_modem 0 12; then rec callui_popup_decline PASS "click on Decline -> modem idle"; else rec callui_popup_decline FAIL "modem $(modem_state) after clicking Decline"; fi
  sleep 2; app_shot call-popup-declined.png
  ocr call-popup-declined.png | grep -qiE "Answers on the phone|Bluetooth headset" && rec callui_popup_gone FAIL "popup still shown after Decline" || rec callui_popup_gone PASS "popup closed after Decline"
  rm -f "$DESK_SHOTS/call-popup-probe.png"
  hang_up_all
}

main_callui() {
  : >"$LOGS/e2e-run.log"
  if [[ "$STOPPING" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    stop_desktop; return 0
  fi
  [[ "$STAGES" == env,pair,notify,popup ]] && : >"$RESULTS"
  local st
  IFS=, read -r -a sts <<<"$STAGES"
  for st in "${sts[@]}"; do log "== stage $st"; "stage_$st" || log "stage $st reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 30
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_callui
