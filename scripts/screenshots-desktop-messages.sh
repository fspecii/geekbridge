#!/usr/bin/env bash
# Light and dark screenshots of the real Messages window, with a scripted phone
# (crates/bridge-phone-sim/examples/data_sim.rs) instead of an emulator. Private Xvfb, private
# service state; nothing touches the user's session.
#   scripts/screenshots-desktop-messages.sh      -> dist/screenshots/desktop/messages-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
OUT="$ROOT/dist/screenshots/desktop"; RUN="$ROOT/target/shots-msg"; rm -rf "$RUN/state"; mkdir -p "$OUT" "$RUN/state"; chmod 700 "$RUN/state"
DISP=:96; CP=17621; AP=17620; DP=17622
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
shot() { # theme name [env...]
  local theme="$1" name="$2"; shift 2
  env -u WAYLAND_DISPLAY DISPLAY=$DISP GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CP "$@" \
    $T/geekbridge-messages --api-port $AP --theme "$theme" >/dev/null 2>&1 &
  local pid=$!; sleep 6
  DISPLAY=$DISP import -window root "$OUT/$name-$theme.png"; kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
for t in light dark; do
  shot $t messages-list
  shot $t messages-search GEEKBRIDGE_MESSAGES_SEARCH=verification
  shot $t messages-compose GEEKBRIDGE_MESSAGES_COMPOSE=1 "GEEKBRIDGE_MESSAGES_DRAFT=Running late, see you at 10 🙂"
  shot $t messages-mms GEEKBRIDGE_MESSAGE_TO="+1 555 010 0002"
  shot $t messages-emoji GEEKBRIDGE_MESSAGE_TO="+44 20 7946 0003"
done
kill $SIM $SVC 2>/dev/null; pkill -f "Xvfb $DISP" 2>/dev/null
ls -1 "$OUT" | grep -E "^messages-"
