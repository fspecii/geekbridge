#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# EXPERIMENT: the computer half of call audio over Wi-Fi against a PRIVATE PipeWire daemon (never the user's session).
# Starts `pipewire` with its own XDG_RUNTIME_DIR (no session manager, no devices), creates two null sinks that stand
# in for the headset ("gb-spk") and the headset microphone ("gb-mic"), then runs geekbridge-callaudio-lab with
#   far party (tone A, phone side) -> Opus -> TLS link code -> jitter buffer -> pw-cat playback -> gb-spk
#   gb-mic <- tone B player;  gb-mic monitor -> pw-cat record -> Opus -> TLS -> phone side (meter)
# and checks with a Goertzel scan that tone A came out of gb-spk's monitor and tone B reached the phone side.
# Ports are configured and linked by hand (pw-cli PortConfig + pw-link), which is what WirePlumber would do.
# Audio is analysed in a pipe; nothing is written to disk. Evidence: dist/android/logs/call-audio-pipewire/.
#
#   scripts/e2e/call-audio-pipewire.sh        (needs pipewire, pw-cli, pw-link, pw-cat, python3, cargo)
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
SCRATCH="${GB_CA_SCRATCH:-$HOME/.cache/gb-callaudio-pw}"
export TMPDIR="$SCRATCH/tmp"
mkdir -p "$TMPDIR"
OUT="$ROOT/dist/android/logs/call-audio-pipewire/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"
RESULTS="$OUT/results.tsv"; : > "$RESULTS"
fails=0
row() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$RESULTS"; [ "$1" = FAIL ] && fails=$((fails + 1)); return 0; }

for t in pipewire pw-cli pw-link pw-cat python3 cargo; do
  command -v "$t" >/dev/null || { row SKIP host "missing $t"; exit 0; }
done
LAB_TARGET="$SCRATCH/target"
(cd "$ROOT/experiments/callaudio-wifi" && CARGO_TARGET_DIR="$LAB_TARGET" cargo build --release --quiet) >"$OUT/lab-build.log" 2>&1 \
  || { row FAIL host "lab build, see $OUT/lab-build.log"; exit 1; }
LAB="$LAB_TARGET/release/geekbridge-callaudio-lab"

RT="$SCRATCH/runtime"
rm -rf "$RT"; mkdir -p "$RT"; chmod 700 "$RT"
export XDG_RUNTIME_DIR="$RT" PIPEWIRE_RUNTIME_DIR="$RT" PIPEWIRE_REMOTE=pipewire-0
unset PULSE_SERVER
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; rm -rf "$RT" "$TMPDIR"; }
trap cleanup EXIT
DBUS_SESSION_BUS_ADDRESS=disabled: pipewire >"$OUT/pipewire.log" 2>&1 &
pids+=($!)
for _ in $(seq 1 50); do [ -S "$RT/pipewire-0" ] && break; sleep 0.1; done
[ -S "$RT/pipewire-0" ] || { row FAIL host "private pipewire did not start"; exit 1; }
row PASS host "private pipewire at $RT (not the user session)"

nid() { pw-cli ls Node | awk -v n="\"$1\"" '/^\tid/{i=$2} $0 ~ "node.name = "n {print i}' | tr -d ',' | tail -1; }
portcfg() { # NODE DIRECTION [monitor]
  local id; id="$(nid "$1")"; [ -n "$id" ] || return 1
  pw-cli set-param "$id" PortConfig "{ direction: $2, mode: dsp, monitor: ${3:-false}, format: { mediaType: audio, mediaSubtype: raw, format: F32P, rate: 16000, channels: 1, position: [ MONO ] } }" >/dev/null
}
waitnode() { for _ in $(seq 1 50); do [ -n "$(nid "$1")" ] && return 0; sleep 0.1; done; return 1; }
for s in gb-spk gb-mic; do
  pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$s node.description=$s media.class=Audio/Sink audio.position=[MONO] audio.rate=16000 object.linger=true node.driver=true }" >/dev/null
  waitnode "$s" && portcfg "$s" Input true
done

# Tone B into gb-mic (the "headset microphone").
python3 -I -c '
import math, struct, sys, time
n = 0
while True:
    sys.stdout.buffer.write(b"".join(struct.pack("<h", int(math.sin(2*math.pi*660*(n+i)/16000)*9000)) for i in range(320)))
    sys.stdout.flush(); n += 320; time.sleep(0.02)
' 2>/dev/null | pw-cat --playback --raw --format s16 --rate 16000 --channels 1 -P node.name=gb-mic-tone - 2>/dev/null &
pids+=($!)
# Analyser on gb-spk's monitor: reads 4 s from a pipe, prints dominant Hz and RMS, keeps nothing.
pw-cat --record --raw --format s16 --rate 16000 --channels 1 -P node.name=gb-spk-probe - 2>/dev/null | python3 -I -c '
import math, struct, sys
need = 16000 * 2 * 6
buf = sys.stdin.buffer.read(need)
x = struct.unpack("<%dh" % (len(buf)//2), buf)[-32000:]
def p(f):
    c = 2*math.cos(2*math.pi*f/16000); s1 = s2 = 0.0
    for v in x: s0 = v + c*s1 - s2; s2 = s1; s1 = s0
    return s1*s1 + s2*s2 - c*s1*s2
best = max(range(100, 4000, 10), key=p)
best = max(range(best-10, best+11), key=p)
rms = math.sqrt(sum(v*v for v in x)/max(1, len(x)))
print("%d %d" % (best, rms))
' > "$OUT/spk-analysis.txt" &
pids+=($!)

"$LAB" --seconds 9 --down-tone 523 --down-pw-target gb-spk --up-pw-target gb-mic --pw-runtime-dir "$RT" > "$OUT/lab.json" 2> "$OUT/lab.err" &
labpid=$!
pids+=("$labpid")
for n in geekbridge-callaudio-out geekbridge-callaudio-in gb-mic-tone gb-spk-probe; do waitnode "$n" || row FAIL host "node $n did not appear"; done
portcfg geekbridge-callaudio-out Output; portcfg gb-mic-tone Output
portcfg geekbridge-callaudio-in Input; portcfg gb-spk-probe Input
sleep 0.3
pw-link geekbridge-callaudio-out:output_MONO gb-spk:playback_MONO \
  && pw-link gb-spk:monitor_MONO gb-spk-probe:input_MONO \
  && pw-link gb-mic-tone:output_MONO gb-mic:playback_MONO \
  && pw-link gb-mic:monitor_MONO geekbridge-callaudio-in:input_MONO \
  && row PASS host "nodes linked (lab out -> gb-spk -> analyser; tone -> gb-mic -> lab in)" \
  || row FAIL host "pw-link failed: $(pw-link -o | tr '\n' ' ')"
pw-link -l > "$OUT/links.txt" 2>&1
wait "$labpid"
read -r hz rms < "$OUT/spk-analysis.txt" || true
if [ -n "${hz:-}" ] && [ "$hz" -ge 515 ] && [ "$hz" -le 531 ] && [ "${rms:-0}" -gt 300 ]; then
  row PASS down "far-end 523 Hz tone came out of the headset stand-in (gb-spk monitor): ${hz} Hz rms ${rms}"
else row FAIL down "gb-spk monitor: hz=${hz:-?} rms=${rms:-?}"; fi
up="$(python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1]))["up"]; print(int(d["dominantHz"]), int(d["rms"]), d["latencyMs"]["p50"], d["latencyMs"]["p95"])' "$OUT/lab.json" 2>/dev/null)"
read -r uhz urms up50 up95 <<<"$up"
if [ -n "${uhz:-}" ] && [ "$uhz" -ge 650 ] && [ "$uhz" -le 670 ]; then
  row PASS up "headset-mic stand-in 660 Hz reached the phone side: ${uhz} Hz rms ${urms}, capture->playout p50 ${up50} ms p95 ${up95} ms"
else row FAIL up "phone side heard: ${up:-nothing} (see $OUT/lab.json)"; fi
down="$(python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1]))["down"]; print(d["latencyMs"]["p50"], d["latencyMs"]["p95"], d["jitterBuffer"]["lost"])' "$OUT/lab.json" 2>/dev/null)"
row INFO down "capture->jitter-buffer-out p50/p95/lost: ${down:-?} (PipeWire's 20 ms node latency comes on top)"
echo "results: $RESULTS"
[ "$fails" -eq 0 ]
