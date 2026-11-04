#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# EXPERIMENT: call audio over Wi-Fi through the shell-uid helper, on emulators (docs/design/call-audio-over-wifi.md,
# results in docs/reports/call-audio-wifi-prototype.md). For each image it:
#   1. boots its own AVD (own name, console port, adb server port, state dir; never the default adb server),
#   2. installs the debug APK, records the build type and which call-audio permissions the shell package holds,
#   3. probes every call source and uplink route from the helper (app_process as uid 2000) with no call, then
#      during an emulated call (adb emu gsm call/accept), and as root where `adb root` works (upper bound),
#   4. runs the chain helper -> adb forward -> geekbridge-callaudio-lab (Opus, real GeekBridge TLS link code,
#      jitter buffer) -> decoded PCM, with a tone as the far party and with VOICE_DOWNLINK, and the computer's tone
#      sent back for injection through the VoIP AudioPolicy mix; then again through the chaos proxy,
#   5. tries to switch on Wireless debugging from the shell and records what the emulator does,
#   6. stops the emulator and deletes the AVD (GB_CA_KEEP=1 keeps it).
# Prints PASS/FAIL/INFO rows; evidence (JSON, logs; never audio) under dist/android/logs/call-audio-emulator/.
#
#   scripts/e2e/call-audio-emulator.sh                 all images in GB_CA_IMAGES
#   GB_CA_IMAGES="36g" scripts/e2e/call-audio-emulator.sh
#
# Images (must be installed in the project-local SDK):
#   36g  system-images;android-36;google_apis;x86_64            (userdebug-like: adb root works; upper bound)
#   36p  system-images;android-36;google_apis_playstore;x86_64  (user build: production-like; lower bound)
#   34p  system-images;android-34;google_apis_playstore;x86_64
# Needs: a debug APK (android/app/build/outputs/apk/debug/app-debug.apk) and the lab binary (built here).
# TMPDIR and every scratch file live under $GB_CA_SCRATCH (default ~/.cache/gb-callaudio-emu), never /tmp.
# The emulator runs with -no-audio: its voice path carries no audio, so call sources can only show *whether*
# capture is allowed, not real call content.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=/dev/null
source "$ROOT/scripts/android-env.sh" >/dev/null 2>&1 || true
SCRATCH="${GB_CA_SCRATCH:-$HOME/.cache/gb-callaudio-emu}"
export TMPDIR="$SCRATCH/tmp" ANDROID_TMP="$SCRATCH/tmp"
mkdir -p "$TMPDIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="$ROOT/dist/android/logs/call-audio-emulator/$STAMP"
mkdir -p "$OUT"
RESULTS="$OUT/results.tsv"
: > "$RESULTS"
APK="${GB_CA_APK:-$ROOT/android/app/build/outputs/apk/debug/app-debug.apk}"
PKG=dev.geekbridge.android
HELPER=dev.geekbridge.android.callaudio.CallAudioHelper
IMAGES="${GB_CA_IMAGES:-36g 36p}"
# A fresh AVD on a busy machine can take far longer than the default 420 s to boot.
export GB_EMU_BOOT_TIMEOUT="${GB_EMU_BOOT_TIMEOUT:-1500}"
fails=0

row() { # STATUS IMAGE CHECK DETAIL
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" | tee -a "$RESULTS"
  [ "$1" = FAIL ] && fails=$((fails + 1))
  return 0
}

image_of() {
  case "$1" in
    36g) echo "system-images;android-36;google_apis;x86_64" ;;
    36p) echo "system-images;android-36;google_apis_playstore;x86_64" ;;
    34p) echo "system-images;android-34;google_apis_playstore;x86_64" ;;
    *) return 1 ;;
  esac
}

# ---- build the lab (standalone crate, own target dir) -------------------------------------------
LAB_TARGET="$SCRATCH/target"
if ! (cd "$ROOT/experiments/callaudio-wifi" && CARGO_TARGET_DIR="$LAB_TARGET" cargo build --release --quiet) >"$OUT/lab-build.log" 2>&1; then
  row FAIL host lab-build "see $OUT/lab-build.log"; exit 1
fi
LAB="$LAB_TARGET/release/geekbridge-callaudio-lab"
[ -r "$APK" ] || { row FAIL host apk "missing $APK (cd android && ./gradlew :app:assembleDebug)"; exit 1; }

json() { python3 -I -c "import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2], {'d': d}))" "$@" 2>/dev/null; }

n=0
for tag in $IMAGES; do
  n=$((n + 1))
  image="$(image_of "$tag")" || { row FAIL "$tag" image "unknown tag"; continue; }
  export GB_EMU_AVD="gbca$tag" GB_EMU_SERIAL="emulator-$((5650 + 2 * n))" GB_EMU_ADB_PORT="$((16140 + n))" \
    GB_EMU_STATE_DIR="$SCRATCH/emu-$tag" GB_EMU_UNIT="gbca-emu-$tag" GB_EMULATOR_IMAGE="$image" GB_EMU_TMPDIR="$SCRATCH/tmp"
  E="$ROOT/scripts/emulator-call.sh"
  adbe() { "$E" adb "$@"; }
  sh() { adbe shell "$@" 2>&1 | tr -d '\r'; }
  log="$OUT/$tag"
  mkdir -p "$log"
  # Start the dedicated adb server ourselves: one spawned inside emulator-call.sh inherits its lock fd and would
  # block the next `start` on the same state dir.
  adb -P "$GB_EMU_ADB_PORT" start-server >/dev/null 2>&1
  # emulator-call.sh re-sources android-env.sh, which resets GB_EMULATOR_IMAGE; create the AVD here so each tag
  # really gets its own image (an existing AVD is used as it is).
  avdhome="${ANDROID_AVD_HOME:-$ROOT/.tools/android-user/avd}"
  if [ ! -d "$avdhome/$GB_EMU_AVD.avd" ]; then
    printf 'no\n' | "$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager" create avd -n "$GB_EMU_AVD" -k "$image" -d pixel_6 >"$log/create-avd.log" 2>&1 \
      || { row FAIL "$tag" create-avd "see $log/create-avd.log"; continue; }
    printf 'hw.ramSize=2G\nhw.cpu.ncore=4\nshowDeviceFrame=no\n' >> "$avdhome/$GB_EMU_AVD.avd/config.ini"
  fi
  row INFO "$tag" image "$(grep -m1 '^image.sysdir' "$avdhome/$GB_EMU_AVD.avd/config.ini" | cut -d= -f2)"
  if ! "$E" start >"$log/emulator-start.log" 2>&1; then
    row FAIL "$tag" boot "see $log/emulator-start.log"; "$E" stop >/dev/null 2>&1; systemctl --user stop "$GB_EMU_UNIT" >/dev/null 2>&1; continue
  fi
  build="$(sh getprop ro.build.type) sdk=$(sh getprop ro.build.version.sdk)"
  row INFO "$tag" build "$build"
  if adbe install -r -t -g "$APK" >"$log/install.log" 2>&1; then row PASS "$tag" install ok; else row FAIL "$tag" install "see $log/install.log"; "$E" stop >/dev/null 2>&1; continue; fi

  # Which call-audio permissions does the shell package hold on this image?
  sh dumpsys package com.android.shell > "$log/shell-package.txt"
  for p in CAPTURE_AUDIO_OUTPUT CAPTURE_VOICE_COMMUNICATION_OUTPUT MODIFY_AUDIO_ROUTING MODIFY_PHONE_STATE CALL_AUDIO_INTERCEPTION RECORD_AUDIO; do
    if grep -q "android.permission.$p: granted=true" "$log/shell-package.txt"; then row INFO "$tag" "shell:$p" granted; else row INFO "$tag" "shell:$p" "not granted"; fi
  done
  # Does pm grant hand CAPTURE_AUDIO_OUTPUT to the app? (expected: no; Android's own message is the evidence)
  row INFO "$tag" "pm-grant-app" "$(sh "pm grant $PKG android.permission.CAPTURE_AUDIO_OUTPUT 2>&1; echo exit=\$?" | grep -v '^\s*at ' | head -2 | tr '\n' ' ' | cut -c1-160)"
  sh dumpsys media.audio_policy > "$log/audio-policy.txt"
  if grep -qi "telephony tx\|AUDIO_DEVICE_OUT_TELEPHONY_TX" "$log/audio-policy.txt"; then row INFO "$tag" telephony-tx-in-policy present; else row INFO "$tag" telephony-tx-in-policy absent; fi

  CP="CLASSPATH=\$(pm path $PKG | sed s/^package://)"
  probe() { # NAME
    sh "$CP app_process /system/bin $HELPER --probe" > "$log/probe-$1.raw"
    grep '^{"v"' "$log/probe-$1.raw" | tail -1 > "$log/probe-$1.json"
    if json "$log/probe-$1.json" "d['uid']" >/dev/null; then
      row PASS "$tag" "probe-$1" "uid=$(json "$log/probe-$1.json" "d['uid']") mode=$(json "$log/probe-$1.json" "d['audioMode']")"
      for s in VOICE_DOWNLINK VOICE_UPLINK VOICE_CALL REMOTE_SUBMIX MIC VOICE_COMMUNICATION DOWNLINK_EXTRACTION; do
        row INFO "$tag" "probe-$1:$s" "$(json "$log/probe-$1.json" "[ (x['outcome'], x.get('error',''), 'rms=%s hz=%s' % (x.get('rms',''), x.get('hz',''))) for x in d['sources'] if x['source']=='$s'][0]")"
      done
      row INFO "$tag" "probe-$1:uplink-injection-api" "$(json "$log/probe-$1.json" "d['uplinkInjectionTrack']")"
      row INFO "$tag" "probe-$1:pstn-interceptable" "$(json "$log/probe-$1.json" "d['pstnInterceptable']")"
      row INFO "$tag" "probe-$1:telephony-tx-device" "$(json "$log/probe-$1.json" "(d['telephonyTxDevice'], d['telephonyTxNote'])")"
      row INFO "$tag" "probe-$1:uplink-loop" "$(json "$log/probe-$1.json" "d.get('uplinkLoop')")"
      row INFO "$tag" "probe-$1:voip-mix" "$(json "$log/probe-$1.json" "d['voipMix']")"
    else
      row FAIL "$tag" "probe-$1" "$(head -c 300 "$log/probe-$1.raw" | tr '\n\t' '  ')"
    fi
  }

  probe idle-shell
  adbe emu gsm call 5551230 >/dev/null 2>&1; sleep 3
  adbe emu gsm accept 5551230 >/dev/null 2>&1; sleep 3
  # `gsm list` prints nothing useful on recent emulators; the telephony registry is the truth (2 = off hook).
  call="$(sh dumpsys telephony.registry | grep -m1 -o 'mCallState=[0-9]')"
  if [ "$call" = "mCallState=2" ]; then row PASS "$tag" emulated-call "$call (off hook)"; else row FAIL "$tag" emulated-call "${call:-no call state}"; fi
  sh dumpsys telecom > "$log/telecom.txt"
  # Telecom switches the audio mode to IN_CALL a moment after the call is off hook; the interception API only
  # answers in IN_CALL. Wait for it (up to 30 s) and say what it was.
  for _ in $(seq 1 30); do sh dumpsys audio | grep -qE 'mode.*(IN_CALL|MODE_IN_CALL)' && break; sleep 1; done
  row INFO "$tag" audio-mode "$(sh dumpsys audio | grep -m1 -oE 'mode[^,]*(IN_CALL|NORMAL|RINGTONE|IN_COMMUNICATION)[^,]*' | cut -c1-80)"
  probe incall-shell

  # Chain: helper -> adb forward -> lab (Opus over the real TLS link code) -> decoded PCM, injection back.
  name="gbca$RANDOM$RANDOM"; token="t$(od -An -N8 -tx8 /dev/urandom | tr -d ' ')"
  adbe shell "$CP app_process /system/bin $HELPER --listen $name --token $token" > "$log/helper-listen.log" 2>&1 &
  hpid=$!
  for _ in $(seq 1 60); do grep -q '^listening' "$log/helper-listen.log" 2>/dev/null && break; sleep 1; done
  port="$(adbe forward tcp:0 "localabstract:$name" 2>/dev/null | tr -d '\r\n')"
  if [ -z "$port" ]; then row FAIL "$tag" helper-forward "no forward"; else
    for spec in TONE:voip_mix VOICE_DOWNLINK:telephony_tx DOWNLINK_EXTRACTION:telephony_tx; do
      src="${spec%%:*}"; inj="${spec#*:}"
      "$LAB" --seconds 6 --helper "127.0.0.1:$port" --source "$src" --token "$token" --inject "$inj" --up-tone 1000 > "$log/chain-$src.json" 2> "$log/chain-$src.err"
      hz="$(json "$log/chain-$src.json" "d['down']['dominantHz']")"; played="$(json "$log/chain-$src.json" "d['down']['played']")"
      lat="$(json "$log/chain-$src.json" "d['down']['latencyMs']")"; st="$(json "$log/chain-$src.json" "d['helperStatus'][-1] if d['helperStatus'] else ''")"
      if [ "$src" = TONE ]; then
        if [ "${hz%.*}" = 440 ] && [ "${played:-0}" -gt 150 ]; then row PASS "$tag" chain-tone "440 Hz far-end tone through helper+Opus+TLS+jitter, played=$played latency=$lat"; else row FAIL "$tag" chain-tone "hz=$hz played=$played"; fi
        row INFO "$tag" chain-tone-helper-status "$st"
        row INFO "$tag" chain-tone-up "$(json "$log/chain-$src.json" "d['up']['latencyMs'], d['up']['dominantHz']")"
      else
        row INFO "$tag" "chain-$src" "played=$played hz=$hz rms=$(json "$log/chain-$src.json" "d['down']['rms']") latency=$lat helper=$st"
      fi
    done
    "$LAB" --seconds 8 --helper "127.0.0.1:$port" --source TONE --token "$token" --inject voip_mix --latency-ms 40 --loss 0.03 --loss-penalty-ms 120 > "$log/chain-chaos.json" 2>&1
    row INFO "$tag" chain-chaos "$(json "$log/chain-chaos.json" "(d['down']['dominantHz'], d['down']['latencyMs'], d['down']['jitterBuffer'])")"
    adbe forward --remove "tcp:$port" >/dev/null 2>&1
  fi
  kill "$hpid" 2>/dev/null; wait "$hpid" 2>/dev/null
  for p in $(sh ps -A -o PID,ARGS | awk '/app_process .*CallAudioHelper/{print $1}'); do sh kill "$p" >/dev/null; done

  # Upper bound: the same probe as root where the image allows it.
  if adbe root 2>&1 | grep -qiE "restarting|already running as root"; then
    sleep 4; adbe wait-for-device
    probe incall-root
    adbe unroot >/dev/null 2>&1; sleep 4; adbe wait-for-device
  else
    row INFO "$tag" adb-root "not available (user build)"
  fi
  adbe emu gsm cancel 5551230 >/dev/null 2>&1

  # Wireless debugging from the shell (what the self-ADB bootstrap would rely on).
  sh settings put global adb_wifi_enabled 1 >/dev/null; sleep 4
  row INFO "$tag" wireless-debugging "adb_wifi_enabled=$(sh settings get global adb_wifi_enabled) tls_port=$(sh getprop service.adb.tls.port)"
  sh settings put global adb_wifi_enabled 0 >/dev/null

  "$E" stop > "$log/emulator-stop.log" 2>&1
  # Belt and braces: the transient unit is ours by name; stopping it ends this emulator only.
  systemctl --user stop "$GB_EMU_UNIT" >/dev/null 2>&1
  adb -P "$GB_EMU_ADB_PORT" kill-server >/dev/null 2>&1
  if [ "${GB_CA_KEEP:-0}" != 1 ]; then
    rm -rf -- "$avdhome/gbca$tag.avd" "$avdhome/gbca$tag.ini" "$SCRATCH/emu-$tag" 2>/dev/null
  fi
done
rm -rf -- "$SCRATCH/tmp"
echo "results: $RESULTS"
[ "$fails" -eq 0 ]
