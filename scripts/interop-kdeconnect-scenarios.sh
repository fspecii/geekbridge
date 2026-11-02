#!/bin/bash
# Inner half of interop-kdeconnect.sh: runs inside the private namespace + D-Bus session.
set -u
CLI=$KC_ROOT/usr/bin/kdeconnect-cli
PROBE=$REPO/target/debug/examples/real_peer_probe
W=$OUT/work; rm -rf "$W"; mkdir -p "$W"
R=$OUT/results.txt; : > "$R"; : > "$OUT/cli.log"
CAP=$OUT/captures; rm -rf "$CAP"; mkdir -p "$CAP"
say() { echo "$*" | tee -a "$R"; }
check() { # check NAME CONDITION-COMMAND...
    local name=$1; shift
    if "$@" >/dev/null 2>&1; then say "PASS  $name"; else say "FAIL  $name"; fi
}
waitfor() { # waitfor FILE PATTERN [seconds]
    local i; for ((i = 0; i < ${3:-20} * 10; i++)); do grep -q -- "$2" "$1" 2>/dev/null && return 0; sleep 0.1; done; return 1
}
busp() { busctl --user "$@" 2>&1; }

dbus-monitor --session > "$OUT/dbus-monitor.log" 2>&1 &
"$KC_ROOT/usr/bin/kdeconnectd" > "$OUT/kdeconnectd.log" 2>&1 &
DPID=$!
for i in $(seq 100); do busp status org.kde.kdeconnect >/dev/null && break; sleep 0.1; done
SELF=$(busp call org.kde.kdeconnect /modules/kdeconnect org.kde.kdeconnect.daemon selfId | sed 's/^s "//;s/"$//')
say "daemon: kdeconnectd $($KC_ROOT/usr/bin/kdeconnectd --version 2>/dev/null | tail -1) selfId=$SELF"
check "daemon listens on TCP 1716 and UDP 1716" bash -c 'ss -lntu | grep -q ":1716 "'
STATE=$W/gb; mkdir -p "$STATE"; chmod 700 "$STATE"

start_probe() { # start_probe TAG ARGS...  (stdin FIFO on fd 3)
    local tag=$1; shift
    P_LOG=$W/probe-$tag.log; P_CAP=$CAP/$tag.jsonl; : > "$P_LOG"
    rm -f "$W/in-$tag"; mkfifo "$W/in-$tag"
    "$PROBE" --state-dir "$STATE" --capture "$P_CAP" --downloads "$W/gb-downloads" --key-file "$W/key" \
        --peer-id "$SELF" "$@" < "$W/in-$tag" > "$P_LOG" 2>&1 &
    P_PID=$!
    exec 3<>"$W/in-$tag"
}
stop_probe() { echo quit >&3; exec 3>&-; wait "$P_PID" 2>/dev/null; }
send() { echo "$*" >&3; }
devprop() { busp get-property org.kde.kdeconnect "/modules/kdeconnect/devices/$GBID" org.kde.kdeconnect.device "$1"; }

# Our identity id (created by the first probe run).
GBID=$("$PROBE" --state-dir "$STATE" --capture /dev/null --downloads /tmp --key-file /dev/null --peer-id "$SELF" --connect 127.0.0.1:9 2>&1 | sed -n 's/^LOCAL-ID //p')
say "geekbridge deviceId=$GBID"

# ---------------------------------------------------------------- S1: GeekBridge dials, requests pairing
say "== S1: GeekBridge = TCP initiator/TLS server, GeekBridge requests pairing"
rm -f "$W/key"
start_probe s1 --connect 127.0.0.1:1716 --pair-mode request
waitfor "$P_LOG" "PAIR-PROMPT" 20 || say "FAIL  S1 no pair prompt"
cat "$P_LOG" | head -20 | sed 's/^/    /' >> "$R"
for i in $(seq 50); do devprop verificationKey | grep -q '"' && break; sleep 0.2; done
KEY=$(devprop verificationKey | sed 's/^s "//;s/"$//'); echo "$KEY" > "$W/key"
say "daemon verificationKey=$KEY"
sleep 0.5
busp call org.kde.kdeconnect "/modules/kdeconnect/devices/$GBID" org.kde.kdeconnect.device acceptPairing >> "$OUT/cli.log" 2>&1
waitfor "$P_LOG" "READY" 20 && say "PASS  S1 pairing completed (verification keys equal)" || say "FAIL  S1 pairing"
grep -E '^(LINK|PAIR-KEYS)' "$P_LOG" | sed 's/^/    /' >> "$R"
check "S1 daemon reports paired + reachable" bash -c "$CLI -l --id-only | grep -q $GBID && $CLI -a --id-only | grep -q $GBID"
$CLI -l >> "$R" 2>&1
$CLI -d "$GBID" --encryption-info >> "$R" 2>&1
sleep 2   # let the daemon send its post-pairing packets

say "-- features"
send ping;  sleep 1
grep -qi "ping" "$OUT/kdeconnectd.log" && say "daemon log mentions a ping" || say "NOTE  no ping line in daemon log (see log)"
$CLI -d "$GBID" --ping-msg "hello from daemon" >> "$OUT/cli.log" 2>&1; sleep 1
send "clip text copied on geekbridge"; sleep 1
$CLI -d "$GBID" --send-clipboard >> "$OUT/cli.log" 2>&1; sleep 1
send "clipconnect clipboard sync state"; sleep 1
$CLI -d "$GBID" --send-clipboard >> "$OUT/cli.log" 2>&1; sleep 1
send "sharetext hello text"; sleep 1
send "shareurl https://example.org/geekbridge"; sleep 1
head -c 300000 /dev/urandom > "$W/from-gb.bin"; send "sharefile $W/from-gb.bin"; sleep 3
head -c 200000 /dev/urandom > "$W/from-daemon.bin"
$CLI -d "$GBID" --share "$W/from-daemon.bin" >> "$OUT/cli.log" 2>&1
$CLI -d "$GBID" --share-text "text from daemon" >> "$OUT/cli.log" 2>&1
$CLI -d "$GBID" --share "https://example.org/from-daemon" >> "$OUT/cli.log" 2>&1
sleep 4
send ext; sleep 1
$CLI -d "$GBID" --ping >> "$OUT/cli.log" 2>&1; sleep 1
grep -E '^(RECV|SENT|SHARE|FILE|ERR|CLOSED|EVENT)' "$P_LOG" | cut -c1-300 | sed 's/^/    /' >> "$R"
DL=$(find "$HOME" -name 'from-gb.bin' 2>/dev/null | head -1)
say "daemon received file at: ${DL:-<none>}"
check "S1 daemon displayed the ping GeekBridge sent (Notify 'Ping!', pingReceived event)" bash -c "grep -q pingReceived '$OUT/dbus-monitor.log'"
check "S1 daemon clipboard adopted GeekBridge's kdeconnect.clipboard update" grep -q 'content":"text copied on geekbridge"' "$P_LOG"
check "S1 file GeekBridge->daemon byte-identical" bash -c "[ -n '$DL' ] && cmp -s '$DL' '$W/from-gb.bin'"
GOT=$(find "$W/gb-downloads" -name 'from-daemon*' 2>/dev/null | head -1)
check "S1 file daemon->GeekBridge byte-identical" bash -c "[ -n '$GOT' ] && cmp -s '$GOT' '$W/from-daemon.bin'"
check "S1 link still open after geekbridge.* extension packets" bash -c "! grep -q '^CLOSED' '$P_LOG' && kill -0 $P_PID"
check "S1 daemon still lists device reachable after extensions" bash -c "$CLI -a --id-only | grep -q $GBID"
check "S1 GeekBridge received daemon ping after extensions" bash -c "grep -c 'RECV type=kdeconnect.ping' '$P_LOG' | grep -qv '^0$'"
grep -iE "unknown|unsupported|ignor|geekbridge" "$OUT/kdeconnectd.log" | head -10 | sed 's/^/    daemon-log: /' >> "$R"

say "-- daemon unpairs"
$CLI -d "$GBID" --unpair >> "$OUT/cli.log" 2>&1; sleep 1.5
check "S1 GeekBridge saw peer unpair" grep -q "EVENT peer-unpaired" "$P_LOG"
stop_probe

# ---------------------------------------------------------------- S2: daemon dials (UDP identity), daemon requests pairing
say "== S2: daemon = TCP initiator (TLS server) after our UDP identity; daemon requests pairing"
rm -f "$W/key"; echo > /dev/null
start_probe s2 --listen 10.77.0.2:1740 --pair-mode await
sleep 0.5
python3 -I - "$GBID" <<'PY'
import json, socket, sys, time
ident = {"id": int(time.time()*1000), "type": "kdeconnect.identity", "body": {
    "deviceId": sys.argv[1], "deviceName": "GeekBridge", "protocolVersion": 8, "deviceType": "desktop",
    "incomingCapabilities": ["kdeconnect.ping", "kdeconnect.clipboard", "kdeconnect.clipboard.connect", "kdeconnect.share.request"],
    "outgoingCapabilities": ["kdeconnect.ping", "kdeconnect.clipboard", "kdeconnect.clipboard.connect", "kdeconnect.share.request"],
    "tcpPort": 1740}}
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("10.77.0.2", 0))
s.sendto((json.dumps(ident) + "\n").encode(), ("10.77.0.2", 1716))
PY
waitfor "$P_LOG" "ACCEPT" 15 && say "PASS  S2 daemon dialled us after UDP identity" || say "FAIL  S2 daemon never dialled"
waitfor "$P_LOG" "PENDING" 15
sleep 1
$CLI -d "$GBID" --pair >> "$OUT/cli.log" 2>&1
waitfor "$P_LOG" "PAIR-PROMPT" 20 || say "FAIL  S2 no pair prompt"
for i in $(seq 50); do devprop verificationKey | grep -q '"' && break; sleep 0.2; done
KEY=$(devprop verificationKey | sed 's/^s "//;s/"$//'); echo "$KEY" > "$W/key"; say "daemon verificationKey=$KEY"
waitfor "$P_LOG" "READY" 20 && say "PASS  S2 pairing completed (daemon requested, GeekBridge approved)" || say "FAIL  S2 pairing"
grep -E '^(LINK|PAIR-KEYS|PAIR-PROMPT)' "$P_LOG" | sed 's/^/    /' >> "$R"
sleep 2
send ping; sleep 1; send "clip second link clipboard"; sleep 1
$CLI -d "$GBID" --ping-msg "second link" >> "$OUT/cli.log" 2>&1; sleep 1
grep -E '^(RECV|SENT|ERR|CLOSED)' "$P_LOG" | cut -c1-250 | sed 's/^/    /' >> "$R"
check "S2 GeekBridge received daemon ping on inverted-role link" grep -q "RECV type=kdeconnect.ping" "$P_LOG"
stop_probe
sleep 1

# ---------------------------------------------------------------- S3: reconnect with pinned certificates, both roles
say "== S3: reconnect, certificates pinned on both sides"
start_probe s3 --connect 127.0.0.1:1716
waitfor "$P_LOG" "TRUSTED" 20 && say "PASS  S3 pinned reconnect, GeekBridge dials" || say "FAIL  S3 not trusted"
waitfor "$P_LOG" "READY" 10
send ping; sleep 1
$CLI -d "$GBID" --ping-msg "pinned link" >> "$OUT/cli.log" 2>&1; sleep 1
check "S3 daemon accepted paired device (ping delivered)" grep -q "RECV type=kdeconnect.ping" "$P_LOG"
grep -E '^(LINK|TRUSTED|RECV|CLOSED)' "$P_LOG" | cut -c1-250 | sed 's/^/    /' >> "$R"
stop_probe

# ---------------------------------------------------------------- S4: protocol 7 is not discoverable by the stock desktop
say "== S4: UDP identity with protocolVersion 7 (characterisation: does the stock desktop dial a v7 peer?)"
python3 -I - <<'PY' > "$W/s4.out" 2>&1
import json, socket, time
l = socket.socket(); l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
l.bind(("10.77.0.2", 1741)); l.listen(1); l.settimeout(8)
ident = {"id": int(time.time()*1000), "type": "kdeconnect.identity", "body": {
    "deviceId": "0123456789abcdef0123456789abcdef", "deviceName": "OldPeer", "protocolVersion": 7, "deviceType": "phone",
    "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": ["kdeconnect.ping"], "tcpPort": 1741}}
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); u.bind(("10.77.0.2", 0))
u.sendto((json.dumps(ident) + "\n").encode(), ("10.77.0.2", 1716))
try:
    c, a = l.accept(); print("DIALLED", a)
except socket.timeout:
    print("NO-CONNECTION")
PY
if grep -q DIALLED "$W/s4.out"; then
    say "NOTE  S4 released kdeconnectd dials a protocol-7 UDP identity (reference master 25c0cc7 drops v<8 in LanLinkProvider::deviceDiscovered); GeekBridge refuses protocol 7 by design"
else
    say "NOTE  S4 this kdeconnectd ignores protocol-7 UDP identities (matches reference master)"
fi

# ---------------------------------------------------------------- S6/S7: TLS 1.2 only, forged certificate
say "== S6: pinned reconnect offering TLS 1.2 only (what Android negotiates)"
start_probe s6 --connect 127.0.0.1:1716 --tls12
waitfor "$P_LOG" "TRUSTED" 20 && say "PASS  S6 TLS 1.2 handshake with the stock desktop, pinned" || say "FAIL  S6"
grep -E '^LINK' "$P_LOG" | cut -c1-120 | sed 's/^/    /' >> "$R"
check "S6 negotiated TLS 1.2" grep -q "tls=TLSv1_2" "$P_LOG"
waitfor "$P_LOG" "READY" 10; send ping; sleep 1
$CLI -d "$GBID" --ping-msg "tls12 link" >> "$OUT/cli.log" 2>&1; sleep 1
check "S6 ping both ways over the TLS 1.2 link" grep -q "RECV type=kdeconnect.ping" "$P_LOG"
stop_probe

say "== S7: a stranger presenting a NEW certificate under the paired device id"
FORGE=$W/forge; mkdir -p "$FORGE"; chmod 700 "$FORGE"
"$PROBE" --state-dir "$FORGE" --capture "$CAP/s7.jsonl" --downloads "$W/forge-dl" --key-file /dev/null --peer-id "$SELF" \
    --forge-id "$GBID" --connect 127.0.0.1:1716 --inspect-only > "$W/probe-s7.log" 2>&1 &
FP=$!; sleep 6; kill $FP 2>/dev/null; wait $FP 2>/dev/null
cut -c1-160 "$W/probe-s7.log" | sed 's/^/    /' >> "$R"
check "S7 daemon refused the forged certificate at the TLS layer (handshake failed on our side)" grep -q "FATAL TLS handshake failed" "$W/probe-s7.log"
grep -iE "$GBID|certificate|Ignoring|not trusted|different" "$OUT/kdeconnectd.log" | tail -4 | cut -c1-220 | sed 's/^/    daemon-log: /' >> "$R"
check "S7 forged certificate never reaches READY (no feature traffic)" bash -c "! grep -q '^READY' '$W/probe-s7.log'"

# ---------------------------------------------------------------- S5: the shipped service + ctl
say "== S5: geekbridge-service / geekbridge-ctl (the shipped binaries) against the real daemon"
SVC=$REPO/target/debug/geekbridge-service; CTL=$REPO/target/debug/geekbridge-ctl
SD=$W/svc; mkdir -p "$SD"; chmod 700 "$SD"
"$SVC" --state-dir "$SD" --name GeekBridgeSvc --dev-loopback --no-announce --no-calling-probe \
    --discovery-port 17160 --api-port 17382 --control-port 17383 --listen-inbound 127.0.0.1:1750 > "$W/svc.log" 2>&1 &
SPID=$!
sleep 2
ctl() { "$CTL" --state-dir "$SD" --control-port 17383 "$@"; }
SVCID=$(ctl status | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["Status"]["device_id"])' 2>/dev/null)
[ -z "$SVCID" ] && SVCID=$(ctl status | grep -o '"device_id": *"[0-9a-f]*"' | head -1 | grep -o '[0-9a-f]\{32\}')
say "service deviceId=$SVCID"
ctl connect "$SELF" 127.0.0.1 1716 >> "$W/ctl.log" 2>&1
for i in $(seq 30); do ctl status 2>&1 | grep -q "$SELF" && break; sleep 0.3; done
sleep 1
ctl pair "$SELF" request >> "$W/ctl.log" 2>&1
for i in $(seq 40); do busp get-property org.kde.kdeconnect "/modules/kdeconnect/devices/$SVCID" org.kde.kdeconnect.device verificationKey | grep -q '"' && break; sleep 0.25; done
KEY=$(busp get-property org.kde.kdeconnect "/modules/kdeconnect/devices/$SVCID" org.kde.kdeconnect.device verificationKey | sed 's/^s "//;s/"$//')
say "daemon verificationKey (service pairing)=$KEY"
CH=$(ctl status | grep -o '"challenge_id": *"[^"]*"' | head -1 | sed 's/.*: *"//;s/"$//')
say "service challenge id=${CH:-<none>}"
busp call org.kde.kdeconnect "/modules/kdeconnect/devices/$SVCID" org.kde.kdeconnect.device acceptPairing >> "$W/ctl.log" 2>&1 &
sleep 0.3
ctl approve "$CH" "$KEY" >> "$W/ctl.log" 2>&1
sleep 2
check "S5 service paired with the real daemon (daemon lists it paired+reachable)" bash -c "$CLI -a --id-only | grep -q $SVCID"
ctl ping "$SELF" >> "$W/ctl.log" 2>&1; sleep 1
check "S5 service ping reached the daemon (Notify 'Ping!' count increased)" bash -c "[ \$(grep -c pingReceived '$OUT/dbus-monitor.log') -ge 4 ]"
$CLI -d "$SVCID" --ping-msg "to service" >> "$OUT/cli.log" 2>&1; sleep 1
ctl status > "$W/svc-status.json" 2>&1
check "S5 service counted the daemon's ping" grep -q '"pings_received": *[1-9]' "$W/svc-status.json"
check "S5 service link negotiated only ping/clipboard/share with the daemon" grep -q 'kdeconnect.share.request' "$W/svc-status.json"
cp "$W/ctl.log" "$OUT/ctl.log"
kill "$SPID" 2>/dev/null; wait "$SPID" 2>/dev/null

kill "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
say "daemon plugins loaded for GeekBridge: $(grep -o 'Loaded plugin: "[a-z_]*"' "$OUT/kdeconnectd.log" | sort -u | sed 's/Loaded plugin: //' | tr '\n' ' ')"
say "daemon discarded: $(grep -o 'discarding unsupported packet "[a-z_.]*"' "$OUT/kdeconnectd.log" | sort -u | sed 's/discarding unsupported packet //' | tr '\n' ' ')"
say "== done; full logs in $OUT"
