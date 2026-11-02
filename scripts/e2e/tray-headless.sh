#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Headless check of the Linux tray on a private session bus (no desktop, no phone, nothing on the
# user's bar): the real geekbridge-service against a fake StatusNotifierWatcher.
#
#   scripts/e2e/tray-headless.sh [--release] [--keep]
#
# 1. no watcher on the bus        -> the service runs normally, nothing is registered
# 2. a watcher                    -> RegisterStatusNotifierItem arrives, properties and the 15-line menu read back
# 3. `geekbridge-ctl tray disable`-> the service starts, nothing is registered
# 4. menu "Quit" clicked          -> the service exits
# Evidence (watcher log, the icon a bar would receive) is written to $OUT (default: target/tray-headless).
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
PROFILE=debug; KEEP=0
for a in "$@"; do case "$a" in --release) PROFILE=release ;; --keep) KEEP=1 ;; *) echo "unknown $a" >&2; exit 2 ;; esac; done
command -v dbus-daemon >/dev/null || { echo "SKIPPED: dbus-daemon not installed"; exit 0; }
T="${CARGO_TARGET_DIR:-$ROOT/target}"
if [[ $PROFILE == release ]]; then
    (cd "$ROOT" && cargo build -q --release -p bridge-service --bins --examples)
else
    (cd "$ROOT" && cargo build -q -p bridge-service --bins --examples)
fi
SVC="$T/$PROFILE/geekbridge-service"; CTL="$T/$PROFILE/geekbridge-ctl"; WATCH="$T/$PROFILE/examples/tray-fake-watcher"
OUT="${OUT:-$T/tray-headless}"; rm -rf "$OUT"; mkdir -p "$OUT"
source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp tray -d)"; PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true
            [[ $KEEP == 1 ]] && echo "kept $TMP" || rm -rf "$TMP"; }
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

cat > "$TMP/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><listen>unix:path=$TMP/bus</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
dbus-daemon --config-file="$TMP/bus.conf" --nofork --print-address=1 > "$TMP/bus.addr" 2>/dev/null &
PIDS+=($!)
until [[ -s "$TMP/bus.addr" ]]; do sleep 0.02; done
export DBUS_SESSION_BUS_ADDRESS="$(head -n1 "$TMP/bus.addr")"
export GEEKBRIDGE_NO_UPDATE_CHECK=1

port() { echo $((20000 + RANDOM % 20000)); }
start_service() { # log name
    local api; api=$(port)
    "$SVC" --state-dir "$TMP/state" --tray --api-port "$api" --control-port $((api + 1)) --discovery-port $((api + 2)) \
        --no-listen-inbound --no-announce --no-calling-probe > "$1" 2>&1 &
    SVC_PID=$!; PIDS+=("$SVC_PID")
    until grep -q 'discovery API' "$1" 2>/dev/null; do kill -0 "$SVC_PID" 2>/dev/null || { cat "$1" >&2; fail "service exited"; }; sleep 0.02; done
}
stop_service() { kill "$SVC_PID" 2>/dev/null || true; wait "$SVC_PID" 2>/dev/null || true; }

echo "== 1. no StatusNotifierWatcher: silently nothing"
start_service "$OUT/service-nohost.log"
sleep 1.5
kill -0 "$SVC_PID" || fail "service died without a tray host"
grep -qi 'tray\|zbus' "$OUT/service-nohost.log" && fail "service logged about the missing host: $(cat "$OUT/service-nohost.log")"
stop_service
echo "   ok"

echo "== 2. with a watcher: registers, properties and menu read back"
"$WATCH" --out "$OUT" > "$OUT/watcher.log" 2>&1 &
WPID=$!; PIDS+=("$WPID")
until grep -q 'WATCHER READY' "$OUT/watcher.log"; do sleep 0.02; done
start_service "$OUT/service.log"
for _ in $(seq 100); do grep -q '^MENU 15 ' "$OUT/watcher.log" && break; sleep 0.1; done
cat "$OUT/watcher.log"
grep -q '^REGISTERED org.kde.StatusNotifierItem-' "$OUT/watcher.log" || fail "not registered"
grep -q 'PROP Status=Active Title=GeekBridge ToolTip=GeekBridge|No phone paired yet Pixmaps=22,32,48' "$OUT/watcher.log" || fail "properties"
grep -q 'LAYOUT revision=1 items=15' "$OUT/watcher.log" || fail "layout"
for want in 'Open GeekBridge' 'Paste clipboard from phone' 'Send file' 'Pause sync' 'Check for updates' 'Quit'; do
    grep -q "^MENU .* $want" "$OUT/watcher.log" || fail "menu lacks $want"
done
[[ -s "$OUT/received-icon.png" ]] || fail "no icon received"
stop_service
echo "   ok"

echo "== 3. tray disabled by setting: nothing registered"
"$CTL" --state-dir "$TMP/state" tray disable
before=$(grep -c '^REGISTERED' "$OUT/watcher.log")
start_service "$OUT/service-disabled.log"
sleep 2
[[ $(grep -c '^REGISTERED' "$OUT/watcher.log") == "$before" ]] || fail "disabled tray still registered"
stop_service
"$CTL" --state-dir "$TMP/state" tray enable
echo "   ok"

echo "== 4. menu Quit stops the service"
kill "$WPID" 2>/dev/null || true; wait "$WPID" 2>/dev/null || true
"$WATCH" --out "$OUT" --click 15 > "$OUT/watcher-quit.log" 2>&1 &
PIDS+=($!)
until grep -q 'WATCHER READY' "$OUT/watcher-quit.log"; do sleep 0.02; done
start_service "$OUT/service-quit.log"
for _ in $(seq 50); do kill -0 "$SVC_PID" 2>/dev/null || break; sleep 0.1; done
kill -0 "$SVC_PID" 2>/dev/null && fail "service still running after Quit"
grep -q '^CLICKED 15' "$OUT/watcher-quit.log" || fail "click not sent"
echo "   ok"
echo "tray-headless OK (evidence in $OUT)"
