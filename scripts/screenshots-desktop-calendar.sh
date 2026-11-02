#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Light and dark screenshots of the real Contacts and Calendar windows, with a scripted phone
# (crates/bridge-phone-sim/examples/data_sim.rs) instead of an emulator. Private Xvfb, private
# service state; nothing touches the user's session.
#   scripts/screenshots-desktop-calendar.sh      -> dist/screenshots/desktop/{calendar,contacts}-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
source scripts/android-env.sh >/dev/null 2>&1
OUT="$ROOT/dist/screenshots/desktop"; RUN="$ROOT/target/shots-cal"; rm -rf "$RUN/state"; mkdir -p "$OUT" "$RUN/state"; chmod 700 "$RUN/state"
DISP=:95; CP=17611; AP=17610; DP=17612
cargo build -q -p bridge-service -p bridge-apps --bins && cargo build -q -p bridge-phone-sim --example data_sim || exit 1
T=target/debug
pkill -f "Xvfb $DISP" 2>/dev/null; (setsid nohup Xvfb $DISP -screen 0 1280x800x24 >"$RUN/xvfb.log" 2>&1 &); sleep 1
env -u WAYLAND_DISPLAY GEEKBRIDGE_NO_UPDATE_CHECK=1 DISPLAY=$DISP setsid nohup $T/geekbridge-service --state-dir "$RUN/state" \
  --api-port $AP --control-port $CP --discovery-port $DP --no-listen-inbound --no-announce --dev-loopback --no-calling-probe >"$RUN/service.log" 2>&1 &
SVC=$!
for _ in $(seq 200); do $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP status >/dev/null 2>&1 && break; sleep 0.2; done
$T/examples/data_sim "$RUN/state" $CP >"$RUN/sim.log" 2>&1 &
SIM=$!
for _ in $(seq 100); do grep -q "paired as" "$RUN/sim.log" && break; sleep 0.2; done
sleep 3
shot() { # app theme name [env...]
  local app="$1" theme="$2" name="$3"; shift 3
  env -u WAYLAND_DISPLAY DISPLAY=$DISP GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CP "$@" \
    $T/$app --api-port $AP --theme "$theme" >/dev/null 2>&1 &
  local pid=$!; sleep 5
  DISPLAY=$DISP import -window root "$OUT/$name-$theme.png"; kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
for t in light dark; do
  shot geekbridge-agenda $t calendar-month
  shot geekbridge-agenda $t calendar-week GEEKBRIDGE_CALENDAR_VIEW=week
  shot geekbridge-agenda $t calendar-day GEEKBRIDGE_CALENDAR_VIEW=day
  shot geekbridge-agenda $t calendar-agenda GEEKBRIDGE_CALENDAR_VIEW=agenda
  shot geekbridge-contacts $t contacts
  shot geekbridge-contacts $t contacts-details GEEKBRIDGE_CONTACTS_SELECT_FIRST=1
  shot geekbridge-agenda $t calendar-event GEEKBRIDGE_CALENDAR_VIEW=week GEEKBRIDGE_CALENDAR_SELECT=Dentist
  shot geekbridge-agenda $t calendar-editor GEEKBRIDGE_CALENDAR_EDITOR=1
done
kill $SIM $SVC 2>/dev/null; pkill -f "Xvfb $DISP" 2>/dev/null
ls -1 "$OUT" | grep -E "^(calendar|contacts)-"
