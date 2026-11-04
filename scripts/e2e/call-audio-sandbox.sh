#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Call audio without Bluetooth hardware: a private PipeWire + WirePlumber + pipewire-pulse, a fake BlueZ
# with a fake phone, and GeekBridge's call-audio code driven against them
# (docs/reports/call-audio-pc-headset.md).
#
#   scripts/e2e/call-audio-sandbox.sh            run the sandbox tests (crates/bridge-calls/tests/audio_sandbox.rs)
#   scripts/e2e/call-audio-sandbox.sh --shell    start the sandbox and open a shell inside it (exit to tear down)
#
# Two stages: (1) cargo test crates/bridge-calls/tests/audio_sandbox.rs (controller, telephony, setup engine);
# (2) a private geekbridge-service (own state dir and ports, no tray, no LAN) driven only through geekbridge-ctl:
# calling checklist, call audio-devices, ring + `call answer` + routing + `call mute` + `call audio phone`, hang-up
# restore, `call setup` with the code confirmed through `--confirm yes`.
#
# What is real and what is faked:
#   real   PipeWire 1.x, WirePlumber 0.5, pipewire-pulse, PipeWire's bluez5 plugin (HFP hands-free code, the
#          `bluez_card.<ADDR>` card with its `audio-gateway` profile, the org.pipewire.Telephony service), libpulse,
#          GeekBridge's telephony backend, call-audio controller and libpulse adapter
#   faked  BlueZ and the phone (scripts/e2e/call-audio/fake_bluez.py: org.bluez on a private "system" bus; the phone
#          speaks HFP AT commands over a socketpair handed to PipeWire as the RFCOMM link), the headset and speaker
#          (null sinks), the microphone (a remap of a null sink's monitor), and the SCO voice link: without a kernel
#          Bluetooth controller no SCO socket can exist, so the two voice streams PipeWire would create are stood in
#          for by two pw-cat streams carrying the same properties (api.bluez5.address, api.bluez5.profile =
#          headset-audio-gateway, api.bluez5.codec)
#   not    the kernel (hci_vhci/btvirt need root: /dev/vhci is root-only), real radio, real audio quality
#
# Isolation: own XDG_RUNTIME_DIR, HOME, XDG_CONFIG_HOME/STATE_HOME (so WirePlumber's stored state is private), own
# session bus and own "system" bus with no service activation, PULSE_SERVER pointing at the private socket, and
# WirePlumber with every hardware monitor (ALSA, V4L2, libcamera, MIDI) disabled. The real session, the real sound
# server and the real Bluetooth stack are not touched. The only host resource shared is the kernel's SCO socket
# namespace: the sandbox's PipeWire tries to listen for SCO and fails with "Address already in use" when the
# desktop's PipeWire already does, which is harmless.
#
# Needs: pipewire, wireplumber, pipewire-pulse (pipewire-pulse binary), pw-cat, pactl, dbus-daemon, python3 with
# python-dbus and python-gobject, cargo.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SCRATCH=${GB_SCRATCH:-${XDG_CACHE_HOME:-$HOME/.cache}/geekbridge-call-audio-sandbox}
MODE=test
[[ ${1:-} == --shell ]] && MODE=shell

for b in pipewire wireplumber pipewire-pulse pw-cat pactl dbus-daemon python3; do
  command -v "$b" >/dev/null || { echo "missing: $b" >&2; exit 2; }
done
python3 -I -c 'import dbus, gi' 2>/dev/null || { echo "missing: python-dbus / python-gobject" >&2; exit 2; }

mkdir -p "$SCRATCH"
SBX=$(mktemp -d "$SCRATCH/run.XXXXXX")
mkdir -m 700 "$SBX/xdg" "$SBX/tmp"
# Nothing of the sandbox goes to /tmp (shared tmpfs): sockets, temp files and logs stay under $SBX.
export TMPDIR="$SBX/tmp"
mkdir -p "$SBX/home/.config/wireplumber/wireplumber.conf.d" "$SBX/home/.config/pipewire/pipewire.conf.d" \
  "$SBX/state" "$SBX/gb-state" "$SBX/log"
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ ${GB_KEEP_SANDBOX:-0} == 1 ]]; then echo "kept: $SBX"; else rm -rf "$SBX"; fi
}
trap cleanup EXIT

cat > "$SBX/home/.config/wireplumber/wireplumber.conf.d/10-sandbox.conf" <<'EOF'
wireplumber.profiles = {
  main = {
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.v4l2 = disabled
    monitor.libcamera = disabled
    monitor.bluez-midi = disabled
    monitor.bluez = required
    support.reserve-device = disabled
  }
}
EOF
cat > "$SBX/home/.config/pipewire/pipewire.conf.d/10-sandbox.conf" <<'EOF'
context.objects = [
  { factory = adapter args = { factory.name = support.null-audio-sink node.name = "sbx_headset_out"
      node.description = "Sandbox headset" media.class = "Audio/Sink" audio.position = [ FL FR ] device.form_factor = headset } }
  { factory = adapter args = { factory.name = support.null-audio-sink node.name = "sbx_speaker"
      node.description = "Sandbox speaker" media.class = "Audio/Sink" audio.position = [ FL FR ] } }
  { factory = adapter args = { factory.name = support.null-audio-sink node.name = "sbx_mic_feed"
      node.description = "Sandbox microphone feed" media.class = "Audio/Sink" audio.position = [ MONO ] } }
]
EOF
cat > "$SBX/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir=$SBX/xdg</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF

REAL_HOME=$HOME
export CARGO_HOME=${CARGO_HOME:-$REAL_HOME/.cargo} RUSTUP_HOME=${RUSTUP_HOME:-$REAL_HOME/.rustup}
export HOME="$SBX/home" XDG_CONFIG_HOME="$SBX/home/.config" XDG_STATE_HOME="$SBX/state" \
  XDG_DATA_HOME="$SBX/home/.local/share" XDG_RUNTIME_DIR="$SBX/xdg"
unset PIPEWIRE_REMOTE PIPEWIRE_RUNTIME_DIR DISPLAY WAYLAND_DISPLAY
export PULSE_SERVER="unix:$SBX/xdg/pulse/native"

bus() { dbus-daemon --config-file="$SBX/bus.conf" --print-address --fork --print-pid 3 3>"$SBX/$1.pid" | head -1; }
DBUS_SESSION_BUS_ADDRESS=$(bus session); export DBUS_SESSION_BUS_ADDRESS
DBUS_SYSTEM_BUS_ADDRESS=$(bus system); export DBUS_SYSTEM_BUS_ADDRESS
PIDS+=("$(cat "$SBX/session.pid")" "$(cat "$SBX/system.pid")")

python3 -I "$ROOT/scripts/e2e/call-audio/fake_bluez.py" >"$SBX/log/fake_bluez.log" 2>&1 & PIDS+=($!)
sleep 0.3
pipewire >"$SBX/log/pipewire.log" 2>&1 & PIDS+=($!)
for _ in $(seq 50); do [[ -S $XDG_RUNTIME_DIR/pipewire-0 ]] && break; sleep 0.1; done
WIREPLUMBER_DEBUG=${WIREPLUMBER_DEBUG:-2} wireplumber >"$SBX/log/wireplumber.log" 2>&1 & PIDS+=($!)
pipewire-pulse >"$SBX/log/pipewire-pulse.log" 2>&1 & PIDS+=($!)
for _ in $(seq 50); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
# The null sinks appear a moment after pipewire-pulse answers: retry until they are there.
for _ in $(seq 50); do pactl list sinks short 2>/dev/null | grep -q sbx_mic_feed && break; sleep 0.1; done
for _ in $(seq 20); do
  pactl load-module module-remap-source master=sbx_mic_feed.monitor source_name=sbx_mic \
    source_properties=device.description=Sandbox-microphone >/dev/null 2>&1 && break
  sleep 0.2
done
for _ in $(seq 20); do pactl set-default-sink sbx_headset_out 2>/dev/null && break; sleep 0.2; done
for _ in $(seq 20); do pactl set-default-source sbx_mic 2>/dev/null && break; sleep 0.2; done
# The fake phone's card appears once PipeWire's hands-free code has finished its setup with it.
for _ in $(seq 100); do pactl list cards short 2>/dev/null | grep -q bluez_card && break; sleep 0.1; done
pactl list cards short | grep -q bluez_card || { echo "the fake phone's card did not appear"; tail -30 "$SBX/log/fake_bluez.log"; exit 1; }

echo "sandbox up: $SBX"
pactl info | grep -E 'Server Name|Default'
pactl list cards short
export GEEKBRIDGE_AUDIO_SANDBOX=1 GEEKBRIDGE_SANDBOX_STATE="$SBX/gb-state"

if [[ $MODE == shell ]]; then
  echo "inside the sandbox; try: pactl list cards, busctl --user tree org.pipewire.Telephony,"
  echo "busctl --address=\$DBUS_SYSTEM_BUS_ADDRESS call org.bluez /geekbridge/FakePhone org.geekbridge.FakePhone Ring s +15550100"
  bash --norc -i || true
  exit 0
fi

cd "$ROOT"
mkdir -p "$SBX/sysfs/hci0"
export GEEKBRIDGE_BT_SYSFS="$SBX/sysfs"   # the sandbox's stand-in for /sys/class/bluetooth
set +e
cargo test -p bridge-calls --test audio_sandbox -- --ignored --nocapture --test-threads=1
rc=$?
set -e

# ---------------------------------------------------------------- stage 2: the real service
if [[ $rc == 0 && ${GB_SKIP_SERVICE:-0} != 1 ]]; then
  cargo build -q -p bridge-service --bins
  BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug"
  export GEEKBRIDGE_WP_RESTART=0   # never restart a WirePlumber from here
  export GEEKBRIDGE_STATE_DIR="$SBX/svc" GEEKBRIDGE_CONTROL_PORT=27381 GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_NO_TRAY=1
  mkdir -m 700 "$SBX/svc"
  "$BIN/geekbridge-service" --api-port 27380 --discovery-port 27716 --no-listen-inbound --no-announce --no-tray \
    >"$SBX/log/service.log" 2>&1 & PIDS+=($!)
  ctl() { "$BIN/geekbridge-ctl" "$@"; }
  for _ in $(seq 100); do ctl status >/dev/null 2>&1 && break; sleep 0.1; done
  fail=0
  ok() { echo "  [ok]   $1"; }
  bad() { echo "  [FAIL] $1"; echo "         $2" | head -c 2000; echo; fail=1; }
  expect() { # expect NAME PATTERN COMMAND...
    local name=$1 pat=$2; shift 2
    local out; out=$("$@" 2>&1) || true
    if grep -q -E -- "$pat" <<<"$out"; then ok "$name"; else bad "$name" "$out"; fi
  }
  until_out() { # until_out NAME PATTERN SECONDS COMMAND...
    local name=$1 pat=$2 secs=$3; shift 3
    local out end=$((SECONDS + secs))
    while :; do
      out=$("$@" 2>&1) || true
      grep -q -E -- "$pat" <<<"$out" && { ok "$name"; return; }
      (( SECONDS >= end )) && { bad "$name" "$out"; return; }
      sleep 0.3
    done
  }
  fakephone() {
    busctl --timeout=10 --address="$DBUS_SYSTEM_BUS_ADDRESS" call org.bluez /geekbridge/FakePhone org.geekbridge.FakePhone "$@" >/dev/null \
      || bad "fake phone: $1" "the fake phone did not answer"
  }
  card_profile() { pactl -f json list cards | python3 -I -c 'import json,sys; print(" ".join(c["name"]+"="+c["active_profile"] for c in json.load(sys.stdin)))'; }
  props() { printf '{ "api.bluez5.address": "AA:BB:CC:00:11:22", "api.bluez5.profile": "headset-audio-gateway", "api.bluez5.codec": "msbc", "device.api": "bluez5", "node.name": "%s" }' "$1"; }
  echo "stage 2: geekbridge-service + geekbridge-ctl"
  # The sandbox phone was taken out of range by the last cargo test: bring it back.
  busctl --timeout=10 --address="$DBUS_SYSTEM_BUS_ADDRESS" call org.bluez /org/bluez/hci0/dev_AA_BB_CC_00_11_22 org.bluez.Device1 Connect >/dev/null \
    || bad "reconnect the sandbox phone" "busctl failed"
  until_out "service sees the phone over hands-free" "Bluetooth hands-free \(Linux\): ready" 15 ctl calls
  expect "calling: checklist through the service" "Phone's calls \(hands-free\) profile connected: PipeWire telephony" ctl calling
  expect "calling: audio-gateway card" "\[ok  \] Phone's audio card in PipeWire \(audio-gateway\)" ctl calling
  expect "doctor: hear-calls line" "Hear calls here" ctl doctor
  expect "audio-devices lists the sandbox devices" "sbx_speaker" ctl call audio-devices
  expect "audio-devices refuses a typo" "no output called sbx_typo" ctl call audio-devices --output sbx_typo
  expect "audio-devices picks the speaker" "calls use: Sandbox speaker" ctl call audio-devices --output sbx_speaker
  fakephone Ring s +15550100
  until_out "ringing shown" "^ringing" 10 ctl calls
  expect "answer from the computer" "^ok" ctl call answer
  until_out "answered" "^active" 10 ctl calls
  pw-cat --playback --raw --format f32 --rate 16000 --channels 1 -P "$(props bluez_input.AA_BB_CC_00_11_22.0)" /dev/urandom \
    >/dev/null 2>&1 & VOICE1=$!; PIDS+=($VOICE1)
  pw-cat --record --raw --format f32 --rate 16000 --channels 1 -P "$(props bluez_output.AA_BB_CC_00_11_22.1)" - \
    >/dev/null 2>&1 & VOICE2=$!; PIDS+=($VOICE2)
  until_out "voice routed to the pick" "on this computer \(speaker: sbx_speaker; microphone: sbx_mic" 10 ctl calls
  until_out "mute offered" "^microphone: on" 10 ctl calls
  expect "call mute on" "^ok" ctl call mute on
  until_out "mute shown" "^microphone: muted" 5 ctl calls
  expect "the real microphone is muted" "Mute: yes" pactl get-source-mute sbx_mic
  expect "call audio phone" "^ok" ctl call audio phone
  until_out "card switched off (voice link closed)" "bluez_card.AA_BB_CC_00_11_22=off" 5 card_profile
  kill $VOICE1 $VOICE2 2>/dev/null || true
  fakephone RemoteHangup
  until_out "call over" "^no calls" 10 ctl calls
  until_out "microphone unmuted after the call" "Mute: no" 5 pactl get-source-mute sbx_mic
  until_out "card restored after the call" "bluez_card.AA_BB_CC_00_11_22=audio-gateway" 5 card_profile
  [[ ! -e $SBX/svc/call-audio-restore.json ]] && ok "restore file gone" || bad "restore file gone" "$(cat "$SBX/svc/call-audio-restore.json")"
  # Guided setup of a phone that is not paired yet; the code is confirmed the way a script does it.
  fakephone Forget
  expect "setup starts" "Pairing code 482910|answer with" ctl call setup --phone "Setup Phone"
  expect "code confirmed" "." ctl call setup --confirm yes
  until_out "setup finished and remembered the phone" "Setup Phone" 20 cat "$SBX/svc/call-audio-phone.json"
  expect "fix-config writes nothing when nothing is wrong" "needs no change" ctl call setup --fix-config
  [[ $fail == 0 ]] || rc=1
  echo "stage 2: $([[ $fail == 0 ]] && echo passed || echo FAILED)"
fi
if [[ $rc != 0 ]]; then
  echo "---- fake_bluez.log (tail)"; tail -40 "$SBX/log/fake_bluez.log"
  [[ -f $SBX/log/service.log ]] && { echo "---- service.log (tail)"; tail -20 "$SBX/log/service.log"; }
  echo "---- wireplumber.log (warnings)"; grep -E '^[EW] ' "$SBX/log/wireplumber.log" | grep -v -E 'mod.rt|upower|battery|sco_listen|esco capability' | tail -20
fi
exit $rc
