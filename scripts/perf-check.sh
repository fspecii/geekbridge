#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Measure the desktop service against the perf budget in docs/performance.md; exit 1 when over.
#
#   scripts/perf-check.sh [--build] [--bin-dir DIR] [--window SECONDS] [--ui]
#
#   --build        cargo build --release --locked first (bridge-service, plus bridge-apps with --ui)
#   --bin-dir DIR  where the release binaries are (default: $CARGO_TARGET_DIR/release or target/release)
#   --window N     seconds of idle observation for the wakeup count (default 20)
#   --ui           also measure geekbridge-messages (needs a display; skipped without DISPLAY/WAYLAND_DISPLAY)
#
# Everything is read from /proc (Linux only): RSS, threads, fds, context switches ("wakeups") and CPU
# ticks. No perf/strace/valgrind needed. Budgets can be overridden through the environment, e.g.
# BUDGET_RSS_KIB=9000. The service runs on private ports in a temp state dir and never touches the
# user's real state, the network beyond loopback/UDP discovery on a private port, or the system bus.
# The tray runs too, on a private session bus with a stand-in StatusNotifierWatcher (see below).
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
WINDOW=20
BUILD=0
UI=0

# GB_CI=1 (set by .github/workflows/check.yml): shared runners have other CPUs, noisy neighbours and a different
# libc/kernel, so the load-dependent rows get documented slack. Sizes stay strict. Explicit BUDGET_* overrides win (CI values are only defaults, applied before the normal ones).
if [[ ${GB_CI:-} == 1 ]]; then
    : "${BUDGET_RSS_KIB:=8000}" "${BUDGET_THREADS:=10}" "${BUDGET_FDS:=24}" "${BUDGET_WAKEUPS_PER_S:=6}"
    : "${BUDGET_CPU_TICKS:=6}" "${BUDGET_STARTUP_MS:=1000}"
fi

# Budgets (service, idle, no phone linked). Measured baseline in docs/performance.md.
: "${BUDGET_RSS_KIB:=6000}"           # resident set (10000 -> 6000: idle code pages are dropped 2 s after start, 3.4 MB measured, docs/performance.md)
: "${BUDGET_THREADS:=8}"              # OS threads
: "${BUDGET_FDS:=20}"                 # open descriptors
: "${BUDGET_WAKEUPS_PER_S:=3}"        # context switches per second, whole process
: "${BUDGET_CPU_TICKS:=2}"            # CPU ticks (10 ms each) burned over the whole window
: "${BUDGET_STARTUP_MS:=300}"         # exec to "discovery API" line, warm cache
: "${BUDGET_SERVICE_BYTES:=6700000}"  # stripped binary size (6.2 -> 6.7 MB: 6 598 488 B locally at the call-audio routing commit: the service now uses the libpulse adapter, the BlueZ pairing agent, the call-audio controller and checklist, ~250 kB of own code plus zbus/serde instantiations, no new dependency, docs/performance.md; 6.1 -> 6.2 MB: CI 6 138 064 B at b5633caee, pairing/call-audio setup/calls window, no new dependency; 5.0 -> 5.3 -> 5.6 -> 5.8 -> 6.0 -> 6.1 MB: updater asset selection, Messages and Wi-Fi bind safety net, 6.003 MB measured in CI; phone mirroring, 5.82 MB measured in CI; the tray, then CI-measured, docs/performance.md)
: "${BUDGET_CTL_BYTES:=1550000}"       # 1 509 632 B locally at the call-audio routing commit (HearCalls/CallAudioDevices wire types, `call setup`/`call audio-devices`; the checklist itself stays in the service), so 1.55 MB; 1 450 704 B on CI at b5633caee (pairing, call-audio setup and calls-window wire types), so 1.5 MB; 1 358 984 B on CI after the share-result commit, so 1.4 MB (docs/performance.md); 1 214 392 B after Response::from_json (1.3 MB budget); 1 318 016 B with the recent-photos requests and `photos` command, so 1.35 MB again (docs/performance.md)
: "${BUDGET_UI_RSS_KIB:=75000}"       # geekbridge-messages, idle, warm GL shader cache (65 MB measured 2026-11-02; 90000 before the memtrim/atlas work)
: "${BUDGET_UI_WAKEUPS_PER_S:=12}"

while (($#)); do
    case "$1" in
        --build) BUILD=1 ;;
        --ui) UI=1 ;;
        --bin-dir) BIN_DIR="$2"; shift ;;
        --window) WINDOW="$2"; shift ;;
        -h|--help) sed -n '3,14p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

if ((BUILD)); then
    pkgs=(-p bridge-service)
    ((UI)) && pkgs+=(-p bridge-apps)
    (cd "$ROOT" && cargo build --release --locked "${pkgs[@]}" --bins --example tray-fake-watcher)
fi
BIN_DIR="$(cd "$BIN_DIR" 2>/dev/null && pwd -P || echo "$BIN_DIR")"
[[ -x "$BIN_DIR/geekbridge-service" ]] || { echo "no release binary in $BIN_DIR (use --build or --bin-dir)" >&2; exit 2; }

source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp perf -d)"
PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    [[ -n "${PERF_KEEP:-}" ]] && echo "kept $TMP" || rm -rf "$TMP"
}
trap cleanup EXIT

FAIL=0
row() { # name value budget unit
    local ok=ok
    if (($2 > $3)); then ok=OVER; FAIL=1; fi
    printf '%-26s %10s %s   (budget %s)  %s\n' "$1" "$2" "$4" "$3" "$ok"
}
# Voluntary context switches summed over every thread: each is a thread that blocked and was woken
# again, i.e. one wakeup. Involuntary (preempted) switches are excluded: they measure machine load.
ctxt() { cat /proc/"$1"/task/*/status | awk '$1 == "voluntary_ctxt_switches:" {s += $2} END {print s + 0}'; }
ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
status() { awk -v k="$2:" '$1 == k {print $2}' "/proc/$1/status"; }
port() { echo $((20000 + RANDOM % 20000)); }

export GEEKBRIDGE_NO_UPDATE_CHECK=1
# The tray is part of the measured service: run it on a private session bus (never the user's) with
# a stand-in StatusNotifierWatcher when the example is built, so the item is registered and idle,
# which is what a desktop with a bar looks like. Without dbus-daemon the tray simply has no bus.
# PERF_NO_TRAY=1 measures the service without it (A/B).
TRAY_FLAG=()
if [[ -z "${PERF_NO_TRAY:-}" ]] && command -v dbus-daemon >/dev/null; then
    cat > "$TMP/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><listen>unix:path=$TMP/bus</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
    dbus-daemon --config-file="$TMP/bus.conf" --nofork --print-address=1 > "$TMP/bus.addr" 2>/dev/null &
    PIDS+=("$!")
    until [[ -s "$TMP/bus.addr" ]]; do sleep 0.02; done
    export DBUS_SESSION_BUS_ADDRESS="$(head -n1 "$TMP/bus.addr")"
    TRAY_FLAG=(--tray)
    if [[ -x "$BIN_DIR/examples/tray-fake-watcher" ]]; then
        "$BIN_DIR/examples/tray-fake-watcher" >"$TMP/watcher.log" 2>&1 &
        PIDS+=("$!")
        until grep -q 'WATCHER READY' "$TMP/watcher.log" 2>/dev/null; do sleep 0.02; done
    fi
fi
API_PORT=$(port) CONTROL_PORT=$((API_PORT + 1)) DISC_PORT=$((API_PORT + 2))

start_ns=$(date +%s%N)
"$BIN_DIR/geekbridge-service" --state-dir "$TMP/state" --api-port "$API_PORT" \
    --control-port "$CONTROL_PORT" --discovery-port "$DISC_PORT" --no-calling-probe "${TRAY_FLAG[@]}" >"$TMP/service.log" 2>&1 &
SVC=$!
PIDS+=("$SVC")
until grep -q 'discovery API' "$TMP/service.log" 2>/dev/null; do
    kill -0 "$SVC" 2>/dev/null || { cat "$TMP/service.log" >&2; echo "service exited" >&2; exit 1; }
    sleep 0.002
done
startup_ms=$((($(date +%s%N) - start_ns) / 1000000))

sleep 3 # settle: first-run key generation and allocator warm-up
c0=$(ctxt "$SVC"); t0=$(ticks "$SVC")
sleep "$WINDOW"
c1=$(ctxt "$SVC"); t1=$(ticks "$SVC")
rss=$(status "$SVC" VmRSS); threads=$(status "$SVC" Threads); fds=$(find "/proc/$SVC/fd" -mindepth 1 | wc -l)
wakeups=$(((c1 - c0 + WINDOW - 1) / WINDOW))

echo "== geekbridge-service (idle ${WINDOW}s window) =="
row "startup" "$startup_ms" "$BUDGET_STARTUP_MS" ms
row "idle RSS" "$rss" "$BUDGET_RSS_KIB" kB
row "threads" "$threads" "$BUDGET_THREADS" ""
row "open fds" "$fds" "$BUDGET_FDS" ""
row "wakeups" "$wakeups" "$BUDGET_WAKEUPS_PER_S" "/s"
row "cpu ticks in window" "$((t1 - t0))" "$BUDGET_CPU_TICKS" "x10ms"
row "geekbridge-service size" "$(stat -c %s "$BIN_DIR/geekbridge-service")" "$BUDGET_SERVICE_BYTES" B
[[ -f "$BIN_DIR/geekbridge-ctl" ]] && row "geekbridge-ctl size" "$(stat -c %s "$BIN_DIR/geekbridge-ctl")" "$BUDGET_CTL_BYTES" B

if ((UI)); then
    if [[ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" || ! -x "$BIN_DIR/geekbridge-messages" ]]; then
        echo "== geekbridge-messages: skipped (no display or binary) =="
    else
        # Warm-up launch: with a cold GL shader disk cache the NVIDIA driver compiles in-process and
        # keeps ~40 MB (110 MB RSS vs 65 MB); that is the driver, not the app, so measure warm.
        (cd "$TMP" && timeout 5 "$BIN_DIR/geekbridge-messages" --api-port "$API_PORT" >/dev/null 2>&1) || true
        (cd "$TMP" && exec "$BIN_DIR/geekbridge-messages" --api-port "$API_PORT" >"$TMP/ui.log" 2>&1) &
        UIP=$!
        PIDS+=("$UIP")
        sleep 6
        u0=$(ctxt "$UIP"); sleep "$WINDOW"; u1=$(ctxt "$UIP")
        echo "== geekbridge-messages (idle ${WINDOW}s window) =="
        row "idle RSS" "$(status "$UIP" VmRSS)" "$BUDGET_UI_RSS_KIB" kB
        row "wakeups" "$(((u1 - u0 + WINDOW - 1) / WINDOW))" "$BUDGET_UI_WAKEUPS_PER_S" "/s"
    fi
fi

if ((FAIL)); then
    echo "PERF BUDGET EXCEEDED" >&2
    exit 1
fi
echo "within budget"
