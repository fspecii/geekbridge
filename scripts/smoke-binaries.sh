#!/usr/bin/env bash
# Smoke-test the built desktop binaries on the host OS (Linux, macOS, or Windows under Git Bash).
#
#   scripts/smoke-binaries.sh [--bin-dir DIR] [--live]
#
#   --bin-dir DIR  where the executables are (default: target/debug)
#   --live         also start geekbridge-service on private loopback ports with a fresh state
#                  directory, query it with geekbridge-ctl, and stop it
#
# Everything runs against a throw-away HOME / state directory, so nothing of the user's is read or
# written. Meant for CI runners, where it is the first time the Windows and macOS binaries execute.
set -Eeuo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN="target/debug"
LIVE=0
while (($#)); do
    case "$1" in
        --bin-dir) BIN="$2"; shift ;;
        --live) LIVE=1 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

EXT=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;; esac
bin() { echo "$BIN/$1$EXT"; }

fresh="$(mktemp -d)"
native() { if [ -n "$EXT" ]; then cygpath -w "$1"; else echo "$1"; fi; }
home="$(native "$fresh/home")"
state="$(native "$fresh/state")"
mkdir -p "$fresh/home"
# Win32 programs read USERPROFILE and LOCALAPPDATA, not HOME.
export HOME="$home" USERPROFILE="$home" LOCALAPPDATA="$home" XDG_DATA_HOME="$home/.local/share" XDG_CONFIG_HOME="$home/.config"
export GEEKBRIDGE_STATE_DIR="$state" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_CLIPBOARD=off
cleanup() { [ -n "${svc:-}" ] && kill "$svc" 2>/dev/null || true; rm -rf "$fresh"; }
trap cleanup EXIT

ok() { echo "ok   $*"; }
fail() { echo "FAIL $*" >&2; exit 1; }

out="$("$(bin geekbridge-ctl)" --version)" && [[ "$out" == geekbridge-ctl\ * ]] || fail "geekbridge-ctl --version: $out"
ok "$out"
"$(bin geekbridge-service)" --help | grep -q -- --state-dir || fail "geekbridge-service --help"
ok "geekbridge-service --help"
"$(bin geekbridge-messages)" --help | grep -qi usage || fail "geekbridge-messages --help"
ok "geekbridge-messages --help"

# No service and no token on a fresh machine: a clean explained error, never a panic.
set +e
out="$("$(bin geekbridge-ctl)" diagnose 2>&1)"; rc=$?
set -e
[[ "$out" != *panicked* ]] || fail "diagnose panicked on a fresh HOME: $out"
[ "$rc" -ne 0 ] && [[ "$out" == *"no control token"* ]] || fail "diagnose on a fresh HOME: rc=$rc: $out"
ok "geekbridge-ctl diagnose on a fresh HOME fails cleanly: $out"

"$(bin geekbridge-ctl)" token-path | grep -q ipc-token || fail "token-path"
"$(bin geekbridge-ctl)" autostart status | grep -q "autostart: off" || fail "autostart status"
"$(bin geekbridge-ctl)" firewall | grep -q 1716 || fail "firewall help"
ok "token-path, autostart status, firewall help"

if [ "$LIVE" = 1 ]; then
    port=$((20000 + RANDOM % 20000))
    "$(bin geekbridge-service)" --state-dir "$state" --api-port "$port" --control-port $((port + 1)) \
        --discovery-port $((port + 2)) --listen-inbound "127.0.0.1:$((port + 3))" --no-announce --no-calling-probe \
        >"$fresh/service.log" 2>&1 &
    svc=$!
    for _ in $(seq 1 50); do [ -s "$fresh/state/ipc-token" ] && break; sleep 0.2; done
    [ -s "$fresh/state/ipc-token" ] || { cat "$fresh/service.log" >&2; fail "service did not create its control token"; }
    ok "service started and created its token"
    ctl() { "$(bin geekbridge-ctl)" --state-dir "$state" --control-port $((port + 1)) "$@"; }
    ctl status >"$fresh/status.txt" 2>&1 || { cat "$fresh/status.txt" "$fresh/service.log" >&2; fail "ctl status against the live service"; }
    ok "ctl status against the live service"
    ctl diagnose | grep -qi version || fail "ctl diagnose against the live service"
    ok "ctl diagnose against the live service"
    # On Windows the entry is a registry value that no fake HOME redirects: only touch it on a CI runner.
    if [ -z "$EXT" ] || [ -n "${CI:-}" ]; then
        ctl autostart enable >/dev/null && ctl autostart status | grep -q "autostart: on" || fail "autostart enable"
        ctl autostart disable >/dev/null && ctl autostart status | grep -q "autostart: off" || fail "autostart disable"
        ok "autostart enable/disable round trip"
    fi
fi
echo "smoke OK"
