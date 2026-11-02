#!/usr/bin/env bash
# Per-app, per-state memory of the desktop GUI apps from /proc/PID/smaps_rollup.
#
#   scripts/ui-mem.sh [--bin-dir DIR] [--apps "home calls ..."] [--wayland]
#
# Starts a private service (temp state, private ports) and an Xvfb (or, with --wayland and sway
# installed, a headless sway) and prints one row per app and state: Rss, Pss, Anonymous, and the
# file-backed split (Rss - Anonymous), all in kB. States: focused (window mapped, focus given),
# unfocused (another window has focus), hidden (window unmapped / not shown). Linux only.
set -Eeuo pipefail
IFS=$'\n\t'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
APPS="home calls messages calendar contacts share clipboard"
WAYLAND=0
while (($#)); do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift ;;
        --apps) APPS="$2"; shift ;;
        --wayland) WAYLAND=1 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp uimem -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

if ((WAYLAND)); then
    command -v sway >/dev/null || { echo "sway not installed" >&2; exit 2; }
    export XDG_RUNTIME_DIR="$TMP/run"; mkdir -p -m 700 "$XDG_RUNTIME_DIR"
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c /dev/null >"$TMP/sway.log" 2>&1 &
    PIDS+=("$!")
    for _ in $(seq 100); do ls "$XDG_RUNTIME_DIR"/wayland-* >/dev/null 2>&1 && break; sleep 0.1; done
    export WAYLAND_DISPLAY="$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-[0-9] | head -n1)")"
    unset DISPLAY
    export WINIT_UNIX_BACKEND=wayland
else
    D=$((90 + RANDOM % 100))
    Xvfb ":$D" -screen 0 1600x1000x24 >"$TMP/xvfb.log" 2>&1 &
    PIDS+=("$!")
    sleep 1
    export DISPLAY=":$D"
    unset WAYLAND_DISPLAY
fi

export GEEKBRIDGE_NO_UPDATE_CHECK=1
API=$((20000 + RANDOM % 20000))
"$BIN_DIR/geekbridge-service" --state-dir "$TMP/state" --api-port "$API" --control-port $((API + 1)) \
    --discovery-port $((API + 2)) --no-calling-probe >"$TMP/service.log" 2>&1 &
PIDS+=("$!")
until grep -q 'discovery API' "$TMP/service.log" 2>/dev/null; do sleep 0.05; done

roll() { awk -v k="$2:" '$1 == k {print $2}' "/proc/$1/smaps_rollup"; }
row() { # app state pid
    local rss pss anon
    rss=$(roll "$3" Rss); pss=$(roll "$3" Pss); anon=$(roll "$3" Anonymous)
    printf '%-10s %-10s rss=%7s pss=%7s anon=%7s file=%7s kB  threads=%s\n' "$1" "$2" "$rss" "$pss" "$anon" \
        "$((rss - anon))" "$(awk '$1=="Threads:"{print $2}' /proc/"$3"/status)"
}
bin_for() { case "$1" in home) echo geekbridge ;; agenda|calendar) echo geekbridge-agenda ;; *) echo "geekbridge-$1" ;; esac; }

IFS=" " read -ra APP_LIST <<<"$APPS"
for app in "${APP_LIST[@]}"; do
    b="$BIN_DIR/$(bin_for "$app")"
    [[ -x "$b" ]] || { echo "$app: no binary" >&2; continue; }
    (cd "$TMP" && exec "$b" --api-port "$API" >"$TMP/$app.log" 2>&1) &
    P=$!; PIDS+=("$P")
    sleep 6
    kill -0 "$P" 2>/dev/null || { echo "$app: exited"; tail -3 "$TMP/$app.log"; continue; }
    row "$app" settled "$P"
    if ((!WAYLAND)); then
        win=$(xdotool search --pid "$P" 2>/dev/null | tail -n1 || true)
        if [[ -n "$win" ]]; then
            xdotool windowfocus "$win" 2>/dev/null || true; sleep 3; row "$app" focused "$P"
            xdotool windowunmap "$win" 2>/dev/null || true; sleep 5; row "$app" hidden "$P"
            xdotool windowmap "$win" 2>/dev/null || true; sleep 3; row "$app" remapped "$P"
        fi
    fi
    kill "$P" 2>/dev/null || true; wait "$P" 2>/dev/null || true
done
