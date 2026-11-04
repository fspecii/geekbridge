#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator end-to-end coverage for three phone features that otherwise only have unit/loopback tests:
# battery sharing, notification mirroring and PC dial (`geekbridge-ctl call dial`).
#
#   scripts/e2e/features.sh [--stop] [STAGE[,STAGE...]]     stages: env pair battery mirror dial footprint   (default: all but footprint)
#
# Everything is private (nothing touches the real service, ~/.config, ports 1716/17381/8000 or the session bus):
#   * `dbus-run-session`   a private session bus (the service's notify-send has no daemon to talk to; that is fine)
#   * Xvfb                 a private X display for the service's clipboard code
#   * emulator             own AVD clone (gbfeat, cloned from $GB_BASE_AVD=gb36, removed on exit unless GB_KEEP_AVD=1),
#                          console port 5654, adb server 16090
#   * geekbridge-service   own state dir, control port 17521, api 17520, discovery 17556, listener 1747
# Stages:
#   battery  phone shares battery only when the phone allows it for this computer AND the computer's consent is on;
#            the desktop status follows `adb shell dumpsys battery set ...` (level, charging, low); withdrawing the
#            consent (either end) erases the reading
#   mirror   notification access is granted with `cmd notification allow_listener`; a test notification (`cmd
#            notification post`) is mirrored only from an allow-listed app with consent on, shows in
#            `geekbridge-ctl notifications`, and dismissing it on the desktop cancels it on the phone
#   dial     `geekbridge-ctl call dial` refusal paths (no confirmation, bad number, no CALL_PHONE, no consent), then with
#            CALL_PHONE the phone really places the call (Telecom log + modem off-hook, reserved test number only), the
#            phone's own rate limit (3 per 10 minutes) and the optional on-phone "Place this call?" question
# Only the reserved emulator number 5550100 is ever dialed; the emulator modem is simulated and reaches no network.
# Results: dist/android/logs/e2e-features-results.tsv     Needs: the .tools Android toolchain, Xvfb, dbus-run-session,
# release binaries (E2E_TARGET) and the debug APK (E2E_SCRATCH/dist/geekbridge-debug.apk) from scripts/build-android.sh.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

if [[ -z "${FEAT_INNER:-}" ]]; then
  exec dbus-run-session -- env FEAT_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

STAGES="env,pair,battery,mirror,dial"
STOPPING=0
for a in "$@"; do
  case "$a" in
    --stop) STOPPING=1 ;;
    -h|--help) sed -n '3,27p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) STAGES="$a" ;;
  esac
done
set --

FEAT_WORK="${FEAT_WORK:-$HOME/.cache/gb-tmp/feat-e2e}"
: "${E2E_AVD:=gbfeat}" "${E2E_EMU_PORT:=5654}" "${E2E_ADB_PORT:=16090}" "${E2E_TAG:=-feat}" "${GB_BASE_AVD:=gb36}"
: "${E2E_RUN:=$FEAT_WORK/run}" "${E2E_SCRATCH:=$FEAT_WORK/android-scratch}" "${E2E_TARGET:=$FEAT_WORK/target}"
: "${E2E_XDISP:=:93}" "${E2E_CTRL_PORT:=17521}" "${E2E_API_PORT:=17520}" "${E2E_DISC_PORT:=17556}" "${E2E_LISTEN_PORT:=1747}" "${E2E_REDIR_PORT:=17571}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP GB_BASE_AVD \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-features-results.tsv"
set -- --no-build
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set --
# FEAT_KEEP=1 (debugging): leave the emulator and the service running when the script ends.
if [[ "${FEAT_KEEP:-0}" == 1 ]]; then gb_avd_on_exit() { :; }; fi

# The host is shared and can be heavily loaded: give the control calls room.
ctl() { timeout 90 "${CTL[@]}" "$@"; }
# `ctl status` appends plain-words hints after the JSON for a phone that is not connected: read only the JSON.
jget() { python3 -c "import json,sys;d=json.JSONDecoder().raw_decode(sys.stdin.read())[0];print($1)" 2>/dev/null; }

PRIV="$RUN/priv"; mkdir -p "$PRIV"/{config,data,state,cache}
XRT="${FEAT_XRT:-$FEAT_WORK/xdg}"; mkdir -p "$XRT"; chmod 700 "$XRT"
SERVICE_LOG="$LOGS/e2e-features-service.log"
APP_ENV=(env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u SWAYSOCK -u NIRI_SOCKET
  XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" XDG_DATA_HOME="$PRIV/data" XDG_STATE_HOME="$PRIV/state"
  XDG_CACHE_HOME="$PRIV/cache" XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=GeekBridgeE2E)
MIRROR_SVC="$PKG/$PKG.MirrorListenerService"
TEST_NUMBER=5550100   # reserved, never a real subscriber

stop_desktop() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null
  pkill -f "Xvfb $XDISP" 2>/dev/null
  true
}
cleanup() {
  $ADB shell dumpsys battery reset >/dev/null 2>&1
  $ADB emu gsm cancel "$TEST_NUMBER" >/dev/null 2>&1
  [[ "${FEAT_KEEP:-0}" == 1 ]] || { $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; stop_desktop; }
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
  [[ -x "$SVC" && -f "$APK" ]] || { rec feat_env FAIL "missing binaries/APK ($SVC, $APK)"; return 1; }
  gb_preflight "$ROOT" "$FEAT_WORK" || { rec feat_env FAIL "not enough free disk"; return 1; }
  pkill -f "Xvfb $XDISP" 2>/dev/null
  wait_for 10 bash -c "! pgrep -f 'Xvfb $XDISP' >/dev/null"
  rm -f "/tmp/.X11-unix/X${XDISP#:}" "/tmp/.X${XDISP#:}-lock"
  (setsid nohup Xvfb "$XDISP" -screen 0 800x600x24 -nolisten tcp >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${XDISP#:}" || { rec feat_env FAIL "Xvfb did not start"; return 1; }
  start_desktop_service || { rec feat_env FAIL "desktop service did not start: $(tail -3 "$SERVICE_LOG" | tr '\n' '|')"; return 1; }
  start_emulator || { rec feat_env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || { rec feat_env FAIL "adb install failed"; return 1; }
  desk_id
  rec feat_env PASS "own AVD $AVD on $SERIAL, own service on control port $CTRL_PORT; desktop $DESK_ID; APK $(sha256sum "$APK" | cut -c1-12)"
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
write_phonedata() { # battery(true|false) notifications(true|false) pkg...
  local b="$1" n="$2"; shift 2
  local apps="" p
  for p in "$@"; do apps+="        <string>$p</string>"$'\n'; done
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="battery" value="$b" />
    <boolean name="notifications" value="$n" />
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
# usage: phone_config "grants..." battery notifications confirm_dial [packages...]
phone_config() {
  local grants="$1" b="$2" n="$3" c="$4"; shift 4
  # Android re-binds an allowed notification listener the moment its process is stopped, and the new process would read the
  # old preference files before they are rewritten below: take the access away while the files are edited.
  local listener=0
  listener_granted && listener=1 && $ADB shell cmd notification disallow_listener "$MIRROR_SVC" >/dev/null 2>&1
  $ADB shell am force-stop $PKG; sleep 1
  # shellcheck disable=SC2086
  write_grants $grants
  write_phonedata "$b" "$n" "$@"
  write_calls_settings "$c"
  ((listener)) && $ADB shell cmd notification allow_listener "$MIRROR_SVC" >/dev/null 2>&1
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
    phone_config "calls" false false false || true
  else
    for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
    $ADB shell pm clear $PKG >/dev/null; wake
    for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS POST_NOTIFICATIONS; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
    local qr; qr="$(ctl pair-qr 2>/dev/null | grep -m1 "^geekbridge://")"
    [[ "$qr" == geekbridge://* ]] || { rec feat_pair FAIL "pair-qr gave: ${qr:0:120}"; return 1; }
    scan "$qr"
    ui wait "Connect to a computer" 20 >/dev/null || { rec feat_pair FAIL "no pairing page after the link: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Continue --expect "Computer identity" >/dev/null
    ui wait "Computer identity" 40 >/dev/null || { rec feat_pair FAIL "no identity step: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Pair >/dev/null
    ui wait "Paired ✓ with" 40 >/dev/null || { rec feat_pair FAIL "QR pairing did not finish: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
    phone_config "calls" false false false || true
    for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  fi
  link_up 120 || { rec feat_pair FAIL "link not connected"; return 1; }
  rec feat_pair PASS "paired $PHONE_ID and connected; phone starts with the calls grant only and every data feature off"
}

# ---------------------------------------------------------------- battery
batt() { # "nolink", "none" or "PERCENT CHARGING LOW" as the desktop status shows it
  ctl status | jget '(lambda L: "nolink" if not L else ("none" if not L[0].get("battery") else "%d %s %s" % (L[0]["battery"]["percent"], L[0]["battery"]["charging"], L[0]["battery"]["low"])))(d["links"])'
}
batt_is() { [[ "$(batt)" == "$1 $2 "* ]]; }
batt_low() { [[ "$(batt)" == "9 False True" ]]; }
batt_none() { [[ "$(batt)" == none ]]; }
stay_none() { # seconds -> it never appears
  local i; for ((i = 0; i < $1; i++)); do batt_none || return 1; sleep 1; done
}
stage_battery() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  link_up 120 || { rec feat_battery FAIL "no link to the phone"; return 1; }
  $ADB shell dumpsys battery unplug >/dev/null; $ADB shell dumpsys battery set level 42 >/dev/null
  # 1. Desktop consent on, but the phone shares nothing with this computer (master switch off, no grant).
  ctl consent "$PHONE_ID" battery on >/dev/null
  if stay_none 15; then rec feat_battery_default_off PASS "computer consent on, phone grants nothing: no reading after 15 s"
  else rec feat_battery_default_off FAIL "a reading appeared without the phone allowing it: $(batt)"; fi
  # 2. The phone's master switch on but no per-computer allow: still nothing.
  phone_config "calls" true false false
  ctl consent "$PHONE_ID" battery on >/dev/null
  if stay_none 15; then rec feat_battery_needs_allow PASS "phone switch on but this computer not allowed: no reading after 15 s"
  else rec feat_battery_needs_allow FAIL "a reading appeared without the per-computer allow: $(batt)"; fi
  # 3. Allowed on the phone, computer consent off: nothing is shown on the computer.
  ctl consent "$PHONE_ID" battery off >/dev/null
  phone_config "calls battery" true false false
  if stay_none 10; then rec feat_battery_needs_consent PASS "phone allows, computer consent off: nothing shown"
  else rec feat_battery_needs_consent FAIL "reading shown with the computer's consent off: $(batt)"; fi
  # 4. Both on: level and charging state arrive.
  ctl consent "$PHONE_ID" battery on >/dev/null
  if wait_for 40 batt_is 42 False; then rec feat_battery_level PASS "both ends allow: desktop status shows 42% not charging (emulator level 42, unplugged)"
  else rec feat_battery_level FAIL "expected 42% not charging, desktop shows: $(batt)"; fi
  # 5. Charging state change is sent at once.
  $ADB shell dumpsys battery set ac 1 >/dev/null
  if wait_for 20 batt_is 42 True; then rec feat_battery_charging PASS "AC plugged in: desktop shows charging"
  else rec feat_battery_charging FAIL "charging not shown: $(batt)"; fi
  # 6. Low flag.
  $ADB shell dumpsys battery unplug >/dev/null; $ADB shell dumpsys battery set level 9 >/dev/null
  if wait_for 20 batt_low; then rec feat_battery_low PASS "level 9 unplugged: desktop shows 9% not charging, low flag set"
  else rec feat_battery_low FAIL "low state not shown: $(batt)"; fi
  # 7. A plain percentage change is sent at most once a minute (with one trailing send), so allow up to 90 s.
  $ADB shell dumpsys battery set level 77 >/dev/null
  if wait_for 90 batt_is 77 False; then rec feat_battery_follows PASS "level 77: desktop follows the emulator battery (rate-limited sends allow up to 60 s)"
  else rec feat_battery_follows FAIL "desktop still shows $(batt) after level 77"; fi
  # 8. Withdrawing the computer's consent erases the reading now.
  ctl consent "$PHONE_ID" battery off >/dev/null
  if wait_for 10 batt_none; then rec feat_battery_withdraw_desktop PASS "consent off on the computer: reading erased"
  else rec feat_battery_withdraw_desktop FAIL "reading still shown: $(batt)"; fi
  # ... and back on: the phone answers the request again.
  ctl consent "$PHONE_ID" battery on >/dev/null
  if wait_for 30 batt_is 77 False; then rec feat_battery_reallow PASS "consent on again: the reading is back"
  else rec feat_battery_reallow FAIL "no reading after re-allowing: $(batt)"; fi
  # 9. Withdrawing on the phone: drop the grant, the link comes back without it, the reading is gone and stays gone.
  phone_config "calls" true false false
  ctl consent "$PHONE_ID" battery on >/dev/null
  if stay_none 20; then rec feat_battery_withdraw_phone PASS "grant removed on the phone: reading erased after reconnect and not re-sent in 20 s"
  else rec feat_battery_withdraw_phone FAIL "reading still shown after the phone withdrew the grant: $(batt)"; fi
  ctl consent "$PHONE_ID" battery off >/dev/null
  $ADB shell dumpsys battery reset >/dev/null
}

# ---------------------------------------------------------------- notification mirroring
notes_json() { ctl notifications "$PHONE_ID" --json 2>/dev/null; }
note_field() { # python expr over d (the view) -> value
  notes_json | jget "$1"
}
note_count() { note_field 'len([n for n in d["items"] if n["title"]=="GBE2E-'"$1"'"])'; }
note_has() { [[ "$(note_count "$1")" =~ ^[1-9] ]]; }
note_gone() { [[ "$(note_count "$1")" == 0 ]]; }
phone_has_note() { $ADB shell dumpsys notification --noredact 2>/dev/null | grep -q "$1"; }
# adb joins its arguments into one remote command line, so the quoting has to be inside one string.
post_note() { $ADB shell "cmd notification post -t 'GBE2E-$1' gbe2e$1 'body text $1'" >/dev/null 2>&1; }
listener_granted() { $ADB shell settings get secure enabled_notification_listeners 2>/dev/null | grep -q "$MIRROR_SVC"; }
stage_mirror() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  link_up 120 || { rec feat_mirror FAIL "no link to the phone"; return 1; }
  # 1. Notification access, granted the way the system settings page would.
  $ADB shell cmd notification allow_listener "$MIRROR_SVC" >/dev/null 2>&1
  if wait_for 10 listener_granted; then rec feat_mirror_access PASS "allow_listener $MIRROR_SVC: $($ADB shell settings get secure enabled_notification_listeners | tr -d '\r' | head -c 100)"
  else rec feat_mirror_access FAIL "listener access not granted: $($ADB shell settings get secure enabled_notification_listeners | head -c 200)"; return 1; fi
  # 2. Feature on and allowed for this computer, but no app on the list yet: nothing leaves the phone.
  phone_config "calls notifications" false true false
  ctl consent "$PHONE_ID" notifications on >/dev/null; sleep 3
  post_note A
  if phone_has_note gbe2eA && ! wait_for 12 note_has A; then rec feat_mirror_app_list PASS "no app allow-listed: notification posted on the phone, not mirrored"
  else rec feat_mirror_app_list FAIL "posted=$(phone_has_note gbe2eA && echo yes || echo no) mirrored=$(note_count A)"; fi
  # 3. com.android.shell (the poster of `cmd notification post`) on the list.
  phone_config "calls notifications" false true false com.android.shell
  ctl consent "$PHONE_ID" notifications on >/dev/null; sleep 3
  post_note B
  if wait_for 30 note_has B; then
    local line; line="$(ctl notifications "$PHONE_ID" | grep GBE2E-B | head -1)"
    rec feat_mirror_shows PASS "allow-listed app: desktop 'geekbridge-ctl notifications' lists it: ${line:0:150}"
  else rec feat_mirror_shows FAIL "not mirrored: $(notes_json | head -c 300)"; fi
  # 4. Dismiss from the desktop cancels it on the phone.
  local id; id="$(note_field '[n["id"] for n in d["items"] if n["title"]=="GBE2E-B"][0]')"
  if [[ -n "$id" ]] && phone_has_note gbe2eB; then
    ctl notifications dismiss "$PHONE_ID" "$id" >/dev/null 2>&1
    if wait_for 15 bash -c "! $ADB shell dumpsys notification --noredact 2>/dev/null | grep -q gbe2eB" && wait_for 10 note_gone B; then
      rec feat_mirror_dismiss PASS "dismiss $id on the desktop: gone from the phone's notification list and from the desktop list"
    else rec feat_mirror_dismiss FAIL "after dismiss: phone has it=$(phone_has_note gbe2eB && echo yes || echo no), desktop count=$(note_count B)"; fi
  else rec feat_mirror_dismiss FAIL "no id ($id) or notification missing on the phone"; fi
  # 5. Withdrawing the computer's consent erases what it holds and mirrors no more.
  post_note C
  wait_for 30 note_has C || rec feat_mirror_setup_c FAIL "second notification not mirrored"
  ctl consent "$PHONE_ID" notifications off >/dev/null
  if wait_for 10 note_gone C; then rec feat_mirror_withdraw PASS "consent off: mirrored list erased"
  else rec feat_mirror_withdraw FAIL "still listed: $(notes_json | head -c 200)"; fi
  post_note D
  if ! wait_for 10 note_has D; then rec feat_mirror_withdraw_quiet PASS "nothing mirrored after the consent was withdrawn"
  else rec feat_mirror_withdraw_quiet FAIL "a notification was mirrored with consent off"; fi
}

# ---------------------------------------------------------------- PC dial
dial() { ctl call dial "$@" </dev/null 2>&1; }
modem_state() { $ADB shell dumpsys telephony.registry 2>/dev/null | grep -m1 -o "mCallState=[0-9]" | cut -d= -f2; }
wait_modem() { local want="$1" n="$2" i; for ((i = 0; i < n * 2; i++)); do [[ "$(modem_state)" == "$want" ]] && return 0; sleep 0.5; done; return 1; }
end_call() { $ADB shell input keyevent KEYCODE_ENDCALL >/dev/null 2>&1; $ADB emu gsm cancel "$TEST_NUMBER" >/dev/null 2>&1; wait_modem 0 10; }
# The phone answers a dial after `ctl call dial` has printed "ok" (that only means the request was queued): the answer's
# words are the "notice:" line of `ctl calls`.
dial_notice() { ctl calls 2>/dev/null | sed -n 's/^notice: //p'; }
notice_has() { dial_notice | grep -qi "$1"; }
phone_link_line() { ctl calls 2>/dev/null | grep -m1 "^source: Phone link"; }
dial_ready() { phone_link_line | grep -q "): ready\.$"; }
dial_needs_permission() { phone_link_line | grep -q "to dial from here"; }
dial_off() { ! dial_ready && ! dial_needs_permission; } # consent off: the phone link is not a calls source at all
stage_dial() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  link_up 120 || { rec feat_dial FAIL "no link to the phone"; return 1; }
  end_call  # nothing left over from an earlier run
  $ADB shell cmd statusbar collapse >/dev/null 2>&1
  ctl consent "$PHONE_ID" telephony on >/dev/null
  $ADB shell pm revoke $PKG android.permission.CALL_PHONE >/dev/null 2>&1
  phone_config "calls" false false false
  ctl consent "$PHONE_ID" telephony on >/dev/null
  local out
  # 1. CALL_PHONE missing on the phone: the phone tells the computer, which does not even offer dialing.
  if wait_for 60 dial_needs_permission; then
    out="$(dial "$TEST_NUMBER" --yes)"
    if grep -qi "not available right now" <<<"$out" && [[ "$(modem_state)" == 0 ]]; then
      rec feat_dial_no_permission PASS "CALL_PHONE not granted: the computer is told dialing is unavailable and the modem stays idle: $(grep -m1 'allow GeekBridge' <<<"$out" | sed 's/^ *//' | head -c 160)"
    else rec feat_dial_no_permission FAIL "unexpected: $out (modem $(modem_state))"; fi
  else rec feat_dial_no_permission FAIL "the phone never reported the missing permission: $(phone_link_line)"; fi
  # 2. Grant CALL_PHONE, consent off on the computer.
  $ADB shell pm grant $PKG android.permission.CALL_PHONE >/dev/null 2>&1
  phone_config "calls" false false false
  ctl consent "$PHONE_ID" telephony off >/dev/null || ctl consent "$PHONE_ID" telephony off >/dev/null
  wait_for 30 dial_off
  out="$(dial "$TEST_NUMBER" --yes)"
  if grep -qi "not available right now" <<<"$out" && [[ "$(modem_state)" == 0 ]]; then rec feat_dial_no_consent PASS "CALL_PHONE granted but telephony consent off: refused, modem idle"
  else rec feat_dial_no_consent FAIL "unexpected: $out (modem $(modem_state)); link: $(phone_link_line)"; fi
  ctl consent "$PHONE_ID" telephony on >/dev/null
  if ! wait_for 90 dial_ready; then rec feat_dial_ready FAIL "dialing never became available: $(phone_link_line)"; return 1; fi
  # 3. Dialing is offered, but without a terminal and without --yes nothing is dialed.
  out="$(dial "$TEST_NUMBER")"
  if grep -qi "refusing to dial without confirmation" <<<"$out" && [[ "$(modem_state)" == 0 ]]; then rec feat_dial_unconfirmed PASS "no --yes and no terminal: $(head -1 <<<"$out"); modem idle"
  else rec feat_dial_unconfirmed FAIL "unexpected: $out (modem $(modem_state))"; fi
  # 4. Not a dialable string.
  out="$(dial "12;rm" --yes)"
  grep -qi "not a number" <<<"$out" && rec feat_dial_bad_number PASS "$out" || rec feat_dial_bad_number FAIL "unexpected: $out"
  # 5. Everything allowed: the phone places the call (a TelecomManager.placeCall -> CALL intent).
  $ADB logcat -c
  out="$(dial "$TEST_NUMBER" --yes)"
  if wait_modem 1 15 || wait_modem 2 5 || [[ "$(modem_state)" != 0 ]]; then
    local ev; ev="$($ADB logcat -d -b all 2>/dev/null | grep -iE "android.intent.action.CALL|CallIntentProcessor|placeCall|processOutgoingCallIntent" | grep -c .)"
    rec feat_dial_places_call PASS "dial $TEST_NUMBER --yes -> '$out'; modem call state $(modem_state); $ev Telecom log line(s) for the outgoing CALL intent"
    $ADB logcat -d -b all 2>/dev/null | grep -iE "android.intent.action.CALL|CallIntentProcessor|placeCall|processOutgoingCallIntent" | head -3 | cut -c1-220 >"$LOGS/e2e-features-dial-logcat.txt"
  else rec feat_dial_places_call FAIL "no call after dial: '$out' (modem $(modem_state))"; fi
  # The phone says so on screen: a visible notice that names the computer.
  if $ADB shell dumpsys notification --noredact 2>/dev/null | grep -q "Call placed for a computer"; then rec feat_dial_notice PASS "phone shows 'Call placed for a computer'"
  else rec feat_dial_notice FAIL "no on-phone notice"; fi
  end_call
  # 6. Rate limit: three placed per 10 minutes per computer (the one above counts), then the phone refuses the next.
  local k n_ok=1 limited="" trace=""
  for k in 2 3 4; do
    wait_modem 0 10 || end_call
    # The phone's "idle" report must reach the computer first: a state report clears the notice of an earlier refusal.
    wait_for 15 bash -c "$(declare -p CTL); timeout 60 \"\${CTL[@]}\" calls 2>/dev/null | head -1 | grep -q '^no calls'"; sleep 2
    dial "$TEST_NUMBER" --yes >/dev/null
    if wait_for 12 notice_has "limits how many calls"; then limited="$(dial_notice)"; break; fi
    wait_modem 2 8 || wait_modem 1 3
    trace+=" [$k: modem=$(modem_state) notice='$(dial_notice)']"
    n_ok=$((n_ok + 1)); end_call
  done
  if [[ -n "$limited" && $n_ok -eq 3 ]]; then rec feat_dial_rate_limit PASS "3 calls placed, the 4th refused by the phone: $limited"
  else rec feat_dial_rate_limit FAIL "placed=$n_ok limited='$limited' notice='$(dial_notice)' trace:$trace"; fi
  end_call
  [[ "$(modem_state)" == 0 ]] && rec feat_dial_idle_after PASS "emulator modem idle again" || rec feat_dial_idle_after FAIL "modem state $(modem_state)"
  # 7. "Ask me before placing a call": the phone asks, Deny leaves the modem idle, Call places it (fresh limiter after the restart).
  phone_config "calls" false false true
  ctl consent "$PHONE_ID" telephony on >/dev/null; sleep 4
  # Earlier stages leave test notifications in the shade; with several stacked, the question's Deny button is not
  # visible (CI runs showed "Expand"). Clear them first so the question is the only entry.
  wake; $ADB shell cmd statusbar expand-notifications >/dev/null 2>&1; sleep 2
  ui tap "Clear all" >/dev/null 2>&1 || ui tap "Clear all" --contains >/dev/null 2>&1
  sleep 1; $ADB shell cmd statusbar collapse >/dev/null 2>&1
  local question="$ADB shell dumpsys notification --noredact 2>/dev/null | grep -q 'Place this call?'"
  dial "$TEST_NUMBER" --yes >/dev/null
  if wait_for 15 bash -c "$question"; then
    wake; shot 30-dial-question
    wake; $ADB shell cmd statusbar expand-notifications >/dev/null 2>&1; sleep 2
    ui tap Deny >/dev/null 2>&1 || ui tap Deny --contains >/dev/null 2>&1
    if wait_for 15 notice_has "did not confirm" && [[ "$(modem_state)" == 0 ]]; then rec feat_dial_confirm_deny PASS "question 'Place this call?' shown on the phone, Deny: '$(dial_notice)'; modem idle"
    else rec feat_dial_confirm_deny FAIL "after Deny: notice='$(dial_notice)' modem $(modem_state); screen: $(ui dump | cut -f1 | head -12 | tr '\n' '|')"; fi
  else rec feat_dial_confirm_deny FAIL "no 'Place this call?' notification on the phone"; fi
  $ADB shell cmd statusbar collapse >/dev/null 2>&1
  dial "$TEST_NUMBER" --yes >/dev/null
  if wait_for 15 bash -c "$question"; then
    wake; $ADB shell cmd statusbar expand-notifications >/dev/null 2>&1; sleep 2
    ui tap Call --exact >/dev/null 2>&1
    if wait_modem 2 15 || [[ "$(modem_state)" != 0 ]]; then rec feat_dial_confirm_allow PASS "question shown, Call: the phone placed it (modem call state $(modem_state))"
    else rec feat_dial_confirm_allow FAIL "after Call: notice='$(dial_notice)' modem $(modem_state); screen: $(ui dump | cut -f1 | head -12 | tr '\n' '|')"; fi
  else rec feat_dial_confirm_allow FAIL "no question on the phone"; fi
  $ADB shell cmd statusbar collapse >/dev/null 2>&1
  end_call
  ctl consent "$PHONE_ID" telephony off >/dev/null
}

# ---------------------------------------------------------------- footprint (not part of the default run)
# Memory, threads, fds, wakeups and start-up of the debug APK with the features switched on one by one, then a long idle run
# with everything on. Results (TSV): $FOOT_OUT (default $FEAT_WORK/footprint). FOOT_MINUTES (30) and FOOT_INTERVAL (60 s).
APP_PID() { $ADB shell pidof $PKG 2>/dev/null | tr -d '\r' | awk '{print $1}'; }
foot_sample() { # label -> one TSV row: pss rss java native code stack graphics threads fds service_rss service_threads
  local pid mem pss rss jh nh code stk gfx thr fds spid srss sthr
  pid="$(APP_PID)"; [[ -n "$pid" ]] || { printf '%s\tno-process\n' "$1"; return 0; }
  mem="$($ADB shell dumpsys meminfo $PKG 2>/dev/null | tr -d '\r')"
  pss="$(awk '/TOTAL PSS:/ {print $3}' <<<"$mem")"; rss="$(awk '/TOTAL RSS:/ {for(i=1;i<=NF;i++) if($i=="RSS:") {print $(i+1); exit}}' <<<"$mem")"
  [[ -n "$pss" ]] || pss="$(awk '$1=="TOTAL" {print $2; exit}' <<<"$mem")"
  jh="$(awk '/^ *Java Heap:/ {print $3}' <<<"$mem")"; nh="$(awk '/^ *Native Heap:/ {print $3}' <<<"$mem")"
  code="$(awk '/^ *Code:/ {print $2}' <<<"$mem")"; stk="$(awk '/^ *Stack:/ {print $2}' <<<"$mem")"; gfx="$(awk '/^ *Graphics:/ {print $2}' <<<"$mem")"
  thr="$($ADB shell "grep ^Threads /proc/$pid/status" 2>/dev/null | tr -d '\r' | awk '{print $2}')"
  fds="$($ADB shell "run-as $PKG sh -c 'ls /proc/$pid/fd | wc -l'" 2>/dev/null | tr -d '\r ')"
  spid="$(pgrep -f "^$SVC --state-dir $RUN/state" | head -1)"
  srss=""; sthr=""
  [[ -n "$spid" ]] && srss="$(awk '/^VmRSS/ {print $2}' /proc/$spid/status)" && sthr="$(awk '/^Threads/ {print $2}' /proc/$spid/status)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$pss" "$rss" "$jh" "$nh" "$code" "$stk" "$gfx" "$thr" "$fds" "$srss" "$sthr"
}
foot_alarms() { $ADB shell dumpsys alarm 2>/dev/null | tr -d '\r' | grep -E "dev\.geekbridge\.android.*(wakeups|alarms)" | head -3 | tr '\n' ' '; }
foot_uid() { $ADB shell "stat -c %u /data/data/$PKG" 2>/dev/null | tr -d '\r'; }
foot_batt() { # uid -> cpu, wakeup alarms, wakelocks, jobs, network from the checkin dump
  $ADB shell dumpsys batterystats --checkin 2>/dev/null | tr -d '\r' | awk -F, -v u="$1" '$2==u && ($4=="cpu"||$4=="wua"||$4=="wl"||$4=="jb"||$4=="nt"||$4=="awl") {print}' | tr '\n' ' '
}
foot_cold_start() { # n -> cold start times (ms), am start -W
  local i t out=""
  for ((i = 0; i < $1; i++)); do
    $ADB shell am force-stop $PKG; sleep 2
    t="$($ADB shell am start -W -n "$PKG/.HomeActivity" 2>/dev/null | tr -d '\r' | awk '/TotalTime/ {print $2}')"
    out+="${t:-?} "; sleep 3
  done
  echo "$out"
}
stage_footprint() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  local OUT="${FOOT_OUT:-$FEAT_WORK/footprint}"; mkdir -p "$OUT"
  local HDR=$'label\tpss_kb\trss_kb\tjava_kb\tnative_kb\tcode_kb\tstack_kb\tgraphics_kb\tthreads\tfds\tsvc_rss_kb\tsvc_threads'
  for p in READ_PHONE_STATE ANSWER_PHONE_CALLS READ_CALL_LOG READ_CONTACTS WRITE_CONTACTS READ_CALENDAR WRITE_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS CALL_PHONE READ_MEDIA_IMAGES POST_NOTIFICATIONS; do
    $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done
  $ADB shell cmd deviceidle whitelist +$PKG >/dev/null 2>&1
  # 1. Feature steps: a fresh process per step, 45 s settled, then a sample.
  local steps=("calls:calls" "contacts+calendar+sms:calls contacts calendar calendar_edit sms" "battery:calls contacts calendar calendar_edit sms battery"
    "notifications:calls contacts calendar calendar_edit sms battery notifications" "photos:calls contacts calendar calendar_edit sms battery notifications photos")
  printf '%s\n' "$HDR" >"$OUT/steps.tsv"
  local st name grants b n
  for st in "${steps[@]}"; do
    name="${st%%:*}"; grants="${st#*:}"; b=false; n=false
    [[ "$grants" == *battery* ]] && b=true
    [[ "$grants" == *notifications* ]] && n=true
    phone_config "$grants" $b $n false com.android.shell || true
    for f in $grants; do ctl consent "$PHONE_ID" "$f" on >/dev/null 2>&1; done
    $ADB shell cmd notification allow_listener "$MIRROR_SVC" >/dev/null 2>&1
    sleep 45
    foot_sample "+$name" | tee -a "$OUT/steps.tsv"
  done
  # 2. Start-up with everything on.
  local cold; cold="$(foot_cold_start 7)"; echo "cold_start_ms: $cold" | tee "$OUT/startup.txt"
  link_up 120 || true
  # 3. Long idle run.
  local uid; uid="$(foot_uid)"
  $ADB shell dumpsys batterystats --enable full-wake-history >/dev/null 2>&1
  $ADB shell dumpsys batterystats --reset >/dev/null 2>&1
  $ADB shell dumpsys battery unplug >/dev/null 2>&1; $ADB shell dumpsys battery set level 80 >/dev/null 2>&1
  foot_alarms >"$OUT/alarms-start.txt"; foot_batt "$uid" >"$OUT/batt-start.txt"
  printf '%s\n' "$HDR" >"$OUT/idle.tsv"
  local n_samples=$(( ${FOOT_MINUTES:-30} * 60 / ${FOOT_INTERVAL:-60} )) i
  for ((i = 0; i <= n_samples; i++)); do
    foot_sample "t+$(( i * ${FOOT_INTERVAL:-60} ))s" | tee -a "$OUT/idle.tsv"
    ((i < n_samples)) && sleep "${FOOT_INTERVAL:-60}"
  done
  foot_alarms >"$OUT/alarms-end.txt"; foot_batt "$uid" >"$OUT/batt-end.txt"
  $ADB shell dumpsys alarm >"$OUT/dumpsys-alarm.txt" 2>&1
  $ADB shell dumpsys batterystats --checkin >"$OUT/batterystats-checkin.txt" 2>&1
  $ADB shell dumpsys meminfo $PKG >"$OUT/meminfo-end.txt" 2>&1
  $ADB shell dumpsys battery reset >/dev/null 2>&1
  is_connected && rec feat_footprint PASS "samples in $OUT" || rec feat_footprint FAIL "link was not connected at the end; samples in $OUT"
}

main_features() {
  mkdir -p "$LOGS"; : >"$LOGS/e2e-run.log"
  if [[ "$STOPPING" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    stop_desktop; return 0
  fi
  [[ "$STAGES" == env,pair,battery,mirror,dial ]] && : >"$RESULTS"
  local st sts
  IFS=, read -r -a sts <<<"$STAGES"
  for st in "${sts[@]}"; do log "== stage $st"; "stage_$st" || log "stage $st reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_features
