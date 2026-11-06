#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Crash and power-loss safety of the desktop service, in sandboxes only (private HOME / XDG / state directory and
# private loopback ports 20000-32000; never ports 1716-1764, 17381 or 8000, never the user's service or files).
# No emulator, no network, no root. docs/reports/crash-safety.md has the results of the last run.
#
#   scripts/chaos-crash.sh [--bin-dir DIR] [--failpoint-bin DIR] [--phone FILE] [--rounds N] [--only a,b,...] [--jobs N] [--keep]
#
#   --bin-dir DIR        geekbridge-service, geekbridge-ctl (and geekbridge-calls for `ring`); default target/release
#   --failpoint-bin DIR  a service built with `--features bridge-kde/failpoints` (scenario `failpoint`); optional
#   --phone FILE         target/<profile>/examples/chaos-phone, the stand-in phone (scenarios needing a link)
#   --rounds N           iterations per scenario (default 20)
#   --only LIST          trust,failpoint,transfer,messages,ring,diskfull,readonly,clock
#   --jobs N             iterations run in parallel (default 6)
#   --keep               leave the sandbox (printed at the end)
#
# Scenarios (each iteration: fresh sandbox, run, SIGKILL or fault at a random moment, restart, check)
#   trust      a writer loop changes consents, privacy, share, update and name files; SIGKILL at a random moment
#   failpoint  the process aborts exactly between "temp file written" and "rename" of each kind of state file
#   transfer   a 64 MiB file arrives from the stand-in phone; SIGKILL at a random moment of the transfer
#   messages   3000 messages are pushed while the service is killed
#   ring       a call rings (or is answered) with the real incoming-call card on a private X server; SIGKILL
#   diskfull   writes under the state folder (and the save folder) fail with ENOSPC while the service runs
#   readonly   the state folder loses write permission while running, and when starting
#   clock      the wall clock of a running service is stepped by -10 years .. +60 years
# A card for an ANSWERED call whose service never comes back closes by itself after 3 minutes (INFO line); a ringing
# card after about 2 s.
# After every kill: the service starts, the pairing is still there, every state file parses, no temp file of a
# dead writer remains, `doctor` answers. Exit 0 only if every check passed.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
BIN="$ROOT/target/release" FPBIN="" PHONE="" ROUNDS=20 ONLY="trust,failpoint,transfer,messages,ring,diskfull,readonly,clock" JOBS=6 KEEP=0
while (($#)); do
  case "$1" in
    --bin-dir) BIN="$2"; shift ;;
    --failpoint-bin) FPBIN="$2"; shift ;;
    --phone) PHONE="$2"; shift ;;
    --rounds) ROUNDS="$2"; shift ;;
    --only) ONLY="$2"; shift ;;
    --jobs) JOBS="$2"; shift ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '3,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 64 ;;
  esac
  shift
done
[[ "$(uname -s)" == Linux ]] || { echo "chaos-crash.sh needs Linux (SIGKILL, /proc, LD_PRELOAD)"; exit 0; }
SVC="$BIN/geekbridge-service" CTLB="$BIN/geekbridge-ctl"
[[ -x "$SVC" && -x "$CTLB" ]] || { echo "geekbridge-service / geekbridge-ctl not found in $BIN" >&2; exit 64; }
W="$(mktemp -d "${TMPDIR:-/tmp}/gb-chaos-crash.XXXXXX")" || exit 2
mkdir -p "$W/res"
XVFB_PID=""
cleanup() {
  [[ -n "$XVFB_PID" ]] && kill "$XVFB_PID" 2>/dev/null
  # only the processes this run started: every sandbox path contains $W
  local p
  for p in $(pgrep -f -- "$W" 2>/dev/null); do [[ "$p" == "$$" ]] || kill -9 "$p" 2>/dev/null; done
  if ((KEEP)); then echo "sandbox kept: $W"; else rm -rf "$W"; fi
}
trap cleanup EXIT
want() { [[ ",$ONLY," == *",$1,"* ]]; }
log() { printf '[chaos-crash] %s\n' "$*" >&2; }

# ---------------------------------------------------------------------------------------------- helpers
unset GEEKBRIDGE_STATE_DIR WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE DISPLAY
py() { python3 -I - "$@"; }
rnd() { echo $((RANDOM * 32768 % $1 + RANDOM % $1)) ; } # not uniform enough for statistics, fine for jitter
ms_sleep() { python3 -I -c 'import sys,time; time.sleep(int(sys.argv[1])/1000)' "$1"; }
rand_ms() { python3 -I -c 'import random,sys; print(random.randint(int(sys.argv[1]),int(sys.argv[2])))' "$1" "$2"; }

# per-iteration environment: a private HOME and a state directory that does not exist yet (the service creates it private)
iter_env() { # name i
  D="$W/$1.$2"; mkdir -p "$D/home"; ST="$D/st"; SAVE="$D/home/Downloads/GeekBridge"
  export HOME="$D/home" USERPROFILE="$D/home" XDG_DATA_HOME="$D/home/.local/share" XDG_CONFIG_HOME="$D/home/.config" \
    XDG_STATE_HOME="$D/home/.state" XDG_CACHE_HOME="$D/home/.cache" GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_CLIPBOARD=off GEEKBRIDGE_NO_TRAY=1
  SVC_PID="" CUR_PORT=0
}
ctl() { timeout 30 "$CTLB" --state-dir "$ST" --control-port $((CUR_PORT + 1)) "$@"; }
# start_svc BIN [extra args]; leaves SVC_PID and CUR_PORT; 4 consecutive random private ports, retried when taken
start_svc() {
  local svc="$1" attempt i; shift
  for attempt in 1 2 3 4 5; do
    CUR_PORT=$((20000 + RANDOM % 12000))
    [[ -n "${REUSE_PORT:-}" && $attempt == 1 ]] && CUR_PORT=$REUSE_PORT # a restart on the same ports, as a real one is
    : >"$D/service.log"
    "$svc" --state-dir "$ST" --api-port "$CUR_PORT" --control-port $((CUR_PORT + 1)) --discovery-port $((CUR_PORT + 2)) \
      --listen-inbound "127.0.0.1:$((CUR_PORT + 3))" --no-announce --no-calling-probe --dev-loopback "$@" >>"$D/service.log" 2>&1 &
    SVC_PID=$!
    for i in $(seq 1 80); do
      kill -0 "$SVC_PID" 2>/dev/null || break
      ctl status >/dev/null 2>&1 && return 0
      sleep 0.1
    done
    stop_svc
  done
  return 1
}
stop_svc() { # polite stop
  [[ -n "${SVC_PID:-}" ]] || return 0
  kill "$SVC_PID" 2>/dev/null
  local i; for i in $(seq 1 30); do kill -0 "$SVC_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$SVC_PID" 2>/dev/null; wait "$SVC_PID" 2>/dev/null; SVC_PID=""
}
kill9() { [[ -n "${SVC_PID:-}" ]] && { kill -9 "$SVC_PID" 2>/dev/null; wait "$SVC_PID" 2>/dev/null; SVC_PID=""; }; }
say() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}"; } # PASS|FAIL|INFO name detail
res() { # name rc detail
  if [[ "$2" == 0 ]]; then say PASS "$1" "${3:-}"; else say FAIL "$1" "${3:-}"; fi
}
paired_in_status() { ctl status >"$D/status.json" 2>/dev/null; py "$D/status.json" <<'PY'
import json,sys
try: print(" ".join(sorted(p["device_id"] for p in json.load(open(sys.argv[1]))["paired"])))
except Exception: print("?")
PY
}
paired_in_file() { py "$ST/trusted_peers.json" <<'PY'
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception: d={"peers":[]}
print(" ".join(sorted(p["device_id"] for p in d.get("peers",[]))))
PY
}
# every state file readable: JSON where JSON, key=value lines where those; prints what is wrong
damaged_files() { py "$ST" <<'PY'
import json,os,re,sys
d=sys.argv[1]; bad=[]
for n in sorted(os.listdir(d)):
    p=os.path.join(d,n)
    if not os.path.isfile(p) or n.endswith(".lock"): continue
    if n.startswith(".") and ".tmp." in n: continue
    try: raw=open(p,"rb").read()
    except Exception as e: bad.append(f"{n}: unreadable {e}"); continue
    if n.endswith((".json",".bak")) and n not in ("identity.pem.bak",):
        if n.endswith(".bak") and not n.endswith(".json.bak"): continue
        try: json.loads(raw)
        except Exception: bad.append(f"{n}: not JSON ({len(raw)} bytes)")
    elif n=="identity.pem":
        if not raw.startswith(b"-----BEGIN") or b"END" not in raw: bad.append("identity.pem: damaged")
    elif n=="privacy":
        for l in raw.decode("utf8","replace").splitlines():
            if l.strip() and not re.fullmatch(r"[a-z_]+=(on|off)",l.strip()): bad.append(f"privacy: bad line {l[:30]!r}")
    if os.stat(p).st_mode & 0o077 and not n.endswith(".log"): bad.append(f"{n}: mode {oct(os.stat(p).st_mode&0o777)}")
print("; ".join(bad))
PY
}
temps_in() { find "$1" -maxdepth 1 -name '.*.tmp.*' 2>/dev/null | wc -l; }
# the checks after a kill: restart, pairing, files, temp files, doctor
verify() { # label expected_pairs [svc binary]
  local label="$1" want="$2" svc="${3:-$SVC}" before got bad left
  before="$(temps_in "$ST")"
  if ! start_svc "$svc"; then res "$label: service restarts" 1 "$(tail -c 400 "$D/service.log" | tr '\n' ' ')"; return 1; fi
  res "$label: service restarts" 0
  got="$(paired_in_status)"
  res "$label: pairings kept" $([[ "$got" == "$want" && -n "$want" ]]; echo $?) "want [$want] got [$got]"
  bad="$(damaged_files)"; res "$label: state files intact" $([[ -z "$bad" ]]; echo $?) "$bad"
  left="$(temps_in "$ST")"; res "$label: no temp file of a dead writer after start" $((left > 0)) "$before before start, $left after"
  ctl doctor >"$D/doctor.txt" 2>&1; local rc=$?
  res "$label: doctor answers" $([[ ($rc == 0 || $rc == 1) && -s "$D/doctor.txt" ]] && ! grep -qi panicked "$D/doctor.txt"; echo $?) "rc=$rc: $(grep -m1 -E 'problem|FAIL|✗|Fix|fix' "$D/doctor.txt" | cut -c1-110)"
}
# a state folder with an identity, and one pairing made by the stand-in phone; $PAIRED is its device id
pair_phone() { # leaves PHONE_PID, fd 7 (commands to the phone), $D/phone.out
  rm -f "$D/in"; mkfifo "$D/in"
  "$PHONE" --state-dir "$ST" --control-port $((CUR_PORT + 1)) --features "${1:-sms,telephony}" <"$D/in" >"$D/phone.out" 2>&1 &
  PHONE_PID=$!
  exec 7>"$D/in"
  local i; PAIRED=""
  for i in $(seq 1 100); do
    PAIRED="$(sed -n 's/^PAIRED //p' "$D/phone.out" 2>/dev/null | head -1)"
    [[ -n "$PAIRED" ]] && return 0
    kill -0 "$PHONE_PID" 2>/dev/null || return 1
    sleep 0.1
  done
  return 1
}
end_phone() { exec 7>&- 2>/dev/null; kill "${PHONE_PID:-0}" 2>/dev/null; wait "${PHONE_PID:-0}" 2>/dev/null; }
par() { # scenario fn: runs fn I for I in 1..ROUNDS, JOBS at a time, results to $W/res/<scenario>.<I>
  local name="$1" fn="$2" i pids=()
  for i in $(seq 1 "$ROUNDS"); do
    ( "$fn" "$i" ) >"$W/res/$name.$i" 2>&1 &
    pids+=($!)
    while (($(jobs -rp | wc -l) >= JOBS)); do sleep 0.1; done
  done
  wait "${pids[@]}"
}
FAILS=0 PASSES=0
report() { # scenario
  local name="$1" f total
  echo "== $name ($ROUNDS rounds)"
  cat "$W/res/$name".* 2>/dev/null | sed -E 's/\[[^]]*\]/[*]/' | awk -F'\t' '
    $1=="PASS"||$1=="FAIL" { n[$2]++; if ($1=="FAIL") { f[$2]++; if (!(($2) in first)) first[$2]=$3 } }
    $1=="INFO" { info[$2]++ ; if (!(($2) in ifirst)) ifirst[$2]=$3 }
    END {
      for (k in n) { printf "%s  %s  %d/%d%s\n", (f[k]?"FAIL":"PASS"), k, n[k]-f[k], n[k], (f[k]? "  e.g. " first[k] : "") }
      for (k in info) printf "INFO  %s  x%d  e.g. %s\n", k, info[k], ifirst[k]
    }' | sort -k2
  PASSES=$((PASSES + $(cat "$W/res/$name".* 2>/dev/null | grep -c '^PASS')))
  FAILS=$((FAILS + $(cat "$W/res/$name".* 2>/dev/null | grep -c '^FAIL')))
}

SHIM=""
if command -v gcc >/dev/null 2>&1 && gcc -shared -fPIC -O2 -o "$W/shim.so" "$ROOT/scripts/chaos/shim.c" -ldl 2>/dev/null; then SHIM="$W/shim.so"; fi
(( $(ulimit -n) < 1024 )) && ulimit -n 4096 2>/dev/null

# ---------------------------------------------------------------------------------------------- trust
# A writer loop of ctl commands that each rewrite one state file; the service is killed after a random time.
WRITES=(
  "consent @ID sms off" "consent @ID sms on" "consent @ID clipboard on" "consent @ID clipboard off"
  "privacy set message_text on" "privacy set message_text off" "privacy set caller_number off" "privacy set caller_number on"
  "share allow @ID on" "share allow @ID off" "share auto-accept @ID on" "share auto-accept @ID off"
  "update disable" "update enable" "update skip 9.9.9" "rename ChaosPC"
)
writer_loop() { # device id
  local c i=0
  while :; do
    for c in "${WRITES[@]}"; do ctl ${c//@ID/$1} >/dev/null 2>&1; done
    i=$((i + 1))
  done
}
t_trust() {
  local i="$1"; iter_env trust "$i"
  start_svc "$SVC" || { say FAIL "trust: first start" "$(tail -c 300 "$D/service.log")"; return; }
  if [[ -n "$PHONE" ]]; then
    pair_phone "sms" || { say FAIL "trust: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
    end_phone
  fi
  local want; want="$(paired_in_file)"
  writer_loop "${PAIRED:-none}" & local wp=$!
  ms_sleep "$(rand_ms 5 900)"
  kill9
  kill -9 "$wp" 2>/dev/null; wait "$wp" 2>/dev/null
  (( $(temps_in "$ST") > 0 )) && say INFO "trust: killed with a temp file on disk" "$(ls -A "$ST" | grep tmp | head -1)"
  verify "trust" "$want"
  # and the state files still take writes after the crash
  ctl privacy set message_text on >/dev/null 2>&1; res "trust: writes work after the crash" $? ""
  stop_svc
}

# ---------------------------------------------------------------------------------------------- failpoint
FP_OPS=("consent @ID sms off" "privacy set message_text on" "share allow @ID on" "update disable" "rename ChaosPC" "update skip 9.9.9")
t_failpoint() {
  local i="$1" op fidx; iter_env failpoint "$i"
  fidx=$(((i - 1) % ${#FP_OPS[@]})); op="${FP_OPS[$fidx]}"
  # state with a pairing, made by the normal build
  start_svc "$SVC" || { say FAIL "failpoint: first start" ""; return; }
  pair_phone "sms" || { say FAIL "failpoint: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
  end_phone; local want; want="$(paired_in_file)"
  ctl privacy set respect_dnd on >/dev/null 2>&1; ctl share allow "$PAIRED" off >/dev/null 2>&1; ctl update enable >/dev/null 2>&1
  stop_svc
  local digest_before; digest_before="$(cd "$ST" && find . -type f ! -name '*.lock' ! -name 'call-trace.log' -print0 | sort -z | xargs -0 sha256sum)"
  if ! GB_FAILPOINT=before_rename start_svc "$FPBIN/geekbridge-service"; then
    say INFO "failpoint: service aborts while starting (first atomic write)" "$(tail -c 100 "$D/service.log" | tr '\n' ' ')"
  fi
  ctl ${op//@ID/$PAIRED} >"$D/op.out" 2>&1
  sleep 0.5
  res "failpoint [$op]: process died between temp write and rename" $(kill -0 "$SVC_PID" 2>/dev/null; echo $((1 - $?))) ""
  kill9
  local digest_after; digest_after="$(cd "$ST" && find . -type f ! -name '*.lock' ! -name 'call-trace.log' ! -name '.*.tmp.*' -print0 | sort -z | xargs -0 sha256sum)"
  # the aborted write may only have added temp files; every real file is as before, except what the service itself rewrote
  local changed; changed="$(diff <(echo "$digest_before") <(echo "$digest_after") | grep -c '^[<>]')"
  res "failpoint [$op]: no existing state file changed" $((changed > 0)) "$changed lines differ"
  say INFO "failpoint [$op]: temp files left by the abort" "$(temps_in "$ST")"
  verify "failpoint [$op]" "$want"
  ctl ${op//@ID/$PAIRED} >/dev/null 2>&1; res "failpoint [$op]: the same change works after the restart" $? ""
  stop_svc
}

# ---------------------------------------------------------------------------------------------- transfer
EXPECT_SHA=""
expected_sha() { # sha256 of the pattern file of $1 MiB
  python3 -I - "$1" <<'PY'
import hashlib,sys
n=int(sys.argv[1])<<20
blk=bytes((i*13)%251 for i in range(251))
h=hashlib.sha256(); left=n; big=blk*4000
while left>0:
    k=min(left,len(big)); h.update(big[:k]); left-=k
# the pattern restarts every 251 bytes only if blocks are a multiple of 251: len(big)=251*4000
print(h.hexdigest())
PY
}
FILE_MIB=64
t_transfer() {
  local i="$1" j; iter_env transfer "$i"
  start_svc "$SVC" || { say FAIL "transfer: first start" ""; return; }
  pair_phone "sms" || { say FAIL "transfer: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
  local want; want="$(paired_in_file)"
  ctl share allow "$PAIRED" on >/dev/null; ctl share auto-accept "$PAIRED" on >/dev/null
  echo "file $FILE_MIB" >&7
  local seen=0
  for j in $(seq 1 400); do
    ls "$SAVE"/.gb-*.part >/dev/null 2>&1 && { seen=1; break; }
    sleep 0.01
  done
  res "transfer: a partial file appears while receiving" $((1 - seen)) ""
  ms_sleep "$(rand_ms 0 700)"
  local part_bytes; part_bytes="$(cat "$SAVE"/.gb-*.part 2>/dev/null | wc -c)"
  kill9; end_phone
  local listing; listing="$(cd "$SAVE" 2>/dev/null && ls -A | tr '\n' ' ')"
  local finals; finals="$(find "$SAVE" -maxdepth 1 -type f ! -name '.gb-*' -printf '%s ' 2>/dev/null)"
  say INFO "transfer: after the kill, in the save folder" "$listing (final-name sizes: ${finals:-none}; part: $part_bytes bytes)"
  verify "transfer" "$want"
  sleep 0.3
  local parts finals_after; parts="$(find "$SAVE" -maxdepth 1 -name '.gb-*' | wc -l)"
  res "transfer: no partial or reserved file after restart" $((parts > 0)) "$parts left"
  finals_after="$(find "$SAVE" -maxdepth 1 -type f)"
  if [[ -n "$finals_after" ]]; then
    local f size sum
    for f in $finals_after; do
      size="$(stat -c %s "$f")"; sum="$(sha256sum "$f" | cut -d' ' -f1)"
      res "transfer: a file under its final name is whole and correct" $([[ "$size" == $((FILE_MIB << 20)) && "$sum" == "$EXPECT_SHA" ]]; echo $?) "$size bytes"
    done
  else
    say INFO "transfer: killed before the file was finished: nothing under the final name" ""
  fi
  stop_svc
}

# ---------------------------------------------------------------------------------------------- messages
t_messages() {
  local i="$1"; iter_env messages "$i"
  start_svc "$SVC" || { say FAIL "messages: first start" ""; return; }
  pair_phone "sms,contacts" || { say FAIL "messages: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
  local want; want="$(paired_in_file)"
  echo "messages 3000" >&7
  ms_sleep "$(rand_ms 0 400)"
  kill9
  sleep 0.2; end_phone
  verify "messages" "$want"
  ctl messages >/dev/null 2>&1; res "messages: the conversation list answers after the restart" $? ""
  stop_svc
}

# ---------------------------------------------------------------------------------------------- ring
card_pids() { # pids of geekbridge-calls started by THIS iteration's service (its environment names the state folder)
  local p
  for p in $(pgrep -f 'geekbridge-call[s] --overlay' 2>/dev/null); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qxF "GEEKBRIDGE_STATE_DIR=$ST" && echo "$p"
  done
}
t_ring() {
  local i="$1" j mode; iter_env ring "$i"
  mode=ring; ((i % 2 == 0)) && mode=talk
  export DISPLAY="$(cat "$W/xdisplay")"
  # no Bluetooth probe: the host's real adapter is never looked at; the call arrives over the phone link
  start_svc "$SVC" || { say FAIL "ring: first start" "$(tail -c 200 "$D/service.log")"; return; }
  pair_phone "telephony" || { say FAIL "ring: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
  local want; want="$(paired_in_file)"
  sleep 0.5
  echo "ring" >&7
  [[ $mode == talk ]] && { sleep 0.6; echo "talk" >&7; }
  local cards=""
  for j in $(seq 1 80); do cards="$(card_pids)"; [[ -n "$cards" ]] && break; sleep 0.1; done
  res "ring [$mode]: the card started" $([[ -n "$cards" ]]; echo $?) "$(ctl calls 2>&1 | head -c 120 | tr '\n' ' ')"
  if [[ -z "$cards" ]]; then ctl calls >"$D/calls-nocard.txt" 2>&1; cp "$ST/call-overlay-status" "$D/" 2>/dev/null; end_phone; stop_svc; return; fi
  ms_sleep "$(rand_ms 100 1500)"
  REUSE_PORT=$CUR_PORT
  kill9; end_phone
  # how long the orphaned card stays
  local gone_after="" lim=60
  [[ $mode == talk ]] && lim=20
  for j in $(seq 1 $lim); do [[ -z "$(card_pids)" ]] && { gone_after=$j; break; }; sleep 0.5; done
  if [[ -n "$gone_after" ]]; then say INFO "ring [$mode]: orphaned card closes by itself after" "$((gone_after / 2)) s"
  else say INFO "ring [$mode]: orphaned card STILL RUNNING $((lim / 2)) s after the service was killed" ""; fi
  if [[ $mode == ring ]]; then
    res "ring [$mode]: the orphaned card closes within 30 s without a service" $([[ -n "$gone_after" ]]; echo $?) ""
  fi
  # the restarted service: no stale card, no call, no history
  verify "ring [$mode]" "$want"
  local after=1
  for j in $(seq 1 30); do after="$(card_pids | wc -l)"; ((after == 0)) && break; sleep 0.5; done
  res "ring [$mode]: no card on screen within 15 s of the restart" $((after > 0)) "$after cards"
  ctl calls >"$D/calls.txt" 2>&1
  res "ring [$mode]: no call and no history after the restart" $(! grep -qiE 'ringing|active|incoming|dialing|held|missed|ended' "$D/calls.txt"; echo $?) "$(head -c 160 "$D/calls.txt" | tr '\n' ' ')"
  for j in $(card_pids); do kill "$j" 2>/dev/null; done
  stop_svc
}

# ---------------------------------------------------------------------------------------------- diskfull
t_diskfull() {
  local i="$1" op; iter_env diskfull "$i"
  [[ -n "$SHIM" ]] || { say INFO "diskfull: skipped (no gcc for the write-failure shim)" ""; return; }
  export GB_SHIM_ENOSPC_DIR="$D/st" GB_SHIM_ENOSPC_FLAG="$D/full"
  start_svc "$SVC" || { say FAIL "diskfull: first start" ""; return; }
  local want
  if [[ -n "$PHONE" ]]; then pair_phone "sms" || { say FAIL "diskfull: pairing" ""; return; }; end_phone; want="$(paired_in_file)"; else want=""; fi
  stop_svc
  local digest_before; digest_before="$(cd "$ST" && find . -type f ! -name '*.lock' ! -name 'call-trace.log' -print0 | sort -z | xargs -0 sha256sum)"
  # the service now runs with the shim; the disk is "full" from here
  LD_PRELOAD="$SHIM" start_svc "$SVC" || { say FAIL "diskfull: start under the shim" ""; return; }
  : >"$GB_SHIM_ENOSPC_FLAG"
  local ops=("consent ${PAIRED:-x} sms off" "privacy set message_text on" "share allow ${PAIRED:-x} on" "update disable" "rename FullDisk") n_err=0 msg=""
  for op in "${ops[@]}"; do
    out="$(ctl $op 2>&1)"; rc=$?
    if ((rc != 0)); then n_err=$((n_err + 1)); msg="$out"; fi
    [[ "$op" == "consent"* && $rc == 0 ]] && true
  done
  res "diskfull: the changes are refused with a message, not silently lost" $((n_err < 3)) "$n_err of ${#ops[@]} refused; e.g. $(echo "$msg" | head -c 160 | tr '\n' ' ')"
  res "diskfull: the service keeps running" $(kill -0 "$SVC_PID" 2>/dev/null; echo $?) ""
  local digest_full; digest_full="$(cd "$ST" && find . -type f ! -name '*.lock' ! -name 'call-trace.log' ! -name '.*.tmp.*' -print0 | sort -z | xargs -0 sha256sum)"
  res "diskfull: no existing state file truncated or changed" $([[ "$digest_before" == "$digest_full" ]]; echo $?) "$(diff <(echo "$digest_before") <(echo "$digest_full") | head -c 200 | tr '\n' ' ')"
  res "diskfull: no temp file left behind" $(($(temps_in "$ST") > 0)) ""
  LD_PRELOAD="$SHIM" ctl doctor >"$D/doctor-full.txt" 2>&1; res "diskfull: doctor says the state folder is full and what to do" $(grep -q "State folder" "$D/doctor-full.txt" && grep -q "is full" "$D/doctor-full.txt" && grep -q "free disk space" "$D/doctor-full.txt"; echo $?) "$(grep -m1 'State folder' "$D/doctor-full.txt" | cut -c1-120)"
  [[ -n "$PHONE" ]] && res "diskfull: pairings kept while full" $([[ "$(paired_in_status)" == "$want" ]]; echo $?) ""
  rm -f "$GB_SHIM_ENOSPC_FLAG"
  ctl privacy set message_text on >/dev/null 2>&1; res "diskfull: the change works once space is back" $? ""
  # killed while full, then restarted
  : >"$GB_SHIM_ENOSPC_FLAG"; ctl update enable >/dev/null 2>&1; ctl rename Again >/dev/null 2>&1
  kill9; rm -f "$GB_SHIM_ENOSPC_FLAG"
  verify "diskfull" "$want"
  stop_svc
}

# ---------------------------------------------------------------------------------------------- readonly
t_readonly() {
  local i="$1" variant; iter_env readonly "$i"
  variant=running; ((i % 2 == 0)) && variant=start
  start_svc "$SVC" || { say FAIL "readonly: first start" ""; return; }
  local want
  if [[ -n "$PHONE" ]]; then pair_phone "sms" || { say FAIL "readonly: pairing" ""; return; }; end_phone; want="$(paired_in_file)"; else want=""; fi
  if [[ $variant == running ]]; then
    chmod 500 "$ST"
    local out rc ok=0
    out="$(ctl privacy set message_text on 2>&1)"; rc=$?; ((rc != 0)) && ok=1
    res "readonly [running]: a change is refused with an error" $((1 - ok)) "$(echo "$out" | head -c 200 | tr '\n' ' ')"
    res "readonly [running]: the service keeps running" $(kill -0 "$SVC_PID" 2>/dev/null; echo $?) ""
    [[ -n "$PHONE" ]] && res "readonly [running]: pairings kept" $([[ "$(paired_in_status)" == "$want" ]]; echo $?) ""
    ctl doctor >"$D/doctor-ro.txt" 2>&1; res "readonly [running]: doctor says the state folder is not writable and what to do" $(grep -q "not writable" "$D/doctor-ro.txt" && grep -q "chmod u+w" "$D/doctor-ro.txt"; echo $?) "$(grep -m1 'State folder' "$D/doctor-ro.txt" | cut -c1-120)"
    chmod 700 "$ST"
    ctl privacy set message_text on >/dev/null 2>&1; res "readonly [running]: works again once writable" $? ""
    kill9
  else
    stop_svc; chmod 500 "$ST"
    local started=0
    start_svc "$SVC" && started=1
    res "readonly [start]: the service starts on a read-only state folder (or says why)" $([[ $started == 1 ]] || grep -qiE 'state|read|permission|writ' "$D/service.log"; echo $?) "started=$started $(head -c 220 "$D/service.log" | tr '\n' ' ')"
    if ((started)); then
      [[ -n "$PHONE" ]] && res "readonly [start]: pairings listed" $([[ "$(paired_in_status)" == "$want" ]]; echo $?) ""
      ctl doctor >"$D/doctor-ro.txt" 2>&1; res "readonly [start]: doctor says the state folder is not writable and what to do" $(grep -q "not writable" "$D/doctor-ro.txt" && grep -q "chmod u+w" "$D/doctor-ro.txt"; echo $?) "$(grep -m1 'State folder' "$D/doctor-ro.txt" | cut -c1-120)"
      kill9
    fi
    chmod 700 "$ST"
  fi
  verify "readonly" "$want"
  stop_svc
}

# ---------------------------------------------------------------------------------------------- clock
STEPS=(-315360000 -86400 -3600 -60 60 3600 86400 315360000 1892160000)
set_offset() { python3 -I -c 'import struct,sys; f=open(sys.argv[1],"r+b"); f.write(struct.pack("<q",int(sys.argv[2]))); f.close()' "$1" "$2"; }
t_clock() {
  local i="$1" s1 s2; iter_env clock "$i"
  [[ -n "$SHIM" ]] || { say INFO "clock: skipped (no gcc for the clock shim)" ""; return; }
  python3 -I -c 'import sys; open(sys.argv[1],"wb").write(b"\0"*8)' "$D/offset"
  export GB_SHIM_TIME_FILE="$D/offset"
  start_svc "$SVC"; local rc0=$?; stop_svc   # creates identity and state with the real clock
  LD_PRELOAD="$SHIM" start_svc "$SVC" || { say FAIL "clock: start under the shim" ""; return; }
  pair_phone "sms" || { say FAIL "clock: pairing" "$(tail -c 300 "$D/phone.out")"; return; }
  end_phone; local want; want="$(paired_in_file)"
  s1="${STEPS[$((RANDOM % ${#STEPS[@]}))]}"; s2="${STEPS[$((RANDOM % ${#STEPS[@]}))]}"
  set_offset "$D/offset" "$s1"
  ctl privacy set message_text on >/dev/null 2>&1; res "clock [$s1 s]: state writes work" $? ""
  ctl update status >"$D/upd.txt" 2>&1; res "clock [$s1 s]: update status answers" $([[ -s "$D/upd.txt" ]]; echo $?) "$(head -c 120 "$D/upd.txt" | tr '\n' ' ')"
  ctl pair-qr >"$D/qr.txt" 2>&1; res "clock [$s1 s]: a pairing link can be made" $([[ -s "$D/qr.txt" ]] && ! grep -qi error "$D/qr.txt"; echo $?) ""
  # a fresh phone pairs with the skewed service (TLS between a skewed and a normal clock)
  # The pairing protocol itself refuses a peer clock more than 30 minutes off (KDE Connect rule): that must be the
  # only reason it fails, and it must name the clocks.
  local skew=$((s1 < 0 ? -s1 : s1))
  if pair_phone "sms"; then
    res "clock [$s1 s]: a new phone pairs while the clocks differ by at most 30 min" $((skew > 1700)) "offset $s1 s"
  else
    res "clock [$s1 s]: a new phone is refused only for a clock more than 30 min off, with the reason" $([[ $skew -gt 1900 ]] && grep -q ClockSkew "$D/phone.out"; echo $?) "$(tail -c 160 "$D/phone.out" | tr '\n' ' ')"
  fi
  end_phone; want="$(paired_in_file)"
  set_offset "$D/offset" "$s2"
  ctl privacy set message_text off >/dev/null 2>&1; res "clock [$s1 -> $s2 s]: still working after a second step" $? ""
  res "clock [$s1 -> $s2 s]: service alive" $(kill -0 "$SVC_PID" 2>/dev/null; echo $?) ""
  kill9
  set_offset "$D/offset" 0
  verify "clock" "$want"
  stop_svc
}

# ---------------------------------------------------------------------------------------------- run
log "sandbox $W; service $SVC; rounds $ROUNDS; jobs $JOBS; phone ${PHONE:-none}; failpoint build ${FPBIN:-none}"
if [[ -z "$PHONE" ]]; then log "no --phone: scenarios that need a link (transfer, messages, ring) are skipped, others pair nothing"; fi
t0=$SECONDS
want trust && { par trust t_trust; report trust; }
if want failpoint; then
  if [[ -x "$FPBIN/geekbridge-service" && -n "$PHONE" ]]; then par failpoint t_failpoint; report failpoint; else echo "== failpoint: skipped (needs --failpoint-bin and --phone)"; fi
fi
if want transfer; then
  if [[ -n "$PHONE" ]]; then EXPECT_SHA="$(expected_sha "$FILE_MIB")"; export EXPECT_SHA; par transfer t_transfer; report transfer; else echo "== transfer: skipped (needs --phone)"; fi
fi
if want messages; then
  if [[ -n "$PHONE" ]]; then par messages t_messages; report messages; else echo "== messages: skipped (needs --phone)"; fi
fi
if want ring; then
  if [[ -n "$PHONE" && -x "$BIN/geekbridge-calls" ]] && command -v Xvfb >/dev/null; then
    XD=$((90 + RANDOM % 9))
    Xvfb ":$XD" -screen 0 1280x800x24 -nolisten tcp >"$W/xvfb.log" 2>&1 & XVFB_PID=$!
    echo ":$XD" >"$W/xdisplay"; sleep 1.5
    par ring t_ring; report ring
    kill "$XVFB_PID" 2>/dev/null; XVFB_PID=""
  else echo "== ring: skipped (needs --phone, geekbridge-calls next to the service, and Xvfb)"; fi
fi
want diskfull && { par diskfull t_diskfull; report diskfull; }
want readonly && { par readonly t_readonly; report readonly; }
want clock && { if [[ -n "$PHONE" ]]; then par clock t_clock; report clock; else echo "== clock: skipped (needs --phone)"; fi; }
echo
echo "chaos-crash: $PASSES checks passed, $FAILS failed, $((SECONDS - t0)) s"
((FAILS == 0))
