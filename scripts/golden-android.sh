#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Golden-image (visual regression) test for the Android app in the Nothing x Continuity look, on a headless emulator
# (JVM unit tests cannot render views).
#   scripts/golden-android.sh --check    install the debug APK, render each screen, compare with tests/golden/android/
#   scripts/golden-android.sh --update   overwrite the goldens. ONLY after a visual review (CONTRIBUTING.md "Golden images").
#   ONLY='^status' scripts/golden-android.sh --check    just matching screens
# Screens: the debug-only DesignGalleryActivity pages (status, controls, calendar) and the real Home screen (MainActivity,
# unpaired). Each in light and dark, at font scale 1.0 and 2.0 -> NAME-{light,dark}-fs{10,20}.png (50 %, 256 colours).
# Fixed state: demo mode (clock 12:00, full battery, wifi, no notification icons), animations off, rotation locked,
# en-US, UTC, date pinned to 2026-06-15 (needs `adb root`, which the google_apis image allows), screencaps cropped
# to the app area (status bar and gesture bar removed: GOLDEN_CROP_TOP/GOLDEN_CROP_BOTTOM px, default 140/100).
# AVD for stable pixels: see docs/golden-images.md (google_apis x86_64 android-36, 1080x2400, density 420, en-US).
# Own emulator (console port EMU_PORT, own adb server ANDROID_ADB_SERVER_PORT) started read-only, so the AVD is not modified
# and no other emulator is touched. Env: GOLDEN_AVD (default gbnothing), EMU_PORT (5654), ANDROID_ADB_SERVER_PORT (16058),
# GOLDEN_APK, GOLDEN_BUILD=1 (build the APK first), GOLDEN_KEEP_EMU=1 (leave the emulator running).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
MODE=check
case "${1:-}" in --update) MODE=update ;; --check|"") ;; *) echo "usage: $0 [--update|--check]"; exit 2 ;; esac
source scripts/android-env.sh >/dev/null 2>&1
export ANDROID_ADB_SERVER_PORT="${ANDROID_ADB_SERVER_PORT:-16058}"
AVD="${GOLDEN_AVD:-gbnothing}"; EMU_PORT="${EMU_PORT:-5654}"; SER="emulator-$EMU_PORT"
APK="${GOLDEN_APK:-$ROOT/dist/android/geekbridge-debug.apk}"
GOLDEN_DIR="${GOLDEN_DIR:-$ROOT/tests/golden/android}"
RUN="${RUN:-$ROOT/target/golden-android}"; DIFF="$ROOT/dist/golden-diff"
IMG="python3 -I $ROOT/scripts/golden/golden_img.py"
ADB="adb -s $SER"
python3 -I -c 'import PIL' 2>/dev/null || { echo "golden-android: needs python3 with Pillow"; exit 2; }
rm -rf "$RUN/raw"; mkdir -p "$RUN/raw" "$RUN/tmp" "$GOLDEN_DIR" "$DIFF"
[ "$MODE" = check ] && rm -f "$DIFF"/android-*.png
[ -n "${GOLDEN_BUILD:-}" ] && { scripts/build-android.sh || exit 1; }
[ -f "$APK" ] || { echo "golden-android: no APK at $APK (GOLDEN_BUILD=1 or make android)"; exit 2; }

STARTED=0
cleanup() { [ -z "${GOLDEN_KEEP_EMU:-}" ] && [ "$STARTED" = 1 ] && { $ADB emu kill >/dev/null 2>&1; adb kill-server >/dev/null 2>&1; }; }
trap cleanup EXIT
adb start-server >/dev/null 2>&1
if ! $ADB get-state >/dev/null 2>&1; then
  echo "golden-android: starting $AVD on port $EMU_PORT (read-only)"
  GB_EMU_AVD="$AVD" GB_EMU_PORT="$EMU_PORT" GB_EMU_ADB_PORT="$ANDROID_ADB_SERVER_PORT" GB_EMU_TMPDIR="$RUN/tmp" GB_EMU_EXTRA="-read-only" \
    setsid nohup scripts/emulator-call-launch.sh >"$RUN/emulator.log" 2>&1 &
  STARTED=1
  for _ in $(seq 120); do $ADB get-state >/dev/null 2>&1 && break; sleep 2; done
fi
for _ in $(seq 120); do [ "$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] && break; sleep 2; done
[ "$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] || { echo "golden-android: emulator did not boot (see $RUN/emulator.log)"; exit 1; }

$ADB install -r -t "$APK" >/dev/null || { echo "golden-android: install failed"; exit 1; }
# Locale, time zone and date need root (google_apis image).
$ADB root >/dev/null 2>&1; sleep 2; $ADB wait-for-device
$ADB shell setprop persist.sys.locale en-US; $ADB shell setprop persist.sys.timezone UTC
$ADB shell settings put global auto_time 0; $ADB shell settings put global auto_time_zone 0
$ADB shell date 061512002026.00 >/dev/null 2>&1
$ADB shell settings put system accelerometer_rotation 0; $ADB shell settings put system user_rotation 0
for s in window_animation_scale transition_animation_scale animator_duration_scale; do $ADB shell settings put global $s 0; done
$ADB shell settings put system time_12_24 24
# System UI: demo mode, so the (cropped) bars are fixed anyway.
$ADB shell settings put global sysui_demo_allowed 1
demo() { $ADB shell am broadcast -a com.android.systemui.demo -e command "$@" >/dev/null 2>&1; }
demo enter; demo clock -e hhmm 1200; demo battery -e level 100 -e plugged false; demo network -e wifi show -e level 4; demo notifications -e visible false
# Resolution check: the goldens assume 1080x2400 at density 420.
SIZE=$($ADB shell wm size | tail -1 | sed 's/.*: //' | tr -d '\r'); DENS=$($ADB shell wm density | tail -1 | sed 's/.*: //' | tr -d '\r')
[ "$SIZE" = 1080x2400 ] && [ "$DENS" = 420 ] || echo "golden-android: WARNING $SIZE @ $DENS, goldens expect 1080x2400 @ 420; expect failures"
W=${SIZE%x*}; H=${SIZE#*x}; TOP=${GOLDEN_CROP_TOP:-140}; BOT=${GOLDEN_CROP_BOTTOM:-100}
export GOLDEN_CROP="0,$TOP,$W,$((H-TOP-BOT))"

grab() { # out : screencap until two consecutive frames match
  local prev="$RUN/prev.png" i; : >"$prev"
  for i in 1 2 3 4 5 6; do $ADB exec-out screencap -p >"$1"; cmp -s "$1" "$prev" && break; cp "$1" "$prev"; sleep 0.8; done
}
FAILED=0
shoot() { # name am-start-args...
  local name="$1"; shift
  for night in no yes; do for fs in 1.0 2.0; do
    local theme=light; [ $night = yes ] && theme=dark
    local f="$name-$theme-fs${fs/./}"
    [ -n "${ONLY:-}" ] && ! [[ "$f" =~ $ONLY ]] && continue
    $ADB shell cmd uimode night $night >/dev/null; $ADB shell settings put system font_scale $fs
    $ADB shell am force-stop dev.geekbridge.android; sleep 1
    $ADB shell am start -W -n "$@" >/dev/null 2>&1 || { echo "golden-android: cannot start for $f"; FAILED=1; continue; }
    sleep 3; grab "$RUN/raw/$f.png"
  done; done
}
shoot status dev.geekbridge.android/.DesignGalleryActivity --es page status
shoot controls dev.geekbridge.android/.DesignGalleryActivity --es page controls
shoot calendar dev.geekbridge.android/.DesignGalleryActivity --es page calendar
shoot home dev.geekbridge.android/.MainActivity
demo exit; $ADB shell cmd uimode night no >/dev/null; $ADB shell settings put system font_scale 1.0

n=0; bad=0
for raw in "$RUN"/raw/*.png; do
  [ -s "$raw" ] || continue; name=$(basename "$raw"); n=$((n+1))
  if [ "$MODE" = update ]; then $IMG reduce "$raw" "$GOLDEN_DIR/$name" || FAILED=1; continue; fi
  if [ ! -f "$GOLDEN_DIR/$name" ]; then echo "NEW   $name (no golden; run --update after reviewing)"; bad=$((bad+1)); continue; fi
  if out=$($IMG compare "$raw" "$GOLDEN_DIR/$name" "$DIFF/android-$name"); then echo "ok    $name"; else echo "FAIL  $name: $out"; bad=$((bad+1)); fi
done
if [ "$MODE" = update ]; then echo "updated $n goldens in $GOLDEN_DIR ($(du -sk "$GOLDEN_DIR" | cut -f1) KiB): review with git diff before committing"; exit "$FAILED"; fi
echo "golden-android: $n screens, $bad failing; diffs in dist/golden-diff/"
[ "$bad" -eq 0 ] && [ "$FAILED" -eq 0 ]
