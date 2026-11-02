#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Real end-to-end test: Android emulator app <-> desktop geekbridge-service on this host.
#
#   scripts/e2e-android.sh [--only STAGE[,STAGE...]] [--no-build] [--rebuild] [--stop]
#
# Stages (run in this order by default; each is idempotent and records PASS/FAIL/SKIP in
# dist/android/logs/e2e-results.tsv):
#   env        private Xvfb clipboard, desktop service, dedicated emulator, build + install the debug APK
#   battery    setup wizard and the battery-exemption prompt (Deny, then Allow)
#   pair       desktop dials the phone (adb emu redir), both sides compare the code and approve
#   reconnect  disconnect + trusted reconnect, app restart
#   clip_d2p   desktop clipboard -> phone
#   clip_p2d   phone clipboard -> desktop (30 s window, locked-phone rule)
#   file_d2p   desktop send-file (+ text) -> phone
#   file_p2d   Share sheet (adb am start ACTION_SEND) -> desktop
#   update     update check against a local mock release server
#
# Isolation: its own AVD (gb36, console port 5594, adb server 16038), its own desktop state dir
# (target/e2e-run), control port 17481, a private Xvfb (:97) so the real user clipboard is never touched,
# and a scratch copy of android/ for Gradle (so a concurrent build elsewhere cannot race on android/app/build).
# Nothing is committed or installed globally. --stop stops what this script started.
# Screenshots: dist/screenshots/android/e2e-*.png   Logs: dist/android/logs/e2e-*
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1

STAGES_ALL=(env battery pair reconnect clip_d2p clip_p2d file_d2p file_p2d update)
ONLY="" BUILD=auto
for a in "$@"; do
  case "$a" in
    --only) ;; --no-build) BUILD=no ;; --rebuild) BUILD=yes ;; --stop) STOP=1 ;;
    -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) if [[ "${prev:-}" == --only ]]; then ONLY="$a"; else echo "unknown argument $a" >&2; exit 2; fi ;;
  esac
  prev="$a"
done

RUN="${E2E_RUN:-$ROOT/target/e2e-run}" SCRATCH="${E2E_SCRATCH:-$ROOT/target/e2e-android}"
LOGS="$ROOT/dist/android/logs" SHOTS="$ROOT/dist/screenshots/android"
RESULTS="${E2E_RESULTS:-$LOGS/e2e-results.tsv}"
mkdir -p "$RUN" "$LOGS" "$SHOTS"
chmod 700 "$RUN"
export ANDROID_ADB_SERVER_PORT="${E2E_ADB_PORT:-16038}"
AVD="${E2E_AVD:-gb36}" CONSOLE_PORT="${E2E_EMU_PORT:-5594}" SERIAL="emulator-${E2E_EMU_PORT:-5594}"
export ADB="adb -s $SERIAL"
PKG=dev.geekbridge.android
XDISP="${E2E_XDISP:-:97}" CTRL_PORT="${E2E_CTRL_PORT:-17481}" API_PORT="${E2E_API_PORT:-17480}" DISC_PORT="${E2E_DISC_PORT:-17416}" LISTEN_PORT="${E2E_LISTEN_PORT:-1730}"
E2E_TARGET="${E2E_TARGET:-$ROOT/target/e2e}"
CTL=("$E2E_TARGET/release/geekbridge-ctl" --state-dir "$RUN/state" --control-port "$CTRL_PORT")
SVC="$E2E_TARGET/release/geekbridge-service"
APK="$SCRATCH/dist/geekbridge-debug.apk"
UI="$ROOT/scripts/e2e/adbui.py"
MOCK_PORT=18890

log() { printf '[e2e %s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOGS/e2e-run.log" >&2; }
rec() { # stage name PASS|FAIL|SKIP detail
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:-}" | tee -a "$RESULTS" >&2
}
shot() { $ADB exec-out screencap -p > "$SHOTS/e2e-$1.png" 2>/dev/null; }
ui() { timeout 90 "$UI" "$@"; }
tap() { ui tap "$@"; }
waitui() { ui wait "$@"; }
wake() { $ADB shell input keyevent KEYCODE_WAKEUP; $ADB shell wm dismiss-keyguard >/dev/null 2>&1; }
ctl() { timeout 20 "${CTL[@]}" "$@"; }
PHONE_ID="" DESK_ID=""
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
desk_id() { DESK_ID="$(ctl status | jget 'd["device_id"]')"; }
paired_ids() { ctl status | jget '" ".join(p["device_id"] for p in d["paired"])'; }
link_phases() { ctl status | jget '" ".join(l["phase"] for l in d["links"])'; }
wait_for() { # seconds cmd...  -> 0 when cmd succeeds
  local n="$1"; shift
  for ((i = 0; i < n; i++)); do "$@" && return 0; sleep 1; done; return 1
}

# ---------------------------------------------------------------- env
start_xvfb() {
  [[ -S /tmp/.X11-unix/X${XDISP#:} ]] && return 0
  (setsid nohup Xvfb "$XDISP" -screen 0 800x600x24 >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S /tmp/.X11-unix/X${XDISP#:}
}
start_service() {
  # A service left over from an earlier run may be an older binary: a rebuild replaces it.
  [[ "$BUILD" == yes ]] && pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null && sleep 1
  ctl status >/dev/null 2>&1 && return 0
  mkdir -p "$RUN/state"; chmod 700 "$RUN/state"
  # No WAYLAND_DISPLAY / Hyprland: the service then uses xclip on the private Xvfb (never the user's clipboard).
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE DISPLAY="$XDISP" GEEKBRIDGE_NO_UPDATE_CHECK=1 \
     setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" --control-port "$CTRL_PORT" \
     --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name E2E-Desktop >>"$LOGS/e2e-service.log" 2>&1 &)
  wait_for 15 ctl status >/dev/null 2>&1
}
start_emulator() {
  [[ "$($ADB get-state 2>/dev/null)" == device ]] && return 0
  mkdir -p "$ROOT/.tools/e2e-emu/tmp"
  local envs=(GB_EMU_AVD="$AVD" GB_EMU_PORT="$CONSOLE_PORT" GB_EMU_ADB_PORT="$ANDROID_ADB_SERVER_PORT" GB_EMU_TMPDIR="$ROOT/.tools/e2e-emu/tmp")
  # A transient systemd user unit keeps the emulator out of this process tree (see scripts/emulator-call.sh).
  if command -v systemd-run >/dev/null 2>&1 && systemd-run --user --collect --quiet --unit "gbe2e-emulator${E2E_TAG:-}" \
       "${envs[@]/#/--setenv=}" "$ROOT/.tools/gbcall/start.sh" >/dev/null 2>&1; then :
  else (env "${envs[@]}" setsid nohup "$ROOT/.tools/gbcall/start.sh" >"$LOGS/e2e-emulator.log" 2>&1 &); fi
  wait_for 300 bash -c "[[ \$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]]"
}
build_all() {
  if [[ ! -x "$SVC" || "$BUILD" == yes ]]; then
    log "building desktop service/ctl (target/e2e)"
    CARGO_TARGET_DIR="$E2E_TARGET" cargo build --release -p bridge-service --bins >"$LOGS/e2e-build-desktop.log" 2>&1 || return 1
  fi
  if [[ ! -f "$APK" || "$BUILD" == yes ]]; then
    log "building the debug APK with the native core (scratch copy; target/e2e-android)"
    mkdir -p "$SCRATCH"; cp "$ROOT/Cargo.toml" "$SCRATCH/Cargo.toml"
    rsync -a --delete --exclude build --exclude .gradle --exclude .kotlin android/ "$SCRATCH/android/"
    GB_ANDROID_DIR="$SCRATCH/android" GB_ANDROID_DIST_DIR="$SCRATCH/dist" \
      scripts/build-android.sh --skip-tests --skip-lint --skip-host-tests >"$LOGS/e2e-build-android.log" 2>&1 || return 1
  fi
}
stage_env() {
  [[ "${STOP:-}" == 1 ]] && return 0
  [[ "$BUILD" != no ]] && { build_all || { rec env FAIL "build failed, see dist/android/logs/e2e-build-*.log"; return 1; }; }
  start_xvfb || { rec env FAIL "Xvfb did not start"; return 1; }
  start_service || { rec env FAIL "desktop service did not start"; return 1; }
  start_emulator || { rec env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || { rec env FAIL "adb install failed"; return 1; }
  # Dedicated desktop clipboard starts empty.
  printf '' | DISPLAY="$XDISP" xclip -selection clipboard -i
  desk_id
  rec env PASS "APK $(sha256sum "$APK" | cut -c1-12) installed on $SERIAL; desktop $DESK_ID"
}

# ---------------------------------------------------------------- shared navigation
goto_home() { $ADB shell am start -f 0x04000000 -n "$PKG/.HomeActivity" >/dev/null; sleep 2; }
goto_diag() {
  goto_home
  ui scrolltap "Diagnostics & about" || return 1
  waitui "THIS PHONE AND CONNECTION" 15
}
phone_id() { # reads the Device id row in Diagnostics
  ui scrollto "Device id" >/dev/null && ui dump | awk -F'\t' '$1 ~ /^[0-9a-f]{32}$/ {print $1; exit}'
}
link_running() { ui has "Stop link"; }
ensure_link() {
  goto_diag || return 1
  local which; which="$(ui scrollto "Start link" "Stop link")" || return 1
  if [[ "$which" == "Start link" ]]; then tap "Start link"; sleep 3; ui scrollto "Stop link" >/dev/null; fi
}
dev_options_open() { # manual connect lives behind the collapsed "Developer options" expander
  ui scrollto "Developer options" >/dev/null || return 1   # bring the header on screen first
  ui has "Developer options, expanded" || { tap "Developer options"; sleep 1; }
}
phone_connect() { # dial the desktop (10.0.2.2 is the emulator's alias for the host's loopback)
  dev_options_open; ui scrollto Connect || return 1
  ui fields 10.0.2.2 "$LISTEN_PORT" "$DESK_ID"
  ui scrollto Connect >/dev/null; tap Connect
}

# ---------------------------------------------------------------- battery / setup wizard
stage_battery() {
  $ADB shell cmd deviceidle whitelist -$PKG >/dev/null 2>&1
  # A wiped phone gets a new identity; forget the desktop's trust in earlier test phones.
  for old in $(paired_ids); do ctl unpair "$old" >/dev/null 2>&1; done
  $ADB shell pm clear $PKG >/dev/null; wake; goto_home
  shot 01-setup-first-launch
  waitui "Step 1 of" 15 || { rec battery FAIL "setup wizard did not open on a fresh install"; return 1; }
  local notif=skip
  # The wizard's length changes with the UI (7 steps, later 9): walk it by step title, not by number.
  for _ in $(seq 1 14); do
    if ui dump | cut -f1 | grep -qx "Battery"; then break; fi
    if ui dump | cut -f1 | grep -qx "Notifications" && tap Allow; then
      sleep 2; shot 02-notification-permission-dialog
      tap Allow >/dev/null 2>&1; sleep 2
      $ADB shell dumpsys package $PKG | grep -q 'POST_NOTIFICATIONS: granted=true' && notif=granted || notif=not-granted
    fi
    tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1; sleep 2
  done
  ui dump | cut -f1 | grep -qx "Battery" || { rec battery FAIL "never reached the Battery step"; return 1; }
  shot 04-battery-step
  tap "Allow unrestricted battery"; sleep 3
  if ! ui has "Let app always run in background?"; then rec battery FAIL "system battery dialog did not appear"; return 1; fi
  shot 05-battery-system-dialog
  tap Deny; sleep 2; shot 05b-battery-after-deny
  local denied_ok=0; ui has "Allow unrestricted battery" && denied_ok=1
  tap "Allow unrestricted battery"; sleep 2; tap Allow; sleep 2; shot 05c-battery-after-allow
  local allowed_ok=0; ui has "Unrestricted" && allowed_ok=1
  local wl=0; $ADB shell dumpsys deviceidle whitelist | grep -q "$PKG" && wl=1
  tap Overview; sleep 2; shot 06-setup-overview; tap Finish; sleep 2; shot 06-home
  if ((denied_ok && allowed_ok && wl)); then rec battery PASS "deny keeps the prompt, allow -> Unrestricted + Doze whitelist; notifications permission: $notif"
  else rec battery FAIL "deny_ok=$denied_ok allow_ok=$allowed_ok whitelist=$wl"; fi
}

# ---------------------------------------------------------------- pairing
is_paired() { [[ " $(paired_ids) " == *" $PHONE_ID "* ]]; }
has_unpaired_link() { [[ "$(link_phases)" == *unpaired* ]]; }
is_connected() { [[ "$(link_phases)" == *connected* ]]; }
REDIR_PORT="${E2E_REDIR_PORT:-17161}"
# The desktop dials the phone through the emulator's port redirection (host :17161 -> guest :1716). The phone sees
# the NAT gateway 10.0.2.2 as the peer, a private address, so the app's default options (no loopback) work.
desk_dial() {
  $ADB emu redir add "tcp:$REDIR_PORT:1716" >/dev/null 2>&1
  # Phone -> desktop file payloads: the phone listens on the first free port of 1739-1764 and the desktop dials it at
  # 127.0.0.1:<port>, so those host ports are forwarded (E2E-7). The desktop's own payload listener (desktop -> phone)
  # simply takes the first port after these, and the phone dials it at 10.0.2.2:<port>. A port another emulator on this
  # host already owns (E2E-11) is kept busy inside this guest, so the phone cannot pick it and be dialled on the wrong phone.
  local p mine=0
  for ((p = 1739; p <= 1764 && mine < 4; p++)); do
    if $ADB emu redir add "tcp:$p:$p" 2>&1 | grep -q '^OK' || $ADB emu redir list 2>/dev/null | grep -q "tcp:$p  *=> *$p"; then
      mine=$((mine + 1))
    else
      $ADB shell "nohup nc -l -p $p >/dev/null 2>&1 </dev/null &" >/dev/null 2>&1
    fi
  done
  ctl connect "$PHONE_ID" 127.0.0.1 "$REDIR_PORT" >/dev/null
}
load_ids() { desk_id; PHONE_ID="$(cat "$RUN/phone-id" 2>/dev/null)"; }
stage_pair() {
  desk_id; goto_diag || { rec pair FAIL "cannot open Diagnostics"; return 1; }
  ensure_link || { rec pair FAIL "link did not start (see E2E-1)"; return 1; }
  shot 10-link-started
  PHONE_ID="$(phone_id)"; [[ -n "$PHONE_ID" ]] || { rec pair FAIL "no phone device id"; return 1; }
  echo "$PHONE_ID" >"$RUN/phone-id"
  if is_paired; then
    ctl unpair "$PHONE_ID" >/dev/null 2>&1; sleep 3
    # Observed (E2E-3): the phone stops listening when its last computer unpairs; start it again.
    ensure_link || { rec pair FAIL "could not restart the phone link after unpair"; return 1; }
  fi
  desk_dial; wait_for 10 has_unpaired_link \
    || { rec pair FAIL "desktop could not open the TLS link to the phone"; return 1; }
  ctl pair "$PHONE_ID" request >/dev/null
  wait_for 15 ui code >/dev/null || { rec pair FAIL "no pairing dialog with a code on the phone"; shot 13-pair-failed; return 1; }
  shot 13-pair-prompt
  local code id dcode
  code="$(ui code)"
  id="$(ctl status | jget 'd["challenges"][0]["challenge_id"]')"
  dcode="$(ctl status | jget 'd["challenges"][0]["verification_key"]')"
  [[ -n "$code" && "$code" == "$dcode" ]] || { rec pair FAIL "verification codes differ: phone=$code desktop=$dcode"; return 1; }
  ctl approve "$id" "$code" >/dev/null || { rec pair FAIL "desktop approve refused"; return 1; }
  tap Pair; sleep 4
  if wait_for 15 is_paired; then
    shot 14-paired
    local pong=ok; ctl ping "$PHONE_ID" >/dev/null 2>&1 || pong=FAILED
    rec pair PASS "code $code matched; both sides approved; desktop trusts $PHONE_ID; ping $pong; negotiated: $(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  else rec pair FAIL "desktop never listed the phone as paired"; shot 14-paired-failed; return 1; fi
}

# ---------------------------------------------------------------- reconnect
stage_reconnect() {
  load_ids; [[ -n "$PHONE_ID" ]] || { rec reconnect SKIP "not paired (run pair first)"; return 1; }
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 3
  [[ -z "$(link_phases)" ]] || { rec reconnect FAIL "link still present after desktop disconnect"; return 1; }
  desk_dial; wait_for 15 is_connected; shot 15-reconnected
  if is_connected && ! ui code >/dev/null 2>&1; then rec reconnect PASS "trusted reconnect without a new code; phases=[$(link_phases)]"
  else rec reconnect FAIL "phases=[$(link_phases)] (a pairing dialog means trust was not remembered)"; fi
  # The phone app is killed and reopened: a paired computer must come back on a fresh dial, still without a code.
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1
  $ADB shell am force-stop $PKG; sleep 2; goto_home; sleep 8; shot 16-after-app-restart
  desk_dial; wait_for 15 is_connected
  if is_connected && ! ui code >/dev/null 2>&1; then rec reconnect_after_restart PASS "paired state survived an app restart; desktop redial connected without a code"
  else rec reconnect_after_restart FAIL "phases=[$(link_phases)] after app restart (does the Home screen start the listener for a paired computer?)"; fi
  # Persistence on the desktop side too: restart the service, trust must remain.
  paired_ids | grep -q "$PHONE_ID" && rec reconnect_trust_listed PASS "desktop still lists the phone as paired"
}

# ---------------------------------------------------------------- session helpers for the feature stages
# A Home-started link (the app starts it for a paired computer) advertises clipboard; the Diagnostics link does not.
clear_dialogs() { tap Decline >/dev/null 2>&1; tap Close >/dev/null 2>&1; true; }
ensure_session() {
  load_ids; [[ -n "$PHONE_ID" ]] || return 1
  clear_dialogs
  # Always redial: after the Share chooser the phone's link is stopped while the desktop still lists it as
  # connected (E2E-4), so "connected" on the desktop proves nothing.
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  goto_home; sleep 3; desk_dial; wait_for 15 is_connected
}
phone_clip_sync_on() {
  # E2E-6: the switch can read "On" while the clipboard engine ignores pushes and answers pulls with
  # "Clipboard sync is off in the phone app"; cycling Off -> On re-arms it. Always cycle.
  goto_home; ui scrollto "Clipboard sync" >/dev/null || return 1
  $ADB shell input swipe 540 1800 540 1200 300; sleep 1   # the row stops under the gesture bar; lift it into reach
  ui has "On. Your computer can ask" && { ui taprow "Clipboard sync" 960; sleep 2; }
  ui taprow "Clipboard sync" 960; sleep 2
  ui has "On. Your computer can ask"
}
phone_copy_nonsensitive() { # text; copy it from a system text field (the app's own Copy marks text sensitive)
  $ADB shell am start -a android.settings.APP_SEARCH_SETTINGS >/dev/null 2>&1; sleep 3
  $ADB shell input tap 600 220
  $ADB shell input keycombination KEYCODE_CTRL_LEFT KEYCODE_A; $ADB shell input keyevent KEYCODE_DEL  # the field keeps old text
  $ADB shell input text "$1"
  $ADB shell input keycombination KEYCODE_CTRL_LEFT KEYCODE_A
  $ADB shell input keycombination KEYCODE_CTRL_LEFT KEYCODE_C
}
# Desktop "paste from phone": the phone answers at once if it remembered a recent copy while focused, otherwise it
# posts "tap to send" (the only way a non-focused app may read the clipboard); tap that notification if it shows up.
pull_with_tap() { # outfile
  "${CTL[@]}" clipboard paste-from-phone >"$1" 2>&1 & local pid=$!
  sleep 5
  if kill -0 $pid 2>/dev/null; then
    $ADB shell cmd statusbar expand-notifications; sleep 2
    tap "Your computer wants your clipboard" >/dev/null 2>&1; sleep 5
    $ADB shell cmd statusbar collapse
  fi
  wait_for 20 bash -c "! kill -0 $pid 2>/dev/null"; kill $pid 2>/dev/null; return 0
}
desk_clip() { DISPLAY="$XDISP" xclip -selection clipboard -o 2>/dev/null; }

stage_clip_d2p() {
  ensure_session || { rec clip_d2p SKIP "no paired, connected phone (run pair)"; return 1; }
  [[ "$(ctl status | jget '",".join(d["links"][0]["negotiated"])')" == *kdeconnect.clipboard* ]] \
    || { rec clip_d2p FAIL "link did not negotiate clipboard"; return 1; }
  phone_clip_sync_on || { rec clip_d2p FAIL "could not turn on Clipboard sync on the phone"; return 1; }
  shot 18-clipboard-settings
  ctl consent "$PHONE_ID" clipboard on >/dev/null; sleep 3   # the watcher primes its baseline when consent is given
  local t="e2e-d2p-$(date +%s)"
  printf '%s' "$t" | DISPLAY="$XDISP" xclip -selection clipboard -i; sleep 4
  goto_diag; ui scrollto Paste >/dev/null; tap Paste; sleep 2; shot 19-clip-d2p-pasted
  if ui has "$t"; then rec clip_d2p PASS "desktop clipboard text arrived on the phone clipboard"
  else rec clip_d2p FAIL "phone clipboard did not receive '$t'"; fi
}

stage_clip_p2d() {
  ensure_session || { rec clip_p2d SKIP "no paired, connected phone"; return 1; }
  phone_clip_sync_on >/dev/null; ctl consent "$PHONE_ID" clipboard on >/dev/null
  local t="e2e-p2d-$(date +%s)"
  phone_copy_nonsensitive "$t"; shot 20-phone-copy-source
  goto_home; sleep 3; printf 'stale' | DISPLAY="$XDISP" xclip -selection clipboard -i
  pull_with_tap "$RUN/pull-fresh.out"
  if [[ "$(desk_clip)" == "$t" ]]; then rec clip_p2d PASS "fresh item, unlocked phone, pulled onto the desktop clipboard ($(head -c 60 "$RUN/pull-fresh.out"))"
  else rec clip_p2d FAIL "desktop clipboard is '$(desk_clip)' (wanted $t); pull said: $(head -c 100 "$RUN/pull-fresh.out")"; fi
  # 30 s window: an item older than the window must not be sent (the phone only asks for a tap, and refuses stale items).
  local old="e2e-old-$(date +%s)"; phone_copy_nonsensitive "$old"; goto_home; sleep 40
  (timeout 80 "${CTL[@]}" clipboard paste-from-phone >"$RUN/pull-stale.out" 2>&1 &); sleep 2
  $ADB shell cmd statusbar expand-notifications; sleep 2; tap "Your computer wants your clipboard" >/dev/null 2>&1; sleep 5
  shot 21-clip-stale
  if [[ "$(desk_clip)" != "$old" ]] && grep -q 'Nothing recent' "$RUN/pull-stale.out"; then rec clip_p2d_window PASS "item older than 30 s refused: $(head -c 90 "$RUN/pull-stale.out")"
  else rec clip_p2d_window FAIL "stale item reached the desktop or no refusal: $(head -c 120 "$RUN/pull-stale.out")"; fi
  # Unlocked rule: screen off -> the phone answers 'locked' without looking at the clipboard.
  local lk="e2e-locked-$(date +%s)"; phone_copy_nonsensitive "$lk"
  $ADB shell input keyevent KEYCODE_SLEEP; sleep 2
  local out; out="$(timeout 30 "${CTL[@]}" clipboard paste-from-phone 2>&1)"
  wake; sleep 2; shot 22-after-unlock
  if [[ "$(desk_clip)" != "$lk" && "$out" == *locked* ]]; then rec clip_p2d_locked PASS "locked phone refused: $out"
  else rec clip_p2d_locked FAIL "got: $out"; fi
}

# Files land in the desktop's real save folder (~/Downloads/GeekBridge); the test removes what it created.
stage_file_p2d() {
  # A Share screen left over from an earlier attempt would be reused (singleTop) with its old session; start clean.
  $ADB shell am force-stop $PKG; sleep 1
  ensure_session || { rec file_p2d SKIP "no paired, connected phone"; return 1; }
  ctl share allow "$PHONE_ID" on >/dev/null; ctl share auto-accept "$PHONE_ID" on >/dev/null
  head -c 200000 /dev/urandom | cat <(printf '\x89PNG\r\n\x1a\n') - >"$RUN/e2e-p2d.png"
  local sum; sum="$(sha256sum "$RUN/e2e-p2d.png" | cut -d' ' -f1)"
  $ADB push "$RUN/e2e-p2d.png" /sdcard/Download/e2e-p2d.png >/dev/null
  $ADB shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file:///sdcard/Download/e2e-p2d.png >/dev/null; sleep 2
  local id; id="$($ADB shell "content query --uri content://media/external/images/media --projection _id --where \"_display_name='e2e-p2d.png'\"" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  [[ -n "$id" ]] || { rec file_p2d FAIL "MediaStore did not index the test image"; return 1; }
  goto_home
  # The chooser's "Just once" picks the first GeekBridge it finds: a leftover build under another package id
  # (dev.geekbridge.android.me on the old AVD) used to win it, which was the whole of E2E-4. Remove such builds.
  for other in $($ADB shell pm list packages 2>/dev/null | tr -d '\r' | sed -n 's/^package://p' | grep '^dev\.geekbridge\.' | grep -vx "$PKG"); do
    $ADB uninstall "$other" >/dev/null 2>&1
  done
  $ADB shell am start -a android.intent.action.SEND -t image/png --eu android.intent.extra.STREAM "content://media/external/images/media/$id" --grant-read-uri-permission >/dev/null
  sleep 3; shot 25-share-sheet
  tap "Send to computer" >/dev/null 2>&1; tap "Just once" >/dev/null 2>&1
  sleep 10; shot 26-share-sending
  local line; line="$(ctl share | grep 'e2e-p2d.png' | tail -1)"
  local got; got="$(ls -t "$HOME"/Downloads/GeekBridge/e2e-p2d*.png 2>/dev/null | head -1)"
  if [[ -n "$got" && "$(sha256sum "$got" | cut -d' ' -f1)" == "$sum" ]]; then rec file_p2d PASS "Share sheet -> desktop, SHA-256 equal ($got)"; rm -f "$got"
  else
    local why="transfer: ${line:-none}"
    ui has "The link is not running" && why="Share screen says 'The link is not running' while the desktop still shows the link connected (E2E-4)"
    rec file_p2d FAIL "$why"
    tap Cancel >/dev/null 2>&1
  fi
}

stage_file_d2p() {
  ensure_session || { rec file_d2p SKIP "no paired, connected phone"; return 1; }
  goto_home; head -c 300000 /dev/urandom >"$RUN/e2e-d2p-file.bin"
  # Media files survive an uninstall; an older copy makes Android save the new one as "name (1).bin".
  $ADB shell 'rm -f "/sdcard/Download/GeekBridge/e2e-d2p-file"*' >/dev/null 2>&1
  ctl send-file "$PHONE_ID" "$RUN/e2e-d2p-file.bin" >/dev/null
  waitui "wants to send a file" 20 || { rec file_d2p FAIL "no offer dialog on the phone"; return 1; }
  shot 23-file-offer-on-phone
  tap Accept >/dev/null 2>&1; sleep 15; shot 24-file-received-on-phone
  local line; line="$(ctl share | grep 'e2e-d2p-file.bin' | tail -1)"
  local want; want="$(sha256sum "$RUN/e2e-d2p-file.bin" | cut -d' ' -f1)"
  local have; have="$($ADB shell sha256sum /sdcard/Download/GeekBridge/e2e-d2p-file.bin 2>/dev/null | cut -d' ' -f1)"
  if [[ -n "$have" && "$have" == "$want" ]]; then rec file_d2p PASS "desktop -> phone, accepted on the phone, SHA-256 equal"
  else rec file_d2p FAIL "transfer: ${line:-none}; phone never opened the payload connection (E2E-5)"; fi
  # Text/link share uses no payload connection and does work: record it separately.
  goto_home; ctl send-text "$PHONE_ID" "hello-e2e" >/dev/null; sleep 4; shot 27-text-received
  if ui has "hello-e2e"; then rec share_text_d2p PASS "text offer shown on the phone"; tap Close >/dev/null 2>&1
  else rec share_text_d2p FAIL "no text dialog on the phone"; fi
}

# ---------------------------------------------------------------- update check against a local mock release server
stage_update() {
  for p in $(pgrep -f "^python3 -I scripts/e2e/mock_release_server"); do kill "$p" 2>/dev/null; done
  cp "$APK" "$RUN/mock-release.apk"
  (setsid nohup python3 -I scripts/e2e/mock_release_server.py "$MOCK_PORT" "$RUN/mock-release.apk" v99.0.0 >"$LOGS/e2e-mock-release.log" 2>&1 &); sleep 1
  $ADB reverse "tcp:$MOCK_PORT" "tcp:$MOCK_PORT" >/dev/null
  $ADB shell am force-stop $PKG
  # Debug builds read this preference (UpdateChecker.forBuild); it needs the debug-only cleartext config in src/debug.
  printf "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n    <string name=\"debug_base_url\">http://127.0.0.1:%s</string>\n    <boolean name=\"auto_check\" value=\"false\" />\n</map>\n" "$MOCK_PORT" >"$RUN/updates.xml"
  $ADB push "$RUN/updates.xml" /data/local/tmp/updates.xml >/dev/null
  $ADB shell run-as $PKG cp /data/local/tmp/updates.xml shared_prefs/updates.xml
  goto_diag; ui scrollto "Check for updates" >/dev/null; tap "Check for updates"; sleep 5; shot 30-update-available
  ui has "Version 99.0.0 is available" || { rec update FAIL "no offer shown: $(ui dump | grep -iE "Couldn|GitHub|up to date" | head -1 | cut -c1-100)"; return 1; }
  # The reskinned Diagnostics screen is taller than the display: the buttons sit below the fold.
  ui scrolltap "Download and verify" >/dev/null; sleep 8; ui scrollto "Install update" >/dev/null; shot 31-update-verified
  ui has "downloaded and verified against the published SHA-256" || { rec update FAIL "download/verify did not complete"; return 1; }
  ui scrolltap "Install update" >/dev/null; sleep 4; shot 32-update-install-handoff
  local focus; focus="$($ADB shell dumpsys window | grep mCurrentFocus)"
  $ADB shell input keyevent KEYCODE_BACK; $ADB shell input keyevent KEYCODE_BACK
  if [[ "$focus" == *settings* || "$focus" == *packageinstaller* ]]; then rec update PASS "check -> v99.0.0 offer -> download + SHA-256 verified -> Android install handoff ($focus); the install itself is not completed"
  else rec update FAIL "no installer / unknown-sources screen: $focus"; fi
}

# ---- STAGES-APPEND-HERE
# ---- MAIN
main() {
  : >"$LOGS/e2e-run.log"
  [[ -z "$ONLY" ]] && : >"$RESULTS"   # a full run starts a fresh results table; --only appends
  if [[ "${STOP:-}" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    ctl status >/dev/null 2>&1 && { :; }
    pkill -f "$SVC" 2>/dev/null; rm -f /tmp/.X11-unix/X${XDISP#:}; pkill -f "Xvfb $XDISP" 2>/dev/null; return 0
  fi
  local stages=("${STAGES_ALL[@]}")
  [[ -n "$ONLY" ]] && IFS=, read -r -a stages <<<"$ONLY"
  [[ "${stages[0]}" != env && ! -f "$RUN/phone-id" ]] || true
  for st in "${stages[@]}"; do
    log "== stage $st"
    "stage_$st" || log "stage $st reported a failure"
  done
  echo; echo "== results =="; column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
# Sourced by other scripts (scripts/e2e-phonedata.sh) to reuse the helpers and stages.
[[ "${E2E_SOURCE_ONLY:-}" == 1 ]] && return 0
main
