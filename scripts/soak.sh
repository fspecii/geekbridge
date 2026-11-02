#!/usr/bin/env bash
# Soak test: long reconnect/junk churn with fd and memory growth assertions, plus long fuzz runs.
#
#   scripts/soak.sh [--cycles N] [--fuzz-iters N] [--seconds N] [--bin-dir DIR] [--no-chaos] [--chaos-only]
#
# Part 1 runs the in-process leak tests (crates/bridge-service/tests/leaks.rs) and the packet and
# discovery fuzz tests in release mode with large counts (defaults: 2000 cycles, 2,000,000 fuzz
# iterations). Part 2 starts the real geekbridge-service and, for --seconds (default 60), hammers
# its inbound listener, discovery UDP port, public API and control API with junk, then fails if
# descriptors or RSS grew past the allowance after a warm-up. Part 3 (chaos) runs the failure-injection
# tests (crates/*/tests/chaos*.rs: lossy / slow / resetting / half-open links, 200 MB transfers with
# cancels, disk full, clipboard bursts, a hostile update server) in GEEKBRIDGE_CHAOS=soak mode: 1000
# reconnect cycles and full-size files. `cargo test` without the variable is the short CI mode.
# Linux only (/proc, bash /dev/tcp).
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
CYCLES=2000
FUZZ=2000000
SECONDS_TOTAL=60
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
: "${SOAK_RSS_GROWTH_KIB:=3072}"
: "${SOAK_FD_SLACK:=4}"

RUN_SOAK=1
RUN_CHAOS=1
while (($#)); do
    case "$1" in
        --cycles) CYCLES="$2"; shift ;;
        --fuzz-iters) FUZZ="$2"; shift ;;
        --seconds) SECONDS_TOTAL="$2"; shift ;;
        --bin-dir) BIN_DIR="$2"; shift ;;
        --no-chaos) RUN_CHAOS=0 ;;
        --chaos-only) RUN_SOAK=0 ;;
        -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

cd "$ROOT"
source "$ROOT/scripts/lib-tmp.sh"   # GB_TMP: scratch dir (also exported as TMPDIR / GEEKBRIDGE_TEST_TMP)
chaos() {
    echo "== part 3: chaos / failure-injection tests (soak mode) =="
    GEEKBRIDGE_CHAOS=soak cargo test --release --locked -p bridge-service \
        --test chaos_link --test chaos_transfer --test chaos_clipboard -- --nocapture --test-threads=1
    GEEKBRIDGE_CHAOS=soak cargo test --release --locked -p bridge-android --test chaos -- --nocapture --test-threads=1
    GEEKBRIDGE_CHAOS=soak cargo test --release --locked -p bridge-update --test chaos_http -- --nocapture
}
if ((!RUN_SOAK)); then chaos; exit 0; fi
echo "== part 1: in-process leak and fuzz tests ($CYCLES cycles, $FUZZ fuzz iterations) =="
GEEKBRIDGE_SOAK_CYCLES="$CYCLES" GEEKBRIDGE_FUZZ_ITERS="$FUZZ" \
    cargo test --release --locked -p bridge-service --test leaks --test parse_fuzz -- --nocapture --test-threads=1
GEEKBRIDGE_FUZZ_ITERS="$FUZZ" cargo test --release --locked -p bridge-kde --test packet_fuzz -- --nocapture

echo "== part 2: real service under junk traffic for ${SECONDS_TOTAL}s =="
cargo build --release --locked -p bridge-service
BIN_DIR="$(cd "$BIN_DIR" && pwd -P)"
TMP="$(gb_mktemp soak -d)"
SVC=""
cleanup() { [[ -n "$SVC" ]] && kill "$SVC" 2>/dev/null || true; wait 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

BASE=$((20000 + RANDOM % 20000))
API=$BASE CONTROL=$((BASE + 1)) DISC=$((BASE + 2)) INBOUND=$((BASE + 3))
GEEKBRIDGE_NO_UPDATE_CHECK=1 "$BIN_DIR/geekbridge-service" --state-dir "$TMP/state" --api-port "$API" \
    --control-port "$CONTROL" --discovery-port "$DISC" --listen-inbound "127.0.0.1:$INBOUND" \
    --no-calling-probe >"$TMP/log" 2>&1 &
SVC=$!
until grep -q 'discovery API' "$TMP/log"; do kill -0 "$SVC" || { cat "$TMP/log" >&2; exit 1; }; sleep 0.02; done

rss() { awk '$1 == "VmRSS:" {print $2}' "/proc/$SVC/status"; }
fds() { find "/proc/$SVC/fd" -mindepth 1 | wc -l; }
junk_round() {
    local i
    for i in 1 2 3 4 5 6 7 8; do
        { head -c $((RANDOM % 3000)) /dev/urandom >"/dev/tcp/127.0.0.1/$INBOUND"; } 2>/dev/null || true
        { head -c $((RANDOM % 1000)) /dev/urandom >"/dev/udp/127.0.0.1/$DISC"; } 2>/dev/null || true
        { printf 'GET /v1/discovery HTTP/1.0\r\n\r\n' >"/dev/tcp/127.0.0.1/$API"; } 2>/dev/null || true
        { head -c $((RANDOM % 200)) /dev/urandom >"/dev/tcp/127.0.0.1/$CONTROL"; } 2>/dev/null || true
        { printf '{"id":1,"type":"kdeconnect.identity","body":{"deviceId":"%032d","deviceName":"soak","deviceType":"phone","tcpPort":1716,"protocolVersion":8}}' "$RANDOM" >"/dev/udp/127.0.0.1/$DISC"; } 2>/dev/null || true
    done
}

warm_end=$((SECONDS + SECONDS_TOTAL / 4))
while ((SECONDS < warm_end)); do junk_round; done
sleep 6 # handshake deadlines (5 s) expire for the connections left half-open
base_rss=$(rss); base_fds=$(fds)
end=$((SECONDS + SECONDS_TOTAL))
rounds=0
while ((SECONDS < end)); do junk_round; rounds=$((rounds + 1)); done
sleep 6
end_rss=$(rss); end_fds=$(fds)
kill -0 "$SVC" || { echo "service died during the soak" >&2; cat "$TMP/log" >&2; exit 1; }
echo "rounds=$rounds fds $base_fds -> $end_fds, rss $base_rss kB -> $end_rss kB"
fail=0
((end_fds <= base_fds + SOAK_FD_SLACK)) || { echo "FAIL: descriptor growth" >&2; fail=1; }
((end_rss <= base_rss + SOAK_RSS_GROWTH_KIB)) || { echo "FAIL: RSS growth" >&2; fail=1; }
((RUN_CHAOS)) && chaos
exit "$fail"
