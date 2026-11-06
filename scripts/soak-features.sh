#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Soak of the real geekbridge-service with the call features active: rings (answer / decline / miss / link drop
# mid-ring), a call during a file transfer, Wi-Fi call audio listen sessions against a private PipeWire, SMS and
# notification bursts, contact / photo / agenda requests, privacy toggles, link drops with jitter, unpair/re-pair.
#
#   SECS=2700 scripts/soak-features.sh [--keep]
#
# Everything is private: scratch HOME and state under $GB_TMP, free ports above 20000, a private PipeWire +
# WirePlumber + pipewire-pulse (null sinks, no BlueZ, no ALSA) with its own runtime dir and D-Bus, a stub
# `geekbridge-calls` beside a copy of the service binary. Never the live service, ~/.config, Hyprland,
# BlueZ or ports 1716 / 17381 / 8000. Driver: crates/bridge-service/tests/soak_real.rs with
# GEEKBRIDGE_SOAK_FEATURES=1; verdict: scripts/soak-fit.py.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
GB_TMP="${GB_TMP:-$HOME/.cache/gb-tmp}"
source "$ROOT/scripts/lib-tmp.sh"
KEEP=0; [[ "${1:-}" == --keep ]] && KEEP=1
SECS="${SECS:-2700}"
TS="$(date +%Y%m%d-%H%M%S)"
OUT="$(gb_mktemp soak-feat -d)"
DIST="$ROOT/dist/soak/features-$TS"
mkdir -p "$DIST"
SBX="$OUT/audio"; mkdir -p -m 700 "$SBX/xdg" "$SBX/tmp" "$SBX/log" \
  "$SBX/home/.config/wireplumber/wireplumber.conf.d" "$SBX/home/.config/pipewire/pipewire.conf.d"
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  wait 2>/dev/null
  ((KEEP)) || rm -rf "$OUT"
}
trap cleanup EXIT
cat > "$SBX/home/.config/wireplumber/wireplumber.conf.d/10-sandbox.conf" <<'EOC'
wireplumber.profiles = { main = { monitor.alsa = disabled monitor.alsa-midi = disabled monitor.v4l2 = disabled
  monitor.libcamera = disabled monitor.bluez-midi = disabled monitor.bluez = disabled support.reserve-device = disabled } }
EOC
cat > "$SBX/home/.config/pipewire/pipewire.conf.d/10-sandbox.conf" <<'EOC'
context.objects = [
  { factory = adapter args = { factory.name = support.null-audio-sink node.name = "sbx_headset_out"
      node.description = "Sandbox headset" media.class = "Audio/Sink" audio.position = [ FL FR ] device.form_factor = headset } }
  { factory = adapter args = { factory.name = support.null-audio-sink node.name = "sbx_mic_feed"
      node.description = "Sandbox microphone feed" media.class = "Audio/Sink" audio.position = [ MONO ] } }
]
EOC
cat > "$SBX/bus.conf" <<EOC
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:dir=$SBX/xdg</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
EOC
(
  export HOME="$SBX/home" XDG_CONFIG_HOME="$SBX/home/.config" XDG_STATE_HOME="$SBX/state" \
    XDG_DATA_HOME="$SBX/home/.local/share" XDG_RUNTIME_DIR="$SBX/xdg" TMPDIR="$SBX/tmp"
  unset PIPEWIRE_REMOTE PIPEWIRE_RUNTIME_DIR DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS DBUS_SYSTEM_BUS_ADDRESS
  dbus-daemon --config-file="$SBX/bus.conf" --print-address --fork --print-pid 3 3>"$SBX/bus.pid" >"$SBX/bus.addr"
) 
export DBUS_SESSION_BUS_ADDRESS; DBUS_SESSION_BUS_ADDRESS="$(head -1 "$SBX/bus.addr")"
PIDS+=("$(cat "$SBX/bus.pid")")
(
  export HOME="$SBX/home" XDG_CONFIG_HOME="$SBX/home/.config" XDG_STATE_HOME="$SBX/state" \
    XDG_DATA_HOME="$SBX/home/.local/share" XDG_RUNTIME_DIR="$SBX/xdg" TMPDIR="$SBX/tmp"
  unset PIPEWIRE_REMOTE PIPEWIRE_RUNTIME_DIR DISPLAY WAYLAND_DISPLAY
  pipewire >"$SBX/log/pipewire.log" 2>&1 & echo $! >"$SBX/pw.pids"
  for _ in $(seq 50); do [[ -S $SBX/xdg/pipewire-0 ]] && break; sleep 0.1; done
  wireplumber >"$SBX/log/wireplumber.log" 2>&1 & echo $! >>"$SBX/pw.pids"
  pipewire-pulse >"$SBX/log/pipewire-pulse.log" 2>&1 & echo $! >>"$SBX/pw.pids"
  export PULSE_SERVER="unix:$SBX/xdg/pulse/native"
  for _ in $(seq 50); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
  for _ in $(seq 50); do pactl list sinks short 2>/dev/null | grep -q sbx_mic_feed && break; sleep 0.1; done
  pactl load-module module-remap-source master=sbx_mic_feed.monitor source_name=sbx_mic >/dev/null 2>&1
  pactl set-default-sink sbx_headset_out; pactl set-default-source sbx_mic
  pactl info | grep -E 'Server Name|Default'
) || true
mapfile -t -O "${#PIDS[@]}" PIDS < "$SBX/pw.pids"
# Optional (SOAK_UPDATE_MOCK=1): a loopback mock release server the service's update checker talks to.
if [[ "${SOAK_UPDATE_MOCK:-0}" == 1 ]]; then
  UPORT="${SOAK_UPDATE_PORT:-27990}"
  head -c 4096 /dev/zero >"$OUT/mock.apk"
  python3 -I scripts/e2e/mock_release_server.py "$UPORT" "$OUT/mock.apk" v99.0.0 >"$SBX/log/mock-update.log" 2>&1 &
  PIDS+=("$!")
  export GEEKBRIDGE_SOAK_UPDATE_URL="http://127.0.0.1:$UPORT"
  sleep 0.5
fi
echo "df before:"; df -h "$GB_TMP" | tail -1
# SOAK_IO=1: three 4 minute windows of two fsyncing dd writers while the control API is pinged every 200 ms.
GEEKBRIDGE_SOAK_IO="${SOAK_IO:-0}" GEEKBRIDGE_SOAK_FEATURES=1 GEEKBRIDGE_SOAK_AUDIO_RUN="$SBX/xdg" GEEKBRIDGE_SOAK_OUT="$OUT/run" GEEKBRIDGE_SOAK_SECS="$SECS" \
  XDG_RUNTIME_DIR= bash -c 'mkdir -p "$GEEKBRIDGE_SOAK_OUT" && exec cargo test --release --locked -p bridge-service --test soak_real -- --ignored --nocapture'
rc=$?
R="$OUT/run"
cp "$R"/*.json "$R"/*.trace "$DIST"/ 2>/dev/null
cp "$R"/samples.csv "$R"/events.tsv "$R"/threads-idle.txt "$R"/service.log "$R"/poll-errors.log "$R"/poll-diag.log \
  "$R"/history.log "$R"/io-stress.log "$R"/extras.txt "$DIST"/ 2>/dev/null
cp "$SBX/log"/*.log "$DIST"/ 2>/dev/null
python3 -I scripts/soak-fit.py "$DIST/samples.csv" --events "$DIST/events.tsv" --poll-errors "$DIST/poll-errors.log" | tee "$DIST/verdict.md"
fit=${PIPESTATUS[0]}
echo "df after:"; df -h "$GB_TMP" | tail -1
echo "driver exit $rc, verdict exit $fit; results in $DIST"
exit $((rc != 0 || fit != 0))
