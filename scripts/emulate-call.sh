#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run the emulated-call end-to-end check against an isolated D-Bus session bus.
#
#   scripts/emulate-call.sh fake     [options]   CI-safe: scripted fake modem, no emulator
#   scripts/emulate-call.sh emulator [options]   real Android emulator modem via adb
#   scripts/emulate-call.sh --help
#
# What it does: builds geekbridge-phone-sim and geekbridge-call-e2e (cargo --locked) and runs
# everything under `dbus-run-session`, so no real session bus is ever touched.
#   fake:     geekbridge-call-e2e --fake. The harness is self-contained: it starts its own private
#             dbus-daemon and an in-process simulator over an in-memory modem (runs S1-S8).
#   emulator: starts geekbridge-phone-sim (a fake org.pipewire.Telephony service backed by the
#             Android emulator modem through adb) on the private bus, then runs the harness
#             against it with the real bridge-calls Session (S1-S6 and S8; S7 is skipped unless
#             --spawn-sim is given).
# The exit status is the harness's: 0 pass, 1 scenario failure, 2 usage/setup error.
#
# Only simulated cellular call signalling is exercised. Bluetooth pairing, HFP AT commands, SCO
# audio, codecs and real phones are NOT covered. See docs/emulated-calls.md.
#
# Options:
#   --only S1,S4        run only these scenarios
#   --spawn-sim         emulator mode only: let the harness run the simulator in-process on its own
#                       private bus instead of a separate geekbridge-phone-sim process. This is
#                       the only way to run S7 (service loss) against the emulator. Default numbers
#                       differ per mode (fake 5551234, emulator 5557701); both are fictional.
#   --json              ask the harness for JSON output
#   --interval-ms N     simulator modem poll interval (emulator mode; default 300)
#   --release           build and run release binaries
#   --no-build          skip cargo build (binaries must already exist)
#   --log-dir DIR       write simulator.log / e2e.log here (default: dist/android/logs/... for
#                       emulator, a temp dir for fake; the temp dir is removed after a pass)
#   --keep-logs         never delete the log directory
#
# Environment: CARGO_TARGET_DIR (honoured), ADB (adb binary; default .tools/android-sdk/
# platform-tools/adb, then `adb` on PATH), GB_EMU_SERIAL (default emulator-5584),
# GB_EMU_ADB_PORT (adb server port, default 16037).
#
# The private bus is the only bus used: a real org.pipewire.Telephony owner on the user's buses is
# reported but never touched; the script refuses if the name is already owned on the private bus.
# Nothing global is changed: no pkill, no installs, only child processes started here are killed.
set -euo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SELF="$ROOT/scripts/emulate-call.sh"
WELL_KNOWN_NAME="org.pipewire.Telephony"

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'; }
die() { echo "emulate-call: $*" >&2; exit 2; }
log() { echo "emulate-call: $*" >&2; }

MODE=""
INNER=0
ONLY=""
JSON=0
SPAWN_SIM=0
INTERVAL=""
PROFILE=debug
BUILD=1
LOG_DIR="${GB_EMULATE_LOG_DIR:-}"
KEEP_LOGS=0
SERIAL="${GB_EMU_SERIAL:-emulator-5584}"
ADB_PORT="${GB_EMU_ADB_PORT:-16037}"
ADB_BIN="${ADB:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        fake|emulator) [ -z "$MODE" ] || die "mode given twice"; MODE="$1" ;;
        --inner) INNER=1 ;;
        --only) [ $# -ge 2 ] || die "--only needs a value"; ONLY="$2"; shift ;;
        --json) JSON=1 ;;
        --spawn-sim) SPAWN_SIM=1 ;;
        --interval-ms) [ $# -ge 2 ] || die "--interval-ms needs a value"; INTERVAL="$2"; shift ;;
        --release) PROFILE=release ;;
        --no-build) BUILD=0 ;;
        --log-dir) [ $# -ge 2 ] || die "--log-dir needs a value"; LOG_DIR="$2"; shift ;;
        --keep-logs) KEEP_LOGS=1 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done
[ -n "$MODE" ] || { usage >&2; exit 2; }
[ "$SPAWN_SIM" -eq 0 ] || [ "$MODE" = emulator ] || die "--spawn-sim applies to emulator mode only (fake mode is already self-contained)"

# Validate user-supplied values before they reach any command line.
ONLY="${ONLY^^}"
case "$ONLY" in *[!S0-9,]*) die "--only must look like S1,S4" ;; esac
case "$INTERVAL" in *[!0-9]*) die "--interval-ms must be a number" ;; esac
case "$ADB_PORT" in ''|*[!0-9]*) die "GB_EMU_ADB_PORT must be a number" ;; esac
case "$SERIAL" in ''|*[!A-Za-z0-9_.:-]*) die "GB_EMU_SERIAL contains unexpected characters" ;; esac

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$PWD/$TARGET_DIR" ;; esac
BIN_DIR="$TARGET_DIR/$PROFILE"
SIM_BIN="$BIN_DIR/geekbridge-phone-sim"
E2E_BIN="$BIN_DIR/geekbridge-call-e2e"

# ---- helpers -------------------------------------------------------------------------------

# name_has_owner BUS_ARG...   (BUS_ARG is a dbus-send/busctl style selector)
# Prints "yes", "no" or "unknown" (no tool, or bus unreachable).
name_owned_on() {
    local kind="$1" addr="${2:-}" out
    if command -v dbus-send >/dev/null 2>&1; then
        local sel
        case "$kind" in
            session) sel="--session" ;;
            system) sel="--system" ;;
            address) sel="--bus=$addr" ;;
        esac
        if out="$(dbus-send "$sel" --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
                org.freedesktop.DBus.NameHasOwner "string:$WELL_KNOWN_NAME" 2>/dev/null)"; then
            case "$out" in *"boolean true"*) echo yes ;; *"boolean false"*) echo no ;; *) echo unknown ;; esac
            return 0
        fi
    fi
    echo unknown
}

resolve_adb() {
    if [ -z "$ADB_BIN" ]; then
        if [ -x "$ROOT/.tools/android-sdk/platform-tools/adb" ]; then
            ADB_BIN="$ROOT/.tools/android-sdk/platform-tools/adb"
        elif command -v adb >/dev/null 2>&1; then
            ADB_BIN="$(command -v adb)"
        else
            die "adb not found (set ADB=/path/to/adb or run scripts/bootstrap-android.sh)"
        fi
    fi
    [ -x "$ADB_BIN" ] || die "ADB is not executable: $ADB_BIN"
}

build() {
    command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH"
    local flags=(--locked -p bridge-phone-sim --bins)
    [ "$PROFILE" = release ] && flags+=(--release)
    log "building (target dir: $TARGET_DIR)"
    (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build "${flags[@]}") >&2
}

# ---- inner phase: runs inside dbus-run-session ------------------------------------------------

sim_pid=""
cleanup() {
    local rc=$?
    trap - EXIT INT TERM
    if [ -n "$sim_pid" ] && kill -0 "$sim_pid" 2>/dev/null; then
        kill "$sim_pid" 2>/dev/null || true
        # Give it a moment to exit, then force. Only the PID we started.
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$sim_pid" 2>/dev/null || break
            sleep 0.2
        done
        kill -9 "$sim_pid" 2>/dev/null || true
        wait "$sim_pid" 2>/dev/null || true
    fi
    exit "$rc"
}

start_simulator() {
    local bus="$1"; shift
    if [ "$(name_owned_on address "$bus")" = yes ]; then
        die "$WELL_KNOWN_NAME already has an owner on the private bus; refusing to start"
    fi
    log "starting simulator (emulator modem); log: $LOG_DIR/simulator.log"
    "$SIM_BIN" "$@" </dev/null >"$LOG_DIR/simulator.log" 2>&1 &
    sim_pid=$!

    local i owned="unknown"
    for i in $(seq 1 100); do
        kill -0 "$sim_pid" 2>/dev/null || { tail -n 20 "$LOG_DIR/simulator.log" >&2 || true; die "simulator exited early"; }
        owned="$(name_owned_on address "$bus")"
        [ "$owned" = no ] || break
        sleep 0.2
    done
    case "$owned" in
        yes) log "simulator owns $WELL_KNOWN_NAME" ;;
        unknown) log "cannot query the bus (no dbus-send?); giving the simulator 2s"; sleep 2 ;;
        *) tail -n 20 "$LOG_DIR/simulator.log" >&2 || true; die "simulator did not claim $WELL_KNOWN_NAME within 20s" ;;
    esac
}

run_inner() {
    [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || die "inner phase must run under dbus-run-session"
    [ -n "$LOG_DIR" ] || die "inner phase needs a log dir"
    local bus="$DBUS_SESSION_BUS_ADDRESS"
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    local e2e_args=()
    if [ "$MODE" = fake ]; then
        # Self-contained: the harness spawns its own private dbus-daemon and in-process simulator.
        e2e_args+=(--fake)
    else
        local extra=(--adb "$ADB_BIN" --serial "$SERIAL" --adb-port "$ADB_PORT")
        if [ "$SPAWN_SIM" -eq 1 ]; then
            # The harness owns the simulator (in-process, own private dbus-daemon); S7 can run.
            e2e_args+=("${extra[@]}" --spawn-sim)
            [ -z "$INTERVAL" ] || e2e_args+=(--interval-ms "$INTERVAL")
        else
            local sim_args=(--bus "$bus" "${extra[@]}")
            [ -z "$INTERVAL" ] || sim_args+=(--interval-ms "$INTERVAL")
            e2e_args+=(--bus "$bus" "${extra[@]}")
            start_simulator "$bus" "${sim_args[@]}"
        fi
    fi
    [ -z "$ONLY" ] || e2e_args+=(--only "$ONLY")
    [ "$JSON" -eq 0 ] || e2e_args+=(--json)

    log "running geekbridge-call-e2e; log: $LOG_DIR/e2e.log"
    local rc=0
    "$E2E_BIN" "${e2e_args[@]}" 2>&1 | tee "$LOG_DIR/e2e.log" || rc=${PIPESTATUS[0]}
    if [ "$rc" -eq 0 ]; then log "PASS (signalling only; Bluetooth/HFP/SCO audio not exercised)"
    else log "FAIL (exit $rc); logs in $LOG_DIR"; fi
    exit "$rc"
}

if [ "$INNER" -eq 1 ]; then
    run_inner
fi

# ---- outer phase ------------------------------------------------------------------------------

command -v dbus-run-session >/dev/null 2>&1 || die "dbus-run-session not found (install the dbus package)"
if [ "$MODE" = fake ]; then
    command -v dbus-daemon >/dev/null 2>&1 || die "dbus-daemon not found (the fake harness starts a private one)"
fi

if [ "$MODE" = emulator ]; then
    resolve_adb
    if [ -x "$ROOT/scripts/emulator-call.sh" ]; then
        "$ROOT/scripts/emulator-call.sh" status >&2 \
            || die "emulator is not ready; run: scripts/emulator-call.sh start && scripts/emulator-call.sh wait"
    else
        # Fallback for trees without the helper: the status file it maintains.
        case "$(cat "$ROOT/.tools/gbcall/STATUS" 2>/dev/null || echo missing)" in
            ready*) ;;
            *) die "emulator is not ready (.tools/gbcall/STATUS is not 'ready'; scripts/emulator-call.sh is absent)" ;;
        esac
    fi
    # Independent of the status file: the device must answer on our adb server.
    state="$("$ADB_BIN" -P "$ADB_PORT" -s "$SERIAL" get-state 2>/dev/null || true)"
    [ "$state" = device ] || die "adb -P $ADB_PORT -s $SERIAL reports '${state:-no device}', expected 'device'"
    # The simulator always runs on a private bus created below, so a real provider on the user's
    # buses cannot collide with it; say so, but only refuse on the private bus (see run_inner).
    for kind in session system; do
        if [ "$kind" = session ] && [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then continue; fi
        if [ "$(name_owned_on "$kind")" = yes ]; then
            log "note: $WELL_KNOWN_NAME is owned on the real $kind bus (real PipeWire); it is not touched, the simulator uses a private bus"
        fi
    done
fi

[ "$BUILD" -eq 0 ] || build
if [ "$MODE" = emulator ] && [ "$SPAWN_SIM" -eq 0 ]; then
    [ -x "$SIM_BIN" ] || die "missing $SIM_BIN (build failed or --no-build without a build)"
fi
[ -x "$E2E_BIN" ] || die "missing $E2E_BIN (build failed or --no-build without a build)"

TEMP_LOGS=0
if [ -z "$LOG_DIR" ]; then
    if [ "$MODE" = emulator ]; then
        LOG_DIR="$ROOT/dist/android/logs/emulate-call-$(date +%Y%m%d-%H%M%S)"
        mkdir -p "$LOG_DIR"
    else
        LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/geekbridge-emulate-call.XXXXXX")"
        TEMP_LOGS=1
    fi
else
    mkdir -p "$LOG_DIR"
fi
LOG_DIR="$(cd "$LOG_DIR" && pwd -P)"

inner_args=("$MODE" --inner --log-dir "$LOG_DIR" --no-build)
[ "$PROFILE" = release ] && inner_args+=(--release)
[ -z "$ONLY" ] || inner_args+=(--only "$ONLY")
[ -z "$INTERVAL" ] || inner_args+=(--interval-ms "$INTERVAL")
[ "$SPAWN_SIM" -eq 0 ] || inner_args+=(--spawn-sim)
[ "$JSON" -eq 0 ] || inner_args+=(--json)

# Run in the background and `wait`, so a signal reaches our trap immediately and is forwarded to
# the one child we started (dbus-run-session), which tears down its bus and the inner script.
child=""
forward() { [ -z "$child" ] || kill -"$1" "$child" 2>/dev/null || true; }
trap 'forward TERM' INT TERM
GB_EMU_SERIAL="$SERIAL" GB_EMU_ADB_PORT="$ADB_PORT" ADB="$ADB_BIN" CARGO_TARGET_DIR="$TARGET_DIR" \
    dbus-run-session -- bash "$SELF" "${inner_args[@]}" &
child=$!
rc=0
wait "$child" || rc=$?
if kill -0 "$child" 2>/dev/null; then wait "$child" || rc=$?; fi   # interrupted by a signal: reap
trap - INT TERM

if [ "$rc" -eq 0 ] && [ "$TEMP_LOGS" -eq 1 ] && [ "$KEEP_LOGS" -eq 0 ]; then
    rm -rf -- "$LOG_DIR"
else
    log "logs kept in $LOG_DIR"
fi
exit "$rc"
