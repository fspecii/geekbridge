#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Golden-image (visual regression) test for the desktop apps in the Nothing x Continuity look.
#   scripts/golden-desktop.sh --check    render every screen and compare with tests/golden/desktop/ (default)
#   scripts/golden-desktop.sh --update   re-render and overwrite the goldens. ONLY after a visual review of the change
#                                        (see "Golden images" in CONTRIBUTING.md): look at the PNGs in git diff.
#   ONLY='^(home|settings)$' scripts/golden-desktop.sh --check    just those screens
# Same capture approach as scripts/screenshots-desktop-nothing.sh (private Xvfb, private service state, scripted phone
# from crates/bridge-phone-sim), made deterministic: frozen wall clock (scripts/golden/fakeclock.c, 2026-06-15 12:00
# UTC), TZ=UTC, 24 h clock, en_US, embedded fonts only (GEEKBRIDGE_BUNDLED_FONTS_ONLY: Noto Sans + Hack from
# third_party/fonts), the call timer pinned to 01:23 (GEEKBRIDGE_FIXED_CALL_SECONDS), the update check only against a loopback mock release server, animations off in the theme.
# Each screen is shot in light and dark at the standard window size ("std") and resized to the app's minimum ("min").
# Goldens: 50 % scale, 256-colour PNG. Tolerance and diff images: scripts/golden/golden_img.py (failures land in
# dist/golden-diff/desktop-NAME.png as expected | actual | highlighted).
# Robustness on a busy machine: --check re-captures a screen that differs (up to 3 tries; a startup race passes, a regression never does), every port is picked free at run time, every worker has its own Xvfb (-displayfd, no
# display numbers), a dead helper fails the run at once with its log tail, the whole run is bounded by GOLDEN_TIMEOUT
# seconds (default 240, build excluded) and every child process dies with the script (process group + EXIT trap).
# Env: GOLDEN_JOBS (parallel workers, default min(8, cores)), GOLDEN_TIMEOUT, GOLDEN_NO_BUILD=1, RUN (work dir), GOLDEN_DIR,
#      GOLDEN_PIXEL_THRESHOLD, GOLDEN_MAX_RATIO, GOLDEN_KEEP=1 (keep the work dir's logs).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
MODE=check
case "${1:-}" in --update) MODE=update ;; --check|"") ;; *) echo "usage: $0 [--update|--check]"; exit 2 ;; esac
source scripts/android-env.sh >/dev/null 2>&1
unset CARGO_TARGET_DIR   # android-env points it at target/android; the build below must land in target/debug, which the run uses
python3 -I -c 'import PIL' 2>/dev/null || { echo "golden-desktop: needs python3 with Pillow (pacman -S python-pillow)"; exit 2; }
for tool in Xvfb xdotool import cc; do command -v $tool >/dev/null || { echo "golden-desktop: needs $tool"; exit 2; }; done

# Build outside the time limit, then re-run ourselves in our own process group under `timeout`: on expiry (or Ctrl-C)
# the whole group, helpers included, is killed.
if [ -z "${GOLDEN_INNER:-}" ]; then
  if [ -z "${GOLDEN_NO_BUILD:-}" ]; then
    timeout 1800 cargo build -q -p bridge-service -p bridge-apps --bins && timeout 1800 cargo build -q -p bridge-phone-sim --example data_sim || exit 1
  fi
  GOLDEN_INNER=1 exec timeout -k 5 "${GOLDEN_TIMEOUT:-240}" "$0" "$@"
fi

GOLDEN_DIR="${GOLDEN_DIR:-$ROOT/tests/golden/desktop}"
RUN="${RUN:-${GOLDEN_SCRATCH:-$HOME/.cache/gb-golden}/desktop}"
DIFF="$ROOT/dist/golden-diff"
JOBS=${GOLDEN_JOBS:-$(n=$(nproc 2>/dev/null || echo 4); [ "$n" -gt 8 ] && n=8; echo "$n")}   # workers; one more X server is for the Messages group
IMG="python3 -I $ROOT/scripts/golden/golden_img.py"
rm -rf "$RUN"; mkdir -p "$RUN"/{state,nostate,raw,xdg,xdg-data,fail}; chmod 700 "$RUN/nostate"; mkdir -p /tmp/gb-golden-home "$GOLDEN_DIR" "$DIFF"; chmod 700 "$RUN/state"   # HOME is a fixed path: the share window prints its save folder
[ "$MODE" = check ] && rm -f "$DIFF"/desktop-*.png
cc -shared -fPIC -O2 -o "$RUN/fakeclock.so" scripts/golden/fakeclock.c -ldl || { echo "golden-desktop: C compiler failed"; exit 2; }
T=target/debug
START=$SECONDS

die() { echo "golden-desktop: $*"; exit 1; }
tail_log() { [ -f "$1" ] && { echo "--- tail of $1"; tail -n 15 "$1"; }; }
cleanup() { trap - EXIT INT TERM; kill $(jobs -p) 2>/dev/null; pkill -P $$ 2>/dev/null; [ -z "${GOLDEN_KEEP:-}" ] && rm -rf "$RUN/state"; }
trap cleanup EXIT; trap 'exit 130' INT; trap 'exit 143' TERM

free_tcp() { python3 -I -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
free_udp() { python3 -I -c 'import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
AP=$(free_tcp); CP=$(free_tcp); DP=$(free_udp)
while [ "$CP" = "$AP" ]; do CP=$(free_tcp); done
FAP=$(free_tcp); FCP=$(free_tcp); FDP=$(free_udp)   # the "fresh" service (no phone ever paired): the first-run guide
while [ "$FAP" = "$AP" ] || [ "$FAP" = "$CP" ]; do FAP=$(free_tcp); done
while [ "$FCP" = "$AP" ] || [ "$FCP" = "$CP" ] || [ "$FCP" = "$FAP" ]; do FCP=$(free_tcp); done

# Mock GitHub release (newer than this build) for the update banner: binds port 0 and reports it. Loopback only.
cat >"$RUN/mock.py" <<'PY'
import http.server, json, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"tag_name": "v9.9.9", "html_url": "https://example.invalid/release", "body": "Fixture release notes.", "assets": []}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
srv = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(srv.server_address[1]))
srv.serve_forever()
PY
python3 -I "$RUN/mock.py" "$RUN/mock.port" >"$RUN/mock.log" 2>&1 & MOCK=$!
for _ in $(seq 50); do [ -s "$RUN/mock.port" ] && break; kill -0 $MOCK 2>/dev/null || { tail_log "$RUN/mock.log"; die "mock release server died"; }; sleep 0.1; done
[ -s "$RUN/mock.port" ] || die "mock release server did not report its port"
MOCKP=$(cat "$RUN/mock.port")

export GOLDEN_NOW=1781524800   # 2026-06-15 12:00:00 UTC, a Monday
DET=(TZ=UTC LANG=en_US.UTF-8 LC_ALL= LANGUAGE= GEEKBRIDGE_CLOCK=24 GEEKBRIDGE_BUNDLED_FONTS_ONLY=1 GEEKBRIDGE_NO_TRAY=1 GEEKBRIDGE_FIXED_CALL_SECONDS=83 GEEKBRIDGE_CALLAUDIO_FIXTURE=ready
     GEEKBRIDGE_FIREWALL_FIXTURE=none GEEKBRIDGE_AUTOSTART_FIXTURE=on   # the first-run guide reads this computer's firewall and autostart: pinned
     GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_UPDATE_BASE_URL=http://127.0.0.1:$MOCKP GOLDEN_NOW=$GOLDEN_NOW LD_PRELOAD="$RUN/fakeclock.so"
     XDG_CONFIG_HOME="$RUN/xdg" XDG_DATA_HOME="$RUN/xdg-data" HOME=/tmp/gb-golden-home)

# One Xvfb per worker; the server picks its own display number (-displayfd).
declare -a DISPS=() XPIDS=()
for w in $(seq 0 $JOBS); do
  Xvfb -displayfd 3 -nolisten tcp -screen 0 1280x860x24 3>"$RUN/disp.$w" >"$RUN/xvfb.$w.log" 2>&1 & XPIDS[$w]=$!
done
for w in $(seq 0 $JOBS); do
  for _ in $(seq 100); do [ -s "$RUN/disp.$w" ] && break; kill -0 "${XPIDS[$w]}" 2>/dev/null || { tail_log "$RUN/xvfb.$w.log"; die "Xvfb $w died"; }; sleep 0.1; done
  [ -s "$RUN/disp.$w" ] || die "Xvfb $w did not report a display"
  DISPS[$w]=":$(tr -d '\n' <"$RUN/disp.$w")"
done

env -u WAYLAND_DISPLAY "${DET[@]}" DISPLAY="${DISPS[0]}" $T/geekbridge-service --state-dir "$RUN/state" \
  --api-port $AP --control-port $CP --discovery-port $DP --no-listen-inbound --no-announce --dev-loopback --no-calling-probe >"$RUN/service.log" 2>&1 &
SVC=$!
up=0; for _ in $(seq 150); do
  kill -0 $SVC 2>/dev/null || { tail_log "$RUN/service.log"; die "service died during start"; }
  timeout 5 $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP status >/dev/null 2>&1 && { up=1; break; }; sleep 0.2
done
[ $up = 1 ] || { tail_log "$RUN/service.log"; die "service did not come up"; }
# A second, never-paired service for the first-run screens (Home with no phone). Its own state, ports and token.
mkdir -p "$RUN/fresh"; chmod 700 "$RUN/fresh"
env -u WAYLAND_DISPLAY "${DET[@]}" DISPLAY="${DISPS[0]}" $T/geekbridge-service --state-dir "$RUN/fresh" \
  --api-port $FAP --control-port $FCP --discovery-port $FDP --no-listen-inbound --no-announce --dev-loopback --no-calling-probe >"$RUN/fresh.log" 2>&1 &
FRESH=$!
up=0; for _ in $(seq 150); do
  kill -0 $FRESH 2>/dev/null || { tail_log "$RUN/fresh.log"; die "fresh service died during start"; }
  timeout 5 $T/geekbridge-ctl --state-dir "$RUN/fresh" --control-port $FCP status >/dev/null 2>&1 && { up=1; break; }; sleep 0.2
done
[ $up = 1 ] || { tail_log "$RUN/fresh.log"; die "fresh service did not come up"; }
helpers_alive() { for p in $SVC $FRESH $SIM $MOCK; do kill -0 $p 2>/dev/null || { echo "golden-desktop: a helper process died"; tail_log "$RUN/service.log"; tail_log "$RUN/sim.log"; return 1; }; done; }
env "${DET[@]}" $T/examples/data_sim "$RUN/state" $CP >"$RUN/sim.log" 2>&1 &
SIM=$!
for _ in $(seq 150); do
  grep -q "paired as" "$RUN/sim.log" && break
  kill -0 $SIM 2>/dev/null || { tail_log "$RUN/sim.log"; die "phone simulator died"; }; sleep 0.2
done
grep -q "paired as" "$RUN/sim.log" || { tail_log "$RUN/sim.log"; die "phone simulator did not pair"; }
# Wait until the service really serves the scripted phone's calendar and conversations (they sync over several seconds),
# then a short settle so the apps' first poll sees them.
ready=0; for _ in $(seq 100); do
  helpers_alive || exit 1
  if timeout 5 $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP agenda "" 2026-06-15 2 2>/dev/null | grep -q "Team offsite" \
     && timeout 5 $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP messages 2>/dev/null | grep -qE "Ada|0001"; then ready=1; break; fi
  sleep 0.5
done
[ $ready = 1 ] || { tail_log "$RUN/service.log"; tail_log "$RUN/sim.log"; die "the scripted phone's calendar and messages never became available"; }
sleep 3

# Minimum window size per binary (crates/bridge-apps/src/identity.rs, home_app.rs, share_app.rs).
declare -A MINSZ=([geekbridge]="560 520" [geekbridge-share]="380 460" [geekbridge-calls]="320 560" [geekbridge-messages]="640 460"
                  [geekbridge-agenda]="760 520" [geekbridge-contacts]="560 460" [geekbridge-clipboard]="360 440")

# Capture until N (default 3; 8 for the first shot) consecutive frames, 0.5 s apart, are byte-identical, for at most ~20 s: the
# phone feeds poll every 2 s (5 s when down), so a stable frame must outlast a poll.
grab() { # display wid out [stable frames, default 3]
  local WIDTH HEIGHT X Y prev="$3.prev" i same=0 need=${4:-3}; : >"$prev"
  eval "$(DISPLAY=$1 timeout 10 xdotool getwindowgeometry --shell "$2")"
  for i in $(seq 40); do
    DISPLAY=$1 timeout 10 import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$3" || return 1
    if cmp -s "$3" "$prev"; then same=$((same+1)); [ $same -ge $need ] && break; else same=0; cp "$3" "$prev"; fi
    sleep 0.5
  done
  rm -f "$prev"
}
shot_once() { # display app theme name keys [ENV=VAL...]   keys is an xdotool key sequence or "-"; returns 1 if no window appeared
  local disp="$1" app="$2" theme="$3" name="$4" keys="$5"; shift 5
  local pid wid="" i floor=3 apport=$AP e
  for e in "$@"; do case $e in GOLDEN_API_PORT=*) apport=${e#*=} ;; esac; done   # a screen backed by the fresh service
  env -u WAYLAND_DISPLAY "${DET[@]}" DISPLAY=$disp GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CP "$@" \
    timeout 90 $T/${app%%+*} $([ "$app" = geekbridge-share ] || echo "--api-port $apport") $([ "$app" = geekbridge-calls+overlay ] && echo --overlay) --theme "$theme" >"$RUN/app.$name-$theme.log" 2>&1 &
  pid=$!
  for i in $(seq 100); do   # up to 30 s for the window
    wid=$(DISPLAY=$disp xdotool search --onlyvisible --pid $pid 2>/dev/null | head -1)
    [ -z "$wid" ] && wid=$(DISPLAY=$disp xdotool search --onlyvisible --class dev.geekbridge 2>/dev/null | head -1)
    [ -n "$wid" ] && break; kill -0 $pid 2>/dev/null || break; sleep 0.3
  done
  if [ -z "$wid" ]; then echo "golden-desktop: no window for $name-$theme"; tail_log "$RUN/app.$name-$theme.log"; kill $pid 2>/dev/null; wait $pid 2>/dev/null; return 1; fi
  # The share window and the home tiles (calendar, messages) fill in several seconds after launch.
  case "$app" in geekbridge-share) floor=8 ;; geekbridge) floor=10 ;; esac
  sleep $floor
  if [ "$keys" != "-" ]; then DISPLAY=$disp timeout 10 xdotool windowfocus --sync "$wid" key --delay 150 $keys; sleep 1.5; fi
  grab "$disp" "$wid" "$RUN/raw/$name-$theme-std.png" 8 || : >"$RUN/fail/$name-$theme"
  read -r mw mh <<<"${MINSZ[$app]:-360 96}"   # the call card (geekbridge-calls+overlay) is fixed-size: min = std
  DISPLAY=$disp timeout 10 xdotool windowsize --sync "$wid" "$mw" "$mh"; sleep 1.5
  grab "$disp" "$wid" "$RUN/raw/$name-$theme-min.png" || : >"$RUN/fail/$name-$theme"
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
  return 0
}
# Photo previews are gradients-and-edges content that the 256-colour reducer and the comparer's palette mapping round
# differently, so the photo screens get a wider (still far below any layout change) allowance of differing pixels.
max_ratio() { case $1 in share-photos*) echo 0.002 ;; *) echo "${GOLDEN_MAX_RATIO:-0.0005}" ;; esac; }
shot_matches() { # NAME-THEME: both sizes within tolerance of their goldens (a missing golden counts as matching; the report says NEW)
  local base=$1 sz
  for sz in std min; do
    [ -f "$GOLDEN_DIR/$base-$sz.png" ] || continue
    GOLDEN_MAX_RATIO=$(max_ratio "$base") $IMG compare "$RUN/raw/$base-$sz.png" "$GOLDEN_DIR/$base-$sz.png" "$RUN/retry.$base-$sz.png" >/dev/null 2>&1 || return 1
  done
}
# A shot is retried (3 tries); if the worker's X server has gone away it is replaced first (SHOT_DISP is the new display).
shot() { # worker display app theme name keys [ENV=VAL...]
  local w="$1" disp="$2" attempt; shift 2
  for attempt in 1 2 3; do
    if ! DISPLAY=$disp timeout 5 xdotool getdisplaygeometry >/dev/null 2>&1; then
      echo "golden-desktop: X server $disp of worker $w is gone, starting a new one"
      Xvfb -displayfd 3 -nolisten tcp -screen 0 1280x860x24 3>"$RUN/disp.$w.$attempt" >"$RUN/xvfb.$w.$attempt.log" 2>&1 &
      for _ in $(seq 100); do [ -s "$RUN/disp.$w.$attempt" ] && break; sleep 0.1; done
      disp=":$(tr -d '\n' <"$RUN/disp.$w.$attempt")"; SHOT_DISP=$disp
    fi
    if shot_once "$disp" "$@"; then
      # --check: a capture that differs from its golden is taken again (up to 3 times). A real regression differs every
      # time; a startup race (a feed tile that was still empty on a loaded machine) does not. --update never retries.
      [ "$MODE" = check ] && ! shot_matches "$3-$2" && [ $attempt -lt 3 ] && { echo "golden-desktop: $3-$2 differs, capturing again ($attempt)"; sleep 1; continue; }
      return 0
    fi
    sleep 1
  done
  : >"$RUN/fail/$3-$2"; return 1
}
# task = "app|theme|name|keys|ENV=VAL ..."
tasks_home() { # theme: screens of the home window; they show the unread count, so they run before Messages is opened
  local t=$1
  echo "geekbridge|$t|home|-|"
  echo "geekbridge|$t|home-no-bluetooth|-|GEEKBRIDGE_CALLAUDIO_FIXTURE=no-bluetooth"
  echo "geekbridge|$t|pair|ctrl+2|"
  echo "geekbridge|$t|settings|ctrl+3|GEEKBRIDGE_CALLAUDIO_FIXTURE=no-bluetooth"   # the fixture keeps the machine's real Bluetooth out of the picture
  echo "geekbridge|$t|settings-call-audio|ctrl+3|GEEKBRIDGE_CALLAUDIO_FIXTURE=connect GEEKBRIDGE_OPEN_CALLAUDIO=1"
  echo "geekbridge|$t|settings-call-audio-test|ctrl+3|GEEKBRIDGE_CALLAUDIO_FIXTURE=testing GEEKBRIDGE_OPEN_CALLAUDIO=1"
  echo "geekbridge|$t|settings-call-audio-nobt|ctrl+3|GEEKBRIDGE_CALLAUDIO_FIXTURE=no-bluetooth GEEKBRIDGE_OPEN_CALLAUDIO=1"
  echo "geekbridge|$t|settings-call-audio-mac|ctrl+3|GEEKBRIDGE_CALLAUDIO_FIXTURE=mac GEEKBRIDGE_OPEN_CALLAUDIO=1"
  echo "geekbridge|$t|diagnostics|-|GEEKBRIDGE_OPEN_REPORT=1"
  echo "geekbridge|$t|home-service-off|-|GEEKBRIDGE_STATE_DIR=$RUN/nostate GEEKBRIDGE_AUTOSTART_FIXTURE=off"   # first-run empty state: no service token to read; the guide leads with the start command
  # First run with the service up and no phone paired: the three-step guide; then with a firewall that blocks phones.
  echo "geekbridge|$t|home-first-run|-|GEEKBRIDGE_STATE_DIR=$RUN/fresh GEEKBRIDGE_CONTROL_PORT=$FCP GOLDEN_API_PORT=$FAP"
  echo "geekbridge|$t|home-first-run-firewall|-|GEEKBRIDGE_STATE_DIR=$RUN/fresh GEEKBRIDGE_CONTROL_PORT=$FCP GOLDEN_API_PORT=$FAP GEEKBRIDGE_FIREWALL_FIXTURE=ufw-blocking GEEKBRIDGE_AUTOSTART_FIXTURE=off"
}
tasks_rest() { # theme
  local t=$1
  echo "geekbridge-clipboard|$t|clipboard|-|"
  echo "geekbridge-share|$t|share|-|"
  # Recent photos tab: the scripted phone declares the photos packets and the service allowed them for it, so the grid is
  # the real thing (list and thumbnails over the link). The "off" screen is the state before the user allowed photos.
  echo "geekbridge-share|$t|share-photos|-|GEEKBRIDGE_SHARE_TAB=photos"
  echo "geekbridge-share|$t|share-photos-detail|-|GEEKBRIDGE_SHARE_TAB=photos GEEKBRIDGE_PHOTOS_SELECT_FIRST=1"
  echo "geekbridge-share|$t|share-photos-off|-|GEEKBRIDGE_SHARE_TAB=photos GEEKBRIDGE_PHOTOS_FIXTURE=offered"
  echo "geekbridge-calls|$t|calls-keypad|-|"
  echo "geekbridge-calls|$t|calls-keypad-no-bluetooth|-|GEEKBRIDGE_CALLAUDIO_FIXTURE=no-bluetooth"
  echo "geekbridge-calls|$t|calls-audio-setup|-|GEEKBRIDGE_CALLS_TAB=recents GEEKBRIDGE_CALLAUDIO_FIXTURE=connect"
  echo "geekbridge-calls|$t|call-incoming|-|GEEKBRIDGE_CALLS_FIXTURE=ringing"
  echo "geekbridge-calls|$t|call-active|-|GEEKBRIDGE_CALLS_FIXTURE=active"
  echo "geekbridge-calls+overlay|$t|call-overlay|-|GEEKBRIDGE_CALLS_FIXTURE=ringing"
  echo "geekbridge-contacts|$t|contacts|-|"
  echo "geekbridge-contacts|$t|contacts-service-off|-|GEEKBRIDGE_STATE_DIR=$RUN/nostate"
  echo "geekbridge-contacts|$t|contacts-details|-|GEEKBRIDGE_CONTACTS_SELECT_FIRST=1"
  echo "geekbridge-agenda|$t|calendar-month|-|"
  echo "geekbridge-agenda|$t|calendar-week|-|GEEKBRIDGE_CALENDAR_VIEW=week"
  echo "geekbridge-agenda|$t|calendar-day|-|GEEKBRIDGE_CALENDAR_VIEW=day"
  echo "geekbridge-agenda|$t|calendar-agenda|-|GEEKBRIDGE_CALENDAR_VIEW=agenda"
}
tasks_messages() { echo "geekbridge-messages|$1|messages|-|"; }   # opening Messages marks conversations read, so light runs before dark
run_tasks() { # reads tasks on stdin, spreads them over the workers, waits
  local -a list=(); local line i=0 w
  while IFS= read -r line; do
    local nm; nm=$(cut -d'|' -f3 <<<"$line")
    # With ONLY set, the Messages group still runs when the banner or Messages is wanted, so the unread state matches a full run.
    [ -n "${ONLY:-}" ] && ! [[ "$nm" =~ $ONLY ]] && ! { [ "$nm" = messages ] && [[ "update-banner" =~ $ONLY ]]; } && continue
    list+=("$line")
  done
  local -a wpids=()
  local nw=${NW:-$JOBS}
  for w in $(seq 0 $((nw-1))); do
    ( wi=$((w+${WBASE:-0}))
      for ((i=w; i<${#list[@]}; i+=nw)); do
        IFS='|' read -r app theme name keys envs <<<"${list[$i]}"
        helpers_alive || exit 1
        # shellcheck disable=SC2086
        shot "$wi" "${SHOT_DISP:-${DISPS[$wi]}}" "$app" "$theme" "$name" "$keys" $envs
      done; exit 0 ) & wpids+=($!)
  done
  local rc=0; for p in "${wpids[@]}"; do wait "$p" || rc=1; done
  return $rc
}
{ tasks_home light; tasks_home dark; } | run_tasks || die "a capture worker failed"
# Messages (light, then dark, on its own X server) overlaps with everything else: nothing else reads the unread state.
{ tasks_messages light; tasks_messages dark; } | NW=1 WBASE=$JOBS run_tasks & MSGP=$!
{ tasks_rest light; tasks_rest dark; } | run_tasks || die "a capture worker failed"
wait $MSGP || die "the Messages capture failed"
# The banner appears once the service has seen a newer release; every shot after this carries it.
timeout 30 $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP update check >"$RUN/update.log" 2>&1; sleep 2
{ echo "geekbridge|light|update-banner|-|"; echo "geekbridge|dark|update-banner|-|"; } | run_tasks || die "a capture worker failed"
ls "$RUN/fail" | grep -q . && die "capture failed for: $(ls "$RUN/fail" | tr '\n' ' ')"
echo "captured $(ls "$RUN/raw" | wc -l) images in $((SECONDS-START)) s"

# Reduce / compare.
n=0; bad=0
for raw in "$RUN"/raw/*.png; do
  [ -e "$raw" ] || continue
  name=$(basename "$raw")
  [ -n "${ONLY:-}" ] && ! [[ "$(sed -E 's/-(light|dark)-(std|min)\.png$//' <<<"$name")" =~ $ONLY ]] && continue
  n=$((n+1))
  if [ "$MODE" = update ]; then $IMG reduce "$raw" "$GOLDEN_DIR/$name" || die "reduce failed for $name"; continue; fi
  if [ ! -f "$GOLDEN_DIR/$name" ]; then echo "NEW   $name (no golden; run --update after reviewing)"; $IMG reduce "$raw" "$DIFF/desktop-NEW-$name"; bad=$((bad+1)); continue; fi
  if out=$(GOLDEN_MAX_RATIO=$(max_ratio "$name") $IMG compare "$raw" "$GOLDEN_DIR/$name" "$DIFF/desktop-$name"); then echo "ok    $name"; else echo "FAIL  $name: $out"; bad=$((bad+1)); fi
done
if [ "$MODE" = check ] && [ -z "${ONLY:-}" ]; then
  for g in "$GOLDEN_DIR"/*.png; do [ -e "$RUN/raw/$(basename "$g")" ] || { echo "STALE $(basename "$g") (golden without a screen)"; bad=$((bad+1)); }; done
fi
if [ "$MODE" = update ]; then echo "updated $n goldens in $GOLDEN_DIR ($(du -sk "$GOLDEN_DIR" | cut -f1) KiB, $((SECONDS-START)) s): review with git diff before committing"; exit 0; fi
echo "golden-desktop: $n screens, $bad failing, $((SECONDS-START)) s; diffs in dist/golden-diff/"
[ "$bad" -eq 0 ]
