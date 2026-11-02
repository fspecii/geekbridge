#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Light and dark screenshots of every desktop screen in the Nothing-inspired look, from the real
# windows against a scripted phone (crates/bridge-phone-sim/examples/data_sim.rs). Private Xvfb and
# private service state; nothing touches the user's session.
#   scripts/screenshots-desktop-nothing.sh [THEME...]   -> dist/screenshots/desktop/nothing-*.png
#   ONLY='^(home|pair)$' scripts/...    re-shoot only the named screens
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
source scripts/android-env.sh >/dev/null 2>&1
OUT="${OUT:-$ROOT/dist/screenshots/desktop}"; PREFIX="${PREFIX:-nothing}"; RUN="${RUN:-$ROOT/target/shots-nothing}"; rm -rf "$RUN/state"; mkdir -p "$OUT" "$RUN/state"; chmod 700 "$RUN/state"
DISP=${DISP:-:94}; CP=${CP:-17711}; AP=${AP:-17710}; DP=${DP:-17712}
cargo build -q -p bridge-service -p bridge-apps --bins && cargo build -q -p bridge-phone-sim --example data_sim || exit 1
T=target/debug
pkill -f "Xvfb $DISP" 2>/dev/null; (setsid nohup Xvfb $DISP -screen 0 1280x860x24 >"$RUN/xvfb.log" 2>&1 &); sleep 1
env -u WAYLAND_DISPLAY GEEKBRIDGE_NO_UPDATE_CHECK=1 DISPLAY=$DISP setsid nohup $T/geekbridge-service --state-dir "$RUN/state" \
  --api-port $AP --control-port $CP --discovery-port $DP --no-listen-inbound --no-announce --dev-loopback --no-calling-probe >"$RUN/service.log" 2>&1 &
SVC=$!
for _ in $(seq 200); do $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP status >/dev/null 2>&1 && break; sleep 0.2; done
$T/examples/data_sim "$RUN/state" $CP >"$RUN/sim.log" 2>&1 &
SIM=$!
for _ in $(seq 100); do grep -q "paired as" "$RUN/sim.log" && break; sleep 0.2; done
sleep 3
# shot APP THEME NAME [KEYS] [ENV=VAL...]   KEYS is an xdotool key sequence ("-" for none)
shot() {
  local app="$1" theme="$2" name="$3" keys="${4:--}"; shift 4 2>/dev/null || shift $#
  [ -n "${ONLY:-}" ] && ! [[ "$name" =~ $ONLY ]] && return 0
  env -u WAYLAND_DISPLAY DISPLAY=$DISP GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CP "$@" \
    $T/$app $([ "$app" = geekbridge-share ] || echo "--api-port $AP") --theme "$theme" >/dev/null 2>&1 &
  local pid=$!; sleep 5; [ "$app" = geekbridge-share ] && sleep 8
  local wid; wid=$(DISPLAY=$DISP xdotool search --onlyvisible --pid $pid 2>/dev/null | head -1)
  [ -z "$wid" ] && wid=$(DISPLAY=$DISP xdotool search --onlyvisible --class dev.geekbridge 2>/dev/null | head -1)
  if [ "$keys" != "-" ] && [ -n "$wid" ]; then
    DISPLAY=$DISP xdotool windowfocus --sync "$wid" key --delay 150 $keys; sleep 1.5
  fi
  if [ -n "$wid" ]; then
    local WIDTH HEIGHT X Y SCREEN WINDOW
    eval "$(DISPLAY=$DISP xdotool getwindowgeometry --shell "$wid")"
    DISPLAY=$DISP import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$OUT/$PREFIX-$name-$theme.png"
  else
    DISPLAY=$DISP import -window root "$OUT/$PREFIX-$name-$theme.png"
  fi
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
THEMES=("$@"); [ ${#THEMES[@]} -eq 0 ] && THEMES=(light dark)
for t in "${THEMES[@]}"; do
  shot geekbridge $t home - 
  shot geekbridge $t whatsnew - GEEKBRIDGE_FORCE_WHATS_NEW=1
  shot geekbridge $t pair ctrl+2
  shot geekbridge $t settings ctrl+3
  shot geekbridge-clipboard $t clipboard -
  shot geekbridge-share $t share -
  shot geekbridge-calls $t calls-keypad -
  shot geekbridge-calls $t call-incoming - GEEKBRIDGE_CALLS_FIXTURE=ringing
  shot geekbridge-calls $t call-active - GEEKBRIDGE_CALLS_FIXTURE=active
  shot geekbridge-messages $t messages -
  shot geekbridge-contacts $t contacts -
  shot geekbridge-contacts $t contacts-details - GEEKBRIDGE_CONTACTS_SELECT_FIRST=1
  shot geekbridge-agenda $t calendar-month -
  shot geekbridge-agenda $t calendar-agenda - GEEKBRIDGE_CALENDAR_VIEW=agenda
done
kill $SIM $SVC 2>/dev/null; pkill -f "Xvfb $DISP" 2>/dev/null
ls -1 "$OUT" | grep -E "^$PREFIX-"
