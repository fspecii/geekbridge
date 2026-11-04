#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Home-network hostility checks of the desktop service, in a throw-away network namespace
# (`unshare -rn`: no root, no sudo, never the live service or its ports).
#
#   scripts/netns-hostility.sh nics    which NICs the UDP announcement leaves on (limited + directed)
#   scripts/netns-hostility.sh isolate AP client isolation: broadcast arrives, TCP is dropped
#   scripts/netns-hostility.sh flood   100 phones, spoofed ids, garbage and oversized datagrams
#   scripts/netns-hostility.sh ipv6    IPv6-only PC: QR address, ULA and link-local dials
#
# Needs a built service: cargo build -p bridge-service --bins (CARGO_TARGET_DIR is honoured).
# Scratch lives under ${GB_NETNS_SCRATCH:-$HOME/.cache/gb-netns}. Report: docs/reports/network-hostility.md
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=${CARGO_TARGET_DIR:-$HERE/../target}/debug
SCRATCH=${GB_NETNS_SCRATCH:-$HOME/.cache/gb-netns}
PY=$HERE/netns
if [ "${1:-}" != "--inside" ]; then
  [ -x "$BIN/geekbridge-service" ] || { echo "build first: cargo build -p bridge-service --bins" >&2; exit 2; }
  unshare -rn true 2>/dev/null || { echo "unprivileged network namespaces are not available here" >&2; exit 2; }
  mkdir -p "$SCRATCH"
  exec unshare -rn env GB_BIN="$BIN" GB_SCRATCH="$SCRATCH" GB_PY="$PY" bash "${BASH_SOURCE[0]}" --inside "${1:-nics}"
fi
scenario=$2
BIN=$GB_BIN S=$GB_SCRATCH PY=$GB_PY
ip link set lo up
start_service() {
  rm -rf "$S/state"
  "$BIN/geekbridge-service" --state-dir "$S/state" --name rig --no-tray --no-calling-probe >"$S/service.log" 2>&1 &
  SVC=$!
  sleep 3
}
case $scenario in
nics)
  mk() { ip link add "$1" type dummy; ip addr add "$2" dev "$1"; ip link set "$1" up; }
  mk eth0 192.168.1.10/24; mk wlan0 10.20.0.10/16; mk docker0 172.17.0.1/16
  mk tailscale0 100.101.0.1/32; mk wg0 10.66.0.2/24
  ip route add default via 192.168.1.1 dev eth0
  python3 -I "$PY/sniff.py" 20 >"$S/sniff.txt" &
  sleep 1; start_service
  ss -lnt | grep 1716
  wait %1; kill "$SVC"; cat "$S/sniff.txt"
  ;;
isolate|flood|ipv6)
  if [ "$scenario" = ipv6 ]; then
    unshare -n sleep 60 & PH=$!; sleep 0.5
    ip link add eth0 type veth peer name ph0; ip link set ph0 netns $PH
    ip link set eth0 up; ip -6 addr add fd00:1:2:3::10/64 dev eth0 nodad
    nsenter -t $PH -n sh -c "ip link set lo up; ip link set ph0 up; ip -6 addr add fd00:1:2:3::50/64 dev ph0 nodad"
    start_service; export GEEKBRIDGE_STATE_DIR=$S/state
    ss -lnt | grep 1716
    "$BIN/geekbridge-ctl" pair-qr 2>&1 | tail -1
    LL=$(ip -6 -o addr show dev eth0 scope link | sed 's/.*inet6 \([^/]*\)\/.*/\1/')
    nsenter -t $PH -n python3 -I -c "
import socket
for host in ('fd00:1:2:3::10', '$LL%ph0'):
    try: socket.create_connection((host, 1716), timeout=3); print('connected to', host)
    except OSError as e: print('FAILED', host, e)"
    kill "$SVC" $PH
  elif [ "$scenario" = isolate ]; then
    unshare -n sleep 60 & PH=$!; sleep 0.5
    ip link add eth0 type veth peer name ph0; ip link set ph0 netns $PH
    ip addr add 192.168.1.10/24 dev eth0; ip link set eth0 up
    nsenter -t $PH -n sh -c "ip link set lo up; ip addr add 192.168.1.50/24 dev ph0; ip link set ph0 up; ip route add blackhole 192.168.1.10/32"
    start_service; export GEEKBRIDGE_STATE_DIR=$S/state
    nsenter -t $PH -n python3 -I "$PY/phone_announce.py" 192.168.1.50 255.255.255.255 3
    sleep 1
    time "$BIN/geekbridge-ctl" test-connection
    kill "$SVC" $PH
  else
    ip link add eth0 type dummy; ip link set eth0 up; ip addr add 192.168.1.10/24 dev eth0
    for i in $(seq 100 199); do ip addr add 192.168.1.$i/24 dev eth0; done
    start_service
    rss() { grep VmRSS /proc/$SVC/status | tr -s ' ' | cut -d' ' -f2; }
    echo "rss before: $(rss) kB"
    for k in 1 2 3 4 5 6 7; do python3 -I "$PY/flood.py"; done
    sleep 1; echo "rss after: $(rss) kB"
    printf 'GET /v1/discovery HTTP/1.0\r\n\r\n' | nc -q1 127.0.0.1 17380 | tail -1 |
      python3 -I -c 'import sys,json; d=json.load(sys.stdin); print("devices listed:", len(d["devices"]))'
    echo "log lines: $(wc -l <"$S/service.log")"
    kill "$SVC"
  fi
  ;;
*) echo "unknown scenario $scenario" >&2; exit 2 ;;
esac
