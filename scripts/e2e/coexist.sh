#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# GeekBridge next to the REAL stock KDE Connect daemon on one machine (docs/coexistence.md).
#
#   scripts/e2e/coexist.sh /path/to/extracted/kdeconnect/root OUTDIR
#
# KC_ROOT is the same extracted pacman tree scripts/interop-kdeconnect.sh uses. Everything runs in a
# throw-away user+network namespace (one dummy private address, no LAN reach), on a private D-Bus and
# a private HOME. The real ~/.config and the real KDE Connect are never touched. Results: OUTDIR/results.txt
set -u
KC_ROOT=${1:?usage: $0 KC_ROOT OUTDIR}
OUT=$(realpath -m "${2:?usage: $0 KC_ROOT OUTDIR}")
REPO=$(cd "$(dirname "$0")/../.." && pwd)
if [ -z "${GB_INNER:-}" ]; then
    mkdir -p "$OUT"
    export GB_INNER=1 KC_ROOT OUT REPO
    exec unshare -Urn "$0" "$@"
fi
if [ -z "${GB_INNER2:-}" ]; then
    ip link set lo up
    ip link add dummy0 type dummy && ip addr add 10.77.0.2/24 dev dummy0 && ip link set dummy0 up
    export HOME=$OUT/home XDG_CONFIG_HOME=$OUT/home/.config XDG_DATA_HOME=$OUT/home/.local/share \
        XDG_CACHE_HOME=$OUT/home/.cache XDG_RUNTIME_DIR=$OUT/home/rt
    rm -rf "$HOME"; mkdir -p "$HOME"/{.config,.local/share,.cache,rt,Downloads}; chmod 700 "$HOME/rt"
    export LD_LIBRARY_PATH=$KC_ROOT/usr/lib QT_PLUGIN_PATH=$KC_ROOT/usr/lib/qt6/plugins:/usr/lib/qt6/plugins
    export XDG_DATA_DIRS=$KC_ROOT/usr/share:/usr/share XDG_CONFIG_DIRS=/etc/xdg
    export QT_QPA_PLATFORM=offscreen GTK_USE_PORTAL=0 QT_NO_XDG_DESKTOP_PORTAL=1 GIO_USE_PORTALS=0
    export QT_LOGGING_RULES='kdeconnect.*=true' QT_FORCE_STDERR_LOGGING=1
    export GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_NO_TRAY=1
    unset QT_QPA_PLATFORMTHEME GTK_MODULES XDG_CURRENT_DESKTOP WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
    mkdir -p "$OUT/bus"
    cat > "$OUT/bus/session.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=$OUT/bus</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
    GB_INNER2=1 exec dbus-run-session --config-file="$OUT/bus/session.conf" -- "$0" "$@"
fi

SVC=$REPO/target/debug/geekbridge-service; CTL=$REPO/target/debug/geekbridge-ctl
CLI=$KC_ROOT/usr/bin/kdeconnect-cli
NET="python3 -I $REPO/scripts/e2e/coexist_net.py"
R=$OUT/results.txt; : > "$R"
say() { echo "$*" | tee -a "$R"; }
check() { local name=$1; shift; if "$@" >/dev/null 2>&1; then say "PASS  $name"; else say "FAIL  $name"; fi; }
waitfor() { local i; for ((i = 0; i < ${3:-20} * 10; i++)); do grep -q -- "$2" "$1" 2>/dev/null && return 0; sleep 0.1; done; return 1; }
busp() { busctl --user "$@" 2>&1; }
PIDS=()
export CTL CLI
bg() { "$@" & PIDS+=($!); }
# `ss` shows the 15-character kernel process name, hence geekbridge-serv.
cleanup() {
    kill "${PIDS[@]}" 2>/dev/null
    local i; for i in $(seq 30); do kill -0 "${PIDS[@]}" 2>/dev/null || break; sleep 0.1; done
    kill -9 "${PIDS[@]}" 2>/dev/null; wait "${PIDS[@]}" 2>/dev/null; PIDS=()
    for i in $(seq 30); do [ -z "$(ss -H -lntu 'sport = :1716 or sport = :1717')" ] && break; sleep 0.1; done
}
trap cleanup EXIT

start_daemon() { # start_daemon LOG
    "$KC_ROOT/usr/bin/kdeconnectd" > "$1" 2>&1 & PIDS+=($!)
    for i in $(seq 100); do busp status org.kde.kdeconnect >/dev/null && break; sleep 0.1; done
    export SELF; SELF=$(busp call org.kde.kdeconnect /modules/kdeconnect org.kde.kdeconnect.daemon selfId | sed 's/^s "//;s/"$//')
}
start_gb() { # start_gb TAG [extra service args]; fresh state dir per phase
    local tag=$1; shift
    SD=$OUT/state-$tag; rm -rf "$SD"; mkdir -p "$SD"; chmod 700 "$SD"
    GBLOG=$OUT/gb-$tag.log; export SD GBLOG
    "$SVC" --state-dir "$SD" --name GeekBridgeCoexist --no-calling-probe --no-tray \
        --api-port 17380 --control-port 17381 "$@" > "$GBLOG" 2>&1 & PIDS+=($!)
    for i in $(seq 100); do ctl status >/dev/null 2>&1 && break; sleep 0.1; done
    export GBID; GBID=$(ctl status 2>/dev/null | grep -o '"device_id": *"[0-9a-f]*"' | head -1 | grep -o '[0-9a-f]\{32\}')
}
ctl() { "$CTL" --state-dir "$SD" --control-port 17381 "$@"; }
api() { exec 5<>/dev/tcp/127.0.0.1/17380; printf 'GET /v1/discovery HTTP/1.0\r\n\r\n' >&5; cat <&5; exec 5>&-; }
listeners() { ss -H -lntup 2>/dev/null; }
export -f ctl api
udp_owners() { listeners | awk '$1=="udp" && $5 ~ /:1716$/' ; }

# ------------------------------------------------------------------ P1: stock daemon first, then GeekBridge
say "== P1: stock kdeconnectd first, then geekbridge-service (default ports)"
bg $NET sniff "$OUT/sniff-p1.txt"
start_daemon "$OUT/kdeconnectd-p1.log"
say "daemon selfId=$SELF"
check "P1 daemon owns TCP 1716" bash -c "ss -H -lntp | grep ':1716 ' | grep -q kdeconnectd"
start_gb p1
say "geekbridge deviceId=$GBID"
check "P1 geekbridge started anyway" ctl status
check "P1 geekbridge TCP listener fell back to 1717" bash -c "ss -H -lntp | grep ':1717 ' | grep -q geekbridge-serv"
check "P1 geekbridge shares UDP 1716 with the daemon (both bound)" bash -c "ss -H -lunp | grep ':1716 ' | grep -q kdeconnectd && ss -H -lunp | grep ':1716 ' | grep -q geekbridge-serv"
check "P1 one clear log line about the port" grep -q "Using port 1717 because 1716 is in use by another program: this is fine" "$GBLOG"
check "P1 geekbridge-ctl status says so (stderr)" bash -c "'$CTL' --state-dir '$SD' --control-port 17381 status 2>&1 >/dev/null | grep -q 'Using port 1717 because 1716 is in use'"
check "P1 no UDP bind error reported" bash -c "! api | grep -q 'Cannot listen on UDP'"
waitfor "$OUT/sniff-p1.txt" "$GBID 1717" 15 && say "PASS  P1 announced tcpPort is the fallback port 1717 (seen on the wire)" || say "FAIL  P1 announcement tcpPort"
"$CLI" --refresh >/dev/null 2>&1
for i in $(seq 40); do api | grep -q "$SELF" && break; sleep 0.5; "$CLI" --refresh >/dev/null 2>&1; done
check "P1 geekbridge hears the daemon's broadcast although the daemon owns UDP 1716" bash -c "$(declare -f api); api | grep -q $SELF"
check "P1 sniffer (third listener) also got both identities" bash -c "grep -q $SELF '$OUT/sniff-p1.txt' && grep -q $GBID '$OUT/sniff-p1.txt'"
for i in $(seq 60); do "$CLI" -l --id-only 2>/dev/null | grep -q "$GBID" && break; sleep 0.5; done
check "P1 daemon sees geekbridge as a separate device (own id)" bash -c "'$CLI' -l --id-only | grep -q $GBID"
check "P1 device ids differ" test "$SELF" != "$GBID"
CERT_DAEMON=$(openssl x509 -in "$HOME/.config/kdeconnect/certificate.pem" -outform DER 2>/dev/null | sha256sum | cut -d' ' -f1)
CERT_GB=$(ctl status | grep -o '"own_cert_sha256": *"[^"]*"' | sed 's/.*: *"//;s/"$//' | tr -d ':' | tr 'A-F' 'a-f')
say "daemon cert sha256=$CERT_DAEMON"; say "geekbridge cert sha256=$CERT_GB"
check "P1 certificates differ (geekbridge's is its own)" test -n "$CERT_GB" -a "$CERT_GB" != "$CERT_DAEMON"
"$CLI" -d "$GBID" --encryption-info > "$OUT/encinfo-p1.txt" 2>&1
check "P1 daemon sees geekbridge's own certificate fingerprint" bash -c "tr -d ': ' < '$OUT/encinfo-p1.txt' | tr 'A-F' 'a-f' | grep -q '$CERT_GB'"
check "P1 geekbridge state dir is its own and has no KDE Connect files" bash -c "[ -d '$SD' ] && ! ls '$SD' | grep -qi kdeconnect"
check "P1 geekbridge made no outgoing TCP connection to the daemon" bash -c "! ss -H -ntp | grep geekbridge-serv | grep -q ':1716 '"
check "P1 nothing paired on either side before anyone asks" bash -c "! ctl status | grep -q '\"paired\": *\\[ *{'; ! '$CLI' --list-devices 2>&1 | grep -q 'paired and reachable'"
# the daemon asks to pair with us: it must wait for OUR user (a pending challenge), not be answered for.
busp call org.kde.kdeconnect "/modules/kdeconnect/devices/$GBID" org.kde.kdeconnect.device requestPairing >/dev/null
for i in $(seq 40); do ctl status | grep -q '"challenge_id"' && break; sleep 0.3; done
check "P1 pairing started by the daemon is only a pending challenge in geekbridge (not answered for)" bash -c "ctl status | grep -q challenge_id"
sleep 2
check "P1 ... and stays unpaired without a decision" bash -c "! '$CLI' -a --id-only | grep -q $GBID"
for i in $(seq 40); do ctl clipboard status 2>&1 | grep -q "KDE Connect is also running" && break; sleep 0.5; done
check "P1 clipboard status warns that KDE Connect is also running (D-Bus org.kde.kdeconnect)" bash -c "ctl clipboard status 2>&1 | grep -q 'KDE Connect is also running'"
ctl clipboard status > "$OUT/clipboard-status-p1.txt" 2>&1
check "P1 nothing was disabled automatically" bash -c "! grep -qi 'disabled' '$OUT/clipboard-status-p1.txt'"
cleanup

# ------------------------------------------------------------------ P2: GeekBridge first, then the stock daemon
say "== P2: geekbridge-service first, then stock kdeconnectd"
bg $NET sniff "$OUT/sniff-p2.txt"
start_gb p2
check "P2 geekbridge on TCP 1716, no fallback note" bash -c "ss -H -lntp | grep ':1716 ' | grep -q geekbridge-serv && ! grep -q 'Using port' '$GBLOG'"
check "P2 no clipboard warning while KDE Connect is not running" bash -c "! ctl clipboard status 2>&1 | grep -q 'KDE Connect is also running'"
start_daemon "$OUT/kdeconnectd-p2.log"
check "P2 daemon started (took UDP 1716 beside us)" bash -c "ss -H -lunp | grep ':1716 ' | grep -q kdeconnectd && ss -H -lunp | grep ':1716 ' | grep -q geekbridge-serv"
check "P2 daemon logged no UDP bind failure" bash -c "! grep -qi 'error.*bind\|failed to bind\|address already in use' '$OUT/kdeconnectd-p2.log' || grep -qi 'tcp.*1717\|1717' '$OUT/kdeconnectd-p2.log'"
say "daemon TCP listener: $(ss -H -lntp | grep kdeconnectd | awk '{print $4}' | tr '\n' ' ')"
for i in $(seq 60); do api | grep -q "$SELF" && break; sleep 0.5; "$CLI" --refresh >/dev/null 2>&1; done
check "P2 geekbridge hears the daemon (daemon's broadcast reaches both sockets)" bash -c "$(declare -f api); api | grep -q $SELF"
for i in $(seq 60); do "$CLI" -l --id-only 2>/dev/null | grep -q "$GBID" && break; sleep 0.5; done
check "P2 daemon sees geekbridge (announced tcpPort 1716)" bash -c "'$CLI' -l --id-only | grep -q $GBID"
waitfor "$OUT/sniff-p2.txt" "$GBID 1716" 10 && say "PASS  P2 announced tcpPort is 1716" || say "FAIL  P2 announcement tcpPort"
for i in $(seq 60); do ctl clipboard status 2>&1 | grep -q "KDE Connect is also running" && break; sleep 0.5; done
check "P2 clipboard warning appears once the daemon is on the bus" bash -c "ctl clipboard status 2>&1 | grep -q 'KDE Connect is also running'"
cleanup

# ------------------------------------------------------------------ P3: a rude program owns UDP 1716 (no reuse) and TCP 1716
say "== P3: UDP 1716 held without SO_REUSEADDR and TCP 1716 held by another program"
bg $NET udp-squat > "$OUT/udp-squat.log"; bg $NET tcp-squat 1716 1716 > "$OUT/tcp-squat.log"
waitfor "$OUT/udp-squat.log" squatting 5; waitfor "$OUT/tcp-squat.log" squatting 5
start_gb p3
check "P3 geekbridge starts anyway" ctl status
check "P3 listener fell back to 1717" bash -c "ss -H -lntp | grep ':1717 ' | grep -q geekbridge-serv"
check "P3 log says why" grep -q "Using port 1717 because 1716 is in use by another program: this is fine" "$GBLOG"
check "P3 discovery API reports the UDP problem and points to QR/IP pairing" bash -c "api | grep -q 'pair with the QR code'"
check "P3 QR pairing still works and carries port 1717" bash -c "ctl pair-qr 2>/dev/null | grep -q 1717"
cleanup

say "== P4: every TCP port 1716-1764 is taken"
bg $NET tcp-squat 1716 1764 > "$OUT/tcp-squat-all.log"; waitfor "$OUT/tcp-squat-all.log" squatting 5
start_gb p4
check "P4 geekbridge still starts (outgoing connections and UI keep working)" ctl status
check "P4 one clear log line" grep -q "not accepting phone connections" "$GBLOG"
cleanup
say "== done; logs in $OUT"
grep -c '^PASS' "$R" | sed 's/^/passed: /' | tee -a "$R"; grep -c '^FAIL' "$R" | sed 's/^/failed: /' | tee -a "$R"
