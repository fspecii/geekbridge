#!/usr/bin/env bash
# Rename scenarios for scripts/e2e/pair-lifecycle.sh (sourced by it; not run on its own).
#
#   rename_good      phone renamed in Settings > "This phone's name", computer renamed with `geekbridge-ctl rename`;
#                    each shows the other's new name after reconnect
#   rename_hostile   phone: too long / control characters / RTL marks / emoji stored in the name preference;
#                    computer: control characters, 40 characters, RTL marks, <b>markup</b>, emoji. Each is refused or
#                    sanitised on the side that sets it, and what the other side shows is clean
#   rename_notify    a text shared from the renamed phone raises a desktop notification that names it, sanitised
#   rename_report    Diagnostics "Copy report" (phone) and `geekbridge-ctl diagnose` (computer) hold neither custom name
# Needs the helpers of pair-lifecycle.sh (notify-send stub, fresh_pair, ...).

PN_GOOD="Zorblax Phone"
PC_GOOD="Quuxdesk"

# Types into the focused field after clearing it (adb `input text` takes ASCII; %s is a space).
type_clear() { $ADB shell input keyevent KEYCODE_MOVE_END; for _ in 1 2 3; do $ADB shell input keyevent --longpress KEYCODE_DEL >/dev/null 2>&1; $ADB shell input keyevent "$(printf 'KEYCODE_DEL %.0s' {1..20})" >/dev/null 2>&1; done; $ADB shell input text "${1// /%s}"; }
phone_rename_ui() { # NAME -> prints the name Home shows afterwards
  goto_home; ui scrolltap "This phone's name" >/dev/null || return 1
  ui wait "Computers show this name" 10 >/dev/null || return 1
  type_clear "$1"; pshot "$TAG-rename-dialog"
  ui tap Save >/dev/null; sleep 2
  ui scrollto "This phone's name" >/dev/null; ui dump | cut -f1 | awk 'f{print;exit} $0=="This phone'"'"'s name"{f=1}'
}
phone_pref_name() { # RAW_XML_TEXT : write the name preference directly (what a hostile or older value could look like)
  $ADB shell am force-stop $PKG; sleep 1
  $ADB shell "run-as $PKG sh -c 'mkdir -p shared_prefs && cat > shared_prefs/geekbridge_device_name.xml'" <<XML
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="name">$1</string>
</map>
XML
}
reconnect_pc_to_phone() { # the computer redials: both sides learn the other's current name
  ctl disconnect "$PHONE_ID" >/dev/null 2>&1; sleep 1
  goto_home; sleep 2; desk_dial; wait_for 20 is_connected
}
pc_sees_phone_name() { ctl status | jget 'd["paired"][0]["name"]'; }
pc_link_name() { ctl status | jget 'd["links"][0]["name"] if d["links"] else ""'; }
diag_find() { # TEXT : scroll Diagnostics from the top until a label contains TEXT
  goto_diag || return 1
  local i; for i in $(seq 1 14); do ui has "$1" && return 0; $ADB shell input swipe 540 1800 540 900 250; sleep 0.4; done; return 1
}
phone_shows_computer() { # prints the computer name Home shows ("To <name>" under Send file)
  goto_home; sleep 2
  local i l; for i in 1 2 3 4; do
    l="$(ui dump | cut -f1 | grep -m1 '^To ')"; [[ -n "$l" ]] && { echo "${l#To }"; return 0; }
    $ADB shell input swipe 540 1800 540 1000 250; sleep 0.4
  done; return 1
}
# Diagnostics > Paired computers keeps the name from pairing time (the saved record is not refreshed on reconnect).
phone_diag_computer() { goto_diag || return 1; local i l; for i in $(seq 1 14); do
    l="$(ui dump | cut -f1 | grep -m1 '^Unpair ')"; [[ -n "$l" ]] && { sed -E 's/^Unpair //; s/…$//' <<<"$l"; return 0; }
    $ADB shell input swipe 540 1800 540 900 250; sleep 0.4; done; return 1; }
has_bad_chars() { LC_ALL=C.UTF-8 python3 -c '
import sys,unicodedata
t=sys.stdin.read()
bad=[c for c in t if unicodedata.category(c) in ("Cc","Cf") and c!="\n"]
sys.exit(0 if bad else 1)'; }

rename_good() {
  fresh_pair || { rec rename_good FAIL "setup pairing failed"; return 1; }
  local fails="" shown
  shown="$(phone_rename_ui "$PN_GOOD")"; pshot rename-phone-home
  [[ "$shown" == "$PN_GOOD" ]] || fails+=" [Home shows '$shown']"
  reconnect_pc_to_phone || fails+=" [no link after rename]"
  [[ "$(pc_sees_phone_name)" == "$PN_GOOD" || "$(pc_link_name)" == "$PN_GOOD" ]] || fails+=" [PC lists '$(pc_sees_phone_name)' / link '$(pc_link_name)' for the phone]"
  # Computer rename: applies to connections made from now on; the phone then shows it for the paired computer.
  ctl rename "$PC_GOOD" >/dev/null || fails+=" [ctl rename refused]"
  [[ "$(ctl status | jget 'd["device_name"]')" == "$PC_GOOD" ]] || fails+=" [status device_name not updated]"
  reconnect_pc_to_phone
  local on_phone; on_phone="$(phone_shows_computer)"; pshot rename-phone-sees-pc
  [[ "$on_phone" == "$PC_GOOD" ]] || fails+=" [phone shows computer as '$on_phone']"
  # The new name survives a service restart (saved in the state dir).
  start_desktop; sleep 2
  [[ "$(ctl status | jget 'd["device_name"]')" == "$PC_GOOD" ]] || fails+=" [name lost on service restart: $(ctl status | jget 'd["device_name"]')]"
  [[ -z "$fails" ]] && rec rename_good PASS "$(date +%F) phone '$PN_GOOD' and computer '$PC_GOOD' visible on the other side after reconnect; saved across restart" \
    || rec rename_good FAIL "$(date +%F)$fails"
}

rename_hostile() {
  local fails="" notes="" out got
  # ---- computer names (ctl rename): refused or sanitised
  for spec in "control|$(printf 'bad\ao\033[31mname')|refuse" "long|$(printf 'x%.0s' {1..40})|refuse" \
              "rtl|$(printf 'Ev\xe2\x80\xaeil\xe2\x80\x8f PC')|Evil PC" "markup|<b>PC</b> & co|bPC/b & co" \
              "emoji|Desk 🖥|Desk 🖥" "only-marks|$(printf '\xe2\x80\xae\xe2\x80\x8f')|refuse"; do
    local k="${spec%%|*}" rest="${spec#*|}" want="${spec##*|}"; local raw="${rest%|*}"
    out="$(ctl rename "$raw" 2>&1)"; local rc=$?; got="$(ctl status | jget 'd["device_name"]')"
    if [[ "$want" == refuse ]]; then
      ((rc != 0)) && [[ "$got" != "$raw" ]] && notes+=" pc-$k:refused" || fails+=" [pc $k accepted: rc=$rc name='$got']"
    else
      if ((rc == 0)) && [[ "$got" == "$want" ]]; then notes+=" pc-$k:'$got'"; else fails+=" [pc $k: rc=$rc got '$got' want '$want' ($out)]"; fi
    fi
    has_bad_chars <<<"$got" && fails+=" [pc $k name keeps control/format characters]"
  done
  # ---- those computer names as the phone shows them
  ctl rename "$(printf 'Ev\xe2\x80\xaeil\xe2\x80\x8f <b>PC</b> \xf0\x9f\x96\xa5')" >/dev/null 2>&1; reconnect_pc_to_phone
  got="$(phone_shows_computer)"; pshot rename-hostile-pc-on-phone
  has_bad_chars <<<"$got" && fails+=" [phone shows computer name with control/format characters: $(printf '%q' "$got")]"
  notes+=" phone-shows-pc:'$got'"
  # ---- phone names: stored preference values the UI would refuse or that arrive from an older build
  local -a cases=("rtl|Ev&#8238;il&#8207; Phone|Evil Phone" "control|bad&#7;name|DEFAULT" \
                  "long|$(printf 'y%.0s' {1..80})|DEFAULT" "emoji|Phone &#128640;|Phone 🚀" "markup|&lt;b&gt;Hi&lt;/b&gt;|bHi/b")
  local c
  for c in "${cases[@]}"; do
    local k="${c%%|*}" body="${c#*|}"; local raw="${body%|*}" want="${body##*|}"
    phone_pref_name "$raw"; reconnect_pc_to_phone >/dev/null 2>&1; sleep 1
    got="$(pc_link_name)"; [[ -z "$got" ]] && got="$(pc_sees_phone_name)"
    has_bad_chars <<<"$got" && fails+=" [phone $k: computer shows control/format characters $(printf '%q' "$got")]"
    if [[ "$want" == DEFAULT ]]; then [[ "$got" == sdk_gphone* ]] && notes+=" phone-$k:default" || fails+=" [phone $k: computer shows '$got', wanted the model name]"
    else [[ "$got" == "$want" ]] && notes+=" phone-$k:'$got'" || fails+=" [phone $k: computer shows '$got', wanted '$want']"; fi
  done
  # The Settings dialog refuses what it cannot keep (too long cannot even be typed: the field is capped at 32).
  goto_home; ui scrolltap "This phone's name" >/dev/null && ui wait "Computers show this name" 10 >/dev/null
  type_clear "$(printf 'z%.0s' {1..60})"; got="$(ui dump | cut -f1 | grep -E '^z+$' | head -1)"
  ((${#got} <= 32)) && notes+=" ui-cap:${#got}" || fails+=" [name field accepted ${#got} characters]"
  ui tap Cancel >/dev/null 2>&1
  ctl rename "$DESK_NAME" >/dev/null 2>&1; phone_pref_name "&#8203;" ; $ADB shell am force-stop $PKG
  [[ -z "$fails" ]] && rec rename_hostile PASS "$(date +%F)$notes" || rec rename_hostile FAIL "$(date +%F)$fails |$notes"
}

rename_notify() {
  fresh_pair || { rec rename_notify FAIL "setup pairing failed"; return 1; }
  phone_rename_ui "$PN_GOOD" >/dev/null; reconnect_pc_to_phone || { rec rename_notify FAIL "no link"; return 1; }
  ctl share allow "$PHONE_ID" on >/dev/null 2>&1; ctl share auto-accept "$PHONE_ID" on >/dev/null 2>&1
  $ADB shell am force-stop $PKG; sleep 1; reconnect_pc_to_phone >/dev/null 2>&1   # fresh share session after the opt-in
  : >"$NOTIFY_LOG"
  # A text shared from the phone (ACTION_SEND to the app's share target); the computer notifies with the phone's name.
  $ADB shell am start -n "$PKG/.ShareActivity" -a android.intent.action.SEND -t text/plain --es android.intent.extra.TEXT "e2e-rename-note" >/dev/null 2>&1
  sleep 12; pshot rename-share
  local good=0; grep -qF "$PN_GOOD" "$NOTIFY_LOG" && ! has_bad_chars <"$NOTIFY_LOG" && good=1
  local first; first="$(head -c 100 "$NOTIFY_LOG" | tr '\n' ' ')"
  # Hostile stored name (direction marks + markup): the notification must carry the cleaned name only.
  phone_pref_name "Ev&#8238;il&#8207; &lt;b&gt;Hi&lt;/b&gt; &amp; Co"; reconnect_pc_to_phone >/dev/null 2>&1
  : >"$NOTIFY_LOG"; $ADB shell am start -n "$PKG/.ShareActivity" -a android.intent.action.SEND -t text/plain --es android.intent.extra.TEXT "e2e-rename-note2" >/dev/null 2>&1
  sleep 12; local hostile; hostile="$(head -c 160 "$NOTIFY_LOG" | tr '\n' ' ')"
  if ((good)) && grep -q "Evil" <<<"$hostile" && ! has_bad_chars <"$NOTIFY_LOG" && ! grep -q '[<>]' <<<"${hostile//--app-name=GeekBridge/}"; then
    rec rename_notify PASS "$(date +%F) notification names the phone: '$first'; hostile name arrives clean: '$hostile'"
  elif [[ -s "$NOTIFY_LOG" || $good == 1 ]]; then rec rename_notify FAIL "$(date +%F) good='$first' hostile='$hostile'"
  else rec rename_notify SKIP "$(date +%F) no notification at all (see pair-rename-share.png)"; fi
}

rename_report() {
  local fails="" txt
  $ADB shell am force-stop $PKG; wake
  goto_diag || { rec rename_report FAIL "cannot open Diagnostics"; return 1; }
  ui scrollto "Copy report" >/dev/null && ui tap "Copy report" >/dev/null; sleep 1
  ui scrollto Paste >/dev/null && ui tap Paste >/dev/null; sleep 2; pshot rename-report-pasted
  txt="$(ui dump | cut -f1)"
  grep -q "GeekBridge Android support report" <<<"$txt" || fails+=" [phone report text not found]"
  for n in "$PN_GOOD" "$PC_GOOD" "Zorblax" "Quuxdesk"; do grep -qF "$n" <<<"$txt" && fails+=" [phone report holds '$n']"; done
  local d; d="$(timeout 30 "${CTL[@]}" diagnose 2>&1)"
  for n in "$PN_GOOD" "$PC_GOOD" "Zorblax" "Quuxdesk" "$DESK_NAME"; do grep -qF "$n" <<<"$d" && fails+=" [computer report holds '$n']"; done
  grep -q "device-1" <<<"$d" || fails+=" [computer report has no device-1 alias: report empty?]"
  [[ -z "$fails" ]] && rec rename_report PASS "$(date +%F) neither custom name appears in the phone's Copy report or in geekbridge-ctl diagnose" || rec rename_report FAIL "$(date +%F)$fails"
}

rename_scenario() {
  TAG=rename
  $ADB shell pm grant $PKG android.permission.POST_NOTIFICATIONS >/dev/null 2>&1
  rename_good
  # leave the phone/computer renamed for the notification and report checks
  ctl rename "$PC_GOOD" >/dev/null 2>&1; phone_rename_ui "$PN_GOOD" >/dev/null
  rename_notify
  rename_report
  rename_hostile
  ctl rename "$DESK_NAME" >/dev/null 2>&1
}
