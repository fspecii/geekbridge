#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Golden-image (visual regression) test for the Android app in the Nothing x Continuity look, on a headless emulator
# (JVM unit tests cannot render views).
#   scripts/golden-android.sh --check    install the debug APK, render each screen, compare with tests/golden/android/
#   scripts/golden-android.sh --update   overwrite the goldens. ONLY after a visual review (CONTRIBUTING.md "Golden images").
#   ONLY='^status' scripts/golden-android.sh --check    just matching screens
# Screens: the debug-only DesignGalleryActivity pages (status, controls, calendar, shareoff, shareon, dial: the Share-with-computer and call
# switches), the real Home screen (MainActivity, unpaired) and, from the Android visual QA, the screens added later: g-scan* (camera
# permission ask / blocked / no camera, the "Link to <computer>?" page for a scanned and an opened link), g-pairdlg* (the "Link with
# <computer>?" dialogs), g-updates* (App updates card, dev-channel warning), g-diagstate (debug state with link health), g-msgson
# (Share with computer, every switch on), and the real ComputerLinkActivity (invite) and SetupActivity (setup-pair, -notifications,
# -answer, -answer-bt, -battery, -sms) and MainActivity (diag, scrolled). The first group is shot in light and dark at font scale 1.0 and
# 2.0 -> NAME-{light,dark}-fs{10,20}.png (50 %, 256 colours); the later group at 1.0 and 1.5 -> -fs{10,15}, and a long screen in
# pages NAME, NAME-p1, NAME-p2... (each page scrolls 85 % of the screen height further: debug-only DebugScroll, `--ei gb_scroll N`).
# Fixed state: demo mode (clock 12:00, full battery, wifi, no notification icons), animations off, rotation locked,
# en-US, UTC, date pinned to 2026-06-15 (needs `adb root`, which the google_apis image allows), screencaps cropped
# to the app area (status bar and gesture bar removed: GOLDEN_CROP_TOP/GOLDEN_CROP_BOTTOM px, default 140/100).
# AVD for stable pixels: see docs/golden-images.md (google_apis x86_64 android-36, 1080x2400, density 420, en-US).
# Own emulator (free console port, own adb server on a free port, both chosen at run time; EMU_PORT / ANDROID_ADB_SERVER_PORT override) started read-only, so the AVD is not modified
# and no other emulator is touched. Env: GOLDEN_AVD (default gbnothing),
# GOLDEN_TIMEOUT (seconds, default 600), GOLDEN_APK, GOLDEN_BUILD=1 (build the APK first), GOLDEN_KEEP_EMU=1 (leave the emulator running).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
MODE=check
case "${1:-}" in --update) MODE=update ;; --check|"") ;; *) echo "usage: $0 [--update|--check]"; exit 2 ;; esac
source scripts/android-env.sh >/dev/null 2>&1
# Re-run ourselves in our own process group under `timeout` (default 600 s, emulator boot included): on expiry the whole
# group, emulator included, is killed.
if [ -z "${GOLDEN_INNER:-}" ]; then GOLDEN_INNER=1 exec timeout -k 10 "${GOLDEN_TIMEOUT:-600}" "$0" "$@"; fi
# Ports are chosen free at run time: the adb server on any free port, the emulator on a free even console port (and +1).
free_tcp() { python3 -I -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
free_console() { python3 -I - <<'PY'
import socket, random
ports = list(range(5556, 5680, 2)); random.shuffle(ports)
for p in ports:
    try:
        a = socket.socket(); a.bind(("127.0.0.1", p)); b = socket.socket(); b.bind(("127.0.0.1", p + 1)); print(p); break
    except OSError:
        pass
PY
}
export ANDROID_ADB_SERVER_PORT="${ANDROID_ADB_SERVER_PORT:-$(free_tcp)}"
AVD="${GOLDEN_AVD:-gbnothing}"; EMU_PORT="${EMU_PORT:-$(free_console)}"; SER="emulator-$EMU_PORT"
[ -n "$EMU_PORT" ] || { echo "golden-android: no free emulator console port"; exit 1; }
APK="${GOLDEN_APK:-$ROOT/dist/android/geekbridge-debug.apk}"
GOLDEN_DIR="${GOLDEN_DIR:-$ROOT/tests/golden/android}"
RUN="${RUN:-${GOLDEN_SCRATCH:-$HOME/.cache/gb-golden}/android}"; DIFF="$ROOT/dist/golden-diff"
IMG="python3 -I $ROOT/scripts/golden/golden_img.py"
ADB="timeout 60 adb -s $SER"
python3 -I -c 'import PIL' 2>/dev/null || { echo "golden-android: needs python3 with Pillow"; exit 2; }
[ -r "$ROOT/scripts/lib/scratch.sh" ] && { . "$ROOT/scripts/lib/scratch.sh"; gb_preflight "$ROOT/.tools" || exit 1; }   # GB_MIN_FREE_GB, default 15
rm -rf "$RUN/raw"; mkdir -p "$RUN/raw" "$RUN/tmp" "$GOLDEN_DIR" "$DIFF"
[ "$MODE" = check ] && rm -f "$DIFF"/android-*.png
[ -n "${GOLDEN_BUILD:-}" ] && { scripts/build-android.sh || exit 1; }
[ -f "$APK" ] || { echo "golden-android: no APK at $APK (GOLDEN_BUILD=1 or make android)"; exit 2; }

STARTED=0; EMU=""
die() { echo "golden-android: $*"; exit 1; }
cleanup() {
  trap - EXIT INT TERM
  if [ -z "${GOLDEN_KEEP_EMU:-}" ] && [ "$STARTED" = 1 ]; then
    $ADB emu kill >/dev/null 2>&1; [ -n "$EMU" ] && kill "$EMU" 2>/dev/null; pkill -P $$ 2>/dev/null; timeout 10 adb kill-server >/dev/null 2>&1
  fi
}
trap cleanup EXIT; trap 'exit 130' INT; trap 'exit 143' TERM
timeout 30 adb start-server >/dev/null 2>&1
if ! $ADB get-state >/dev/null 2>&1; then
  echo "golden-android: starting $AVD on port $EMU_PORT (read-only)"
  GB_EMU_AVD="$AVD" GB_EMU_PORT="$EMU_PORT" GB_EMU_ADB_PORT="$ANDROID_ADB_SERVER_PORT" GB_EMU_TMPDIR="$RUN/tmp" GB_EMU_EXTRA="-read-only" \
    scripts/emulator-call-launch.sh >"$RUN/emulator.log" 2>&1 &
  EMU=$!; STARTED=1
  for _ in $(seq 90); do
    $ADB get-state >/dev/null 2>&1 && break
    kill -0 $EMU 2>/dev/null || { tail -n 15 "$RUN/emulator.log"; die "emulator exited during start"; }; sleep 2
  done
fi
for _ in $(seq 120); do
  [ "$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] && break
  [ -z "$EMU" ] || kill -0 $EMU 2>/dev/null || { tail -n 15 "$RUN/emulator.log"; die "emulator died while booting"; }; sleep 2
done
[ "$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] || { tail -n 15 "$RUN/emulator.log"; die "emulator did not boot in time (see $RUN/emulator.log)"; }

timeout 180 adb -s "$SER" install -r -t "$APK" >/dev/null || die "install failed"
# Locale, time zone and date need root (google_apis image).
$ADB root >/dev/null 2>&1; sleep 2; timeout 60 adb -s "$SER" wait-for-device
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
  local same=0
  for i in $(seq 15); do
    $ADB exec-out screencap -p >"$1" || return 1
    if cmp -s "$1" "$prev"; then same=$((same+1)); [ $same -ge 2 ] && break; else same=0; cp "$1" "$prev"; fi
    sleep 0.8
  done
}
FAILED=0
# shoot NAME START-ARGS...: light and dark at the font scales in $FS (default "1.0 2.0"), pages 0..$PAGES-1 of a long screen
# (page k scrolls the screen k times 85 % of its height: debug-only DebugScroll; a page that looks like the one before it is dropped).
FS="1.0 2.0"; PAGES=1
shoot() {
  local name="$1"; shift
  for night in no yes; do for fs in $FS; do
    local theme=light prev=""; [ $night = yes ] && theme=dark
    $ADB shell cmd uimode night $night >/dev/null; $ADB shell settings put system font_scale $fs
    for pg in $(seq 0 $((PAGES-1))); do
      local pn="$name"; [ $pg -gt 0 ] && pn="$name-p$pg"
      local f="$pn-$theme-fs${fs/./}"
      [ -n "${ONLY:-}" ] && ! [[ "$f" =~ $ONLY ]] && continue
      local extra=(); [ $pg -gt 0 ] && extra=(--ei gb_scroll $pg)
      $ADB shell am force-stop dev.geekbridge.android; sleep 1
      $ADB shell am start -W -n "$@" "${extra[@]}" >/dev/null 2>&1 || { echo "golden-android: cannot start for $f"; FAILED=1; continue; }
      sleep 3; [ $pg -gt 0 ] && sleep 2
      grab "$RUN/raw/$f.png" || { echo "golden-android: screencap failed for $f"; FAILED=1; }
      if [ -n "$prev" ] && $IMG same "$RUN/raw/$f.png" "$prev"; then rm -f "$RUN/raw/$f.png"; break; fi
      cp "$RUN/raw/$f.png" "$RUN/prevpage.png"; prev="$RUN/prevpage.png"
    done
  done; done
}
G=dev.geekbridge.android
shoot status $G/.DesignGalleryActivity --es page status
shoot controls $G/.DesignGalleryActivity --es page controls
shoot calendar $G/.DesignGalleryActivity --es page calendar
shoot shareoff $G/.DesignGalleryActivity --es page shareoff
shoot shareon $G/.DesignGalleryActivity --es page shareon
shoot dial $G/.DesignGalleryActivity --es page dial
shoot home $G/.MainActivity
# The screens added after the first goldens: font scales 1.0 and 1.5, long screens in several pages. They start from the
# app's first-run state (no saved settings, no permission, nothing asked before).
$ADB shell pm clear $G >/dev/null 2>&1
FS="1.0 1.5"
for pg in scanask scanblocked scannocamera scanconfirm scanconfirmlink pairdlgqr pairdlgcode updatesdev updateswarn diagstate; do
  PAGES=1; shoot "g-$pg" $G/.DesignGalleryActivity --es page $pg
done
for pg in updates updatesavail; do PAGES=3; shoot "g-$pg" $G/.DesignGalleryActivity --es page $pg; done
PAGES=3; shoot invite $G/.ComputerLinkActivity
PAGES=3; shoot setup-pair $G/.SetupActivity --es step Pair
$ADB shell pm revoke $G android.permission.POST_NOTIFICATIONS >/dev/null 2>&1
PAGES=1; shoot setup-notifications $G/.SetupActivity --es step Notifications
$ADB shell pm revoke $G android.permission.ANSWER_PHONE_CALLS >/dev/null 2>&1; $ADB shell pm revoke $G android.permission.BLUETOOTH_CONNECT >/dev/null 2>&1
PAGES=5; shoot setup-answer $G/.SetupActivity --es step AnswerCalls
$ADB shell pm grant $G android.permission.BLUETOOTH_CONNECT >/dev/null 2>&1
PAGES=5; shoot setup-answer-bt $G/.SetupActivity --es step AnswerCalls --ez more true
$ADB shell pm revoke $G android.permission.BLUETOOTH_CONNECT >/dev/null 2>&1
PAGES=3; shoot setup-battery $G/.SetupActivity --es step Battery
PAGES=6; shoot setup-sms $G/.SetupActivity --es step Sms
PAGES=8; shoot g-msgson $G/.DesignGalleryActivity --es page msgson
# Diagnostics: the update check that runs when the app opens needs the network (and the emulator's pinned date breaks TLS): say
# it was done a minute ago, so the card shows "Not checked yet." every time.
$ADB shell am force-stop $G; UID_=$($ADB shell stat -c %u /data/data/$G | tr -d '\r')
printf '%s' "<?xml version='1.0' encoding='utf-8' standalone='yes' ?><map><boolean name=\"auto_check\" value=\"true\" /><string name=\"channel\">stable</string><long name=\"last_check_ms\" value=\"1781524740000\" /></map>" >"$RUN/updates.xml"
$ADB push "$RUN/updates.xml" /data/local/tmp/gb-updates.xml >/dev/null
$ADB shell "mkdir -p /data/data/$G/shared_prefs; cp /data/local/tmp/gb-updates.xml /data/data/$G/shared_prefs/updates.xml; chown $UID_:$UID_ /data/data/$G/shared_prefs/updates.xml; chmod 660 /data/data/$G/shared_prefs/updates.xml; rm /data/local/tmp/gb-updates.xml"
PAGES=6; shoot diag $G/.MainActivity
FS="1.0 2.0"; PAGES=1
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
