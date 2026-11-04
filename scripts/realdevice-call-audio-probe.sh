#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Guided, owner-run probe for "can a normal GeekBridge app read the downlink of a real cellular call?"
# (docs/reports/call-audio-over-wifi-feasibility.md). EXPERIMENT; DEBUG APK ONLY; nothing is sent anywhere.
#
#   scripts/realdevice-call-audio-probe.sh [--serial SERIAL] [--device-id ID] [--control-port PORT]
#                                          [--bin-dir DIR] [--yes] [--no-scrcpy]
#
# Steps it walks you through: 1. Wireless debugging (or USB debugging) on, `adb connect`; 2. grant
# RECORD_AUDIO and try `pm grant ... CAPTURE_AUDIO_OUTPUT` (expected to FAIL for a normal app: it is a
# signature/privileged permission; the script prints Android's own words); 3. arm the hidden
# `call_audio_experiment` switch; 4. you place or answer a call (iPhone -> this phone) and press Enter;
# 5. the app records VOICE_DOWNLINK for 5 s and the result code is read back with `geekbridge-ctl phone-state`
# (SECURITY_EXCEPTION | ILLEGAL_STATE | SILENCED_BY_POLICY | SILENT (rms=0) | AUDIO_PRESENT).
# 6. If scrcpy is installed it also tries the shell-uid route (`scrcpy --audio-source=voice-call-downlink`,
# the same hidden-API route Shizuku call recorders use), 5 s, and reports whether the recording has signal.
#
# Writes nothing outside ~/.cache/geekbridge-call-audio-probe/. Never touches the live service or config;
# phone-state needs a running desktop service that is paired with the phone (skipped with a note otherwise).
# Do not claim the route works unless a line says AUDIO_PRESENT (or the scrcpy file has signal).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ADB="${GEEKBRIDGE_ADB:-adb}"
CTL="${GEEKBRIDGE_BIN_DIR:-$ROOT/target/release}/geekbridge-ctl"
PORT="${GEEKBRIDGE_CONTROL_PORT:-17381}"
SERIAL="${ANDROID_SERIAL:-}" DEVID="" YES=0 SCRCPY=1
PKG=dev.geekbridge.android
OUT="${HOME}/.cache/geekbridge-call-audio-probe"

usage() { sed -n '3,21p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
while (($#)); do
  case "$1" in
    --serial) shift; SERIAL="${1:?--serial needs a value}" ;;
    --device-id) shift; DEVID="${1:?--device-id needs a value}" ;;
    --control-port) shift; PORT="${1:?--control-port needs a value}" ;;
    --bin-dir) shift; CTL="${1:?--bin-dir needs a value}/geekbridge-ctl" ;;
    --yes) YES=1 ;;
    --no-scrcpy) SCRCPY=0 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

command -v "$ADB" >/dev/null 2>&1 || { echo "adb not found (set GEEKBRIDGE_ADB)" >&2; exit 2; }
a() { if [ -n "$SERIAL" ]; then "$ADB" -s "$SERIAL" "$@"; else "$ADB" "$@"; fi; }
pause() { ((YES)) || read -r -p "$1 [Enter] " _; }
mkdir -p "$OUT"

cat <<'TXT'
== GeekBridge call-audio probe (experiment) ==
You need a DEBUG build of GeekBridge on the phone (the release app has no probe).
1. On the phone: Settings > Developer options > Wireless debugging ON (or plug in USB with USB debugging).
   Wireless: tap "Pair device with pairing code", then on this computer:  adb pair HOST:PORT  and  adb connect HOST:PORT
TXT
pause "Done?"
n=$(a devices | awk 'NR>1 && $2=="device"' | wc -l)
[ "$n" -ge 1 ] || { echo "no adb device in state 'device'. Run: adb devices" >&2; exit 1; }
[ "$n" -eq 1 ] || [ -n "$SERIAL" ] || { echo "several devices: pass --serial" >&2; exit 1; }
a shell pm path "$PKG" >/dev/null 2>&1 || { echo "$PKG is not installed" >&2; exit 1; }
echo "device: $(a shell getprop ro.product.manufacturer | tr -d '\r') $(a shell getprop ro.product.model | tr -d '\r'), Android $(a shell getprop ro.build.version.release | tr -d '\r') (SDK $(a shell getprop ro.build.version.sdk | tr -d '\r'))"

echo "-- 2. grants"
a shell pm grant "$PKG" android.permission.RECORD_AUDIO && echo "RECORD_AUDIO: granted"
g=$(a shell pm grant "$PKG" android.permission.CAPTURE_AUDIO_OUTPUT 2>&1 | tr -d '\r')
if [ -z "$g" ]; then
  echo "CAPTURE_AUDIO_OUTPUT: pm grant said nothing (success?). Verify below with the probe, do not assume."
else
  echo "CAPTURE_AUDIO_OUTPUT: pm grant FAILED, as expected for a normal app (signature permission):"
  echo "  $g"
fi
a shell dumpsys package "$PKG" | grep -i "CAPTURE_AUDIO_OUTPUT" | head -2 || true

echo "-- 3. arm the hidden switch"
a shell am broadcast -n "$PKG/.DebugControlReceiver" -a dev.geekbridge.debug.SET --es what call_audio_experiment --ez on true >/dev/null
a shell am start -n "$PKG/.HomeActivity" >/dev/null 2>&1 || true   # a foreground app is not silenced by the capture policy

echo "-- 3b. control: the same probe with NO call (an emulator returned loud synthetic audio with no call, so compare)"
a shell am broadcast -n "$PKG/.DebugControlReceiver" -a dev.geekbridge.debug.SET --es what call_audio_probe --ez on true >/dev/null
sleep 8
a logcat -d -s GB 2>/dev/null | grep -i "call audio probe" | tail -1

echo "-- 4. the call"
pause "Place or answer a CELLULAR call now (iPhone -> this phone), get a far-end voice talking, then"
SC_PID=""
if ((SCRCPY)) && command -v scrcpy >/dev/null 2>&1; then
  rm -f "$OUT/scrcpy-downlink.opus"
  timeout 12 scrcpy ${SERIAL:+-s "$SERIAL"} --no-video --no-playback --audio-source=voice-call-downlink \
    --record="$OUT/scrcpy-downlink.opus" >"$OUT/scrcpy.log" 2>&1 &
  SC_PID=$!
  sleep 3
fi
a shell am broadcast -n "$PKG/.DebugControlReceiver" -a dev.geekbridge.debug.SET --es what call_audio_probe --ez on true >/dev/null
echo "probing for 5 s... keep the far end talking"
sleep 8
[ -n "$SC_PID" ] && wait "$SC_PID" 2>/dev/null

echo "-- 5. result from the phone's debug state dump"
if [ -x "$CTL" ] && [ -n "$DEVID" ]; then
  GEEKBRIDGE_CONTROL_PORT="$PORT" "$CTL" phone-state "$DEVID" 2>&1 | grep -E "call_audio_experiment|call audio probe|process importance" || echo "(probe lines not found in the dump)"
else
  echo "(skipped: pass --device-id ID with a built geekbridge-ctl; reading the app log instead)"
  a logcat -d -s GB 2>/dev/null | grep -i "call audio probe" | tail -3
fi

if [ -n "$SC_PID" ]; then
  echo "-- 6. scrcpy shell-uid route"
  tail -3 "$OUT/scrcpy.log"
  if [ -s "$OUT/scrcpy-downlink.opus" ]; then
    echo "recording written: $(wc -c <"$OUT/scrcpy-downlink.opus") bytes. LISTEN to it: a non-empty file can still be silence."
  else
    echo "no recording: the shell-uid route did not produce a file (see scrcpy.log)"
  fi
fi

echo "-- 7. disarm"
a shell am broadcast -n "$PKG/.DebugControlReceiver" -a dev.geekbridge.debug.SET --es what call_audio_experiment --ez on false >/dev/null
echo "Done. Expected for a normal app: SECURITY_EXCEPTION or ILLEGAL_STATE. Only AUDIO_PRESENT means the route is open."
echo "Send the result lines above (no recording) with your notes; hang up the call."
