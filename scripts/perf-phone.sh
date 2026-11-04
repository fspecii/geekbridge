#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Measure the desktop service WITH a stand-in phone linked: idle, a ring/answer/hang-up loop, and a Wi-Fi call audio
# listen session (docs/performance.md, "Phone linked"). Everything runs sandboxed: a private HOME / XDG / state dir,
# private loopback ports, a private session bus, stub `pw-cat` / `pactl` / `geekbridge-calls` in a private bin dir
# (the call card and the sound server are counted, never started for real). Nothing touches ~/.config, the live
# session, Bluetooth, or ports 1716, 17381 and 8000.
#
#   scripts/perf-phone.sh [--bin-dir DIR] [--cycles N] [--listen SECONDS] [--idle SECONDS] [--scenarios "idle cycles listen seed"] [--seed MESSAGES CONTACTS]
#
# Needs: cargo build --release -p bridge-service --bins --example perf-phone --example tray-fake-watcher
set -Eeuo pipefail
IFS=$'\n\t'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
CYCLES=200 LISTEN=60 IDLE=20 SCEN="idle cycles listen" MSGS=5000 CONTACTS=2000
while (($#)); do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift ;;
        --cycles) CYCLES="$2"; shift ;;
        --listen) LISTEN="$2"; shift ;;
        --idle) IDLE="$2"; shift ;;
        --scenarios) SCEN="$2"; shift ;; # any of: idle cycles listen seed
        --seed) MSGS="$2"; CONTACTS="$3"; shift 2 ;;
        -h|--help) sed -n '3,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
BIN_DIR="$(cd "$BIN_DIR" && pwd -P)"
source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp perfphone -d)"
PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    [[ -n "${PERF_KEEP:-}" ]] && echo "kept $TMP" || rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$TMP/bin" "$TMP/home" "$TMP/xdg/config" "$TMP/xdg/data" "$TMP/xdg/state" "$TMP/xdg/run"
chmod 700 "$TMP/xdg/run"
cp "$BIN_DIR/geekbridge-service" "$TMP/bin/"
# Stub card (waits until killed, like the overlay window), stub sound tools.
printf '#!/bin/sh\nexec sleep 3600\n' > "$TMP/bin/geekbridge-calls"
printf '#!/bin/sh\ncat >/dev/null\n' > "$TMP/bin/pw-cat"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/notify-send"
cat > "$TMP/bin/pactl" <<'STUB'
#!/bin/sh
for a in "$@"; do
    case "$a" in
        info) echo '{"server_name":"PulseAudio (on PipeWire 1.6.9)","server_version":"15.0.0","host_name":"perf","default_sink_name":"out","default_source_name":"in","cookie":"cb0e:172e"}'; exit 0 ;;
        cards) echo '[{"index":1,"name":"alsa_card.x","properties":{"object.id":"74"},"profiles":{"off":{"available":true}},"active_profile":"off"}]'; exit 0 ;;
        sinks) echo '[{"index":65,"name":"out","description":"Stand-in speaker","mute":false,"monitor_source":"out.monitor","properties":{"device.id":"74"}}]'; exit 0 ;;
        sources) echo '[{"index":68,"name":"in","description":"Stand-in microphone","mute":false,"properties":{"device.id":"74","device.class":"sound"}}]'; exit 0 ;;
        sink-inputs|source-outputs) echo '[]'; exit 0 ;;
    esac
done
echo '[]'
STUB
chmod +x "$TMP/bin/"*

# A private session bus with a stand-in StatusNotifierWatcher, as in perf-check.sh.
if command -v dbus-daemon >/dev/null; then
    cat > "$TMP/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><listen>unix:path=$TMP/bus</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
    dbus-daemon --config-file="$TMP/bus.conf" --nofork --print-address=1 > "$TMP/bus.addr" 2>/dev/null &
    PIDS+=("$!")
    until [[ -s "$TMP/bus.addr" ]]; do sleep 0.02; done
    export DBUS_SESSION_BUS_ADDRESS="$(head -n1 "$TMP/bus.addr")"
    TRAY=(--tray)
    if [[ -x "$BIN_DIR/examples/tray-fake-watcher" ]]; then
        "$BIN_DIR/examples/tray-fake-watcher" >"$TMP/watcher.log" 2>&1 &
        PIDS+=("$!")
        until grep -q 'WATCHER READY' "$TMP/watcher.log" 2>/dev/null; do sleep 0.02; done
    fi
else
    TRAY=()
fi

P=$((20000 + RANDOM % 20000))
env -i PATH="$TMP/bin:/usr/bin:/bin" HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/xdg/config" XDG_DATA_HOME="$TMP/xdg/data" \
    XDG_STATE_HOME="$TMP/xdg/state" XDG_RUNTIME_DIR="$TMP/xdg/run" TMPDIR="$TMP" DISPLAY=:99 \
    ${DBUS_SESSION_BUS_ADDRESS:+DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"} \
    GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_CALL_OVERLAY=on ${PERF_MALLOC_ARENA_MAX:+MALLOC_ARENA_MAX=$PERF_MALLOC_ARENA_MAX} \
    "$TMP/bin/geekbridge-service" --state-dir "$TMP/state" --api-port "$P" --control-port $((P + 1)) --discovery-port $((P + 2)) \
    --listen-inbound "127.0.0.1:$((P + 3))" --no-announce --no-calling-probe --dev-loopback "${TRAY[@]}" >"$TMP/service.log" 2>&1 &
SVC=$!
PIDS+=("$SVC")
until grep -q 'discovery API' "$TMP/service.log" 2>/dev/null; do
    kill -0 "$SVC" 2>/dev/null || { cat "$TMP/service.log" >&2; echo "service exited" >&2; exit 1; }
    sleep 0.01
done
sleep 3

mkfifo "$TMP/cmd"
"$BIN_DIR/examples/perf-phone" --state-dir "$TMP/state" --control-port $((P + 1)) --pid "$SVC" < "$TMP/cmd" > "$TMP/phone.out" 2>"$TMP/phone.err" &
PHONE=$!
PIDS+=("$PHONE")
exec 7> "$TMP/cmd"
declare -A DONE_N
send() { # command: send it and wait for its next DONE line
    local name="${1%% *}"
    DONE_N[$name]=$(( ${DONE_N[$name]:-0} + 1 ))
    echo "$1" >&7
    until [[ "$(grep -c "^DONE $name\$" "$TMP/phone.out" 2>/dev/null || true)" -ge "${DONE_N[$name]}" ]]; do
        kill -0 "$PHONE" 2>/dev/null || { cat "$TMP/phone.err" >&2; echo "stand-in phone exited" >&2; exit 1; }
        sleep 0.2
    done
}
until grep -q '^PAIRED' "$TMP/phone.out" 2>/dev/null; do
    kill -0 "$PHONE" 2>/dev/null || { cat "$TMP/phone.err" "$TMP/service.log" >&2; echo "stand-in phone exited" >&2; exit 1; }
    sleep 0.1
done
sleep 4 # let the first reports settle, then the 2 s code page drop has certainly run
send "sample linked-start" 1
IFS=" " read -ra SCEN_LIST <<< "$SCEN"
for sc in "${SCEN_LIST[@]}"; do
    case "$sc" in
        idle) send "idle $IDLE" ;;
        cycles) send "cycles $CYCLES"; send "idle 10" ;;
        listen) send "listen $LISTEN" ;;
        seed) send "seed $MSGS $CONTACTS"; send "idle 20" ;;
        ui) # geekbridge-messages on a private Xvfb against the seeded service (software GL: the absolute figure is not a GPU desktop's)
            D=$((200 + RANDOM % 50)); Xvfb ":$D" -screen 0 1600x1000x24 >"$TMP/xvfb.log" 2>&1 & PIDS+=("$!"); sleep 1
            for warm in 1 2; do
                (cd "$TMP" && DISPLAY=":$D" XDG_RUNTIME_DIR="$TMP/xdg/run" HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/xdg/config" XDG_DATA_HOME="$TMP/xdg/data" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_STATE_DIR="$TMP/state" GEEKBRIDGE_CONTROL_PORT=$((P + 1)) \
                    timeout "$([ $warm = 1 ] && echo 6 || echo 1000)" "$BIN_DIR/geekbridge-messages" --api-port "$P" >"$TMP/ui.log" 2>&1) & UIP=$!
                [[ $warm = 1 ]] && wait $UIP || true
            done
            PIDS+=("$UIP"); sleep 12
            UIPID=$(pgrep -f "geekbridge-messages --api-port $P" | tail -1)
            echo "UI geekbridge-messages: $(grep -E 'VmRSS|RssAnon' /proc/$UIPID/status | tr -s ' \n\t' ' ') threads=$(grep Threads /proc/$UIPID/status | cut -f2)"
            ;;
    esac
done
echo quit >&7
wait "$PHONE" || true
echo "== stand-in phone linked: service $(stat -c %s "$BIN_DIR/geekbridge-service") B =="
cat "$TMP/phone.out" | grep -v '^DONE'
[[ -s "$TMP/phone.err" ]] && { echo "-- phone stderr --"; tail -5 "$TMP/phone.err"; }
exit 0
