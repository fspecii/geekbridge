#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Upgrade path of the desktop service, in sandboxes only (private HOME, private loopback ports, a scratch state
# directory; nothing of the user's is read or written, and ports 1716-1764, 17381 and 8000 are never used).
# No emulator, no network. docs/reports/upgrade-path.md has the results of the last full run.
#
#   scripts/e2e/upgrade-path.sh [--bin-dir DIR] [--old-bin DIR]... [--phone-example FILE] [--capture DIR] [--keep]
#
#   --bin-dir DIR        the NEW build (geekbridge-service, geekbridge-ctl); default target/debug (what CI has built)
#   --old-bin [NAME=]DIR an OLD build to produce a state directory with (repeatable; needs --phone-example). The state
#                        is then also read back by the old build after the upgrade (the rollback).
#   --phone-example FILE target/release/examples/upgrade-phone: the stand-in phone that pairs over loopback
#   --capture DIR        copy each state directory an old build produced to DIR/<label> (fixture refresh)
#   --keep               leave the sandbox (printed at the end)
#
# What runs
#   1. every directory in tests/fixtures/upgrade/ (state written by real old builds, see its README): the new service
#      starts on a copy and must list the same pairings, keep consents, privacy choices, update state and the identity
#      byte for byte, log no migration or damage notes, create new files owner-only, keep working after a write
#      through the new commands, and (with --old-bin) be read back by the old build without trouble.
#   2. variants of the update state and the privacy / call-reply files, with and without the newer fields, damaged,
#      and written by a newer version than this build knows (the file must stay untouched).
#   3. with --old-bin: a fresh state produced by the old build itself (pairing with a stand-in phone, consents, messages,
#      switches), then 1 on it, then the rollback to the old build and the new one again.
# Exit 0 only if every check passed.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
BIN="$ROOT/target/debug" OLDS=() PHONE="" CAPTURE="" KEEP=0
while (($#)); do
  case "$1" in
    --bin-dir) BIN="$2"; shift ;;
    --old-bin) OLDS+=("$2"); shift ;;
    --phone-example) PHONE="$2"; shift ;;
    --capture) CAPTURE="$2"; shift ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '3,26p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 64 ;;
  esac
  shift
done
EXT=""; case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;; esac
NEWSVC="$BIN/geekbridge-service$EXT" NEWCTL="$BIN/geekbridge-ctl$EXT"
[[ -x "$NEWSVC" && -x "$NEWCTL" ]] || { echo "geekbridge-service / geekbridge-ctl not found in $BIN" >&2; exit 64; }
if ((${#OLDS[@]})); then [[ -x "$PHONE" ]] || { echo "--old-bin needs --phone-example FILE" >&2; exit 64; }; fi

W="$(mktemp -d "${TMPDIR:-/tmp}/gb-upgrade-path.XXXXXX")" || exit 2
HOME_DIR="$W/home"; mkdir -p "$HOME_DIR"
SVC_PID=""
FAILS=0 PASSES=0
log() { printf '[upgrade-path] %s\n' "$*" >&2; }
chk() { # name rc [detail]
  if [[ "$2" == 0 ]]; then printf 'PASS  %s %s\n' "$1" "${3:+($3)}"; PASSES=$((PASSES + 1)); else printf 'FAIL  %s %s\n' "$1" "${3:+($3)}"; FAILS=$((FAILS + 1)); fi
}
cleanup() { stop_svc; if ((KEEP)); then echo "sandbox kept: $W"; else rm -rf "$W"; fi; }
trap cleanup EXIT

# ---------------------------------------------------------------------------------------------- helpers
export HOME="$HOME_DIR" USERPROFILE="$HOME_DIR" XDG_DATA_HOME="$HOME_DIR/.local/share" XDG_CONFIG_HOME="$HOME_DIR/.config" \
  XDG_STATE_HOME="$HOME_DIR/.state" XDG_CACHE_HOME="$HOME_DIR/.cache" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_CLIPBOARD=off \
  GEEKBRIDGE_NO_TRAY=1
unset GEEKBRIDGE_STATE_DIR WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE DISPLAY
# shellcheck disable=SC2034
CUR_PORT=0 CUR_STATE="" CUR_CTLBIN=""
py() { python3 -I - "$@"; }
jfile() { py "$@"; }
ctl() { timeout 30 "$CUR_CTLBIN" --state-dir "$CUR_STATE" --control-port $((CUR_PORT + 1)) "$@"; }
# start_svc SERVICE CTL STATE [extra args]: four consecutive private ports in 20000-32000, retried when one is taken
start_svc() {
  local svc="$1" ctlb="$2" state="$3" attempt i; shift 3
  stop_svc
  CUR_STATE="$state"; CUR_CTLBIN="$ctlb"
  for attempt in 1 2 3 4 5; do
    CUR_PORT=$((20000 + RANDOM % 12000))
    : >"$W/service.log"
    "$svc" --state-dir "$state" --api-port "$CUR_PORT" --control-port $((CUR_PORT + 1)) --discovery-port $((CUR_PORT + 2)) \
      --listen-inbound "127.0.0.1:$((CUR_PORT + 3))" --no-announce --no-calling-probe "$@" >>"$W/service.log" 2>&1 &
    SVC_PID=$!
    for i in $(seq 1 60); do
      kill -0 "$SVC_PID" 2>/dev/null || break
      ctl status >/dev/null 2>&1 && return 0
      sleep 0.25
    done
    stop_svc
  done
  return 1
}
stop_svc() {
  [[ -n "$SVC_PID" ]] || return 0
  kill "$SVC_PID" 2>/dev/null
  local i; for i in $(seq 1 40); do kill -0 "$SVC_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$SVC_PID" 2>/dev/null; wait "$SVC_PID" 2>/dev/null; SVC_PID=""
  return 0
}
# a digest of every file in a state directory except the lock files (empty, recreated by every run)
digest() { (cd "$1" && find . -type f ! -name '*.lock' ! -name 'call-trace.log' -print0 | sort -z | xargs -0 sha256sum 2>/dev/null); }
mode_of() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
paired_in_status() { ctl status >"$W/status.json" 2>/dev/null; py "$W/status.json" <<'PY'
import json,sys
print(" ".join(sorted(p["device_id"] for p in json.load(open(sys.argv[1]))["paired"])))
PY
}
paired_in_file() { py "$1/trusted_peers.json" <<'PY'
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception: d={"peers":[]}
print(" ".join(sorted(p["device_id"] for p in d.get("peers",[]))))
PY
}
# consents as "id:feature" lines: the file against what the service reports
consents_in_file() { py "$1/consent.json" <<'PY'
import json,sys
try: d=json.load(open(sys.argv[1]))["devices"]
except Exception: d={}
print(" ".join(sorted(f"{k}:{f}" for k,v in d.items() for f in v)))
PY
}
consents_in_status() { ctl status >"$W/status.json" 2>/dev/null; py "$W/status.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
print(" ".join(sorted(f'{p["device_id"]}:{c["feature"]}' for p in d["paired"] for c in p["consents"] if c["enabled"])))
PY
}
# "key value" lines of `privacy show` (forced marks dropped)
privacy_show() { ctl privacy show 2>/dev/null | awk '{print $1"="$2}' | tr '\n' ' '; }
privacy_in_file() { py "$1/privacy" <<'PY'
import sys
want={}
try: text=open(sys.argv[1],encoding="utf-8",errors="replace").read()
except Exception: text=""
for line in text.splitlines()[:16]:
    if "=" not in line: continue
    k,v=line.split("=",1); k=k.strip().lower().replace("-","_"); v=v.strip().lower()
    if v in("on","true","yes","1"): want[k]="on"
    elif v in("off","false","no","0"): want[k]="off"
print(" ".join(f"{k}={v}" for k,v in sorted(want.items())))
PY
}
notes_in_log() { grep -iE 'migrat|corrupt|recover|reset|newer|regenerat|damaged|state_insecure|unreadable' "$W/service.log" | head -5 | tr '\n' '|'; }
json_subset() { # OLD.json NEW.json: every key of OLD is in NEW with the same value (NEW may add keys), except the listed volatile ones
  py "$1" "$2" "${3:-}" <<'PY'
import json,sys
a=json.load(open(sys.argv[1])); b=json.load(open(sys.argv[2])); skip=set(filter(None,sys.argv[3].split(",")))
bad=[k for k in a if k not in skip and (k not in b or a[k]!=b[k])]
print(",".join(bad)); sys.exit(1 if bad else 0)
PY
}

# ---------------------------------------------------------------------------------------------- the checks on one state
# verify_state LABEL BEFORE_DIR [OLD_SVC OLD_CTL]
# BEFORE_DIR is never written; the check runs on a copy.
verify_state() {
  local label="$1" before="$2" oldsvc="${3:-}" oldctl="${4:-}" st="$W/v-$1/state" d
  rm -rf "$W/v-$label"; mkdir -p "$W/v-$label"; cp -a -- "$before" "$st"; chmod 700 "$st"
  find "$st" -type f -exec chmod 600 {} + 2>/dev/null   # git keeps no modes; the service insists on owner-only files
  find "$st" -name '*.lock' -delete 2>/dev/null
  local base; base="$(digest "$st")"
  local pf cf pr; pf="$(paired_in_file "$st")"; cf="$(consents_in_file "$st")"; pr="$(privacy_in_file "$st")"
  # -- the new service opens the old state
  start_svc "$NEWSVC" "$NEWCTL" "$st"; chk "$label: new service starts on the old state" $? "$(head -c 200 "$W/service.log" | tr '\n' '|')"
  [[ -n "$SVC_PID" ]] || return 1
  local got; got="$(paired_in_status)"
  chk "$label: pairings intact" $([[ "$got" == "$pf" ]]; echo $?) "file [$pf] service [$got]"
  got="$(consents_in_status)"
  # nothing the user gave is lost, and nothing is added by an upgrade (a debug build of the phone adds 'debug' when it connects)
  local lost added
  lost="$(comm -23 <(tr ' ' '\n' <<<"$cf" | sort) <(tr ' ' '\n' <<<"$got" | sort) | grep -v '^$' | tr '\n' ' ')"
  added="$(comm -13 <(tr ' ' '\n' <<<"$cf" | sort) <(tr ' ' '\n' <<<"$got" | sort) | grep -v '^$' | grep -v ':debug$' | tr '\n' ' ')"
  chk "$label: consents unchanged" $([[ -z "$lost" && -z "$added" ]]; echo $?) "lost [$lost] added [$added]"
  if [[ -f "$st/privacy" ]]; then
    local show; show="$(privacy_show)"; local bad="" kv
    for kv in $pr; do [[ " $show " == *" $kv "* ]] || bad="$bad $kv"; done
    # a stored choice is what `privacy show` reports, unless a marker file or an environment variable forces another
    chk "$label: privacy choices as stored" $([[ -z "$bad" || -e "$st/sms-notify-content" ]]; echo $?) "file [$pr] show [$show] differs:[$bad]"
  fi
  ctl update >"$W/update.txt" 2>&1; chk "$label: update status answers" $? "$(grep -m1 checking "$W/update.txt")"
  if [[ -f "$st/update-state.json" ]]; then
    local want; want="$(py "$st/update-state.json" <<'PY'
import json,sys
print("on" if json.load(open(sys.argv[1])).get("enabled",True) else "off")
PY
)"
    chk "$label: update switch as stored ($want)" $(grep -q "checking: automatic $want" "$W/update.txt"; echo $?) ""
  fi
  ctl doctor >"$W/doctor.txt" 2>&1; chk "$label: doctor runs" $(grep -q 'problem(s)' "$W/doctor.txt" && ! grep -qi panicked "$W/doctor.txt"; echo $?) "$(grep -m1 'problem(s)' "$W/doctor.txt")"
  chk "$label: ctl diagnose works" $(ctl diagnose 2>&1 | grep -qi version; echo $?) ""
  chk "$label: no migration or damage notes in the log" $([[ -z "$(notes_in_log)" ]]; echo $?) "$(notes_in_log)"
  chk "$label: no .corrupt files" $([[ -z "$(find "$st" -name '*.corrupt')" ]]; echo $?) ""
  # -- untouched by starting: byte for byte
  local f changed=""
  for f in trusted_peers.json identity.pem consent.json share.json device-name privacy call-replies sms-notify-content tray-disabled; do
    [[ -e "$before/$f" ]] || continue
    cmp -s "$before/$f" "$st/$f" || changed="$changed $f"
  done
  chk "$label: pairing, identity, consent, share, privacy and reply files byte-identical after start" $([[ -z "$changed" ]]; echo $?) "changed:[$changed]"
  for f in update-state.json clipboard-settings.json; do
    [[ -e "$before/$f" ]] || continue
    local miss; miss="$(json_subset "$before/$f" "$st/$f" "last_checked_unix,etag,release,checked_for,backoff_until,last_attempt_unix")"
    chk "$label: $f keeps every stored field" $? "$miss"
  done
  # -- writes through the new commands
  local id; id="$(awk '{print $1}' <<<"$pf")"
  if [[ -n "$id" ]]; then
    ctl consent "$id" photos on >/dev/null 2>&1; chk "$label: new consent can be given" $? ""
  fi
  ctl privacy set respect_dnd on >/dev/null 2>&1; chk "$label: privacy switch writes" $? ""
  ctl privacy set caller_number off >/dev/null 2>&1
  ctl update disable >/dev/null 2>&1; ctl update enable >/dev/null 2>&1; chk "$label: update switch writes" $? ""
  # -- modes of files the new build created, and of the directory
  local loose="" n
  while IFS= read -r -d '' n; do
    [[ -e "$before/${n#"$st"/}" ]] && continue
    case "$n" in *.lock) continue ;; esac
    case "$(mode_of "$n")" in 600|400) ;; *) loose="$loose ${n#"$st"/}=$(mode_of "$n")" ;; esac
  done < <(find "$st" -type f -print0)
  chk "$label: files created by the new build are owner-only" $([[ -z "$loose" ]]; echo $?) "loose:[$loose]"
  chk "$label: state directory mode 700" $([[ "$(mode_of "$st")" == 700 ]]; echo $?) "$(mode_of "$st")"
  # what it wrote must still hold the old facts
  chk "$label: pairings and consents survive the writes" $([[ "$(paired_in_status)" == "$pf" && -z "$(comm -23 <(tr ' ' '\n' <<<"$cf" | sort) <(tr ' ' '\n' <<<"$(consents_in_status)" | sort) | grep -v '^$')" ]]; echo $?) ""
  stop_svc
  chk "$label: trusted_peers.json and identity.pem still byte-identical after the writes" $(cmp -s "$before/trusted_peers.json" "$st/trusted_peers.json" && cmp -s "$before/identity.pem" "$st/identity.pem"; echo $?) ""
  # -- the way back: the old build reads what the new one wrote
  if [[ -n "$oldsvc" ]]; then
    start_svc "$oldsvc" "$oldctl" "$st" --dev-loopback; chk "$label: ROLLBACK old service starts on the state the new one wrote" $? "$(head -c 200 "$W/service.log" | tr '\n' '|')"
    if [[ -n "$SVC_PID" ]]; then
      got="$(paired_in_status)"; chk "$label: ROLLBACK pairings intact" $([[ "$got" == "$pf" ]]; echo $?) "[$got]"
      chk "$label: ROLLBACK no damage notes" $([[ -z "$(notes_in_log)" ]]; echo $?) "$(notes_in_log)"
      [[ -n "$id" ]] && ctl consent "$id" sms on >/dev/null 2>&1; ctl update skip 8.8.8 >/dev/null 2>&1
      stop_svc
      chk "$label: ROLLBACK rewrote consent.json without dropping the new consent" $(grep -q photos "$st/consent.json"; echo $?) ""
      chk "$label: ROLLBACK kept the update fields it does not know" $(py "$st/update-state.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("skipped_version")=="8.8.8" else 1)
PY
echo $?) ""
      start_svc "$NEWSVC" "$NEWCTL" "$st"; chk "$label: new service starts again after the rollback" $? ""
      got="$(paired_in_status)"; chk "$label: pairings intact after the round trip" $([[ "$got" == "$pf" ]]; echo $?) "[$got]"
      stop_svc
    fi
  fi
  return 0
}

# ---------------------------------------------------------------------------------------------- 1. committed fixtures
FIX="$ROOT/tests/fixtures/upgrade"
OLDSVC="" OLDCTL=""
if ((${#OLDS[@]})); then OLDSVC="${OLDS[0]#*=}/geekbridge-service$EXT"; OLDCTL="${OLDS[0]#*=}/geekbridge-ctl$EXT"; fi
for d in "$FIX"/*/; do
  [[ -d "$d" ]] || continue
  name="$(basename "$d")"
  verify_state "fixture-$name" "${d%/}" "$OLDSVC" "$OLDCTL"
done

# ---------------------------------------------------------------------------------------------- 2. variants of single files
# Start from the first fixture's pairing (or a fresh state) and change one file at a time.
BASEFIX="$(find "$FIX" -mindepth 1 -maxdepth 1 -type d | sort | head -1)"
variant() { # NAME MUTATOR-function : a copy of BASEFIX, mutated, then opened by the new service
  local name="$1" fn="$2" v="$W/variant-$1"
  rm -rf "$v"; cp -a -- "$BASEFIX" "$v"; chmod 700 "$v"; find "$v" -type f -exec chmod 600 {} +; find "$v" -name '*.lock' -delete
  "$fn" "$v"
  local vf="update-state.json"
  case "$name" in pr-*) vf=privacy ;; rp-*) vf=call-replies ;; peers-*) vf=trusted_peers.json ;; consent-*) vf=consent.json ;; esac
  cp -- "$v/$vf" "$W/vfile.before" 2>/dev/null || : >"$W/vfile.before"
  start_svc "$NEWSVC" "$NEWCTL" "$v"; local up=$?
  chk "variant $name: service starts" $up "$(head -c 200 "$W/service.log" | tr '\n' '|')"
  ((up == 0)) || return
  chk "variant $name: pairing listed" $([[ "$(paired_in_status)" == "$(paired_in_file "$v")" && -n "$(paired_in_status)" ]]; echo $?) ""
  ctl update >/dev/null 2>&1; chk "variant $name: update status answers" $? ""
  ctl privacy show >"$W/priv.txt" 2>&1; chk "variant $name: privacy show answers" $? "$(tr '\n' ';' <"$W/priv.txt" | tr -s ' ')"
  stop_svc
  [[ "$name" == *newer* || "$name" == *damaged* || "$name" == *garbage* ]] && \
    chk "variant $name: $vf is left as it was (nothing silently rewritten)" $(cmp -s "$W/vfile.before" "$v/$vf"; echo $?) ""
  VARIANT_DIR="$v"
}
# update-state.json
us() { py "$1/update-state.json" "$2" <<'PY'
import json,sys
p=sys.argv[1]
try: d=json.load(open(p))
except Exception: d={}
exec(sys.argv[2])
json.dump(d,open(p,"w"),indent=2)
PY
}
m_us_old() { echo '{"enabled":true,"skipped_version":null,"etag":null,"last_checked_unix":1700000000,"release":null,"no_public_release":false}' >"$1/update-state.json"; }
m_us_new() { us "$1" 'd.update({"version":1,"backoff_until":4102444800,"last_attempt_unix":1793000000,"checked_for":"0.1.0","last_install":{"outcome":"installed","version":"0.2.0","previous":"0.1.0","detail":"","unix":1793000000},"future_field":[1,2]})'; }
m_us_newer() { us "$1" 'd.update({"version":99,"enabled":False})'; }
m_us_damaged() { printf '{"enabled": tru' >"$1/update-state.json"; rm -f "$1/update-state.json.bak"; }   # no backup to fall back on
m_us_empty() { : >"$1/update-state.json"; }   # with a backup
m_us_missing() { rm -f "$1/update-state.json" "$1/update-state.json.bak"; }
# privacy and call-replies
m_pr_old() { printf 'message_text=on\ncaller_number=off\n' >"$1/privacy"; chmod 600 "$1/privacy"; }
m_pr_dnd() { printf 'message_text=off\ncaller_number=on\nphone_notification_text=on\nrespect_dnd=on\n' >"$1/privacy"; chmod 600 "$1/privacy"; }
m_pr_crlf() { printf 'respect_dnd=on\r\nfuture_switch=on\r\n\r\ngarbage\r\nmessage_text=maybe\r\n' >"$1/privacy"; chmod 600 "$1/privacy"; }
m_pr_garbage() { head -c 300 /dev/urandom >"$1/privacy"; chmod 600 "$1/privacy"; }
m_pr_missing() { rm -f "$1/privacy"; }
m_rp_ok() { printf 'Busy, call you later\nIn a meeting\nCan I call back?\n' >"$1/call-replies"; chmod 600 "$1/call-replies"; }
m_rp_toomany() { printf 'a\nb\nc\nd\ne\n' >"$1/call-replies"; chmod 600 "$1/call-replies"; }
m_rp_long() { printf '%0100d\n' 7 >"$1/call-replies"; chmod 600 "$1/call-replies"; }
m_rp_garbage() { head -c 200 /dev/urandom >"$1/call-replies"; chmod 600 "$1/call-replies"; }
m_peers_newer() { py "$1/trusted_peers.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d["version"]=99; d["new_thing"]=True
json.dump(d,open(p,"w"))
PY
}
m_consent_newer() { py "$1/consent.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d["version"]=99
json.dump(d,open(p,"w"))
PY
}
if [[ -n "$BASEFIX" ]]; then
  for v in us_old us_new us_newer us_damaged us_empty us_missing pr_old pr_dnd pr_crlf pr_garbage pr_missing \
           rp_ok rp_toomany rp_long rp_garbage peers_newer consent_newer; do
    variant "${v//_/-}" "m_$v"
  done
  # the variant files really are read: the update switch and the privacy values of the new-field variants
  v="$W/variant-pr-dnd"
  if [[ -d "$v" ]]; then
    start_svc "$NEWSVC" "$NEWCTL" "$v"
    chk "variant pr-dnd: respect_dnd=on and message_text=off are read back" $([[ "$(ctl privacy show | tr -s ' ')" == *"respect_dnd on"* && "$(ctl privacy show | tr -s ' ')" == *"message_text off"* ]]; echo $?) ""
    stop_svc
  fi
  v="$W/variant-us-newer"
  if [[ -d "$v" ]]; then
    start_svc "$NEWSVC" "$NEWCTL" "$v"; ctl update disable >/dev/null 2>&1; ctl consent "$(paired_in_file "$v")" photos on >/dev/null 2>&1; stop_svc
    chk "variant us-newer: a write is refused or ignored, the newer file is not overwritten" $(py "$v/update-state.json" <<'PY'
import json,sys
sys.exit(0 if json.load(open(sys.argv[1])).get("version")==99 else 1)
PY
echo $?) ""
  fi
fi

# ---------------------------------------------------------------------------------------------- 3. state produced by the old build itself
live_old() { # OLD_BIN_DIR
  local ob="${1#*=}" label; label="$(basename "$ob")"; [[ "$1" == *=* ]] && label="${1%%=*}"
  local osvc="$ob/geekbridge-service$EXT" octl="$ob/geekbridge-ctl$EXT"
  [[ -x "$osvc" && -x "$octl" ]] || { chk "old build $ob present" 1 "missing binaries"; return; }
  local ver; ver="$("$octl" --version 2>&1 | head -1)"
  local st="$W/live-$label/state"; mkdir -p "$st"; chmod 700 "$st"
  start_svc "$osvc" "$octl" "$st" --dev-loopback --name "Old-PC"; chk "live $label: old service ($ver) starts on an empty state" $? ""
  [[ -n "$SVC_PID" ]] || return
  local out; out="$(timeout 90 "$PHONE" --state-dir "$st" --control-port $((CUR_PORT + 1)) --messages 6 --hold 4 2>&1)"
  local id; id="$(sed -n 's/^PAIRED //p' <<<"$out" | head -1)"
  chk "live $label: stand-in phone pairs, consents given, messages sent" $([[ -n "$id" ]]; echo $?) "${id:-$(tail -c 300 <<<"$out")}"
  [[ -n "$id" ]] || return
  ctl consent "$id" telephony on >/dev/null 2>&1
  ctl share allow "$id" on >/dev/null 2>&1; ctl share auto-accept "$id" on >/dev/null 2>&1
  ctl rename "Renamed-Old-PC" >/dev/null 2>&1
  ctl tray disable >/dev/null 2>&1
  ctl update skip 9.9.9 >/dev/null 2>&1; ctl update disable >/dev/null 2>&1
  if ctl privacy show >/dev/null 2>&1; then
    ctl privacy set message_text on >/dev/null 2>&1; ctl privacy set caller_number off >/dev/null 2>&1
    ctl privacy set respect_dnd on >/dev/null 2>&1
    printf 'Busy, call you later\nIn a meeting\n' >"$st/call-replies"; chmod 600 "$st/call-replies"   # what "Decline with message" saves
  else
    touch "$st/sms-notify-content"   # the only privacy switch this build has
  fi
  sleep 1; stop_svc
  [[ -n "$CAPTURE" ]] && { mkdir -p "$CAPTURE/$label"; rm -rf "$CAPTURE/$label"; cp -a -- "$st" "$CAPTURE/$label"; find "$CAPTURE/$label" \( -name '*.lock' -o -name 'call-trace.log' \) -delete; }
  verify_state "live-$label" "$st" "$osvc" "$octl"
}
for ob in "${OLDS[@]}"; do live_old "$ob"; done

echo
echo "upgrade-path: $PASSES passed, $FAILS failed"
((FAILS == 0))
