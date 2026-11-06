#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator end-to-end coverage for the recent-photos feed (docs/design/recent-photos.md, docs/reports/recent-photos.md):
# the phone's media store -> the desktop service -> `geekbridge-ctl photos ...`.
#
#   scripts/e2e/photos.sh [--stop] [STAGE[,STAGE...]]     stages: env pair photos   (default: all)
#
# Everything is private (nothing touches the real service, ~/.config, ports 1716/17381/8000 or the session bus):
#   * `dbus-run-session`   a private session bus
#   * Xvfb :95             a private X display for the service's clipboard code (and the xclip round trip below)
#   * emulator             own AVD clone (gbphotos, cloned from $GB_BASE_AVD=gb36, removed on exit unless GB_KEEP_AVD=1),
#                          console port 5674, adb server 16170
#   * geekbridge-service   own state dir and its own HOME (so the save folder is private too), control port 17891,
#                          api 17890, discovery 17846, listener 1757
# Stage photos (each line is one row in the results file):
#   off_default      the phone's photo switch is off: the phone does not offer photos, `photos refresh` is refused and
#                    nothing arrives, even with the computer's consent on
#   needs_grant      switch on but this computer not allowed on the phone: a refresh is not answered
#   needs_consent    switch on and allowed, computer's consent off: refresh and save are refused here, nothing is held
#   list_newest      all on: the list shows the generated JPEGs newest first, with name, size and pixel size right
#   thumbnails       every thumbnail arrives, is a JPEG, at most 24 KiB and 256 px (the noisy picture needs the shrink)
#   save             `photos save` writes the full picture into the save folder, sha256 identical
#   copy             `photos copy` writes it to the private scratch folder, sha256 identical, and it survives the
#                    clipboard round trip with the Linux tool the window uses (xclip on the private Xvfb)
#   unknown_id       a photo id that was never listed is refused
#   withdraw_desktop consent off: list and thumbnails erased, refresh/save refused; on again: the list returns
#   withdraw_phone   the grant removed on the phone: the list is erased and a refresh is not answered
#   switch_off       the phone's switch turned off again while paired: nothing is answered
#   no_access        Android photo access revoked: a refresh is not answered (no names, no error text)
#   partial          only READ_MEDIA_VISUAL_USER_SELECTED (Android 14+ "selected photos"): the list is marked partial
#                    and shows none of the library until the user picks; a picked photo then appears
# Results: dist/android/logs/e2e-photos-results.tsv     Needs: the .tools Android toolchain, Xvfb, xclip, dbus-run-session,
# ImageMagick (magick or convert) or python3-PIL, release binaries (E2E_TARGET) and the debug APK
# (E2E_SCRATCH/dist/geekbridge-debug.apk) from scripts/build-android.sh.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

if [[ -z "${PHOTOS_INNER:-}" ]]; then
  exec dbus-run-session -- env PHOTOS_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

STAGES="env,pair,photos"
STOPPING=0
for a in "$@"; do
  case "$a" in
    --stop) STOPPING=1 ;;
    -h|--help) sed -n '3,36p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) STAGES="$a" ;;
  esac
done
set --

PHOTOS_WORK="${PHOTOS_WORK:-$HOME/.cache/gb-tmp/photos-e2e}"
: "${E2E_AVD:=gbphotos}" "${E2E_EMU_PORT:=5674}" "${E2E_ADB_PORT:=16170}" "${E2E_TAG:=-photos}" "${GB_BASE_AVD:=gb36}"
: "${E2E_RUN:=$PHOTOS_WORK/run}" "${E2E_SCRATCH:=$PHOTOS_WORK/android-scratch}" "${E2E_TARGET:=$PHOTOS_WORK/target}"
: "${E2E_XDISP:=:95}" "${E2E_CTRL_PORT:=17891}" "${E2E_API_PORT:=17890}" "${E2E_DISC_PORT:=17846}" "${E2E_LISTEN_PORT:=1757}" "${E2E_REDIR_PORT:=17961}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP GB_BASE_AVD \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-photos-results.tsv"
set -- --no-build
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set --
# PHOTOS_KEEP=1 (debugging): leave the emulator and the service running when the script ends.
if [[ "${PHOTOS_KEEP:-0}" == 1 ]]; then gb_avd_on_exit() { :; }; fi

# The host is shared and can be heavily loaded: give the control calls room.
ctl() { timeout 90 "${CTL[@]}" "$@"; }
# `ctl status` appends plain-words hints after the JSON for a phone that is not connected: read only the JSON.
jget() { python3 -c "import json,sys;d=json.JSONDecoder().raw_decode(sys.stdin.read())[0];print($1)" 2>/dev/null; }

PRIV="$RUN/priv"; mkdir -p "$PRIV"/{config,data,state,cache,home}
XRT="$PHOTOS_WORK/xrt"; mkdir -p "$XRT"; chmod 700 "$XRT"
SERVICE_LOG="$LOGS/e2e-photos-service.log"
APP_ENV=(env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u SWAYSOCK -u NIRI_SOCKET
  HOME="$PRIV/home" XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" XDG_DATA_HOME="$PRIV/data" XDG_STATE_HOME="$PRIV/state"
  XDG_CACHE_HOME="$PRIV/cache" XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=GeekBridgeE2E)
SAVE_DIR="$PRIV/home/Downloads/GeekBridge"
COPY_DIR="$SAVE_DIR/.geekbridge-photo-copy"
PHOTO_DIR="$RUN/photos"   # the generated pictures on the host
PIC_PREFIX=gbe2e_photo_

stop_desktop() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null
  pkill -f "Xvfb $XDISP" 2>/dev/null
  true
}
cleanup() {
  [[ "${PHOTOS_KEEP:-0}" == 1 ]] || { $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; stop_desktop; }
}
trap cleanup EXIT

# ---------------------------------------------------------------- env
start_desktop_service() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
  mkdir -p "$RUN/state"; chmod 700 "$RUN/state"
  ("${APP_ENV[@]}" DISPLAY="$XDISP" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_QR_ADDRESSES="10.0.2.2:$LISTEN_PORT" \
     setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" --control-port "$CTRL_PORT" \
     --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name E2E-Desktop >>"$SERVICE_LOG" 2>&1 &)
  wait_for 15 ctl status >/dev/null 2>&1
}
stage_env() {
  [[ "$STOPPING" == 1 ]] && return 0
  [[ -x "$SVC" && -f "$APK" ]] || { rec photos_env FAIL "missing binaries/APK ($SVC, $APK)"; return 1; }
  gb_preflight "$ROOT" "$PHOTOS_WORK" || { rec photos_env FAIL "not enough free disk"; return 1; }
  pkill -f "Xvfb $XDISP" 2>/dev/null
  wait_for 10 bash -c "! pgrep -f 'Xvfb $XDISP' >/dev/null"
  rm -f "/tmp/.X11-unix/X${XDISP#:}" "/tmp/.X${XDISP#:}-lock"
  (setsid nohup Xvfb "$XDISP" -screen 0 800x600x24 -nolisten tcp >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${XDISP#:}" || { rec photos_env FAIL "Xvfb did not start"; return 1; }
  start_desktop_service || { rec photos_env FAIL "desktop service did not start: $(tail -3 "$SERVICE_LOG" | tr '\n' '|')"; return 1; }
  start_emulator || { rec photos_env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  adb_install_retry "$APK" || { rec photos_env FAIL "adb install failed: $GB_INSTALL_ERR"; return 1; }
  desk_id
  rec photos_env PASS "own AVD $AVD on $SERIAL, own service on control port $CTRL_PORT; desktop $DESK_ID; APK $(sha256sum "$APK" | cut -c1-12)"
}

# ---------------------------------------------------------------- phone-side state (the app's own preference files)
scan() { $ADB shell am start -W -a android.intent.action.VIEW -d "'$1'" $PKG >/dev/null; }
# Per-computer grants for the desktop's id: a space separated feature list ("" = nothing).
write_grants() {
  desk_id
  local list="" f
  for f in "$@"; do list+="${list:+,}&quot;$f&quot;"; done
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_grants.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="grants">{&quot;migrated&quot;:true,&quot;grants&quot;:{&quot;$DESK_ID&quot;:[$list]}}</string>
</map>
XML
}
# Master switches of the phone's data features: battery on|off, notifications on|off, space separated allow-listed packages.
write_phonedata() { # battery(true|false) notifications(true|false) photos(true|false) pkg...
  local b="$1" n="$2" ph="$3"; shift 3
  local apps="" p
  for p in "$@"; do apps+="        <string>$p</string>"$'\n'; done
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="battery" value="$b" />
    <boolean name="notifications" value="$n" />
    <boolean name="photos" value="$ph" />
    <set name="notification_apps">
$apps    </set>
</map>
XML
}
write_calls_settings() { # confirm_dial true|false
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
    <boolean name="confirm_dial" value="$1" />
</map>
XML
}
# The desktop dials the phone through the emulator redirection until the link is up (the phone side never dials in here).
link_up() { # seconds
  local i; for ((i = 0; i < $1 / 5; i++)); do is_connected && return 0; desk_dial >/dev/null 2>&1; sleep 5; done
  is_connected
}
# Rewrite the phone's settings while the app is stopped, then start it and wait for the link.
# usage: phone_config "grants..." photos(true|false)   (battery, notifications and the call question stay off)
phone_config() {
  local grants="$1" ph="$2" b=false n=false c=false
  $ADB shell am force-stop $PKG; sleep 1
  # shellcheck disable=SC2086
  write_grants $grants
  write_phonedata "$b" "$n" "$ph"
  write_calls_settings "$c"
  goto_home; sleep 3
  desk_dial >/dev/null 2>&1
  link_up 180
}

stage_pair() {
  [[ "$STOPPING" == 1 ]] && return 0
  desk_id; PHONE_ID="$(cat "$RUN/phone-id" 2>/dev/null)"
  wake
  for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  if [[ -n "$PHONE_ID" ]] && is_paired; then
    phone_config "calls" false || true
  else
    for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
    $ADB shell pm clear $PKG >/dev/null; wake
    for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
    local qr; qr="$(ctl pair-qr 2>/dev/null | grep -m1 "^geekbridge://")"
    [[ "$qr" == geekbridge://* ]] || { rec photos_pair FAIL "pair-qr gave: ${qr:0:120}"; return 1; }
    scan "$qr"
    ui wait "Link to a computer?" 20 >/dev/null || { rec photos_pair FAIL "no pairing page after the link: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Continue --expect "Computer identity" >/dev/null
    ui wait "Computer identity" 40 >/dev/null || { rec photos_pair FAIL "no identity step: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Link >/dev/null
    ui wait "Linked ✓ with" 40 >/dev/null || { rec photos_pair FAIL "QR pairing did not finish: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
    phone_config "calls" false || true
    for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  fi
  link_up 120 || { rec photos_pair FAIL "link not connected"; return 1; }
  rec photos_pair PASS "paired $PHONE_ID and connected; phone starts with the calls grant only and every data feature off"
}


# ---------------------------------------------------------------- the photos
# Five generated JPEGs; the fourth is noise, which a 160 px preview cannot squeeze under 24 KiB without the shrink step.
gen_photos() {
  rm -rf "$PHOTO_DIR"; mkdir -p "$PHOTO_DIR"
  python3 - "$PHOTO_DIR" "$PIC_PREFIX" <<'PY' || return 1
import os, sys
from PIL import Image, ImageDraw
d, pre = sys.argv[1], sys.argv[2]
specs = [(1, 640, 480, "smooth"), (2, 1200, 1600, "smooth"), (3, 1920, 1080, "smooth"), (4, 2400, 1600, "noise"), (5, 800, 800, "smooth")]
for n, w, h, kind in specs:
    if kind == "noise":
        im = Image.frombytes("RGB", (w, h), os.urandom(w * h * 3))
    else:
        im = Image.new("RGB", (w, h), (40 * n % 256, 200 - 30 * n, 60 + 25 * n))
        dr = ImageDraw.Draw(im)
        for i in range(0, w, 40):
            dr.line([(i, 0), (w - i, h)], fill=(255 - 40 * n, 30 * n, 128), width=6)
        dr.ellipse([w // 4, h // 4, 3 * w // 4, 3 * h // 4], outline=(255, 255, 255), width=10)
    im.save(os.path.join(d, "%s%d.jpg" % (pre, n)), "JPEG", quality=92)
PY
}
# The media store, asked the way an app would see it.
media_rows() { $ADB shell content query --uri content://media/external/images/media --projection _display_name --where "\"_display_name LIKE '${PIC_PREFIX}%'\"" 2>/dev/null | tr -d '\r'; }
media_scan() { # file name on the phone
  $ADB shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d "file:///sdcard/Pictures/$1" >/dev/null 2>&1
}
media_has() { media_rows | grep -q "_display_name=$1\$"; }
push_photos() {
  # Earlier runs: remove the rows and the files so the order below is the order of this run.
  $ADB shell content delete --uri content://media/external/images/media --where "\"_display_name LIKE '${PIC_PREFIX}%'\"" >/dev/null 2>&1
  $ADB shell "rm -f /sdcard/Pictures/${PIC_PREFIX}*" >/dev/null 2>&1
  $ADB shell mkdir -p /sdcard/Pictures >/dev/null 2>&1
  local n f
  for n in 1 2 3 4 5; do
    f="${PIC_PREFIX}$n.jpg"
    $ADB push "$PHOTO_DIR/$f" "/sdcard/Pictures/$f" >/dev/null 2>&1 || return 1
    media_scan "$f"
    wait_for 30 media_has "$f" || return 1
    sleep 2   # DATE_ADDED has a resolution of one second; the order below must be the order of the scans
  done
}
grant_photo_access() { # full | partial | none
  $ADB shell pm revoke $PKG android.permission.READ_MEDIA_IMAGES >/dev/null 2>&1
  $ADB shell pm revoke $PKG android.permission.READ_MEDIA_VISUAL_USER_SELECTED >/dev/null 2>&1
  case "$1" in
    full) $ADB shell pm grant $PKG android.permission.READ_MEDIA_IMAGES >/dev/null 2>&1
          $ADB shell pm grant $PKG android.permission.READ_MEDIA_VISUAL_USER_SELECTED >/dev/null 2>&1 ;;
    partial) $ADB shell pm grant $PKG android.permission.READ_MEDIA_VISUAL_USER_SELECTED >/dev/null 2>&1 ;;
  esac
}
has_perm() { $ADB shell dumpsys package $PKG 2>/dev/null | grep -E "android.permission.$1: granted=true" >/dev/null; }
# Changing a runtime permission with `pm` kills the app (and with it the link): always call this BEFORE phone_config,
# which restarts the app and brings the link back.

pjson() { ctl photos "$PHONE_ID" --json 2>/dev/null; }
# Names of the generated pictures in the list, in the order the desktop shows them.
list_names() { pjson | jget '" ".join(i["name"] for i in d["items"] if i["name"].startswith("'"$PIC_PREFIX"'"))'; }
list_count() { pjson | jget 'len(d["items"])'; }
item_field() { # name field
  pjson | jget '[i["'"$2"'"] for i in d["items"] if i["name"]=="'"$1"'"][0]'
}
item_id() { item_field "$1" id; }
all_thumbs() { [[ "$(pjson | jget 'all(i["has_thumb"] for i in d["items"]) and len(d["items"])>0')" == True ]]; }
dev_partial() { pjson | jget 'any(x["partial"] for x in d["devices"])'; }
# Must test the printed value: jget exits 0 whatever it prints, so refresh_until returned after one 4 s round (runs 42155454627, 42167708317).
dev_listed() { [[ "$(pjson | jget 'any(x["listed"] for x in d["devices"])')" == True ]]; }
want_pics="${PIC_PREFIX}5.jpg ${PIC_PREFIX}4.jpg ${PIC_PREFIX}3.jpg ${PIC_PREFIX}2.jpg ${PIC_PREFIX}1.jpg"
have_pics() { [[ "$(list_names)" == "$want_pics" ]]; }
# Ask until the pictures are listed (a refresh asked within a few seconds of the last one is not repeated).
refresh_until() { # seconds cmd...
  local secs="$1" i; shift
  for ((i = 0; i < secs / 4; i++)); do
    # A request sent while the emulator has just destroyed the link socket is lost: bring the link back, then ask again.
    is_connected || { desk_dial >/dev/null 2>&1; link_up 40; }
    LAST_REFRESH="$(ctl photos refresh "$PHONE_ID" 2>&1)"; sleep 4; "$@" && return 0
  done
  return 1
}
# The phone must stay silent: ask a few times, nothing is listed, `listed` never turns true.
stays_empty() { # seconds
  local i; for ((i = 0; i < $1 / 4; i++)); do
    # Silence on a dropped link proves nothing: bring it back once, otherwise this is not a pass.
    is_connected || { desk_dial >/dev/null 2>&1; link_up 40; } || { echo "link down" >>"$LOGS/e2e-photos-notes.txt"; return 1; }
    ctl photos refresh "$PHONE_ID" >/dev/null 2>&1; sleep 4
    [[ "$(list_count)" == 0 ]] || return 1
    dev_listed && return 1
  done
}
# What the computer and the phone hold when a list does not come (the whole logcat is in logcat-full.txt).
photos_diag() { # why
  {
    echo "== photos diag $(date +%T): $1"
    echo "last refresh answer: ${LAST_REFRESH:-none}"
    echo "list: $(pjson | head -c 600)"
    $ADB shell "run-as $PKG sh -c 'cat shared_prefs/geekbridge_phonedata.xml shared_prefs/geekbridge_grants.xml'" 2>&1 | tr -d '\r' | head -30
    $ADB shell dumpsys package $PKG 2>/dev/null | grep -E "READ_MEDIA|READ_EXTERNAL" | head -6
    ctl status 2>&1 | head -c 1500; echo
  } >>"$LOGS/e2e-photos-notes.txt"
}
sha() { sha256sum "$1" | cut -d' ' -f1; }
fetch_field() { # photo name, field
  pjson | jget '[f["'"$2"'"] for f in d["fetches"] if f["name"]=="'"$1"'"][-1]'
}
fetch_done() { [[ "$(fetch_field "$1" state)" == done ]]; }

stage_photos() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  link_up 120 || { rec photos_stage FAIL "no link to the phone"; return 1; }
  gen_photos || { rec photos_generate FAIL "could not generate the JPEGs (python3 PIL missing?)"; return 1; }
  grant_photo_access full
  if push_photos; then rec photos_media_store PASS "5 JPEGs pushed to /sdcard/Pictures and scanned: $(media_rows | grep -c "^Row") rows in the media store, READ_MEDIA_IMAGES granted"
  else rec photos_media_store FAIL "media scan did not index the pictures: $(media_rows | head -3 | tr '\n' '|')"; return 1; fi
  local out out2 rc rc2 rc3 rc4

  # 1. Switch off (the default): the phone does not offer photos at all.
  grant_photo_access full; phone_config "calls" false
  ctl consent "$PHONE_ID" photos on >/dev/null
  out="$(ctl photos refresh "$PHONE_ID" 2>&1)"; rc=$?
  if [[ $rc -ne 0 ]] && grep -qi "does not offer" <<<"$out" && stays_empty 12; then rec photos_off_default PASS "phone switch off, computer consent on: '$out'; nothing listed after 12 s"
  else rec photos_off_default FAIL "refresh said '$out' (rc $rc) or something was listed: $(pjson | head -c 200)"; fi

  # 2. Switch on, but this computer is not allowed on the phone: the request is not answered.
  grant_photo_access full; phone_config "calls" true
  ctl consent "$PHONE_ID" photos on >/dev/null
  if stays_empty 16; then rec photos_needs_grant PASS "switch on, no per-computer grant: refresh is not answered (16 s)"
  else rec photos_needs_grant FAIL "photos listed without the per-computer allow: $(pjson | head -c 200)"; fi

  # 3. Allowed on the phone, consent off on the computer: refused here, nothing held.
  grant_photo_access full; phone_config "calls photos" true
  ctl consent "$PHONE_ID" photos off >/dev/null
  out="$(ctl photos refresh "$PHONE_ID" 2>&1)"; rc=$?
  out2="$(ctl photos save "$PHONE_ID" p1 2>&1)"; rc2=$?
  if [[ $rc -ne 0 && $rc2 -ne 0 ]] && grep -qi "off for this phone" <<<"$out" && [[ "$(list_count)" == 0 ]]; then rec photos_needs_consent PASS "computer consent off: refresh '$out', save '$out2'; nothing held"
  else rec photos_needs_consent FAIL "rc=$rc/$rc2 refresh='$out' save='$out2' list=$(list_count)"; fi

  # 4. Everything on: the list, newest first.
  ctl consent "$PHONE_ID" photos on >/dev/null
  if refresh_until 60 have_pics; then
    local bad="" n sz w h
    for n in 1 2 3 4 5; do
      sz="$(item_field "${PIC_PREFIX}$n.jpg" size)"
      [[ "$sz" == "$(stat -c %s "$PHOTO_DIR/${PIC_PREFIX}$n.jpg")" ]] || bad+=" size$n=$sz"
      w="$(item_field "${PIC_PREFIX}$n.jpg" width)"; h="$(item_field "${PIC_PREFIX}$n.jpg" height)"
      [[ "${w}x$h" == "$(python3 -c "from PIL import Image;print('%dx%d' % Image.open('$PHOTO_DIR/${PIC_PREFIX}$n.jpg').size)")" ]] || bad+=" dims$n=${w}x$h"
    done
    if [[ -z "$bad" ]]; then rec photos_list_newest PASS "list newest first: $(list_names | sed "s/${PIC_PREFIX}//g"); sizes and pixel sizes match the files"
    else rec photos_list_newest FAIL "order ok but:$bad"; fi
  else rec photos_list_newest FAIL "expected '$want_pics' got '$(list_names)': $(pjson | head -c 300)"; return 1; fi

  # 5. Thumbnails follow (asked for by the desktop), within the byte and pixel caps.
  if wait_for 60 all_thumbs; then
    local tbad="" msg bytes dims fmt tw th
    for n in 1 2 3 4 5; do
      rm -f "$RUN/thumb$n.jpg"
      msg="$(ctl photos thumb "$PHONE_ID" "$(item_id "${PIC_PREFIX}$n.jpg")" "$RUN/thumb$n.jpg" 2>&1)"
      bytes="$(stat -c %s "$RUN/thumb$n.jpg" 2>/dev/null || echo 0)"
      dims="$(python3 -c "from PIL import Image;im=Image.open('$RUN/thumb$n.jpg');print(im.format, im.size[0], im.size[1])" 2>/dev/null)"
      read -r fmt tw th <<<"$dims"
      if [[ "$fmt" != JPEG || "$bytes" -gt 24576 || "$bytes" -lt 100 || "${tw:-999}" -gt 256 || "${th:-999}" -gt 256 ]]; then tbad+=" $n:[$msg|$dims|${bytes}B]"; fi
    done
    if [[ -z "$tbad" ]]; then rec photos_thumbnails PASS "5 thumbnails are JPEG, <= 24 KiB and <= 256 px; the 2400x1600 noise picture's is $(stat -c %s "$RUN/thumb4.jpg") bytes"
    else rec photos_thumbnails FAIL "bad thumbnails:$tbad"; fi
  else rec photos_thumbnails FAIL "thumbnails did not arrive: $(pjson | head -c 400)"; fi

  # 6. Save: the full file, byte for byte, in the save folder.
  rm -rf "$SAVE_DIR"
  local pid; pid="$(item_id "${PIC_PREFIX}4.jpg")"
  out="$(ctl photos save "$PHONE_ID" "$pid" 2>&1)"
  if wait_for 60 fetch_done "${PIC_PREFIX}4.jpg"; then
    local path; path="$(fetch_field "${PIC_PREFIX}4.jpg" path)"
    if [[ "$path" == "$SAVE_DIR/"* && -f "$path" && "$(sha "$path")" == "$(sha "$PHOTO_DIR/${PIC_PREFIX}4.jpg")" ]]; then
      rec photos_save PASS "saved $(stat -c %s "$path") bytes to $path, sha256 $(sha "$path" | cut -c1-16) identical to the generated file"
    else rec photos_save FAIL "wrong content or place: $path (sha $(sha "$path" 2>/dev/null | cut -c1-16) vs $(sha "$PHOTO_DIR/${PIC_PREFIX}4.jpg" | cut -c1-16))"; fi
  else rec photos_save FAIL "no finished fetch after save ('$out'): $(pjson | jget 'd["fetches"]')"; fi

  # 7. Copy: a private scratch file, then the clipboard tool the window uses.
  out="$(ctl photos copy "$PHONE_ID" "$(item_id "${PIC_PREFIX}2.jpg")" 2>&1)"
  if wait_for 60 fetch_done "${PIC_PREFIX}2.jpg"; then
    local cpath; cpath="$(fetch_field "${PIC_PREFIX}2.jpg" path)"
    local ok=1 note=""
    [[ "$cpath" == "$COPY_DIR/"* && -f "$cpath" ]] || { ok=0; note+=" not in the scratch folder: $cpath;"; }
    [[ "$(sha "$cpath" 2>/dev/null)" == "$(sha "$PHOTO_DIR/${PIC_PREFIX}2.jpg")" ]] || { ok=0; note+=" sha differs;"; }
    [[ ! -e "$SAVE_DIR/${PIC_PREFIX}2.jpg" ]] || { ok=0; note+=" copy also appeared in the save folder;"; }
    [[ "$(stat -c %a "$COPY_DIR" 2>/dev/null)" == 700 ]] || { ok=0; note+=" scratch folder mode $(stat -c %a "$COPY_DIR" 2>/dev/null);"; }
    if ((ok)); then rec photos_copy PASS "copy lands in the private scratch folder (mode 700), sha256 identical, not in the save folder"
    else rec photos_copy FAIL "$note"; fi
    # Clipboard round trip with the same tool the window runs on X11 (xclip, image/jpeg), on the private display.
    if command -v xclip >/dev/null; then
      DISPLAY="$XDISP" xclip -selection clipboard -t image/jpeg -i "$cpath" >/dev/null 2>&1; sleep 1
      DISPLAY="$XDISP" xclip -selection clipboard -t image/jpeg -o >"$RUN/clip-out.jpg" 2>/dev/null
      if [[ "$(sha "$RUN/clip-out.jpg")" == "$(sha "$PHOTO_DIR/${PIC_PREFIX}2.jpg")" ]]; then rec photos_copy_clipboard PASS "xclip image/jpeg on the private Xvfb returns the identical bytes"
      else rec photos_copy_clipboard FAIL "clipboard bytes differ ($(stat -c %s "$RUN/clip-out.jpg" 2>/dev/null) B)"; fi
      DISPLAY="$XDISP" xclip -selection clipboard -i </dev/null >/dev/null 2>&1
    else rec photos_copy_clipboard SKIP "xclip not installed"; fi
  else rec photos_copy FAIL "no finished fetch after copy ('$out'): $(pjson | jget 'd["fetches"]')"; fi

  # 8. A photo id that was never listed.
  out="$(ctl photos save "$PHONE_ID" p99999 2>&1)"; rc=$?
  sleep 8
  if [[ "$(find "$SAVE_DIR" -maxdepth 1 -type f | wc -l)" == 1 ]]; then rec photos_unknown_id PASS "unknown id: rc=$rc '$out'; nothing new in the save folder"
  else rec photos_unknown_id FAIL "rc=$rc '$out'; save folder: $(ls "$SAVE_DIR")"; fi

  # 9. Withdraw on the computer.
  ctl consent "$PHONE_ID" photos off >/dev/null
  out="$(ctl photos refresh "$PHONE_ID" 2>&1)"; rc=$?
  out2="$(ctl photos save "$PHONE_ID" "$pid" 2>&1)"; rc3=$?
  local tout; rm -f "$RUN/thumb-after.jpg"; tout="$(ctl photos thumb "$PHONE_ID" "$pid" "$RUN/thumb-after.jpg" 2>&1)"; rc4=$?
  if [[ "$(list_count)" == 0 && "$(pjson | jget 'len(d["devices"])')" == 0 && $rc -ne 0 && $rc3 -ne 0 && $rc4 -ne 0 && ! -s "$RUN/thumb-after.jpg" ]]; then
    rec photos_withdraw_desktop PASS "consent off: list and thumbnails gone; refresh '$out', save '$out2', thumb '$tout' all refused"
  else rec photos_withdraw_desktop FAIL "count=$(list_count) rc=$rc/$rc3/$rc4 refresh='$out' save='$out2' thumb='$tout'"; fi
  ctl consent "$PHONE_ID" photos on >/dev/null
  if [[ "$(list_count)" == 0 ]] && refresh_until 40 have_pics; then rec photos_reallow PASS "consent on again: nothing held until asked, then the list is back"
  else rec photos_reallow FAIL "after re-allowing: '$(list_names)'"; fi

  # 10. Withdraw on the phone: the grant for this computer is removed.
  grant_photo_access full; phone_config "calls" true
  ctl consent "$PHONE_ID" photos on >/dev/null
  if [[ "$(list_count)" == 0 ]] && stays_empty 16; then rec photos_withdraw_phone PASS "grant removed on the phone: list erased by the reconnect and refresh is not answered (16 s)"
  else rec photos_withdraw_phone FAIL "photos still held or answered: $(pjson | head -c 200)"; fi

  # 11. The phone's switch turned off again.
  grant_photo_access full; phone_config "calls photos" true
  ctl consent "$PHONE_ID" photos on >/dev/null
  if ! refresh_until 40 have_pics; then
    photos_diag "no list 40 s after configuring the phone"
    # Once more with a fresh start of the app; the notes above say what the first round looked like.
    grant_photo_access full; phone_config "calls photos" true
    ctl consent "$PHONE_ID" photos on >/dev/null
    if refresh_until 40 have_pics; then echo "photos_switch_setup: the list came only after a second configuration" >>"$LOGS/e2e-photos-notes.txt"
    else photos_diag "no list after the second configuration"; rec photos_switch_setup FAIL "no list before switching off"; fi
  fi
  grant_photo_access full; phone_config "calls photos" false
  out="$(ctl photos refresh "$PHONE_ID" 2>&1)"; rc=$?
  if [[ $rc -ne 0 ]] && grep -qi "does not offer" <<<"$out" && [[ "$(list_count)" == 0 ]] && stays_empty 8; then rec photos_switch_off PASS "switch off on the phone: '$out'; list erased, nothing answered"
  else rec photos_switch_off FAIL "rc=$rc '$out' count=$(list_count)"; fi

  # 12. No Android photo access at all: asked, never answered, no list.
  grant_photo_access none; phone_config "calls photos" true
  ctl consent "$PHONE_ID" photos on >/dev/null
  if ! has_perm READ_MEDIA_IMAGES && stays_empty 16; then rec photos_no_access PASS "photo permissions revoked: refresh is not answered, no list"
  else rec photos_no_access FAIL "perm still granted or a list appeared: $(pjson | head -c 200)"; fi

  # 13. Partial access (Android 14+): only the user-selected pictures are visible to the app.
  local api; api="$($ADB shell getprop ro.build.version.sdk | tr -d '\r')"
  if [[ "${api:-0}" -ge 34 ]]; then
    grant_photo_access partial; phone_config "calls photos" true
    ctl consent "$PHONE_ID" photos on >/dev/null
    if has_perm READ_MEDIA_VISUAL_USER_SELECTED && ! has_perm READ_MEDIA_IMAGES; then
      if ! refresh_until 24 dev_listed; then
        # Run 42155454627: the link was up for minutes and the phone never answered until it was started afresh (the same
        # second round as in step 11).
        photos_diag "no answer 24 s after configuring the phone for partial access"
        grant_photo_access partial; phone_config "calls photos" true
        ctl consent "$PHONE_ID" photos on >/dev/null
        refresh_until 40 dev_listed || photos_diag "no answer after the second configuration"
      fi
      if [[ "$(dev_partial)" == True && -z "$(list_names)" ]]; then
        rec photos_partial_none PASS "only READ_MEDIA_VISUAL_USER_SELECTED (API $api): list marked partial and holds none of the library; the CLI says: $(ctl photos "$PHONE_ID" 2>&1 >/dev/null | head -1)"
      else rec photos_partial_none FAIL "partial=$(dev_partial) names='$(list_names)' $(pjson | head -c 300)"; fi
    else rec photos_partial_none SKIP "the emulator image did not accept the selected-photos-only grant"; fi
    # The user picks two pictures in the system's photo picker (opened by the app's "Choose which photos" button):
    # exactly those two, the newest two of the five, must now be listed, still marked partial.
    if has_perm READ_MEDIA_VISUAL_USER_SELECTED && ! has_perm READ_MEDIA_IMAGES; then
      local wh sw sh; wh="$($ADB shell wm size | grep -o '[0-9]*x[0-9]*' | tail -1)"; sw="${wh%x*}"; sh="${wh#*x}"
      goto_home; wake
      # The first-run wizard may still be open on a freshly paired phone: leave it.
      for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
      # The page with the photo settings is the "Share with computer" step, reached from the wizard's overview or the home tile.
      if { tap Overview >/dev/null 2>&1 && ui scrolltap "Share with computer" >/dev/null 2>&1; } || { goto_home; ui tap "Messages" --contains >/dev/null 2>&1; }; then :; fi
      if ui scrolltap "Choose which photos GeekBridge may see" >/dev/null 2>&1 \
         && ui tap "Allow limited access" >/dev/null 2>&1 && ui wait "Done" 15 >/dev/null 2>&1; then
        sleep 2
        $ADB shell input tap $((sw / 6)) $((sh * 585 / 1000)); $ADB shell input tap $((sw / 2)) $((sh * 585 / 1000)); sleep 1
        $ADB shell input tap $((sw * 84 / 100)) $((sh * 891 / 1000)); sleep 3
        is_connected || { desk_dial >/dev/null 2>&1; link_up 60; }
        local want2="${PIC_PREFIX}5.jpg ${PIC_PREFIX}4.jpg"
        two_picked() { [[ "$(list_names)" == "$want2" ]]; }
        if refresh_until 40 two_picked && [[ "$(list_count)" == 2 && "$(dev_partial)" == True ]]; then
          rec photos_partial_picked PASS "two pictures picked in the system picker: only those two are listed (newest first), list still marked partial"
        else rec photos_partial_picked FAIL "after picking: names='$(list_names)' count=$(list_count) $(pjson | head -c 300)"; fi
      else rec photos_partial_picked SKIP "could not drive the photo picker: $(ui dump | cut -f1 | head -6 | tr '\n' '|')"; fi
    fi
  else rec photos_partial_none SKIP "API $api has no selected-photos permission"; fi
  ctl consent "$PHONE_ID" photos off >/dev/null
  grant_photo_access full
}

main_photos() {
  mkdir -p "$LOGS"; : >"$LOGS/e2e-run.log"
  if [[ "$STOPPING" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    stop_desktop; return 0
  fi
  [[ "$STAGES" == env,pair,photos ]] && : >"$RESULTS"
  local st sts
  IFS=, read -r -a sts <<<"$STAGES"
  for st in "${sts[@]}"; do log "== stage $st"; "stage_$st" || log "stage $st reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_photos
