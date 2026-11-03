#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests for the real-device kit with a stub geekbridge-ctl (no service, no phone, no network, no fixed ports):
#   1. scripts/lib/report_redact.py removes canary secrets and keeps the diagnostic facts
#   2. scripts/realdevice-report.sh output and tarball contain no canary, and carry the sections
#   3. scripts/realdevice.sh link-watch logs transitions with reason codes and computes uptime
# Run: scripts/tests/realdevice-kit-test.sh   (about 20 s)
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/kittest-XXXXXX")"; : >"$T/.gb-owned"
trap 'rm -rf -- "$T"' EXIT
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }
has() { grep -qF -- "$2" "$1"; }
absent() { # file canary...: none of the canaries may appear
  local f="$1" c; shift
  for c in "$@"; do if grep -qF -- "$c" "$f"; then bad "canary leaked into $(basename "$f"): $(grep -F -- "$c" "$f" | head -2 | tr '\n' '|')"; fi; done
}

# Canaries: every one must be gone from every output.
C_TOKEN="CANARYTOKEN7f3a9c1e5b2d4f60a8c3e1b7d9f2a4c6"
C_SMS="CANARY-SMS-BODY-meet-me-at-the-bridge"
C_NOTIF="CANARY-NOTIFICATION-BODY-your-code-is-481516"
C_CLIP="CANARY-CLIPBOARD-hunter2-secret"
C_CONTACT="CANARY-Contact-Zelda-Fitzgerald"
C_NUM1="+4915112345678"; C_NUM2="0049 151 1234 5678"; C_NUM3="5551234567"; C_NUM4="(555) 123-4567"
C_MAIL="canary.user@example.org"
C_KEY="CANARYPRIVATEKEYMATERIAL0123456789"
C_PAIR="CANARYPAIRSECRET"
C_PHONENAME="CANARY-Fold5-of-Alice"
CANARIES=("$C_TOKEN" "$C_SMS" "$C_NOTIF" "$C_CLIP" "$C_CONTACT" "$C_NUM1" "$C_NUM2" "$C_NUM3" "$C_NUM4" "$C_MAIL" "$C_KEY" "$C_PAIR" "$C_PHONENAME" "PRIVATE KEY-----")

# ---------------------------------------------------------------- 1. redactor
cat >"$T/in.txt" <<EOT
service version 0.1.0 reason link_timed_out
HASH 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  /home/$USER/bin/geekbridge-service
{"name": "$C_PHONENAME", "body": "$C_SMS", "text": "$C_NOTIF", "title": "$C_CONTACT", "clipboard": "$C_CLIP"}
  "peer_cert_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
  "since_unix_ms": 1730000000123,
  "code": "phone_silent",
Authorization: Bearer $C_TOKEN
token=$C_TOKEN
-----BEGIN PRIVATE KEY-----
$C_KEY
-----END PRIVATE KEY-----
from $C_NUM1 and $C_NUM2 and $C_NUM3 and $C_NUM4 mail $C_MAIL
link geekbridge://pair?code=$C_PAIR&host=192.168.1.5
peer 192.168.77.12:1716 loopback 127.0.0.1 mac aa:bb:cc:dd:ee:ff
2026-11-03T10:11:12 masked +49 ••• ••• 78
EOT
echo "$C_TOKEN" >"$T/tok"
python3 -I "$src/scripts/lib/report_redact.py" --needle-file "$T/tok" <"$T/in.txt" >"$T/out.txt"
absent "$T/out.txt" "${CANARIES[@]}"
for keep in "version 0.1.0" "link_timed_out" "phone_silent" "1730000000123" "2026-11-03T10:11:12" "127.0.0.1" "192.168.x.x" "geekbridge://pair?<redacted>" "ffffffffffff..." "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  /home/USER/bin"; do
  has "$T/out.txt" "$keep" || bad "redactor removed or changed what it should keep: $keep"
done
[[ $fail == 0 ]] && ok "redactor: canaries removed, versions/hashes/reason codes/timestamps kept"

# ---------------------------------------------------------------- stub geekbridge-ctl
STUB="$T/bin"; mkdir -p "$STUB" "$T/state" "$T/w"
echo "$C_TOKEN" >"$T/state/ipc-token"
PHONE=aabbccdd11223344aabbccdd11223344
cat >"$STUB/geekbridge-ctl" <<'EOS'
#!/usr/bin/env bash
# stub: state in $STUB_W (seq = scripted statuses, n = call counter)
while [[ ${1:-} == --state-dir || ${1:-} == --control-port ]]; do shift 2; done
case "${1:-}" in
  --version) echo "geekbridge-ctl 9.9.9-stub" ;;
  status)
    n=$(( $(cat "$STUB_W/n" 2>/dev/null || echo 0) + 1 )); echo "$n" >"$STUB_W/n"
    line="$(sed -n "${n}p" "$STUB_W/seq")"; [[ -n $line ]] || line="$(tail -n1 "$STUB_W/seq")"
    case "$line" in
      svcdown) echo "error: cannot reach the service" >&2; exit 1 ;;
      up) cat <<J
{"kind":"status","ipc_version":1,"service_version":"9.9.9-stub","device_id":"cccc","device_name":"CANARY-Fold5-of-Alice","own_cert_sha256":"$(printf 'a%.0s' {1..64})","paired":[{"device_id":"$PHONE","name":"CANARY-Fold5-of-Alice"}],"links":[{"device_id":"$PHONE","name":"CANARY-Fold5-of-Alice","phase":"connected","pings_received":$n,"peer_ip":"192.168.77.12"}],"disconnected":[]}
J
      ;;
      down:*) code="${line#down:}"; cat <<J
{"kind":"status","ipc_version":1,"service_version":"9.9.9-stub","device_id":"cccc","paired":[{"device_id":"$PHONE","name":"CANARY-Fold5-of-Alice"}],"links":[],"disconnected":[{"device_id":"$PHONE","code":"$code","sentence":"x","fix":"y","last_seen_ago_s":42,"tries":3}]}
J
      ;;
    esac ;;
  doctor) echo "[ ok ] Service: geekbridge-service 9.9.9-stub answers as \"CANARY-Fold5-of-Alice\"."; echo "[warn] Phones: reachable at 192.168.77.12 token=$CANARY_TOKEN" ;;
  events) echo '{"kind":"events","events":[{"seq":1,"kind":"link_lost","text":"CANARY-SMS-BODY-meet-me-at-the-bridge"}]}' ;;
  clipboard) echo "backend: stub"; echo "last: CANARY-CLIPBOARD-hunter2-secret" ;;
  calling|update|firewall) echo "stub $1" ;;
  phone-state) echo "debug state for $2"; echo "notification: CANARY-NOTIFICATION-BODY-your-code-is-481516 from +4915112345678"; echo "contact CANARY-Contact-Zelda-Fitzgerald 0049 151 1234 5678 (555) 123-4567 5551234567" ;;
  phone-log) echo "I/Link heartbeat ok"; echo "D/Sms body=CANARY-SMS-BODY-meet-me-at-the-bridge mail canary.user@example.org" ;;
  *) echo "stub: unhandled $*" >&2; exit 2 ;;
esac
EOS
chmod +x "$STUB/geekbridge-ctl"
export STUB_W="$T/w" CANARY_TOKEN="$C_TOKEN" PHONE

# ---------------------------------------------------------------- 2. report
echo up >"$T/w/seq"; rm -f "$T/w/n"
cat >"$T/svc.log" <<EOT
2026-11-03 10:00:00 INFO service started 9.9.9-stub
2026-11-03 10:00:01 DEBUG control token $C_TOKEN accepted
2026-11-03 10:00:02 WARN sms body "$C_SMS" from $C_NUM1
-----BEGIN PRIVATE KEY-----
$C_KEY
-----END PRIVATE KEY-----
2026-11-03 10:00:03 INFO pairing geekbridge://pair?secret=$C_PAIR clipboard=$C_CLIP notification=$C_NOTIF
contact $C_CONTACT $C_NUM4 $C_NUM3 $C_NUM2 $C_MAIL
EOT
mkdir -p "$T/out"
GEEKBRIDGE_BIN_DIR="$STUB" "$src/scripts/realdevice-report.sh" --state-dir "$T/state" --control-port 1 --service-log "$T/svc.log" --out "$T/out" --no-adb >"$T/report.stdout" 2>&1 || bad "report script failed: $(cat "$T/report.stdout")"
rep="$(ls "$T"/out/geekbridge-report-*.txt 2>/dev/null | head -1)"
if [[ -z $rep ]]; then bad "no report file written"; else
  absent "$rep" "${CANARIES[@]}"
  mkdir -p "$T/untar"; tar -C "$T/untar" -xzf "$rep.tar.gz"; absent "$T"/untar/*.txt "${CANARIES[@]}"
  for want in "## doctor" "## status (JSON)" "## events" "phone-state aabbccdd" "phone-log aabbccdd" "## Firewall and listening ports" "## Service log" "geekbridge-ctl 9.9.9-stub" "HASH " "heartbeat ok" "service started 9.9.9-stub"; do
    has "$rep" "$want" || bad "report lacks: $want"
  done
  [[ "$(stat -c %a "$rep")" == 600 ]] && ok "report file mode 600" || bad "report file mode is not 600"
  [[ -z "$(ls -A "$T/out" | grep '^\.work' || true)" ]] && ok "no work directory left behind" || bad "work directory left behind"
fi
# the served page: an .apk link is fetched and hashed (local http server on a random port)
mkdir -p "$T/site"; printf 'not really an apk' >"$T/site/app.apk"; echo '<a href="app.apk">download</a>' >"$T/site/index.html"
port=$((30000 + RANDOM % 20000))
(cd "$T/site" && exec python3 -I -m http.server "$port" --bind 127.0.0.1 >/dev/null 2>&1) & srv=$!
trap 'kill $srv 2>/dev/null; rm -rf -- "$T"' EXIT
sleep 1
GEEKBRIDGE_BIN_DIR="$STUB" "$src/scripts/realdevice-report.sh" --state-dir "$T/state" --control-port 1 --service-log "$T/svc.log" --out "$T/out2" --no-adb --page "http://127.0.0.1:$port/index.html" >/dev/null 2>&1 || bad "report with --page failed"
want="$(sha256sum "$T/site/app.apk" | cut -d' ' -f1)"
has "$(ls "$T"/out2/*.txt | head -1)" "HASH $want  served APK http://127.0.0.1:$port/app.apk" && ok "report: hash of the APK linked from the served page" || bad "served APK hash missing"
kill $srv 2>/dev/null || true
# the script must never ask for content
if grep -nE 'ctl +(notifications|messages|agenda|contacts|clipboard +(paste|push)|calendar)' "$src/scripts/realdevice-report.sh"; then bad "report script asks for personal content"; else ok "report script never requests notifications, messages, contacts, calendar or clipboard text"; fi
[[ $fail == 0 ]] && ok "report: no canary in the text or the tarball; sections present"

# ---------------------------------------------------------------- 3. link-watch
watch() { # seq-lines... ; runs 8 s at 1 s steps
  printf '%s\n' "$@" >"$T/w/seq"; rm -f "$T/w/n"
  GEEKBRIDGE_BIN_DIR="$STUB" GEEKBRIDGE_REALDEVICE_STATE="$T/state" GEEKBRIDGE_CONTROL_PORT=1 \
    "$src/scripts/realdevice.sh" link-watch --seconds 8 --interval 1 --log "$T/watch.log" >"$T/watch.out" 2>&1 || true
}
watch up up up up up up up up up
grep -q 'uptime 100.0%' "$T/watch.out" && ok "link-watch: steady link is 100% up" || { bad "steady link not 100%"; cat "$T/watch.out"; }
! grep -q ' DOWN ' "$T/watch.log" && ok "link-watch: no DOWN line for a steady link" || bad "unexpected DOWN"
watch up up up down:phone_silent down:link_timed_out down:link_timed_out up up up
for want in "DOWN  aabbccdd reason=phone_silent tries=3 last_seen_ago=42s" "STILL aabbccdd reason=link_timed_out" "UP    aabbccdd after" "codes seen: phone_silent link_timed_out"; do
  has "$T/watch.log" "$want" || { bad "watch log lacks: $want"; cat "$T/watch.log"; }
done
pct="$(sed -n 's/.*uptime \([0-9.]*\)%.*/\1/p' "$T/watch.out" | head -1)"
awk -v p="${pct:-0}" 'BEGIN{exit !(p>=30 && p<=80)}' && ok "link-watch: outage shows as ${pct}% uptime" || { bad "uptime ${pct:-?}% outside 30..80"; cat "$T/watch.out"; }
grep -q 'outages: 1' "$T/watch.out" && ok "link-watch: summary counts the outage" || bad "no outage summary"
grep -qE '^[0-9-]+ [0-9:]+ DOWN ' "$T/watch.log" && ok "link-watch: timestamps on every line" || bad "log lines lack timestamps"
watch up up svcdown svcdown up up up up up
has "$T/watch.log" "reason=service_unreachable" && ok "link-watch: unreachable service is DOWN with its own code, not blamed on the phone" || { bad "service outage not logged"; cat "$T/watch.log"; }
watch down:firewall_blocking
has "$T/watch.log" "already down at the first sample" && has "$T/watch.out" "run geekbridge-ctl doctor" && ok "link-watch: down from the start names the cause and the next step" || { bad "down-at-start handling"; cat "$T/watch.out"; }

echo
if [[ $fail == 0 ]]; then echo "realdevice kit tests passed"; else echo "realdevice kit tests FAILED"; exit 1; fi
