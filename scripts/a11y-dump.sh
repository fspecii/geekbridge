#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prove a desktop app exposes an AT-SPI accessibility tree and dump it.
#
#   scripts/a11y-dump.sh [--bin-dir DIR] APP...        APP is one of: geekbridge calls messages agenda contacts clipboard
#
# Starts a private session bus, the AT-SPI bus launcher (so nothing touches your real session) and
# Xvfb, enables accessibility, launches the app from DIR (default target/release) and prints its
# tree with scripts/atspi_dump.py. Needs: dbus-run-session, Xvfb, at-spi-bus-launcher, python3 with
# PyGObject (Atspi 2.0). Linux only. The binaries must be built with the default `accesskit` feature.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
if [[ "${1:-}" == --bin-dir ]]; then BIN_DIR="$2"; shift 2; fi
(($#)) || { sed -n 2,5p "${BASH_SOURCE[0]}"; exit 2; }

if [[ -z "${A11Y_INNER:-}" ]]; then
    export A11Y_INNER=1 A11Y_ROOT="$ROOT" A11Y_BIN="$BIN_DIR"
    exec dbus-run-session -- "${BASH_SOURCE[0]}" "$@"
fi

source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp a11y -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done; rm -rf "$TMP"; }
trap cleanup EXIT

LAUNCHER=/usr/lib/at-spi-bus-launcher
[[ -x $LAUNCHER ]] || LAUNCHER=/usr/libexec/at-spi-bus-launcher
"$LAUNCHER" --launch-immediately >"$TMP/atspi.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 50); do busctl --user list 2>/dev/null | grep -q org.a11y.Bus && break; sleep 0.1; done
busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true
# The registry is normally started by systemd / session activation, which a private bus lacks.
REGISTRYD=/usr/lib/at-spi2-registryd
[[ -x $REGISTRYD ]] || REGISTRYD=/usr/libexec/at-spi2-registryd
"$REGISTRYD" --use-gnome-session >"$TMP/registryd.log" 2>&1 &
PIDS+=($!)
sleep 1

DISP=":$((90 + RANDOM % 100))"
Xvfb "$DISP" -screen 0 1280x900x24 >"$TMP/xvfb.log" 2>&1 &
PIDS+=($!)
sleep 1
export DISPLAY="$DISP" HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" XDG_DATA_HOME="$TMP/home/.local/share"
mkdir -p "$HOME"
export GEEKBRIDGE_NO_UPDATE_CHECK=1 GDK_BACKEND=x11

for app in "$@"; do
    bin="$A11Y_BIN/geekbridge"; title="geekbridge"
    [[ $app == geekbridge ]] || { bin="$A11Y_BIN/geekbridge-$app"; title="geekbridge $app"; }
    [[ -x $bin ]] || { echo "missing $bin" >&2; exit 2; }
    "$bin" --api-port 38997 >"$TMP/$app.log" 2>&1 &
    pid=$!
    echo "== $app"
    python3 -I "$A11Y_ROOT/scripts/atspi_dump.py" "geekbridge" 20 || { cat "$TMP/$app.log" >&2; }
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    sleep 0.5
done
