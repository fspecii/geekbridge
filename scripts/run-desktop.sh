#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Start everything the desktop side needs for a real-phone session, in one command.
#
#   scripts/run-desktop.sh [--fresh] [--name NAME] [--no-ui] [--qr] [--default-state]
#   scripts/run-desktop.sh --status | --stop | --env
#
# Starts geekbridge-service (detached, private state dir, log in dist/realdevice/service.log) and then the
# GeekBridge home window (geekbridge). The helper windows (calls popup, share, clipboard) are launched by
# the service by name, so target/release is put on PATH for both processes.
#
#   --fresh          wipe the session state dir first (forget every pairing)
#   --name NAME      name phones see (default: GeekBridge-<hostname>)
#   --no-ui          service only, no home window
#   --qr             also print a one-time pairing QR code in this terminal (valid 2 minutes, single use)
#   --default-state  use the normal per-user state dir instead of the private one. Needed only if you want the
#                    Hyprland hotkey (which runs a bare `geekbridge-ctl`) to find this service.
#   --status         show whether the service answers, then exit
#   --stop           stop the service and the home window started by this script
#   --env            print `export` lines so a shell can run geekbridge-ctl against this session
#
# Environment: GEEKBRIDGE_REALDEVICE_STATE (state dir), GEEKBRIDGE_BIN_DIR (default target/release),
# GEEKBRIDGE_CONTROL_PORT (default 17381), GEEKBRIDGE_API_PORT (default 17380).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN="${GEEKBRIDGE_BIN_DIR:-$ROOT/target/release}"
OUT="$ROOT/dist/realdevice"
STATE="${GEEKBRIDGE_REALDEVICE_STATE:-$ROOT/target/realdevice-state}"
CTRL_PORT="${GEEKBRIDGE_CONTROL_PORT:-17381}"
API_PORT="${GEEKBRIDGE_API_PORT:-17380}"
NAME="GeekBridge-$(hostname -s 2>/dev/null || echo pc)"
FRESH=0 UI=1 QR=0 MODE=start DEFAULT_STATE=0

c() { if [[ -t 1 ]]; then printf '\033[%sm%s\033[0m' "$1" "$2"; else printf '%s' "$2"; fi; }
say() { printf '%s %s\n' "$(c 36 '==>')" "$*"; }
ok() { printf '%s %s\n' "$(c 32 ' ok')" "$*"; }
warn() { printf '%s %s\n' "$(c 33 'warn')" "$*" >&2; }
die() { printf '%s %s\n' "$(c 31 'error')" "$*" >&2; exit 1; }

while (($#)); do
  case "$1" in
    --fresh) FRESH=1 ;;
    --name) shift; NAME="${1:?--name needs a value}" ;;
    --no-ui) UI=0 ;;
    --qr) QR=1 ;;
    --default-state) DEFAULT_STATE=1 ;;
    --status) MODE=status ;;
    --stop) MODE=stop ;;
    --env) MODE=env ;;
    -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
  shift
done

if ((DEFAULT_STATE)); then
  STATE="${GEEKBRIDGE_STATE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/geekbridge/service}"
fi
PIDS="$OUT/pids"
SVC="$BIN/geekbridge-service" CTL="$BIN/geekbridge-ctl" UIBIN="$BIN/geekbridge"
ctl() { timeout 15 "$CTL" --state-dir "$STATE" --control-port "$CTRL_PORT" "$@"; }

if [[ $MODE == env ]]; then
  printf 'export PATH=%q:$PATH\nexport GEEKBRIDGE_STATE_DIR=%q\nexport GEEKBRIDGE_CONTROL_PORT=%q\n' "$BIN" "$STATE" "$CTRL_PORT"
  exit 0
fi

stop_pidfile() { # file label
  local pid
  [[ -f $1 ]] || return 0
  pid="$(cat "$1")"
  if [[ -n $pid ]] && kill -0 "$pid" 2>/dev/null && grep -q geekbridge "/proc/$pid/cmdline" 2>/dev/null; then
    kill "$pid" 2>/dev/null && ok "stopped $2 (pid $pid)"
  fi
  rm -f "$1"
}

if [[ $MODE == stop ]]; then
  stop_pidfile "$PIDS.ui" "home window"
  stop_pidfile "$PIDS.service" "service"
  exit 0
fi
if [[ $MODE == status ]]; then
  [[ -x $CTL ]] || die "no $CTL (build first, see --help)"
  if ctl status >/dev/null 2>&1; then ok "service answers on 127.0.0.1:$CTRL_PORT (state dir $STATE)"; ctl status | head -40
  else warn "service not running (state dir $STATE)"; exit 1; fi
  exit 0
fi

# ---------------------------------------------------------------- preflight
for b in "$SVC" "$CTL" "$UIBIN"; do
  [[ -x $b ]] || die "missing $b
       build it:  cargo build --release --locked -p bridge-apps -p bridge-service"
done
mkdir -p "$OUT"
if ((FRESH)); then
  ((DEFAULT_STATE)) && die "--fresh with --default-state would wipe your real GeekBridge state; refusing"
  stop_pidfile "$PIDS.ui" "home window"; stop_pidfile "$PIDS.service" "service"
  sleep 1
  rm -rf -- "$STATE"; ok "wiped session state $STATE"
fi
mkdir -p "$STATE" && chmod 700 "$STATE"
export PATH="$BIN:$PATH"
export GEEKBRIDGE_STATE_DIR="$STATE" GEEKBRIDGE_CONTROL_PORT="$CTRL_PORT"

say "GeekBridge desktop for a real-phone session"
if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
  warn "no WAYLAND_DISPLAY/DISPLAY in this shell: windows and clipboard will not work (run from a terminal inside Hyprland)"
  UI=0
fi

# ---------------------------------------------------------------- service
if ctl status >/dev/null 2>&1; then
  ok "service already running (reusing it, state dir $STATE)"
else
  if ss -Htln 2>/dev/null | awk '{print $4}' | grep -q ":$CTRL_PORT\$"; then
    die "port $CTRL_PORT is taken by something that is not this session's service (another state dir?).
       Stop it, or set GEEKBRIDGE_CONTROL_PORT=<other>."
  fi
  say "starting geekbridge-service as \"$NAME\""
  (GEEKBRIDGE_NO_UPDATE_CHECK=1 setsid nohup "$SVC" --state-dir "$STATE" --api-port "$API_PORT" \
     --control-port "$CTRL_PORT" --name "$NAME" >>"$OUT/service.log" 2>&1 & echo $! >"$PIDS.service")
  # the very first start creates the identity key, which can take 10+ s on a busy machine
  for _ in $(seq 60); do ctl status >/dev/null 2>&1 && break; sleep 0.5; done
  ctl status >/dev/null 2>&1 || { tail -15 "$OUT/service.log" >&2; die "service did not come up; log: $OUT/service.log"; }
  ok "service up, log: dist/realdevice/service.log"
fi

# ---------------------------------------------------------------- home window
if ((UI)); then
  if [[ -f $PIDS.ui ]] && kill -0 "$(cat "$PIDS.ui")" 2>/dev/null; then
    ok "home window already open"
  else
    (setsid nohup "$UIBIN" --api-port "$API_PORT" >>"$OUT/home.log" 2>&1 & echo $! >"$PIDS.ui")
    sleep 1
    if kill -0 "$(cat "$PIDS.ui")" 2>/dev/null; then ok "home window started (log: dist/realdevice/home.log)"
    else warn "home window exited at once; see dist/realdevice/home.log"; tail -5 "$OUT/home.log" >&2; fi
  fi
fi

# ---------------------------------------------------------------- what the phone needs
port="$(ss -Htlnp 2>/dev/null | awk '/geekbridge-service/ {n=split($4,a,":"); print a[n]; exit}')"
ips="$(ip -4 -o addr show scope global 2>/dev/null | awk '{sub(/\/.*/,"",$4); printf "%s ", $4}')"
echo
echo "  name shown to phones : $NAME"
echo "  LAN address(es)      : ${ips:-none found}"
echo "  phone connects to    : TCP ${port:-1716} on this PC (discovery: UDP 1716 broadcast)"
echo "  state dir            : $STATE"
echo "  ctl in this shell    : eval \"\$(scripts/run-desktop.sh --env)\"   then  geekbridge-ctl status"
if command -v ufw >/dev/null 2>&1 && [[ "$(systemctl is-active ufw 2>/dev/null)" == active ]] \
   && ! grep -qE -- '--dport (1716|1716:1764) ' /etc/ufw/user.rules 2>/dev/null; then
  echo
  warn "ufw is active and has no rule for 1716-1764: the phone will not reach this PC."
  echo "       run:  scripts/realdevice.sh check      (prints the exact commands; add --apply to run them)"
fi
if ((QR)); then
  echo
  payload="$(ctl pair-qr 2>/dev/null)"
  if [[ $payload == geekbridge://pair?* ]]; then
    command -v qrencode >/dev/null && qrencode -t ANSIUTF8 -m 1 "$payload"
    echo "  one-time link (2 min, single use): $payload"
  else warn "could not create a pairing link: $payload"; fi
fi
echo
echo "  Pair: phone app > Scan QR code, scanning the QR in the GeekBridge window$( ((QR)) && echo ' or the one above')."
((QR)) || echo "        No window? rerun with --qr for a terminal QR."
echo "  Stop everything: scripts/run-desktop.sh --stop"
