#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator end-to-end coverage for the AirDrop-like "Send to computer" flows (docs/sharing.md, docs/reports/share-e2e.md):
# the Android Share sheet target (ShareActivity: text, links, images, many files from Gallery, Browser, Files) and the
# computer's files offered to the phone (Accept / Decline).
#
#   scripts/e2e/share.sh [--stop] [STAGE[,STAGE...]]     stages: env pair share   (default: all)
#
# Everything is private (nothing touches the real service, ~/.config, ports 1716/17381/8000 or the session bus):
#   * `dbus-run-session`   a private session bus
#   * Xvfb :96             a private X display for the service's clipboard code
#   * emulator             own AVD clone (gbshare, cloned from $GB_BASE_AVD=gb36, removed on exit unless GB_KEEP_AVD=1),
#                          console port 5676, adb server 16190
#   * geekbridge-service   own state dir and its own HOME (so the save folder is private), control port 17911,
#                          api 17910, discovery 17866, listener 1777; xdg-open and notify-send are logging shims
#   * "other app"          scripts/e2e/share-sender (built here): a content provider that serves generated files (any
#                          name or size, throttled, slow to open, grant revoked after the first open) and a trampoline
#                          activity that starts ShareActivity with EXTRA_STREAM / ClipData and a read grant, as Gallery,
#                          Browser and Files do
# Stage share (each line is one row in the results file):
#   text            ACTION_SEND text/plain -> a text transfer on the computer; not on the clipboard
#   url             ACTION_SEND a link -> a link transfer; not opened (no xdg-open), clipboard untouched, notified
#   mediastore      `am start -a SEND -t image/jpeg --eu EXTRA_STREAM content://media/...` (MediaStore) -> sha256 equal
#   provider_uri    the same from another app's provider (FileProvider style), no size column, no name column
#   multiple        ACTION_SEND_MULTIPLE with 3 files -> 3 files, each sha256 equal
#   names           unicode, RTL override, a 300 character name, ../ path tricks, dots, empty -> safe saved names
#   duplicates      the same name five times (one batch, then again) -> unique names, nothing overwritten
#   grant_revoked   Android 14 temporary grant revoked right after the first open: all files still complete
#   large           200 MiB streamed: the phone's peak PSS grows by less than 40 MB, sha256 equal
#   survives_screen the Share screen is left (Home) and the app process asked to die (`am kill`) mid-transfer
#   cancel_phone    "Cancel transfers" on the phone mid-transfer: cancelled on both sides, no partial file
#   cancel_desktop  `geekbridge-ctl share cancel` mid-transfer: the phone says so in words
#   offline         the computer is off when sharing: the Share screen says so; it sends when the computer is back
#   link_lost       the computer vanishes mid-transfer: failure in words, then a retry works
#   receive_off     the computer has "receive" off: nothing is stored; the phone says so in words (geekbridge.share.result)
#   answer_declined the user declines the phone's file on the computer: the phone says "the computer declined it" at once
#   answer_saved    a text and a link are kept by the computer: the phone says "received ... not opened" (and not "waiting")
#   d2p_accept      a computer file offered to the phone: Accept -> sha256 equal; offered again -> unique name
#   d2p_decline     Decline -> nothing saved; cancel by the computer mid-transfer
#   d2p_link        a link and a text from the computer: the phone offers Open / Copy
# Results: dist/android/logs/e2e-share-results.tsv     Needs: the .tools Android toolchain (javac, aapt2, d8, apksigner),
# Xvfb, xclip, dbus-run-session, python3, release binaries (E2E_TARGET) and the debug APK
# (E2E_SCRATCH/dist/geekbridge-debug.apk) from scripts/build-android.sh.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

if [[ -z "${SHARE_INNER:-}" ]]; then
  exec dbus-run-session -- env SHARE_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

STAGES="env,pair,share"
STOPPING=0
ONLY_STEPS=""
for a in "$@"; do
  case "$a" in
    --stop) STOPPING=1 ;;
    -h|--help) sed -n '3,43p' "${BASH_SOURCE[0]}"; exit 0 ;;
    steps=*) ONLY_STEPS="${a#steps=}" ;;   # steps=text,url : only these steps of stage share
    *) STAGES="$a" ;;
  esac
done
set --

SHARE_WORK="${SHARE_WORK:-$HOME/.cache/gb-tmp/share-e2e}"
: "${E2E_AVD:=gbshare}" "${E2E_EMU_PORT:=5676}" "${E2E_ADB_PORT:=16190}" "${E2E_TAG:=-share}" "${GB_BASE_AVD:=gb36}"
: "${E2E_RUN:=$SHARE_WORK/run}" "${E2E_SCRATCH:=$SHARE_WORK/android-scratch}" "${E2E_TARGET:=$SHARE_WORK/target}"
: "${E2E_XDISP:=:96}" "${E2E_CTRL_PORT:=17911}" "${E2E_API_PORT:=17910}" "${E2E_DISC_PORT:=17866}" "${E2E_LISTEN_PORT:=1777}" "${E2E_REDIR_PORT:=17981}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP GB_BASE_AVD \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-share-results.tsv"
set -- --no-build
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set --
# SHARE_KEEP=1 (debugging): leave the emulator and the service running when the script ends.
if [[ "${SHARE_KEEP:-0}" == 1 ]]; then gb_avd_on_exit() { :; }; fi

# The host is shared and can be heavily loaded: give the control calls room.
ctl() { timeout 90 "${CTL[@]}" "$@"; }
jget() { python3 -c "import json,sys;d=json.JSONDecoder().raw_decode(sys.stdin.read())[0];print($1)" 2>/dev/null; }

PRIV="$RUN/priv"; mkdir -p "$PRIV"/{config,data,state,cache,home}
XRT="$SHARE_WORK/xrt"; mkdir -p "$XRT"; chmod 700 "$XRT"
SERVICE_LOG="$LOGS/e2e-share-service.log"
SHIM="$RUN/shim"; SHIM_LOG="$RUN/shim.log"
APP_ENV=(env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u SWAYSOCK -u NIRI_SOCKET
  HOME="$PRIV/home" XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" XDG_DATA_HOME="$PRIV/data" XDG_STATE_HOME="$PRIV/state"
  XDG_CACHE_HOME="$PRIV/cache" XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=GeekBridgeE2E "PATH=$SHIM:$PATH")
SAVE_DIR="$PRIV/home/Downloads/GeekBridge"
SENDER_PKG=dev.geekbridge.e2e.sender
SENDER_APK="${SHARE_SENDER_APK:-$SHARE_WORK/sender/share-sender.apk}"
AUTH="content://$SENDER_PKG"
SHARE_ACT="$PKG/.ShareActivity"

stop_desktop() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null
  pkill -f "Xvfb $XDISP" 2>/dev/null
  true
}
cleanup() {
  [[ "${SHARE_KEEP:-0}" == 1 ]] || { $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; stop_desktop; }
}
trap cleanup EXIT

# ---------------------------------------------------------------- env
make_shims() {
  mkdir -p "$SHIM"; : >"$SHIM_LOG"
  local t
  for t in xdg-open notify-send gio firefox chromium google-chrome sensible-browser x-www-browser; do
    printf '#!/bin/sh\nprintf "%%s %%s\\n" "%s" "$*" >>"%s"\nexit 0\n' "$t" "$SHIM_LOG" >"$SHIM/$t"
    chmod +x "$SHIM/$t"
  done
}
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
  [[ -x "$SVC" && -f "$APK" ]] || { rec share_env FAIL "missing binaries/APK ($SVC, $APK)"; return 1; }
  gb_preflight "$ROOT" "$SHARE_WORK" || { rec share_env FAIL "not enough free disk"; return 1; }
  make_shims
  [[ -f "$SENDER_APK" ]] || scripts/e2e/share-sender/build.sh "$SHARE_WORK/sender" >/dev/null 2>&1 || { rec share_env FAIL "could not build the test sender app"; return 1; }
  pkill -f "Xvfb $XDISP" 2>/dev/null
  wait_for 10 bash -c "! pgrep -f 'Xvfb $XDISP' >/dev/null"
  rm -f "/tmp/.X11-unix/X${XDISP#:}" "/tmp/.X${XDISP#:}-lock"
  (setsid nohup Xvfb "$XDISP" -screen 0 800x600x24 -nolisten tcp >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${XDISP#:}" || { rec share_env FAIL "Xvfb did not start"; return 1; }
  start_desktop_service || { rec share_env FAIL "desktop service did not start: $(tail -3 "$SERVICE_LOG" | tr '\n' '|')"; return 1; }
  start_emulator || { rec share_env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || { rec share_env FAIL "adb install failed"; return 1; }
  $ADB install -r "$SENDER_APK" 2>&1 | grep -q Success || { rec share_env FAIL "installing the test sender app failed"; return 1; }
  desk_id
  rec share_env PASS "own AVD $AVD on $SERIAL, own service on control port $CTRL_PORT; desktop $DESK_ID; APK $(sha256sum "$APK" | cut -c1-12); sender app installed"
}

# ---------------------------------------------------------------- pairing (the phone starts with every data feature off)
write_prefs_off() {
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="battery" value="false" />
    <boolean name="notifications" value="false" />
    <boolean name="photos" value="false" />
</map>
XML
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
    <boolean name="confirm_dial" value="false" />
</map>
XML
}
link_up() { # seconds
  local i; for ((i = 0; i < $1 / 5; i++)); do is_connected && return 0; desk_dial >/dev/null 2>&1; sleep 5; done
  is_connected
}
phone_restart() { # restart the app and bring the link back
  $ADB shell am force-stop $PKG; sleep 1
  goto_home; sleep 3
  desk_dial >/dev/null 2>&1
  link_up 180
}
stage_pair() {
  [[ "$STOPPING" == 1 ]] && return 0
  desk_id; PHONE_ID="$(cat "$RUN/phone-id" 2>/dev/null)"
  wake
  local p
  for p in POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  if [[ -n "$PHONE_ID" ]] && is_paired; then
    phone_restart || true
  else
    for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
    $ADB shell pm clear $PKG >/dev/null; wake
    for p in POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
    local qr; qr="$(ctl pair-qr 2>/dev/null | grep -m1 "^geekbridge://")"
    [[ "$qr" == geekbridge://* ]] || { rec share_pair FAIL "pair-qr gave: ${qr:0:120}"; return 1; }
    $ADB shell am start -W -a android.intent.action.VIEW -d "'$qr'" $PKG >/dev/null
    ui wait "Link to a computer?" 20 >/dev/null || { rec share_pair FAIL "no pairing page after the link: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Continue --expect "Computer identity" >/dev/null
    ui wait "Computer identity" 40 >/dev/null || { rec share_pair FAIL "no identity step: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Link >/dev/null
    ui wait "Paired ✓ with" 40 >/dev/null || { rec share_pair FAIL "QR pairing did not finish: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
    $ADB shell am force-stop $PKG; sleep 1; write_prefs_off
    phone_restart || true
    for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  fi
  link_up 120 || { rec share_pair FAIL "link not connected"; return 1; }
  rec share_pair PASS "paired $PHONE_ID and connected"
}

# ---------------------------------------------------------------- helpers for the share stage
sha() { sha256sum "$1" | cut -d' ' -f1; }
# The sha256 of the sender app's generated bytes: byte i = (i*7 + seed) mod 251.
gen_sha() { # size seed
  python3 -I - "$1" "$2" <<'PY'
import hashlib, sys
size, seed = int(sys.argv[1]), int(sys.argv[2])
period = bytes(((i * 7 + seed) % 251) for i in range(251))
block = period * 4096
h = hashlib.sha256()
left = size
while left > 0:
    n = min(left, len(block))
    h.update(block[:n]); left -= n
print(h.hexdigest())
PY
}
urlenc() { python3 -I -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"; }
gen_uri() { # name size seed [query]
  printf '%s/gen/%s/%s/%s%s' "$AUTH" "$2" "$3" "$(urlenc "$1")" "${4:+?$4}"
}
# Quote one word for the phone's shell.
shq() { printf "'%s'" "${1//\'/\'\\\'\'}"; }
# Rows of the computer's transfer list: id dir state bytes size name code (tab separated).
tx_rows() {
  ctl share status 2>/dev/null | python3 -c '
import re, sys
for l in sys.stdin:
    m = re.match(r"^#(\d+) (<-|->) (.*?) (Offered|Waiting|Active|Done|Failed|Cancelled|Declined) (\d+)(?:/(\d+))? bytes (.*?)(?: \((\w+)\))?$", l.rstrip("\n"))
    if m:
        g = m.groups()
        print("\t".join([g[0], g[1], g[3], g[4], g[5] or "", g[6], g[7] or ""]))
'
}
tx_count() { tx_rows | wc -l; }
# tx_field NAME FIELDNO(1 id,2 dir,3 state,4 bytes,5 size,6 name,7 code) [DIR]  -> of the newest row with that name
tx_field() { tx_rows | awk -F'\t' -v n="$1" -v f="$2" -v d="${3:-}" '$6 == n && (d == "" || $2 == d) {v = $f} END {print v}'; }
tx_state() { tx_field "$1" 3 "${2:-}"; }
tx_wait() { # name state secs [dir]
  local i; for ((i = 0; i < $3; i++)); do [[ "$(tx_state "$1" "${4:-}")" == "$2" ]] && return 0; sleep 1; done; return 1
}
# Transfers that exist on the computer (newest id) - used to tell a new one from an old one.
last_id() { tx_rows | awk -F'\t' '{v = $1} END {print v + 0}'; }
tx_wait_new() { # since_id state secs  -> row of the first transfer newer than since_id in that state
  local i r; for ((i = 0; i < $3; i++)); do
    r="$(tx_rows | awk -F'\t' -v s="$1" -v st="$2" '$1 > s && $3 == st {print; exit}')"
    [[ -n "$r" ]] && { echo "$r"; return 0; }; sleep 1
  done; return 1
}
saved_files() { find "$SAVE_DIR" -maxdepth 1 -type f -printf '%f\n' 2>/dev/null | sort; }
reset_save() { rm -rf "$SAVE_DIR"; mkdir -p "$SAVE_DIR"; }
screen_texts() { ui dump | cut -f1; }
# The per-file status lines only ("name — state"): words inside a file name must not count as the phone's message.
status_lines() { screen_texts | grep ' — ' | tr '\n' '|'; }
screen_has() { screen_texts | grep -qF -- "$1"; }
logcat_clear() { $ADB logcat -c 2>/dev/null; }
sender_log() { $ADB logcat -d -s GBSender:I 2>/dev/null | tr -d '\r'; }
# Starts the share like another app: URIS comma separated, extra args are more `am start` extras.
sender_send() {
  local uris="$1" extra=("${@:2}")
  local cmd="am start -W --activity-multiple-task -n $SENDER_PKG/.SendActivity"
  [[ -n "$uris" ]] && cmd+=" --es uris $(shq "$uris")"
  local e; for e in "${extra[@]}"; do cmd+=" $(shq "$e")"; done
  $ADB shell "$cmd" >/dev/null 2>&1
}
sender_text() { $ADB shell "am start -W --activity-multiple-task -n $SENDER_PKG/.SendActivity --es text $(shq "$1")" >/dev/null 2>&1; }
# Clean slate before a step: no share screen, desktop transfer list is not reset (ids only grow), link up.
fresh() {
  $ADB shell input keyevent KEYCODE_BACK >/dev/null 2>&1
  $ADB shell am force-stop $SENDER_PKG >/dev/null 2>&1
  # The phone stops its own link when a share ends while the app is in the background, and the computer can keep
  # listing the old connection as up (docs/reports/e2e-findings.md E2E-4): always redial.
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  goto_home; sleep 2; desk_dial >/dev/null 2>&1
  wait_for 20 is_connected || { desk_dial >/dev/null 2>&1; wait_for 20 is_connected; }
  $ADB shell input keyevent KEYCODE_HOME >/dev/null 2>&1
}
allow_receive() { ctl share allow "$PHONE_ID" on >/dev/null; ctl share auto-accept "$PHONE_ID" on >/dev/null; }
ensure_phone_link() {
  is_connected && return 0
  desk_dial >/dev/null 2>&1; link_up 90
}
# Waits for the phone's Share screen to settle and returns what it shows.
share_screen() { sleep "${1:-3}"; screen_texts | tr '\n' '|'; }
step_wanted() { [[ -z "$ONLY_STEPS" || ",$ONLY_STEPS," == *",$1,"* ]]; }

pss_kb() {
  $ADB shell dumpsys meminfo $PKG 2>/dev/null | tr -d '\r' | awk '/TOTAL PSS:/ {gsub(",", "", $3); print $3; exit} /^ +TOTAL +[0-9]/ {print $2; exit}'
}
# Samples the phone app's PSS (kB) about every second into $1 until $1.stop exists.
pss_sampler() {
  rm -f "$1" "$1.stop"
  ( while [[ ! -e "$1.stop" ]]; do v="$(pss_kb)"; [[ -n "$v" ]] && echo "$v" >>"$1"; done ) &
  PSS_PID=$!
}
pss_stop() { touch "$1.stop"; wait "$PSS_PID" 2>/dev/null; }

# ---------------------------------------------------------------- the steps
step_text() {
  allow_receive; fresh
  local t="E2E hello from the phone — ünïcode, العربية, 日本語, emoji 🎉" before; before="$(last_id)"
  sender_text "$t"
  local row; row="$(tx_wait_new "$before" Done 40)"
  local got; got="$(ctl share status | grep -F "text: " | tail -1)"
  if [[ -n "$row" && "$got" == *"$t"* ]]; then
    local clip; clip="$(DISPLAY="$XDISP" xclip -selection clipboard -o 2>/dev/null)"
    if [[ "$clip" == "$t" ]]; then rec share_text FAIL "received text was also put on the computer's clipboard"
    else rec share_text PASS "ACTION_SEND text (unicode, Arabic, CJK, emoji) arrived whole as a text transfer; clipboard untouched; notification: $(grep -m1 'notify-send' "$SHIM_LOG" | cut -c1-90)"; fi
  else rec share_text FAIL "no text transfer: row='$row' status='$got'; screen: $(share_screen 1)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_url() {
  allow_receive; fresh
  : >"$SHIM_LOG"
  printf 'sentinel-before-link' | DISPLAY="$XDISP" xclip -selection clipboard -i; sleep 1
  local u="https://example.org/path/ü?q=1&r=two#frag" before; before="$(last_id)"
  sender_text "$u"
  local row; row="$(tx_wait_new "$before" Done 40)"
  local got; got="$(ctl share status | grep -F "link: " | tail -1)"
  sleep 2
  local clip; clip="$(DISPLAY="$XDISP" xclip -selection clipboard -o 2>/dev/null)"
  if [[ -z "$row" || "$got" != *"$u"* ]]; then rec share_url FAIL "no link transfer: row='$row' status='$got'"
  elif grep -q '^xdg-open\|^firefox\|^chromium\|^google-chrome\|^gio\|^sensible-browser\|^x-www-browser' "$SHIM_LOG"; then rec share_url FAIL "the link was opened on the computer by itself: $(grep -m1 -E '^(xdg-open|firefox|chromium|google-chrome|gio)' "$SHIM_LOG")"
  elif [[ "$clip" != sentinel-before-link ]]; then rec share_url FAIL "the link replaced the computer's clipboard: '$clip'"
  else rec share_url PASS "a shared link arrives as a link transfer, is not opened and the clipboard is untouched; notification: $(grep -m1 'notify-send' "$SHIM_LOG" | cut -c1-100)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
push_jpeg() { # prints the MediaStore id
  python3 - "$RUN/gbe2e_share.jpg" <<'PY' || return 1
import os, sys
from PIL import Image
Image.frombytes("RGB", (1600, 1200), os.urandom(1600 * 1200 * 3)).save(sys.argv[1], "JPEG", quality=90)
PY
  $ADB shell content delete --uri content://media/external/images/media --where "\"_display_name LIKE 'gbe2e_share%'\"" >/dev/null 2>&1
  $ADB shell rm -f /sdcard/Pictures/gbe2e_share.jpg >/dev/null 2>&1
  $ADB push "$RUN/gbe2e_share.jpg" /sdcard/Pictures/gbe2e_share.jpg >/dev/null 2>&1 || return 1
  $ADB shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file:///sdcard/Pictures/gbe2e_share.jpg >/dev/null 2>&1
  local i id
  for ((i = 0; i < 30; i++)); do
    id="$($ADB shell "content query --uri content://media/external/images/media --projection _id --where \"_display_name='gbe2e_share.jpg'\"" 2>/dev/null | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
    [[ -n "$id" ]] && { echo "$id"; return 0; }; sleep 1
  done; return 1
}
step_mediastore() {
  allow_receive; fresh; reset_save
  local id; id="$(push_jpeg)"
  [[ -n "$id" ]] || { rec share_mediastore FAIL "MediaStore did not index the test image"; return 1; }
  local want before; want="$(sha "$RUN/gbe2e_share.jpg")"; before="$(last_id)"
  # Through the system chooser, like a real app: `am` does not move EXTRA_STREAM into ClipData (apps do), so a direct
  # component start would carry no read grant for the MediaStore Uri.
  $ADB shell am start -a android.intent.action.SEND -t image/jpeg --eu android.intent.extra.STREAM "content://media/external/images/media/$id" --grant-read-uri-permission >/dev/null 2>&1
  sleep 3
  tap "Send to computer" >/dev/null 2>&1 || tap "Send to computer" --contains >/dev/null 2>&1; sleep 1
  tap "Just once" >/dev/null 2>&1
  if tx_wait_new "$before" Done 60 >/dev/null && [[ "$(sha "$SAVE_DIR/gbe2e_share.jpg" 2>/dev/null)" == "$want" ]]; then
    rec share_mediastore PASS "ACTION_SEND image/jpeg with a MediaStore EXTRA_STREAM ($(stat -c %s "$RUN/gbe2e_share.jpg") bytes): saved as gbe2e_share.jpg, sha256 equal"
  else rec share_mediastore FAIL "not delivered: saved=[$(saved_files | tr '\n' ' ')] rows=[$(tx_rows | tail -2 | tr '\t\n' ' |')] screen: $(share_screen 1)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_provider_uri() {
  allow_receive; fresh; reset_save
  local before; before="$(last_id)"
  sender_send "$(gen_uri "IMG_0042.JPG" 262144 3 "")" --es type image/jpeg
  if tx_wait_new "$before" Done 40 >/dev/null && [[ "$(sha "$SAVE_DIR/IMG_0042.JPG" 2>/dev/null)" == "$(gen_sha 262144 3)" ]]; then
    rec share_provider_uri PASS "another app's provider URI (ClipData + EXTRA_STREAM + read grant): IMG_0042.JPG saved, sha256 equal"
  else rec share_provider_uri FAIL "not delivered: saved=[$(saved_files | tr '\n' ' ')] screen: $(share_screen 1) log: $(sender_log | tail -4 | tr '\n' '|')"; fi
  $ADB shell input keyevent KEYCODE_BACK
  # A provider that reports neither a name nor a size (the size is then taken from the descriptor, or refused in words).
  fresh; local b2; b2="$(last_id)"
  sender_send "$(gen_uri "nameless-source.bin" 4096 5 "nosize=1&noname=1")"
  sleep 8
  local sc; sc="$(share_screen 1)"
  if tx_wait_new "$b2" Done 20 >/dev/null; then rec share_provider_nosize PASS "no size and no name from the provider: sent anyway (names it from the Uri)"
  elif grep -qiE "size|could not|cannot|empty" <<<"$sc"; then rec share_provider_nosize PASS "no size from the provider: refused in words on the Share screen ($(grep -oiE '[^|]*(size|could not|cannot|empty)[^|]*' <<<"$sc" | head -1))"
  else rec share_provider_nosize FAIL "silent: screen $sc"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_multiple() {
  allow_receive; fresh; reset_save
  local u1 u2 u3 before; before="$(last_id)"
  u1="$(gen_uri "multi-a.jpg" 300000 11)"; u2="$(gen_uri "multi-b.png" 1500000 12)"; u3="$(gen_uri "multi-c.pdf" 700000 13)"
  sender_send "$u1,$u2,$u3" --es action SEND_MULTIPLE --es type "*/*"
  local ok=1 note="" f s seed=11
  for f in "multi-a.jpg:300000:11" "multi-b.png:1500000:12" "multi-c.pdf:700000:13"; do
    IFS=: read -r n s seed <<<"$f"
    tx_wait "$n" Done 60 || { ok=0; note+=" $n:$(tx_state "$n")"; continue; }
    [[ "$(sha "$SAVE_DIR/$n" 2>/dev/null)" == "$(gen_sha "$s" "$seed")" ]] || { ok=0; note+=" $n:sha"; }
  done
  if ((ok)); then rec share_multiple PASS "ACTION_SEND_MULTIPLE with 3 files: all three saved, each sha256 equal"
  else rec share_multiple FAIL "problems:$note; saved=[$(saved_files | tr '\n' ' ')] screen: $(share_screen 1)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_names() {
  allow_receive; fresh; reset_save
  local long; long="$(python3 -c 'print("L" * 296 + ".jpg")')"
  local names=("résumé – 写真 🎉.jpg" $'photo‮gpj.exe' "$long" "../../etc/passwd" "..." "a<b>c:d|e?f*g.txt" "نص عربي.txt" "CON.txt")
  local uris="" i=0 n
  for n in "${names[@]}"; do uris+="${uris:+,}$(gen_uri "$n" $((2000 + i * 100)) $((20 + i)))"; i=$((i + 1)); done
  local before; before="$(last_id)"
  sender_send "$uris" --es action SEND_MULTIPLE
  local i2 deadline=90
  for ((i2 = 0; i2 < deadline; i2++)); do
    [[ "$(tx_rows | awk -F'\t' -v s="$before" '$1 > s && $3 == "Done"' | wc -l)" -ge ${#names[@]} ]] && break; sleep 1
  done
  local files; files="$(saved_files)"
  local bad="" cnt; cnt="$(saved_files | wc -l)"
  [[ "$cnt" == "${#names[@]}" ]] || bad+=" saved $cnt of ${#names[@]};"
  grep -q $'‮' <<<"$files" && bad+=" RTL override survived in a saved name;"
  grep -qE '[<>:|?*]' <<<"$files" && bad+=" reserved characters survived;"
  grep -q '^\.' <<<"$files" && bad+=" a saved name starts with a dot;"
  [[ -f "$SAVE_DIR/résumé – 写真 🎉.jpg" ]] || bad+=" unicode name changed;"
  local maxlen; maxlen="$(saved_files | awk '{ print length($0) }' | sort -n | tail -1)"
  local maxbytes; maxbytes="$(saved_files | while IFS= read -r f; do printf '%s' "$f" | wc -c; done | sort -n | tail -1)"
  [[ "${maxbytes:-0}" -le 255 ]] || bad+=" a name is $maxbytes bytes;"
  ls "$SAVE_DIR" | grep -q '\.jpg$' && ls "$SAVE_DIR" | grep -E '^L+.*\.jpg$' | grep -q . || bad+=" the long name lost its .jpg extension;"
  [[ ! -e "$PRIV/home/Downloads/passwd" && ! -e "$PRIV/home/etc" ]] || bad+=" ../ escaped the save folder;"
  [[ -z "$(find "$PRIV/home" -name passwd -not -path "$SAVE_DIR/*" 2>/dev/null)" ]] || bad+=" a passwd file outside the save folder;"
  if [[ -z "$bad" ]]; then rec share_names PASS "8 hostile names saved safely: $(printf '%s' "$files" | cut -c1-40 | tr '\n' '|')"
  else rec share_names FAIL "$bad files: $(printf '%s' "$files" | cut -c1-60 | tr '\n' '|')"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_duplicates() {
  allow_receive; fresh; reset_save
  local uris="" i
  for i in 1 2 3 4 5; do uris+="${uris:+,}$(gen_uri "same.bin" $((1000 + i)) $((40 + i)))"; done
  local before; before="$(last_id)"
  sender_send "$uris" --es action SEND_MULTIPLE
  local k; for ((k = 0; k < 60; k++)); do [[ "$(tx_rows | awk -F'\t' -v s="$before" '$1 > s && $3 == "Done"' | wc -l)" -ge 5 ]] && break; sleep 1; done
  local cnt1; cnt1="$(saved_files | wc -l)"
  $ADB shell input keyevent KEYCODE_BACK; fresh
  local b2; b2="$(last_id)"
  sender_send "$(gen_uri "same.bin" 999 77)"
  tx_wait_new "$b2" Done 40 >/dev/null
  local cnt2 shas; cnt2="$(saved_files | wc -l)"; shas="$(cd "$SAVE_DIR" && sha256sum -- * 2>/dev/null | cut -d' ' -f1 | sort -u | wc -l)"
  if [[ "$cnt1" == 5 && "$cnt2" == 6 && "$shas" == 6 ]]; then rec share_duplicates PASS "five files named same.bin, then a sixth later: six unique names ($(saved_files | tr '\n' '|' | cut -c1-80)), six different contents, nothing overwritten"
  else rec share_duplicates FAIL "after batch: $cnt1 files, after the next: $cnt2, distinct contents $shas: $(saved_files | tr '\n' '|')"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_grant_revoked() {
  allow_receive; fresh; reset_save; logcat_clear
  local uris="" i
  for i in 1 2 3; do uris+="${uris:+,}$(gen_uri "grant-$i.bin" $((600000 + i)) $((50 + i)) "revoke=150")"; done
  local before; before="$(last_id)"
  sender_send "$uris" --es action SEND_MULTIPLE
  local ok=1 note="" n
  for i in 1 2 3; do
    tx_wait "grant-$i.bin" Done 60 || { ok=0; note+=" grant-$i:$(tx_state "grant-$i.bin")"; continue; }
    [[ "$(sha "$SAVE_DIR/grant-$i.bin" 2>/dev/null)" == "$(gen_sha $((600000 + i)) $((50 + i)))" ]] || { ok=0; note+=" grant-$i:sha"; }
  done
  local lg; lg="$(sender_log)"
  local opens; opens="$(grep -c ' open grant-' <<<"$lg")"
  if ! grep -q REVOKED <<<"$lg"; then rec share_grant_revoked SKIP "the sender never revoked the grants (log: $(tail -3 <<<"$lg" | tr '\n' '|'))"
  elif ((ok)); then rec share_grant_revoked PASS "grants of all 3 URIs revoked 150 ms after the first open: all files still complete (opened $opens times), sha256 equal"
  else rec share_grant_revoked FAIL "after the grant was revoked:$note; sender log: $(tail -6 <<<"$lg" | tr '\n' '|'); phone: $(share_screen 1)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_large() {
  allow_receive; fresh; reset_save
  local size=$((200 * 1024 * 1024)) seed=61
  phone_restart >/dev/null 2>&1 || true
  fresh
  # Warm up (the link and the Share screen's classes) so the baseline is the app at rest.
  sleep 5
  local base1 base2 base; base1="$(pss_kb)"; sleep 2; base2="$(pss_kb)"; base=$(( base1 > base2 ? base1 : base2 ))
  local samples="$RUN/pss.txt" before; before="$(last_id)"
  pss_sampler "$samples"
  local t0=$SECONDS
  sender_send "$(gen_uri "large-200mib.bin" $size $seed)"
  local done=0; tx_wait "large-200mib.bin" Done 400 && done=1
  local took=$((SECONDS - t0))
  pss_stop "$samples"
  local peak; peak="$(sort -n "$samples" 2>/dev/null | tail -1)"
  local growth=$(( (${peak:-0} - base) / 1024 ))
  local nsamp; nsamp="$(wc -l <"$samples" 2>/dev/null || echo 0)"
  if ((done)) && [[ "$(sha "$SAVE_DIR/large-200mib.bin" 2>/dev/null)" == "$(gen_sha $size $seed)" ]]; then
    if ((growth < 40 && nsamp >= 3)); then rec share_large PASS "200 MiB in ${took}s, sha256 equal; phone PSS $((base / 1024)) MB at rest, peak $((peak / 1024)) MB (+${growth} MB < 40) over $nsamp samples"
    else rec share_large FAIL "200 MiB arrived but the phone's PSS grew by ${growth} MB (base $((base / 1024)), peak $((${peak:-0} / 1024)) MB, $nsamp samples)"; fi
  else rec share_large FAIL "200 MiB not delivered in 400 s: state $(tx_state large-200mib.bin) bytes $(tx_field large-200mib.bin 4); PSS growth ${growth} MB"; fi
  $ADB shell input keyevent KEYCODE_BACK
  rm -f "$SAVE_DIR/large-200mib.bin"
}
step_survives_screen() {
  allow_receive; fresh; reset_save
  local size=$((48 * 1024 * 1024)) seed=62 before; before="$(last_id)"
  sender_send "$(gen_uri "survive.bin" $size $seed "rate=6144")"
  local id; id=""
  for ((k = 0; k < 40; k++)); do [[ "$(tx_state survive.bin)" == Active ]] && break; sleep 1; done
  [[ "$(tx_state survive.bin)" == Active ]] || { rec share_survives_screen FAIL "the transfer never became active: $(tx_state survive.bin) $(share_screen 1)"; return 1; }
  # Leave the Share screen the way a user does, then ask Android to kill whatever it is allowed to kill.
  $ADB shell input keyevent KEYCODE_HOME; sleep 3
  local fgs; fgs="$($ADB shell dumpsys activity services $PKG 2>/dev/null | tr -d '\r' | grep -c 'isForeground=true')"
  $ADB shell am kill $PKG >/dev/null 2>&1
  local ok=0; tx_wait survive.bin Done 90 && ok=1
  if ((ok)) && [[ "$(sha "$SAVE_DIR/survive.bin" 2>/dev/null)" == "$(gen_sha $size $seed)" ]]; then
    rec share_survives_screen PASS "left the Share screen mid-transfer and asked Android to kill the app (am kill): completed, sha256 equal (foreground service running: $fgs)"
  else rec share_survives_screen FAIL "the transfer did not survive leaving the screen: state $(tx_state survive.bin) bytes $(tx_field survive.bin 4) (foreground services: $fgs; process alive: $($ADB shell pidof $PKG | tr -d '\r'))"; fi
}
# "Stay connected" (the background link) on or off, edited in the app's own preference file.
set_state_bool() { # name true|false : a boolean in the app's own state preferences (app stopped first)
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG cat shared_prefs/geekbridge_state.xml" 2>/dev/null | tr -d '\r' | python3 -I -c '
import re, sys
x = sys.stdin.read() or "<?xml version=\x271.0\x27 encoding=\x27utf-8\x27 standalone=\x27yes\x27 ?>\n<map>\n</map>\n"
name, val = sys.argv[1], sys.argv[2]
x = re.sub(r"<boolean name=\"" + name + r"\" value=\"[a-z]+\" />\s*", "", x)
x = x.replace("</map>", "    <boolean name=\"" + name + "\" value=\"" + val + "\" />\n</map>")
sys.stdout.write(x)' "$1" "$2" >"$RUN/state-prefs.xml"
  $ADB shell "run-as $PKG sh -c 'cat > shared_prefs/geekbridge_state.xml'" <"$RUN/state-prefs.xml"
}
set_keep_connected() { set_state_bool keep_connected "$1"; }
step_survives_background_off() {
  allow_receive; set_keep_connected false; phone_restart >/dev/null 2>&1; fresh; reset_save
  local size=$((48 * 1024 * 1024)) seed=65
  sender_send "$(gen_uri "survive-off.bin" $size $seed "rate=6144")"
  local k; for ((k = 0; k < 40; k++)); do [[ "$(tx_state survive-off.bin)" == Active ]] && break; sleep 1; done
  [[ "$(tx_state survive-off.bin)" == Active ]] || { rec share_survives_no_service FAIL "never active: $(tx_state survive-off.bin) $(share_screen 1)"; set_keep_connected true; return 1; }
  $ADB shell input keyevent KEYCODE_HOME; sleep 3
  local fgs; fgs="$($ADB shell dumpsys activity services $PKG 2>/dev/null | tr -d '\r' | grep -c 'isForeground=true')"
  local prio; prio="$($ADB shell dumpsys activity processes 2>/dev/null | tr -d '\r' | grep -A3 "ProcessRecord.*$PKG" | grep -oE 'procState=[A-Z_]+|state=[A-Z_ ]+' | head -2 | tr '\n' ' ')"
  $ADB shell am kill $PKG >/dev/null 2>&1
  local ok=0; tx_wait survive-off.bin Done 90 && ok=1
  if ((ok)) && [[ "$(sha "$SAVE_DIR/survive-off.bin" 2>/dev/null)" == "$(gen_sha $size $seed)" ]]; then
    rec share_survives_no_service PASS "'Stay connected' off, screen left mid-transfer, am kill: completed, sha256 equal (foreground service: $fgs)"
  else rec share_survives_no_service FAIL "with the background connection off the transfer dies when the screen is left: state $(tx_state survive-off.bin) bytes $(tx_field survive-off.bin 4); foreground services $fgs; process $prio"; fi
  set_keep_connected true; phone_restart >/dev/null 2>&1
}
cancel_setup() { # prints nothing; leaves a throttled transfer Active
  allow_receive; fresh; reset_save
  sender_send "$(gen_uri "cancel-me.bin" $((64 * 1024 * 1024)) 63 "rate=256")"
  local k; for ((k = 0; k < 40; k++)); do
    [[ "$(tx_state cancel-me.bin)" == Active && "$(tx_field cancel-me.bin 4)" -gt 100000 ]] 2>/dev/null && return 0; sleep 1
  done; return 1
}
step_cancel_phone() {
  cancel_setup || { rec share_cancel_phone FAIL "the transfer never became active: $(share_screen 1)"; return 1; }
  # A slow transfer (256 KB/s) changes the progress line only every few seconds, so uiautomator can see an idle screen.
  tap "Cancel transfers" >/dev/null 2>&1 || tap "Cancel transfers" >/dev/null 2>&1
  local st=""; for ((k = 0; k < 30; k++)); do st="$(tx_state cancel-me.bin)"; [[ "$st" == Cancelled || "$st" == Failed ]] && break; sleep 1; done
  sleep 3
  local left; left="$(saved_files | grep -c cancel-me)"
  local sc; sc="$(share_screen 1)"; local stl; stl="$(status_lines)"
  if [[ ( "$st" == Cancelled || "$st" == Failed ) && "$left" == 0 ]] && grep -qiE "cancel" <<<"$stl"; then rec share_cancel_phone PASS "Cancel on the phone mid-transfer: the computer shows $st ($(tx_field cancel-me.bin 7)), no partial file left, the phone says: $stl"
  elif [[ "$st" == Cancelled || "$st" == Failed ]] && [[ "$left" == 0 ]]; then rec share_cancel_phone FAIL "cancelled on the computer ($st) but the phone does not say so: $sc"
  else rec share_cancel_phone FAIL "state after Cancel: $st; partial files left: $left; phone: $sc"; fi
  $ADB shell input keyevent KEYCODE_BACK
  # And the phone can share again at once.
  fresh; local b; b="$(last_id)"; sender_send "$(gen_uri "after-cancel.bin" 5000 64)"
  if tx_wait_new "$b" Done 40 >/dev/null; then rec share_after_cancel PASS "a new share right after a cancel works"; else rec share_after_cancel FAIL "no new share after a cancel: $(share_screen 1)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_cancel_desktop() {
  cancel_setup || { rec share_cancel_desktop FAIL "the transfer never became active: $(share_screen 1)"; return 1; }
  local id; id="$(tx_field cancel-me.bin 1)"
  ctl share cancel "$id" >/dev/null 2>&1
  sleep 6
  local sc; sc="$(share_screen 1)"; local stl; stl="$(status_lines)"
  local left; left="$(saved_files | grep -c cancel-me)"
  if [[ "$left" == 0 ]] && grep -qiE "cancel|stopped|interrupted|declined|ended" <<<"$stl" && ! grep -qE "\([a-z]+_[a-z_]+\)" <<<"$stl"; then rec share_cancel_desktop PASS "cancelled on the computer mid-transfer: the phone says: $stl); no partial file"
  else rec share_cancel_desktop FAIL "phone shows: $sc; partial files: $left; computer: $(tx_state cancel-me.bin)"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
service_down() { pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; wait_for 10 bash -c "! pgrep -f '^$SVC --state-dir $RUN/state' >/dev/null"; }
step_second_share() {
  allow_receive; fresh; reset_save
  sender_send "$(gen_uri "first-slow.bin" $((6 * 1024 * 1024)) 81 "rate=2048")"
  local k; for ((k = 0; k < 30; k++)); do [[ "$(tx_state first-slow.bin)" == Active ]] && break; sleep 1; done
  # Another app shares while the first share is still running: both must arrive, nothing is dropped silently.
  sender_send "$(gen_uri "second-file.bin" 30000 82)"
  sender_text "second share, a text, while a file is moving"
  local ok=1 note=""
  tx_wait first-slow.bin Done 90 || { ok=0; note+=" first:$(tx_state first-slow.bin)"; }
  tx_wait second-file.bin Done 60 || { ok=0; note+=" second-file:$(tx_state second-file.bin)"; }
  local k2 t=0; for ((k2 = 0; k2 < 30; k2++)); do ctl share status 2>/dev/null | grep -qF "second share, a text, while a file is moving" && { t=1; break; }; sleep 1; done
  ((t)) || { ok=0; note+=" text missing"; }
  if ((ok)) && [[ "$(sha "$SAVE_DIR/second-file.bin" 2>/dev/null)" == "$(gen_sha 30000 82)" ]]; then rec share_second_while_running PASS "a file and a text shared while another share was running joined it: all three delivered"
  else rec share_second_while_running FAIL "a share started while another ran was lost:$note; phone: $(status_lines)"; fi
}
step_offline() {
  allow_receive; fresh
  service_down; sleep 3
  local before; before="$(last_id 2>/dev/null)"; before="${before:-0}"
  sender_text "sent while the computer was off"
  sleep 6
  local sc; sc="$(share_screen 1)"
  local said=0; grep -qiE "not found|connecting|not connected|offline|unreachable|not reachable|turned on|not on this network|waiting for" <<<"$sc" && said=1  # the screen, as a whole: this one has no file name
  # The computer comes back: the open Share screen sends by itself.
  start_desktop_service
  desk_dial >/dev/null 2>&1; link_up 90
  local sent=0 k; for ((k = 0; k < 60; k++)); do ctl share status 2>/dev/null | grep -qF "sent while the computer was off" && { sent=1; break; }; sleep 1; done
  if ((said)) && ((sent)); then rec share_offline PASS "computer off: the Share screen says so ($(grep -oiE '[^|]*(not found|connecting|not connected|offline|unreachable|not reachable|waiting for)[^|]*' <<<"$sc" | head -1 | cut -c1-80)); when it came back the text was delivered without sharing again"
  elif ((sent)); then rec share_offline FAIL "delivered later, but the screen while the computer was off gave no reason: $sc"
  elif ((said)); then rec share_offline FAIL "the screen explained the wait but the text never arrived after the computer returned: $(share_screen 1)"
  else rec share_offline FAIL "no reason on screen and nothing delivered: $sc"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_link_lost() {
  allow_receive; fresh; reset_save
  ensure_phone_link
  sender_send "$(gen_uri "lost-link.bin" $((64 * 1024 * 1024)) 66 "rate=1024")"
  local k; for ((k = 0; k < 40; k++)); do [[ "$(tx_state lost-link.bin)" == Active ]] && break; sleep 1; done
  [[ "$(tx_state lost-link.bin)" == Active ]] || { rec share_link_lost FAIL "never became active: $(share_screen 1)"; return 1; }
  service_down
  sleep 12
  local sc; sc="$(share_screen 1)"; local stl; stl="$(status_lines)"
  local words=0; grep -qiE "fail|lost|stopped|ended|could not|not connected|interrupted" <<<"$stl" && words=1
  local raw=0; grep -qE '\([a-z]+_[a-z_]+\)' <<<"$stl" && raw=1
  start_desktop_service; desk_dial >/dev/null 2>&1; link_up 90
  $ADB shell input keyevent KEYCODE_BACK; fresh
  local b; b="$(last_id 2>/dev/null)"; b="${b:-0}"
  sender_send "$(gen_uri "lost-link-retry.bin" 100000 67)"
  local retry=0; tx_wait "lost-link-retry.bin" Done 60 && retry=1
  if ((words)) && ((!raw)) && ((retry)); then rec share_link_lost PASS "the computer vanished mid-transfer: the phone said so in words ($stl); the retry after it returned worked"
  else rec share_link_lost FAIL "words=$words raw_code_shown=$raw retry=$retry; phone said: $sc"; fi
  $ADB shell input keyevent KEYCODE_BACK
}
step_receive_off() {
  ctl share allow "$PHONE_ID" off >/dev/null; fresh; reset_save
  local before; before="$(last_id)"; local rows0; rows0="$(tx_count)"
  sender_send "$(gen_uri "refused.bin" 20000 68)"
  sleep 12
  local sc; sc="$(share_screen 1)"; local stl; stl="$(status_lines)"
  local rows1; rows1="$(tx_count)"
  local hint=0; grep -qF "is not set to receive files from this phone" <<<"$stl" && hint=1
  sender_text "text to a computer that is not receiving"
  sleep 6
  local sc2; sc2="$(share_screen 1)"; local stl2; stl2="$(status_lines)"
  local texts; texts="$(ctl share status | grep -cF 'text to a computer that is not receiving')"
  local hint2=0; grep -qF "is not set to receive text and links from this phone" <<<"$stl2" && hint2=1
  # The file's wait ended with the answer, not after the two minute payload timeout.
  local waiting=0; grep -qF "waiting for the computer" <<<"$stl" && waiting=1
  ((waiting)) && hint=0
  if [[ "$rows1" == "$rows0" && -z "$(saved_files)" && "$texts" == 0 ]]; then
    if ((hint)) && ((hint2)); then rec share_receive_off PASS "receive off: nothing stored; the phone tells why in words, right away, for the file ($stl) and for the text ($stl2)"
    else rec share_receive_off FAIL "nothing stored (good), but the phone gives no clear reason: file screen [$sc] text screen [$sc2]"; fi
  else rec share_receive_off FAIL "something was stored although receive is off: rows $rows0 -> $rows1, files [$(saved_files | tr '\n' ' ')], texts $texts"; fi
  $ADB shell input keyevent KEYCODE_BACK; $ADB shell am force-stop $SENDER_PKG
  ctl share allow "$PHONE_ID" on >/dev/null
}

step_answer_declined() {
  ctl share allow "$PHONE_ID" on >/dev/null; ctl share auto-accept "$PHONE_ID" off >/dev/null; fresh; reset_save
  local before; before="$(last_id)"
  sender_send "$(gen_uri "decline-me.bin" 30000 81)"
  local id="" k; for ((k = 0; k < 30; k++)); do id="$(tx_field decline-me.bin 1 '<-')"; [[ -n "$id" && "$id" -gt "$before" ]] && break; sleep 1; done
  if [[ -z "$id" || "$id" -le "$before" ]]; then rec share_answer_declined FAIL "the offer never reached the computer: $(share_screen 1)"; allow_receive; return 1; fi
  ctl share decline "$id" >/dev/null 2>&1
  local words=0; for ((k = 0; k < 15; k++)); do status_lines | grep -qF "the computer declined it" && { words=1; break; }; sleep 1; done
  local stl; stl="$(status_lines)"
  if ((words)) && ! grep -qF "waiting for the computer" <<<"$stl" && [[ -z "$(saved_files)" ]]; then rec share_answer_declined PASS "Decline on the computer: the phone says '$stl' within seconds (no wait for the payload timeout), nothing stored"
  else rec share_answer_declined FAIL "words=$words phone said: $stl"; fi
  $ADB shell input keyevent KEYCODE_BACK; $ADB shell am force-stop $SENDER_PKG
  allow_receive
}
step_answer_saved() {
  allow_receive; fresh
  local before; before="$(last_id)"
  sender_text "a note the computer keeps"
  local row; row="$(tx_wait_new "$before" Done 40)"
  local words=0 k; for ((k = 0; k < 15; k++)); do status_lines | grep -qF "received by the computer (shown there, nothing is opened)" && { words=1; break; }; sleep 1; done
  local stt; stt="$(status_lines)"
  $ADB shell input keyevent KEYCODE_BACK; $ADB shell am force-stop $SENDER_PKG
  fresh; : >"$SHIM_LOG"
  before="$(last_id)"
  sender_text "https://example.org/kept-not-opened"
  local row2; row2="$(tx_wait_new "$before" Done 40)"
  local words2=0; for ((k = 0; k < 15; k++)); do status_lines | grep -qF "received by the computer as a link (shown there, not opened)" && { words2=1; break; }; sleep 1; done
  local stl; stl="$(status_lines)"
  sleep 1
  local opened=0; grep -q '^xdg-open\|^firefox\|^chromium\|^gio' "$SHIM_LOG" && opened=1
  if [[ -n "$row" && -n "$row2" ]] && ((words)) && ((words2)) && ((!opened)); then rec share_answer_saved PASS "text: '$stt'; link: '$stl'; the link was not opened"
  else rec share_answer_saved FAIL "rows '$row' '$row2' words=$words/$words2 opened=$opened text screen: $stt link screen: $stl"; fi
  $ADB shell input keyevent KEYCODE_BACK; $ADB shell am force-stop $SENDER_PKG
}
# Desktop -> phone
# The first-run wizard covers Home on a freshly paired phone: leave it.
leave_wizard() {
  local k; for k in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1 || tap Done >/dev/null 2>&1; sleep 1; done
}
d2p_file() { # name size seed -> path on the host
  local p="$RUN/d2p/$1"; mkdir -p "$RUN/d2p"
  python3 -I - "$p" "$2" "$3" <<'PY'
import sys
p, size, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
period = bytes(((i * 7 + seed) % 251) for i in range(251))
block = period * 4096
with open(p, "wb") as f:
    left = size
    while left > 0:
        n = min(left, len(block)); f.write(block[:n]); left -= n
PY
  echo "$p"
}
phone_download() { echo "/sdcard/Download/GeekBridge"; }
phone_sha() { $ADB shell sha256sum "$1" 2>/dev/null | tr -d '\r' | cut -d' ' -f1; }
step_d2p_accept() {
  fresh; goto_home; sleep 2; leave_wizard
  $ADB shell 'rm -f /sdcard/Download/GeekBridge/d2p-share*' >/dev/null 2>&1
  local f first_sha; f="$(d2p_file d2p-share.bin 400000 71)"; first_sha="$(sha "$f")"
  ctl send-file "$PHONE_ID" "$f" >/dev/null
  if ! waitui "wants to send a file" 25 >/dev/null; then rec share_d2p_accept FAIL "no offer dialog on the phone: $(screen_texts | head -8 | tr '\n' '|')"; return 1; fi
  local dlg; dlg="$(screen_texts | tr '\n' '|')"
  tap Accept >/dev/null 2>&1
  local ok=0 k; for ((k = 0; k < 40; k++)); do [[ "$(tx_state d2p-share.bin '->')" == Done ]] && { ok=1; break; }; sleep 1; done
  sleep 2
  if ((ok)) && [[ "$(phone_sha /sdcard/Download/GeekBridge/d2p-share.bin)" == "$(sha "$f")" ]]; then rec share_d2p_accept PASS "computer file offered to the phone (dialog: $(grep -oE '[^|]*wants to send[^|]*' <<<"$dlg" | head -1 | cut -c1-60)): Accept -> saved to Downloads/GeekBridge, sha256 equal"
  else rec share_d2p_accept FAIL "state $(tx_state d2p-share.bin '->'); phone sha $(phone_sha /sdcard/Download/GeekBridge/d2p-share.bin | cut -c1-12) vs $(sha "$f" | cut -c1-12)"; fi
  # Offered again with the same name: a unique name, the first copy untouched.
  local f2; f2="$(d2p_file d2p-share.bin 300000 72)"
  ctl send-file "$PHONE_ID" "$f2" >/dev/null
  if waitui "wants to send a file" 25 >/dev/null; then tap Accept >/dev/null 2>&1; fi
  sleep 8
  local names; names="$($ADB shell ls /sdcard/Download/GeekBridge/ 2>/dev/null | tr -d '\r' | grep '^d2p-share' | sort | tr '\n' ' ')"
  local first_ok=0; [[ "$(phone_sha /sdcard/Download/GeekBridge/d2p-share.bin)" == "$first_sha" ]] && first_ok=1
  local nfiles; nfiles="$($ADB shell ls /sdcard/Download/GeekBridge/ 2>/dev/null | tr -d '\r' | grep -c '^d2p-share')"
  if [[ "$nfiles" == 2 ]] && ((first_ok)); then rec share_d2p_unique PASS "a second offer with the same name got a unique name on the phone ($names), the first copy is intact"
  else rec share_d2p_unique FAIL "phone files: [$names], first copy intact: $first_ok"; fi
}
step_d2p_decline() {
  fresh; goto_home; sleep 2; leave_wizard
  local f; f="$(d2p_file d2p-decline.bin 200000 73)"
  ctl send-file "$PHONE_ID" "$f" >/dev/null
  waitui "wants to send a file" 25 >/dev/null || { rec share_d2p_decline FAIL "no offer dialog"; return 1; }
  tap Decline >/dev/null 2>&1; sleep 6
  local st; st="$(tx_state d2p-decline.bin '->')"
  local there; there="$($ADB shell ls /sdcard/Download/GeekBridge/ 2>/dev/null | tr -d '\r' | grep -c '^d2p-decline')"
  if [[ "$there" == 0 ]]; then rec share_d2p_decline PASS "Decline on the phone: nothing saved there; the computer shows '$st' (the computer's side of a decline is only visible as a timeout, no refusal message exists in the protocol)"
  else rec share_d2p_decline FAIL "a file was saved after Decline"; fi
  # Cancel by the computer mid-transfer after Accept.
  local big; big="$(d2p_file d2p-cancel.bin $((120 * 1024 * 1024)) 74)"
  ctl send-file "$PHONE_ID" "$big" >/dev/null
  waitui "wants to send a file" 25 >/dev/null || { rec share_d2p_cancel FAIL "no offer dialog for the big file"; return 1; }
  tap Accept >/dev/null 2>&1
  local k; for ((k = 0; k < 30; k++)); do [[ "$(tx_state d2p-cancel.bin '->')" == Active ]] && break; sleep 1; done
  local id; id="$(tx_field d2p-cancel.bin 1 '->')"
  ctl share cancel "$id" >/dev/null 2>&1; sleep 6
  local st2; st2="$(tx_state d2p-cancel.bin '->')"
  local residue; residue="$($ADB shell ls /sdcard/Download/GeekBridge/ 2>/dev/null | tr -d '\r' | grep '^d2p-cancel' | tr '\n' ' ')"
  local pend; pend="$($ADB shell content query --uri content://media/external/downloads --projection _display_name:is_pending --where "\"_display_name LIKE 'd2p-cancel%'\"" 2>/dev/null | tr -d '\r' | grep -c 'is_pending=0')"
  if [[ ( "$st2" == Cancelled || "$st2" == Failed ) && "$pend" == 0 ]]; then rec share_d2p_cancel PASS "computer cancels mid-transfer after Accept: $st2; no completed partial file on the phone (listing [$residue])"
  else rec share_d2p_cancel FAIL "state $st2; a completed-looking file remains: [$residue] pending=0 rows $pend"; fi
  rm -f "$big"
}
step_d2p_link() {
  fresh; goto_home; sleep 2; leave_wizard
  ctl send-url "$PHONE_ID" "https://example.org/from-the-computer" >/dev/null
  if waitui "https://example.org/from-the-computer" 25 >/dev/null; then
    local sc; sc="$(screen_texts | tr '\n' '|')"
    if grep -qF "Open" <<<"$sc" && grep -qF "Close" <<<"$sc"; then rec share_d2p_link PASS "a link from the computer is shown on the phone with Open (to the browser, on tap only) and Close"
    else rec share_d2p_link FAIL "link shown without Open/Close: $sc"; fi
    tap Close >/dev/null 2>&1
    ctl send-text "$PHONE_ID" "a note from the computer" >/dev/null
    if waitui "a note from the computer" 25 >/dev/null && screen_has "Copy"; then rec share_d2p_text PASS "a text from the computer is shown on the phone with Copy"
    else rec share_d2p_text FAIL "no text dialog with Copy: $(screen_texts | head -6 | tr '\n' '|')"; fi
    tap Close >/dev/null 2>&1
  else rec share_d2p_link FAIL "no link dialog on the phone: $(screen_texts | head -8 | tr '\n' '|')"; fi
}

stage_share() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  link_up 120 || { rec share_stage FAIL "no link to the phone"; return 1; }
  ctl consent "$PHONE_ID" clipboard off >/dev/null 2>&1
  set_state_bool setup_finished true; phone_restart >/dev/null 2>&1   # no first-run wizard over Home
  local s
  for s in text url mediastore provider_uri multiple names duplicates grant_revoked large survives_screen cancel_phone \
           survives_background_off second_share cancel_desktop offline link_lost receive_off answer_declined answer_saved d2p_accept d2p_decline d2p_link; do
    step_wanted "$s" || continue
    log "== step $s"
    "step_$s" || true
    is_connected || { desk_dial >/dev/null 2>&1; link_up 60; } || true
  done
  allow_receive
}

main_share() {
  mkdir -p "$LOGS"; : >"$LOGS/e2e-run.log"
  if [[ "$STOPPING" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    stop_desktop; return 0
  fi
  [[ "$STAGES" == env,pair,share ]] && : >"$RESULTS"
  local st sts
  IFS=, read -r -a sts <<<"$STAGES"
  for st in "${sts[@]}"; do log "== stage $st"; "stage_$st" || log "stage $st reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_share
