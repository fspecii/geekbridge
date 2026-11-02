#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Golden-image (visual regression) test for the desktop apps in the Nothing x Continuity look.
#   scripts/golden-desktop.sh --check    render every screen and compare with tests/golden/desktop/ (default)
#   scripts/golden-desktop.sh --update   re-render and overwrite the goldens. ONLY after a visual review of the
#                                        change (see "Golden images" in CONTRIBUTING.md): look at the PNGs in git diff.
#   ONLY='^(home|settings)$' scripts/golden-desktop.sh --check    just those screens
# Same capture approach as scripts/screenshots-desktop-nothing.sh (private Xvfb, private service state, scripted phone
# from crates/bridge-phone-sim), made deterministic: frozen wall clock (scripts/golden/fakeclock.c, 2026-06-15 12:00
# UTC), TZ=UTC, 24 h clock, en_US, embedded fonts only (GEEKBRIDGE_BUNDLED_FONTS_ONLY: Noto Sans + Hack from
# third_party/fonts), no update check except the mock release server, no network, animations are off in the theme.
# Each screen is shot in light and dark at the standard window size ("std") and resized to the app's minimum ("min").
# Goldens: 50 % scale, 256-colour PNG. Tolerance and diff images: scripts/golden/golden_img.py (failures land in
# dist/golden-diff/NAME.png as expected | actual | highlighted).
# Env overrides: DISP, CP/AP/DP (ports), RUN (work dir), GOLDEN_DIR, GOLDEN_PIXEL_THRESHOLD, GOLDEN_MAX_RATIO.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"; cd "$ROOT"
MODE=check
case "${1:-}" in --update) MODE=update ;; --check|"") ;; *) echo "usage: $0 [--update|--check]"; exit 2 ;; esac
source scripts/android-env.sh >/dev/null 2>&1
GOLDEN_DIR="${GOLDEN_DIR:-$ROOT/tests/golden/desktop}"
RUN="${RUN:-$ROOT/target/golden-desktop}"
DIFF="$ROOT/dist/golden-diff"
DISP=${DISP:-:96}; CP=${CP:-17811}; AP=${AP:-17810}; DP=${DP:-17812}; MOCKP=${MOCKP:-17813}
IMG="python3 -I $ROOT/scripts/golden/golden_img.py"
python3 -I -c 'import PIL' 2>/dev/null || { echo "golden-desktop: needs python3 with Pillow (pacman -S python-pillow)"; exit 2; }
rm -rf "$RUN/state" "$RUN/raw" "$RUN/actual"; mkdir -p "$RUN/state" "$RUN/raw" "$RUN/actual" "$GOLDEN_DIR" "$DIFF"; chmod 700 "$RUN/state"
[ "$MODE" = check ] && rm -f "$DIFF"/desktop-*.png
cc -shared -fPIC -O2 -o "$RUN/fakeclock.so" scripts/golden/fakeclock.c -ldl || { echo "golden-desktop: need a C compiler"; exit 2; }
if [ -z "${GOLDEN_NO_BUILD:-}" ]; then
  timeout 1800 cargo build -q -p bridge-service -p bridge-apps --bins && timeout 1800 cargo build -q -p bridge-phone-sim --example data_sim || exit 1
fi
T=target/debug
export GOLDEN_NOW=1781524800   # 2026-06-15 12:00:00 UTC, a Monday
DET=(TZ=UTC LANG=en_US.UTF-8 LC_ALL= LANGUAGE= GEEKBRIDGE_CLOCK=24 GEEKBRIDGE_BUNDLED_FONTS_ONLY=1 GEEKBRIDGE_NO_TRAY=1
     GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_UPDATE_BASE_URL=http://127.0.0.1:$MOCKP GOLDEN_NOW=$GOLDEN_NOW LD_PRELOAD="$RUN/fakeclock.so"
     XDG_CONFIG_HOME="$RUN/xdg" XDG_DATA_HOME="$RUN/xdg-data" HOME="$RUN/home")
mkdir -p "$RUN/xdg" "$RUN/xdg-data" "$RUN/home"
cleanup() { kill ${SIM:-} ${SVC:-} ${MOCK:-} 2>/dev/null; pkill -f "Xvfb $DISP" 2>/dev/null; }
trap cleanup EXIT
pkill -f "Xvfb $DISP" 2>/dev/null; (setsid nohup Xvfb $DISP -screen 0 1280x860x24 >"$RUN/xvfb.log" 2>&1 &); sleep 1

# Mock GitHub release (newer than this build) for the update banner. Loopback only.
cat >"$RUN/mock.py" <<PY
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"tag_name": "v9.9.9", "html_url": "https://example.invalid/release", "body": "Fixture release notes.", "assets": []}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", $MOCKP), H).serve_forever()
PY
python3 -I "$RUN/mock.py" & MOCK=$!

env -u WAYLAND_DISPLAY "${DET[@]}" DISPLAY=$DISP setsid nohup $T/geekbridge-service --state-dir "$RUN/state" \
  --api-port $AP --control-port $CP --discovery-port $DP --no-listen-inbound --no-announce --dev-loopback --no-calling-probe >"$RUN/service.log" 2>&1 &
SVC=$!
for _ in $(seq 200); do $T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP status >/dev/null 2>&1 && break; sleep 0.2; done
env "${DET[@]}" $T/examples/data_sim "$RUN/state" $CP >"$RUN/sim.log" 2>&1 &
SIM=$!
for _ in $(seq 100); do grep -q "paired as" "$RUN/sim.log" && break; sleep 0.2; done
sleep 12   # let the service finish syncing the scripted phone (unread counts and the like fill in over several seconds)

# Minimum window size per binary (crates/bridge-apps/src/identity.rs, home_app.rs, share_app.rs).
declare -A MINSZ=([geekbridge]="560 520" [geekbridge-share]="380 460" [geekbridge-calls]="320 560" [geekbridge-messages]="640 460"
                  [geekbridge-agenda]="760 520" [geekbridge-contacts]="560 460" [geekbridge-clipboard]="360 440")
# Capture the window until three consecutive frames (1 s apart) are byte-identical (blinking carets, late repaints).
grab() { # wid out
  local WIDTH HEIGHT X Y; eval "$(DISPLAY=$DISP xdotool getwindowgeometry --shell "$1")"
  local prev="$RUN/prev.png" i same=0; : >"$prev"
  for i in $(seq 12); do
    DISPLAY=$DISP import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$2"
    if cmp -s "$2" "$prev"; then same=$((same+1)); [ $same -ge 3 ] && break; else same=0; cp "$2" "$prev"; fi
    sleep 1
  done
}
shot() { # app theme name keys [ENV=VAL...]   keys is an xdotool key sequence or "-"
  local app="$1" theme="$2" name="$3" keys="$4"; shift 4
  [ -n "${ONLY:-}" ] && ! [[ "$name" =~ $ONLY ]] && return 0
  env -u WAYLAND_DISPLAY "${DET[@]}" DISPLAY=$DISP GEEKBRIDGE_STATE_DIR="$RUN/state" GEEKBRIDGE_CONTROL_PORT=$CP "$@" \
    $T/$app $([ "$app" = geekbridge-share ] || echo "--api-port $AP") --theme "$theme" >/dev/null 2>&1 &
  local pid=$! wid=""; sleep 5; [ "$app" = geekbridge-share ] && sleep 8
  wid=$(DISPLAY=$DISP xdotool search --onlyvisible --pid $pid 2>/dev/null | head -1)
  [ -z "$wid" ] && wid=$(DISPLAY=$DISP xdotool search --onlyvisible --class dev.geekbridge 2>/dev/null | head -1)
  if [ -z "$wid" ]; then echo "golden-desktop: no window for $name-$theme"; FAILED=1; kill $pid 2>/dev/null; wait $pid 2>/dev/null; return; fi
  if [ "$keys" != "-" ]; then DISPLAY=$DISP xdotool windowfocus --sync "$wid" key --delay 150 $keys; sleep 1.5; fi
  grab "$wid" "$RUN/raw/$name-$theme-std.png"
  read -r mw mh <<<"${MINSZ[$app]}"
  DISPLAY=$DISP xdotool windowsize --sync "$wid" "$mw" "$mh"; sleep 2
  grab "$wid" "$RUN/raw/$name-$theme-min.png"
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
FAILED=0
screens() { # theme
  local t=$1
  shot geekbridge $t home -
  shot geekbridge $t pair ctrl+2
  shot geekbridge $t settings ctrl+3
  shot geekbridge-clipboard $t clipboard -
  shot geekbridge-share $t share -
  shot geekbridge-calls $t calls-keypad -
  shot geekbridge-calls $t call-incoming - GEEKBRIDGE_CALLS_FIXTURE=ringing
  shot geekbridge-calls $t call-active - GEEKBRIDGE_CALLS_FIXTURE=active
  shot geekbridge-messages $t messages -
  shot geekbridge-contacts $t contacts -
  shot geekbridge-contacts $t contacts-details - GEEKBRIDGE_CONTACTS_SELECT_FIRST=1
  shot geekbridge-agenda $t calendar-month -
  shot geekbridge-agenda $t calendar-week - GEEKBRIDGE_CALENDAR_VIEW=week
  shot geekbridge-agenda $t calendar-day - GEEKBRIDGE_CALENDAR_VIEW=day
  shot geekbridge-agenda $t calendar-agenda - GEEKBRIDGE_CALENDAR_VIEW=agenda
  shot geekbridge $t diagnostics - GEEKBRIDGE_OPEN_REPORT=1
}
for t in light dark; do screens $t; done
# The banner appears once the service has seen a newer release; every shot after this carries it.
$T/geekbridge-ctl --state-dir "$RUN/state" --control-port $CP update check >"$RUN/update.log" 2>&1; sleep 2
for t in light dark; do shot geekbridge $t update-banner -; done

# Reduce / compare.
n=0; bad=0
for raw in "$RUN"/raw/*.png; do
  [ -e "$raw" ] || continue
  name=$(basename "$raw"); n=$((n+1))
  if [ "$MODE" = update ]; then $IMG reduce "$raw" "$GOLDEN_DIR/$name" || FAILED=1; continue; fi
  if [ ! -f "$GOLDEN_DIR/$name" ]; then echo "NEW   $name (no golden; run --update after reviewing)"; $IMG reduce "$raw" "$DIFF/desktop-NEW-$name"; bad=$((bad+1)); continue; fi
  if out=$($IMG compare "$raw" "$GOLDEN_DIR/$name" "$DIFF/desktop-$name"); then echo "ok    $name"; else echo "FAIL  $name: $out"; bad=$((bad+1)); fi
done
if [ "$MODE" = check ] && [ -z "${ONLY:-}" ]; then
  for g in "$GOLDEN_DIR"/*.png; do [ -e "$RUN/raw/$(basename "$g")" ] || { echo "STALE $(basename "$g") (golden without a screen)"; bad=$((bad+1)); }; done
fi
if [ "$MODE" = update ]; then echo "updated $n goldens in $GOLDEN_DIR ($(du -sk "$GOLDEN_DIR" | cut -f1) KiB): review with git diff before committing"; fi
if [ "$MODE" = check ]; then
  echo "golden-desktop: $n screens, $bad failing; diffs in dist/golden-diff/"
  [ "$bad" -eq 0 ] && [ "$FAILED" -eq 0 ] || exit 1
fi
[ "$FAILED" -eq 0 ]
