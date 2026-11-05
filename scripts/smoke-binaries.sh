#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
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

source "scripts/lib-tmp.sh"
fresh="$(gb_mktemp smoke -d)"
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
captured="$("$(bin geekbridge-service)" --help 2>&1)" && grep -q -- --state-dir <<<"$captured" || fail "geekbridge-service --help"
ok "geekbridge-service --help"
captured="$("$(bin geekbridge-messages)" --help 2>&1)" && grep -qi usage <<<"$captured" || fail "geekbridge-messages --help"
ok "geekbridge-messages --help"

# No service and no token on a fresh machine: a clean explained error, never a panic.
set +e
out="$("$(bin geekbridge-ctl)" diagnose 2>&1)"; rc=$?
set -e
[[ "$out" != *panicked* ]] || fail "diagnose panicked on a fresh HOME: $out"
[ "$rc" -ne 0 ] && [[ "$out" == *"no control token"* ]] || fail "diagnose on a fresh HOME: rc=$rc: $out"
ok "geekbridge-ctl diagnose on a fresh HOME fails cleanly: $out"

captured="$("$(bin geekbridge-ctl)" token-path 2>&1)" && grep -q ipc-token <<<"$captured" || fail "token-path"
captured="$("$(bin geekbridge-ctl)" autostart status 2>&1)" && grep -q "autostart: off" <<<"$captured" || fail "autostart status"
captured="$("$(bin geekbridge-ctl)" firewall 2>&1)" && grep -q 1716 <<<"$captured" || fail "firewall help"
ok "token-path, autostart status, firewall help"

if [ "$LIVE" = 1 ]; then
    # Four consecutive ports below the kernel's ephemeral range (32768+), where another process may hold one;
    # a port that is taken (the service exits with "Address already in use") gets another try.
    ctl() { "$(bin geekbridge-ctl)" --state-dir "$state" --control-port $((port + 1)) "$@"; }
    up=0
    for _attempt in 1 2 3 4 5; do
        port=$((20000 + RANDOM % 12000))
        rm -rf "$fresh/state"
        "$(bin geekbridge-service)" --state-dir "$state" --api-port "$port" --control-port $((port + 1)) \
            --discovery-port $((port + 2)) --listen-inbound "127.0.0.1:$((port + 3))" --no-announce --no-calling-probe \
            >"$fresh/service.log" 2>&1 &
        svc=$!
        for _ in $(seq 1 50); do
            kill -0 "$svc" 2>/dev/null || break
            [ -s "$fresh/state/ipc-token" ] && ctl status >/dev/null 2>&1 && break
            sleep 0.2
        done
        if kill -0 "$svc" 2>/dev/null && [ -s "$fresh/state/ipc-token" ]; then up=1; break; fi
        wait "$svc" 2>/dev/null || true
    done
    [ "$up" = 1 ] || { cat "$fresh/service.log" >&2; fail "service did not create its control token"; }
    ok "service started and created its token"
    ctl status >"$fresh/status.txt" 2>&1 || { cat "$fresh/status.txt" "$fresh/service.log" >&2; fail "ctl status against the live service"; }
    ok "ctl status against the live service"
    captured="$(ctl diagnose 2>&1)" && grep -qi version <<<"$captured" || fail "ctl diagnose against the live service"
    ok "ctl diagnose against the live service"
    # On Windows the entry is a registry value that no fake HOME redirects: only touch it on a CI runner.
    if [ -z "$EXT" ] || [ -n "${CI:-}" ]; then
        ctl autostart enable >/dev/null && captured="$(ctl autostart status 2>&1)" && grep -q "autostart: on" <<<"$captured" || fail "autostart enable"
        ctl autostart disable >/dev/null && captured="$(ctl autostart status 2>&1)" && grep -q "autostart: off" <<<"$captured" || fail "autostart disable"
        ok "autostart enable/disable round trip"
    fi
fi
echo "smoke OK"
