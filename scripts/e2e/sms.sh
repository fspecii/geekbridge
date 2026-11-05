#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Emulator end-to-end coverage for Messages (SMS): the phone's SMS provider and SmsManager <-> the desktop service
# (`geekbridge-ctl messages`, `messages-send`, `phone-state`). See docs/reports/sms-emulator-e2e.md.
#
#   scripts/e2e/sms.sh [--stop] [STAGE[,STAGE...]]     stages: env pair sms   (default: all)
#
# Everything is private (nothing touches the real service, ~/.config, ports 1716/17381/8000 or the session bus):
#   * `dbus-run-session`   a private session bus
#   * Xvfb                 a private X display
#   * emulator             own AVD clone (gbsms, cloned from $GB_BASE_AVD=gb36, removed on exit unless GB_KEEP_AVD=1),
#                          console port 5688, adb server 16188
#   * geekbridge-service   own state dir, control port 17931, api 17930, discovery 17886, listener 1787
# Stage sms (each line is one row in the results file):
#   declared         with the SMS permissions (pm grant), the switch (geekbridge_phonedata.xml) and the per-computer
#                    grant (geekbridge_grants.xml) the phone declares sms; the app process is alive and logcat has no
#                    FATAL EXCEPTION / ANR; `phone-state` and `status` show sms offered
#   not_declared     without the per-computer grant the phone declares no sms (the state the old e2e-phonedata.sh left it in)
#   incoming         `adb emu sms send` appears on the desktop (`messages`) with an unread count
#   thread           the conversation shows the text
#   short_code       a number shorter than 7 digits is refused by the phone (premium-number protection)
#   send             a text from the desktop to a normal number is accepted by the phone and reaches "sent"
#   limiter          the 11th quick text is held back with the limiter wording, the phone shows the "Texts held back"
#                    notification, the earlier ones are not affected
#   mms              an MMS row inserted in the provider is listed as a picture-message placeholder (SKIP when the shell
#                    may not write the provider on this image)
#   sim              the SIM list the phone reports once READ_PHONE_STATE is granted (one SIM on the emulator)
#   no_crash         at the end: the app process is the one that started, no crash/ANR in the whole log
# Results: dist/android/logs/e2e-sms-results.tsv     Needs: the .tools Android toolchain, Xvfb, dbus-run-session,
# release binaries (E2E_TARGET) and the debug APK (E2E_SCRATCH/dist/geekbridge-debug.apk) from scripts/build-android.sh.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

if [[ -z "${SMS_INNER:-}" ]]; then
  exec dbus-run-session -- env SMS_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

STAGES="env,pair,sms"
STOPPING=0
for a in "$@"; do
  case "$a" in
    --stop) STOPPING=1 ;;
    -h|--help) sed -n '3,33p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) STAGES="$a" ;;
  esac
done
set --

SMS_WORK="${SMS_WORK:-$HOME/.cache/gb-tmp/sms-e2e}"
: "${E2E_AVD:=gbsms}" "${E2E_EMU_PORT:=5688}" "${E2E_ADB_PORT:=16188}" "${E2E_TAG:=-sms}" "${GB_BASE_AVD:=gb36}"
: "${E2E_RUN:=$SMS_WORK/run}" "${E2E_SCRATCH:=$SMS_WORK/android-scratch}" "${E2E_TARGET:=$SMS_WORK/target}"
: "${E2E_XDISP:=:84}" "${E2E_CTRL_PORT:=17931}" "${E2E_API_PORT:=17930}" "${E2E_DISC_PORT:=17886}" "${E2E_LISTEN_PORT:=1787}" "${E2E_REDIR_PORT:=17991}"
export E2E_SOURCE_ONLY=1 E2E_AVD E2E_EMU_PORT E2E_ADB_PORT E2E_TAG E2E_RUN E2E_SCRATCH E2E_TARGET E2E_XDISP GB_BASE_AVD \
  E2E_CTRL_PORT E2E_API_PORT E2E_DISC_PORT E2E_LISTEN_PORT E2E_REDIR_PORT
export E2E_RESULTS="$ROOT/dist/android/logs/e2e-sms-results.tsv"
set -- --no-build
# shellcheck disable=SC1091
source scripts/e2e-android.sh
set --
# SMS_KEEP=1 (debugging): leave the emulator and the service running when the script ends.
if [[ "${SMS_KEEP:-0}" == 1 ]]; then gb_avd_on_exit() { :; }; fi

# The host can be heavily loaded: give the control calls room.
ctl() { timeout 90 "${CTL[@]}" "$@"; }
# `ctl status` appends plain-words hints after the JSON for a phone that is not connected: read only the JSON.
jget() { python3 -c "import json,sys;d=json.JSONDecoder().raw_decode(sys.stdin.read())[0];print($1)" 2>/dev/null; }

PRIV="$RUN/priv"; mkdir -p "$PRIV"/{config,data,state,cache}
XRT="/tmp/gb-sms-xdg-$UID"; mkdir -p "$XRT"; chmod 700 "$XRT"
SERVICE_LOG="$LOGS/e2e-sms-service.log"
APP_ENV=(env -u LD_LIBRARY_PATH -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u SWAYSOCK -u NIRI_SOCKET
  XDG_RUNTIME_DIR="$XRT" XDG_CONFIG_HOME="$PRIV/config" XDG_DATA_HOME="$PRIV/data" XDG_STATE_HOME="$PRIV/state"
  XDG_CACHE_HOME="$PRIV/cache" XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=GeekBridgeE2E)
# 7+ digits so the phone's short-code rule lets it through; nobody owns it on the emulator's simulated network.
NUMBER=5551234567
SHORT=5554        # an emulator console port number, but also a "short code" to the phone
SMS_PERMS=(READ_SMS SEND_SMS RECEIVE_SMS READ_PHONE_STATE POST_NOTIFICATIONS)

stop_desktop() {
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null
  pkill -f "Xvfb $XDISP" 2>/dev/null
  true
}
cleanup() {
  [[ "${SMS_KEEP:-0}" == 1 ]] || { $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1; stop_desktop; }
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
  [[ -x "$SVC" && -f "$APK" ]] || { rec sms_env FAIL "missing binaries/APK ($SVC, $APK)"; return 1; }
  gb_preflight "$ROOT" "$SMS_WORK" || { rec sms_env FAIL "not enough free disk"; return 1; }
  pkill -f "Xvfb $XDISP" 2>/dev/null
  wait_for 10 bash -c "! pgrep -f 'Xvfb $XDISP' >/dev/null"
  rm -f "/tmp/.X11-unix/X${XDISP#:}" "/tmp/.X${XDISP#:}-lock"
  (setsid nohup Xvfb "$XDISP" -screen 0 800x600x24 -nolisten tcp >"$RUN/xvfb.log" 2>&1 &)
  wait_for 10 test -S "/tmp/.X11-unix/X${XDISP#:}" || { rec sms_env FAIL "Xvfb did not start"; return 1; }
  start_desktop_service || { rec sms_env FAIL "desktop service did not start: $(tail -3 "$SERVICE_LOG" | tr '\n' '|')"; return 1; }
  start_emulator || { rec sms_env FAIL "emulator did not boot"; return 1; }
  $ADB shell svc power stayon true; wake
  $ADB install -r "$APK" 2>&1 | tail -1 | grep -q Success || { rec sms_env FAIL "adb install failed"; return 1; }
  desk_id
  rec sms_env PASS "own AVD $AVD on $SERIAL, own service on control port $CTRL_PORT; desktop $DESK_ID; APK $(sha256sum "$APK" | cut -c1-12)"
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
# The master switch of Messages (and whether short codes / premium numbers are allowed).
write_phonedata() { # sms(true|false) special(true|false)
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_phonedata.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="sms" value="$1" />
    <boolean name="sms_special" value="$2" />
</map>
XML
}
write_calls_settings() {
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_calls.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <boolean name="enabled" value="true" />
    <boolean name="confirm_dial" value="false" />
</map>
XML
}
grant_sms() { local p; for p in "${SMS_PERMS[@]}"; do $ADB shell pm grant $PKG android.permission.$p >/dev/null 2>&1; done; }
# The desktop dials the phone through the emulator redirection until the link is up (the phone side never dials in here).
link_up() { # seconds
  local i; for ((i = 0; i < $1 / 5; i++)); do is_connected && return 0; desk_dial >/dev/null 2>&1; sleep 5; done
  is_connected
}
# Rewrite the phone's settings while the app is stopped, then start it and wait for the link.
# usage: phone_config "grants..." sms special     (granting permissions can end the app's process: do it first)
phone_config() {
  local grants="$1" sms="$2" special="$3"
  $ADB shell am force-stop $PKG; sleep 1
  grant_sms
  $ADB shell am force-stop $PKG; sleep 1
  # The old link must be seen going down, or the status read right after shows the previous declaration.
  wait_for 20 not_connected
  # shellcheck disable=SC2086
  write_grants $grants
  write_phonedata "$sms" "$special"
  write_calls_settings
  goto_home; sleep 3
  desk_dial >/dev/null 2>&1
  link_up 180
}

# ---------------------------------------------------------------- crash and process-death evidence
app_pid() { $ADB shell pidof $PKG 2>/dev/null | tr -d '\r' | awk '{print $1}'; }
# Everything that means the app died or hung: the crash buffer plus the system's own lines.
crash_lines() {
  $ADB logcat -d -b all 2>/dev/null | grep -E "FATAL EXCEPTION|ANR in $PKG|am_crash.*$PKG|am_anr.*$PKG|Fatal signal.*$PKG" | head -20
}
logcat_save() { $ADB logcat -d -b all >"$LOGS/e2e-sms-logcat.txt" 2>&1; }

stage_pair() {
  [[ "$STOPPING" == 1 ]] && return 0
  desk_id; PHONE_ID="$(cat "$RUN/phone-id" 2>/dev/null)"
  wake
  grant_sms
  if [[ -n "$PHONE_ID" ]] && is_paired; then
    phone_config "sms" true false || true
  else
    for id in $(paired_ids); do ctl unpair "$id" >/dev/null; done
    $ADB shell pm clear $PKG >/dev/null; wake
    grant_sms
    local qr; qr="$(ctl pair-qr 2>/dev/null | grep -m1 "^geekbridge://")"
    [[ "$qr" == geekbridge://* ]] || { rec sms_pair FAIL "pair-qr gave: ${qr:0:120}"; return 1; }
    scan "$qr"
    ui wait "Link to a computer?" 20 >/dev/null || { rec sms_pair FAIL "no pairing page after the link: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Continue --expect "Computer identity" >>"$LOGS/e2e-sms-tap.log" 2>&1
    ui wait "Computer identity" 40 >/dev/null || { rec sms_pair FAIL "no identity step: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    tap_to Link >>"$LOGS/e2e-sms-tap.log" 2>&1
    ui wait "Linked ✓ with" 40 >/dev/null || { rec sms_pair FAIL "QR pairing did not finish: $(ui dump | cut -f1 | head -8 | tr '\n' '|')"; return 1; }
    PHONE_ID="$(ctl status | jget 'd["paired"][0]["device_id"]')"; echo "$PHONE_ID" >"$RUN/phone-id"
    tap "Back to home" >/dev/null 2>&1 || tap "Done" >/dev/null 2>&1
    phone_config "sms" true false || true
    for _ in $(seq 1 14); do ui dump | cut -f1 | grep -q "^Step " || break; tap Skip >/dev/null 2>&1 || tap Next >/dev/null 2>&1 || tap Finish >/dev/null 2>&1; sleep 1; done
  fi
  link_up 120 || { rec sms_pair FAIL "link not connected"; return 1; }
  rec sms_pair PASS "paired $PHONE_ID and connected"
}

# ---------------------------------------------------------------- messages
msgs() { ctl messages "$PHONE_ID" 2>/dev/null; }
conv_has() { msgs | jget 'any("'"$1"'" in c["last_body"] for c in d["conversations"])' | grep -q True; }
send_phase() { # tag -> the phase of the send whose text has the tag ("" = not listed)
  msgs | jget '([s["phase"] for s in d["sends"] if "'"$1"'" in s["text"]] or [""])[0]'
}
send_reason() { msgs | jget '([str(s.get("reason") or s.get("detail") or "") for s in d["sends"] if "'"$1"'" in s["text"]] or [""])[0]'; }
wait_phase() { # tag secs  -> prints the settled phase
  local i p=""
  for ((i = 0; i < $2 / 2; i++)); do
    p="$(send_phase "$1")"
    case "$p" in accepted_by_phone|sent|delivered|failed|uncertain) break;; esac
    sleep 2
  done
  echo "$p"
}
phone_notice() { $ADB shell dumpsys notification --noredact 2>/dev/null | grep -c "Texts held back"; }

# The desktop's view of this phone's Messages: "ready" when the phone offers it and the computer's consent is on.
dev_field() { msgs | jget '(d["devices"] or [{}])[0].get("'"$1"'")'; }
sms_state() { dev_field sms; }
sims_listed() { msgs | jget 'len((d["devices"] or [{}])[0].get("sims") or [])'; }
not_connected() { ! is_connected; }
phone_sms_line() { ctl phone-state "$PHONE_ID" 2>&1 | grep -m1 '^sms:' | tr -d '\r'; }
# Root is only needed to write the telephony provider (the shell may not); the emulator image is a userdebug build.
as_root() { $ADB root >/dev/null 2>&1; $ADB wait-for-device; sleep 2; }
as_user() { $ADB unroot >/dev/null 2>&1; $ADB wait-for-device; sleep 2; }

stage_sms() {
  [[ "$STOPPING" == 1 ]] && return 0
  load_ids
  [[ -n "$PHONE_ID" ]] || { rec sms SKIP "not paired"; return 1; }
  $ADB logcat -c >/dev/null 2>&1
  link_up 120 || { rec sms FAIL "no link to the phone"; return 1; }

  # 0. Without the per-computer grant the phone offers no Messages, whatever else is on (the old e2e-phonedata.sh left
  #    the phone in exactly this state: it wrote the switch but never the grant).
  phone_config "" true false || { rec sms_not_declared FAIL "link did not come back"; return 1; }
  ctl consent "$PHONE_ID" sms on >/dev/null
  sleep 5
  local st; st="$(sms_state)"
  if [[ "$st" != ready ]]; then rec sms_not_declared PASS "switch on, permissions granted, no per-computer grant: desktop sms '$st'; phone says '$(phone_sms_line)'; app pid $(app_pid)"
  else rec sms_not_declared FAIL "Messages offered without the per-computer grant (desktop '$st')"; fi

  # 1. Everything in place: Messages is offered, the app is alive and did not crash.
  phone_config "sms" true false || { rec sms_declared FAIL "link did not come back"; return 1; }
  ctl consent "$PHONE_ID" sms on >/dev/null
  local i; for i in $(seq 20); do [[ "$(sms_state)" == ready ]] && break; sleep 2; done
  st="$(sms_state)"
  local pid1 crashes; pid1="$(app_pid)"; crashes="$(crash_lines)"
  local caps; caps="$(ctl status | jget '",".join(d["links"][0]["negotiated"])')"
  if [[ "$st" == ready && ",$caps," == *,kdeconnect.sms.messages,* && -n "$pid1" && -z "$crashes" ]]; then
    rec sms_declared PASS "desktop sms '$st'; negotiated has kdeconnect.sms.messages; app pid $pid1 alive, no FATAL EXCEPTION/ANR; phone says '$(phone_sms_line)'"
  else
    rec sms_declared FAIL "desktop sms '$st'; negotiated [$caps]; app pid '${pid1}'; crash lines: ${crashes:0:300}; phone says '$(phone_sms_line)'"
    logcat_save
  fi
  shot 40-sms-home

  # 2. A text arriving on the phone appears on the computer, unread.
  local tag="gbin$RANDOM"
  $ADB emu sms send 5551234 "hello from the console $tag" >/dev/null
  if wait_for 60 conv_has "$tag"; then
    local row; row="$(msgs | jget '[ (c["title"], c["unread"]) for c in d["conversations"] if "'"$tag"'" in c["last_body"]][0]')"
    rec sms_incoming PASS "console text listed on the computer: $row"
    local tid; tid="$(msgs | jget '[c["thread_id"] for c in d["conversations"] if "'"$tag"'" in c["last_body"]][0]')"
    if ctl messages "$PHONE_ID" "$tid" | jget 'any("'"$tag"'" in m["body"] for m in d["selected"]["messages"])' | grep -q True; then
      rec sms_thread PASS "thread $tid shows the message"
    else rec sms_thread FAIL "thread $tid does not show the message"; fi
  else
    rec sms_incoming FAIL "console text never listed: $(msgs | head -c 400); app pid $(app_pid); crash: $(crash_lines | head -c 300)"
    logcat_save
  fi

  # 3. Short codes are refused unless the phone's owner allowed them.
  local stag="gbshort$RANDOM"
  ctl messages-send "$PHONE_ID" "$SHORT" "short $stag" >/dev/null 2>&1
  local sp; sp="$(wait_phase "$stag" 30)"
  if [[ "$sp" == failed ]]; then rec sms_short_code PASS "text to $SHORT: $sp ($(send_reason "$stag"))"
  else rec sms_short_code FAIL "text to a $SHORT short number ended as '$sp'"; fi

  # 4. A normal send: accepted by the phone, then reported sent by the radio.
  local otag="gbout$RANDOM"
  ctl messages-send "$PHONE_ID" "$NUMBER" "from the computer $otag" >/dev/null
  local op; op="$(wait_phase "$otag" 60)"
  case "$op" in
    accepted_by_phone|sent|delivered) rec sms_send PASS "send to $NUMBER: phone reported $op" ;;
    *) rec sms_send FAIL "send to $NUMBER ended as '$op' ($(send_reason "$otag"))"; logcat_save ;;
  esac

  # 5. The phone's limiter. The computer already limits itself to 10 texts a minute, the same number the phone allows,
  #    so the phone's own rule is reached through the other one: at most 8 different numbers in 10 minutes. Wait out the
  #    computer's minute first so only the phone can say no.
  sleep 62
  local ltag="gblim$RANDOM" n ph okn=0 held=0 lastphase="" lastwhy=""
  for n in $(seq 1 10); do
    ctl messages-send "$PHONE_ID" "555700000$n" "number $ltag $n" >/dev/null 2>&1 || { rec sms_limiter_setup FAIL "computer refused text $n"; break; }
    ph="$(wait_phase "number $ltag $n" 40)"
    case "$ph" in failed) held=$((held + 1)); lastphase=$ph; lastwhy="$(send_reason "number $ltag $n")" ;; accepted_by_phone|sent|delivered) okn=$((okn + 1)) ;; esac
  done
  if ((held >= 1 && okn >= 6)) && [[ "$lastwhy" == recipients ]]; then
    rec sms_limiter PASS "10 different numbers: $okn accepted, $held held back by the phone (reason: $lastwhy)"
  else rec sms_limiter FAIL "10 different numbers: $okn accepted, $held held back; last '$lastphase' ($lastwhy)"; logcat_save; fi
  if wait_for 20 bash -c "[[ \$($ADB shell dumpsys notification --noredact 2>/dev/null | grep -c 'Texts held back') -gt 0 ]]"; then
    rec sms_limiter_notice PASS "the phone shows the 'Texts held back' notification"; shot 42-limit-notice
  else rec sms_limiter_notice FAIL "no 'Texts held back' notification on the phone"; fi
  # ... and the computer's own minute limit: 11 quick texts to a number the phone already allows, the 11th is refused here.
  sleep 62
  local queued=0 refused=0 msg
  for n in $(seq 1 11); do
    msg="$(ctl messages-send "$PHONE_ID" 5557000001 "quick $ltag $n" 2>&1)" && queued=$((queued + 1)) || { refused=$((refused + 1)); lastwhy="$msg"; }
  done
  if ((queued == 10 && refused == 1)); then rec sms_quick_eleven PASS "11 quick texts: 10 queued, the 11th refused: $(echo "$lastwhy" | tr -d '\n' | cut -c1-120)"
  else rec sms_quick_eleven FAIL "11 quick texts: $queued queued, $refused refused ($lastwhy)"; fi

  # 6. MMS: a picture-message row in the provider is listed as a picture placeholder.
  local mtag="gbmms$RANDOM" mid
  as_root
  mid="$($ADB shell "content insert --uri content://mms/inbox --bind thread_id:i:3 --bind date:l:$(date +%s) --bind read:i:0 --bind m_type:i:132 --bind sub:s:$mtag; content query --uri content://mms --projection _id --sort '_id DESC'" 2>&1 | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
  if [[ -z "$mid" ]]; then
    rec sms_mms SKIP "could not write content://mms (adb root: $($ADB shell id 2>&1 | cut -c1-20))"
  else
    $ADB shell "content insert --uri content://mms/$mid/addr --bind address:s:5551234 --bind type:i:137 --bind charset:i:106 --bind msg_id:i:$mid" >/dev/null 2>&1
    sleep 8
    ctl messages-refresh "$PHONE_ID" >/dev/null 2>&1
    ctl messages-refresh "$PHONE_ID" 3 >/dev/null 2>&1; sleep 5
    if ctl messages "$PHONE_ID" 3 | jget 'any(m["mms"] for m in d["selected"]["messages"])' | grep -q True; then
      rec sms_mms PASS "MMS row $mid is listed in thread 3 as a picture message (no body, attachment marker)"
    else
      rec sms_mms FAIL "MMS row $mid not listed as a picture message; phone log: $($ADB logcat -d 2>/dev/null | grep -E ' GB .*(sms|picture)' | tail -n 3 | cut -c1-200 | tr '\n' '|')"
    fi
  fi
  as_user

  # 7. SIMs: with READ_PHONE_STATE the phone lists its SIMs (one on the emulator); without it the list is empty and
  #    every text uses the default SIM (the chooser stays hidden on the computer).
  local with without
  ctl messages-refresh "$PHONE_ID" >/dev/null 2>&1; sleep 4; with="$(sims_listed)"
  $ADB shell pm revoke $PKG android.permission.READ_PHONE_STATE >/dev/null 2>&1
  link_up 120 >/dev/null; wait_for 30 is_connected; sleep 5; goto_home; sleep 3; link_up 120 >/dev/null
  ctl consent "$PHONE_ID" sms on >/dev/null 2>&1
  ctl messages-refresh "$PHONE_ID" >/dev/null 2>&1; sleep 6; without="$(sims_listed)"
  if [[ "$with" -ge 1 && "$without" == 0 ]]; then rec sms_sim PASS "READ_PHONE_STATE granted: $with SIM listed; revoked: $without (default SIM used, chooser hidden)"
  else rec sms_sim FAIL "SIMs listed with the permission: '$with', without it: '$without'"; fi

  # 8. The whole run: the app is still running and nothing crashed or hung.
  local pid2; pid2="$(app_pid)"; crashes="$(crash_lines)"
  logcat_save
  if [[ -z "$crashes" ]]; then rec sms_no_crash PASS "no FATAL EXCEPTION/ANR for $PKG in the whole log; app pid ${pid2:-none} at the end"
  else rec sms_no_crash FAIL "app pid '${pid2}'; crash lines: ${crashes:0:400}"; fi
  shot 41-sms-end
}

main_sms() {
  mkdir -p "$LOGS"; : >"$LOGS/e2e-run.log"
  if [[ "$STOPPING" == 1 ]]; then
    $ADB emu kill >/dev/null 2>&1; systemctl --user stop "gbe2e-emulator${E2E_TAG:-}" >/dev/null 2>&1
    stop_desktop; return 0
  fi
  [[ "$STAGES" == env,pair,sms ]] && : >"$RESULTS"
  local st sts
  IFS=, read -r -a sts <<<"$STAGES"
  for st in "${sts[@]}"; do log "== stage $st"; "stage_$st" || log "stage $st reported a failure"; done
  column -t -s $'\t' "$RESULTS" | tail -n 40
  ! grep -q $'\tFAIL\t' "$RESULTS"
}
main_sms
