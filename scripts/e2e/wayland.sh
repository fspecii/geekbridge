#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The Wayland paths for real, in an ISOLATED headless compositor (the user's session is never touched).
#
#   scripts/e2e/wayland.sh [--keep] [STAGE[,STAGE...]]     stages: clip hotkey apps notify tray   (default: all)
#
# Skips with a clear message (exit 0) when no compositor / tool is available.
#
# What runs, all private: a headless `sway` (wlroots, pixman, no input devices) in a 0700 runtime dir under the scratch
# area, a `dbus-run-session` bus with `mako`, a private HOME/XDG tree, the real geekbridge-service and apps. Hyprland
# itself cannot run headless on a machine whose only KMS device is the real GPU (aquamarine needs a DRM allocator),
# so Hyprland syntax is checked with `Hyprland --verify-config` and the key/rule behaviour on sway (docs/reports/wayland-e2e.md).
#   clip     wl-clipboard backend and service against the compositor: text/image/HTML/primary, event-driven (no polling when
#            idle), loop suppression, apply-from-phone, size cap (cargo test wayland_clipboard)
#   hotkey   `geekbridge-ctl hotkey install` dry-run/apply/verify/uninstall (byte-for-byte) on temp .conf and .lua configs;
#            then the sway binding from `hotkey print`, triggered with wtype: the real command runs and mako shows its result
#   apps     the egui apps with WAYLAND_DISPLAY and no DISPLAY: app_id, floating/sticky/centred via the printed sway rules,
#            tiled for the others, startup log clean, scale 1 / 1.5 / 2, screenshots (grim)
#   notify   notify-send with the service's actions through mako, invoked with makoctl
#   tray     StatusNotifierItem registers and its dbusmenu opens (waybar if installed, else the fake watcher)
# Results: dist/wayland-e2e/results.tsv   Pictures: dist/screenshots/desktop/wayland-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"

if [[ -z "${WAYLAND_E2E_INNER:-}" ]]; then
  command -v dbus-run-session >/dev/null || { echo "SKIPPED: dbus-run-session not installed"; exit 0; }
  exec dbus-run-session -- env WAYLAND_E2E_INNER=1 "${BASH_SOURCE[0]}" "$@"
fi

KEEP=0 STAGES="clip,hotkey,apps,notify,tray"
for a in "$@"; do
  case "$a" in
    --keep) KEEP=1 ;;
    -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) STAGES="$a" ;;
  esac
done
want() { [[ ",$STAGES," == *",$1,"* ]]; }

missing=()
for t in sway swaymsg grim wl-copy wl-paste mako makoctl notify-send wtype busctl; do command -v "$t" >/dev/null || missing+=("$t"); done
if ((${#missing[@]})); then echo "SKIPPED: no Wayland test compositor stack (missing: ${missing[*]})"; exit 0; fi

# The toolchain lives under the real HOME; keep finding it after HOME is made private below.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
: "${GB_TMP:=${TMPDIR:-$HOME/.cache/gb-tmp}}"
# shellcheck disable=SC1091
source "$ROOT/scripts/lib-tmp.sh"
RUNROOT="$GB_TMP/wl-xdg"; mkdir -p "$RUNROOT"; chmod 700 "$RUNROOT"
# A short, private runtime dir (a compositor socket path is limited to ~100 bytes).
XRT="$(mktemp -d "$RUNROOT/r.XXXXXX")"; chmod 700 "$XRT"
PRIV="$XRT/priv"; mkdir -p "$PRIV"/{home,state,cfg}
OUTD="$ROOT/dist/wayland-e2e"; SHOTS="$ROOT/dist/screenshots/desktop"; mkdir -p "$OUTD" "$SHOTS"
RES="$OUTD/results.tsv"; : >"$RES"
TGT="${CARGO_TARGET_DIR:-$ROOT/target}/debug"
SVC="$TGT/geekbridge-service" CTL="$TGT/geekbridge-ctl"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null; done
  sleep 0.3
  [[ $KEEP == 1 ]] && echo "kept $XRT" || rm -rf "$XRT"
}
trap cleanup EXIT
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$RES" >&2; }
FAILS=0
check() { # name, command... ; records PASS/FAIL
  local name="$1"; shift
  if "$@" >>"$OUTD/checks.log" 2>&1; then rec "$name" PASS ""; else rec "$name" FAIL "$*"; FAILS=$((FAILS + 1)); fi
}
wait_for() { local n="$1" i; shift; for ((i = 0; i < n * 10; i++)); do "$@" >/dev/null 2>&1 && return 0; sleep 0.1; done; return 1; }

for b in geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-share; do
  [[ -x "$TGT/$b" ]] || { echo "building ($b missing)"; cargo build -q -p bridge-service -p bridge-apps --bins || { rec build FAIL "cargo build"; exit 1; }; break; }
done

# ---------------------------------------------------------------- compositor + notification daemon + service
export XDG_RUNTIME_DIR="$XRT" HOME="$PRIV/home" XDG_CONFIG_HOME="$PRIV/cfg" XDG_DATA_HOME="$PRIV/home/.local/share"
export XDG_STATE_HOME="$PRIV/home/.local/state" XDG_CACHE_HOME="$PRIV/home/.cache" XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland
unset DISPLAY HYPRLAND_INSTANCE_SIGNATURE LD_LIBRARY_PATH WAYLAND_DISPLAY
export SWAYSOCK="$XRT/sway.sock"
# Programs the compositor starts (the hotkey command) must find this service, like a session started after it.
export GEEKBRIDGE_STATE_DIR="$PRIV/state" GEEKBRIDGE_CONTROL_PORT=17611
mkdir -p "$HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
printf 'output HEADLESS-1 resolution 1280x800 scale 1\ndefault_border pixel 2\n' >"$XRT/sway.conf"
(WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman setsid timeout 1500 sway -c "$XRT/sway.conf" >"$OUTD/sway.log" 2>&1 & echo $! >"$XRT/sway.pid")
PIDS+=("$(cat "$XRT/sway.pid" 2>/dev/null)")
wait_for 15 test -S "$XRT/wayland-1" || { echo "SKIPPED: headless sway did not start: $(tail -2 "$OUTD/sway.log" | tr '\n' ' ')"; exit 0; }
export WAYLAND_DISPLAY=wayland-1
printf 'default-timeout=0\nignore-timeout=1\nlayer=overlay\nmax-visible=8\n' >"$XRT/mako.conf"
(setsid mako --config "$XRT/mako.conf" >"$OUTD/mako.log" 2>&1 & echo $! >"$XRT/mako.pid"); PIDS+=("$(cat "$XRT/mako.pid")")
wait_for 15 bash -c "busctl --user list --no-pager 2>/dev/null | grep -q 'org.freedesktop.Notifications.*mako'" \
  || { rec env FAIL "mako did not own org.freedesktop.Notifications"; exit 1; }

# Every notification the session sends, as the daemon receives it (mako's `list` does not show bodies).
NOTIFY_LOG="$XRT/notify.log"
(setsid dbus-monitor "interface='org.freedesktop.Notifications',member='Notify'" >"$NOTIFY_LOG" 2>&1 & echo $! >"$XRT/mon.pid"); PIDS+=("$(cat "$XRT/mon.pid")")

API=17610 CTRL=17611 DISC=17646 LISTEN=17637
start_service() {
  mkdir -p "$PRIV/state"; chmod 700 "$PRIV/state"
  (GEEKBRIDGE_NO_UPDATE_CHECK=1 setsid "$SVC" --state-dir "$PRIV/state" --api-port $API --control-port $CTRL --discovery-port $DISC \
     --listen-inbound 127.0.0.1:$LISTEN --dev-loopback --no-calling-probe --name WL-Desktop >"$OUTD/service.log" 2>&1 & echo $! >"$XRT/svc.pid")
  PIDS+=("$(cat "$XRT/svc.pid")")
  wait_for 20 "$CTL" status
}
ctl() { "$CTL" "$@"; }
shot() { grim "$SHOTS/wayland-$1.png"; }
get_tree() { swaymsg -t get_tree; }
# app_id floating sticky x y w h of every toplevel, one per line
windows() {
  get_tree | python3 -I -c '
import json,sys
def walk(n):
    if n.get("pid") and n.get("app_id") is not None:
        r=n["rect"]; print(n["app_id"], int(n["type"]=="floating_con"), int(bool(n.get("sticky"))), r["x"], r["y"], r["width"], r["height"])
    for c in n.get("nodes",[])+n.get("floating_nodes",[]): walk(c)
walk(json.load(sys.stdin))'
}
rec env PASS "sway $(sway --version | head -1), mako, runtime $XRT, WAYLAND_DISPLAY=$WAYLAND_DISPLAY, no DISPLAY"

# ================================================================ clip
stage_clip() {
  local out
  out="$(GEEKBRIDGE_TEST_WAYLAND=1 cargo test -q -p bridge-service --test wayland_clipboard -- --test-threads=1 2>&1)"
  echo "$out" >"$OUTD/clip-cargo.log"
  if grep -q "test result: ok. 3 passed" <<<"$out"; then
    rec clip_backend_and_service PASS "3 tests: text/png/jpeg/primary/sensitive, idle=0 reads, 1 change=few reads, primary ignored, no echo, 3 MiB image refused over a 1 MiB cap"
  else rec clip_backend_and_service FAIL "$(tail -5 <<<"$out" | tr '\n' '|')"; FAILS=$((FAILS + 1)); fi
  # Observe the stream directly: `wl-paste --watch` is silent when idle and speaks once per change.
  local log="$XRT/watch.log"; : >"$log"
  (wl-paste --watch sh -c 'echo ev >>"$0"' "$log" >/dev/null 2>&1 & echo $! >"$XRT/watch.pid")
  sleep 1; local base; base=$(wc -l <"$log")
  sleep 3; check clip_idle_is_silent test "$(wc -l <"$log")" -eq "$base"
  printf 'one' | wl-copy; sleep 0.5; printf 'two' | wl-copy; sleep 0.5
  printf '\x89PNG\r\n\x1a\nxx' | wl-copy --type image/png; sleep 0.5
  check clip_one_event_per_change test "$(($(wc -l <"$log") - base))" -eq 3
  printf 'prim' | wl-copy --primary; sleep 0.5
  check clip_primary_is_not_the_clipboard test "$(($(wc -l <"$log") - base))" -eq 3
  kill "$(cat "$XRT/watch.pid")" 2>/dev/null
  # No focused surface needed: nothing is mapped in this compositor, and wl-copy/wl-paste work (wlr-data-control).
  check clip_works_with_no_focused_window bash -c 'printf unfocused | wl-copy && [[ "$(wl-paste -n)" == unfocused ]]'
  check clip_compositor_has_data_control bash -c 'wayland-info 2>/dev/null | grep -qi "data_control" || swaymsg -t get_version >/dev/null'
}

# ================================================================ hotkey
stage_hotkey() {
  local T="$XRT/hk" h="$XRT/hk/home" f
  mkdir -p "$h/.config/hypr"
  hk() { env -u HYPRLAND_INSTANCE_SIGNATURE HOME="$h" XDG_CONFIG_HOME="$h/.config" "$CTL" "$@"; }
  printf '# user\nbind = SUPER, RETURN, exec, kitty\nbind = SUPER SHIFT, V, exec, other\n' >"$h/.config/hypr/bindings.conf"
  cp "$h/.config/hypr/bindings.conf" "$T/orig.conf"
  f="$h/.config/hypr/bindings.conf"
  hk hotkey install >"$T/dry.txt" 2>&1; check hotkey_dry_run_writes_nothing cmp "$T/orig.conf" "$f"
  check hotkey_dry_run_shows_diff grep -q '^+bindd = SHIFT SUPER, V' "$T/dry.txt"
  hk hotkey install --apply >"$T/apply.txt" 2>&1
  check hotkey_apply_has_block grep -q '>>> GeekBridge hotkey' "$f"
  check hotkey_apply_made_backup bash -c "ls '$f'.bak.geekbridge.* >/dev/null"
  if command -v Hyprland >/dev/null; then
    printf 'source = %s\n' "$f" >"$T/verify.conf"
    HOME="$h" Hyprland --verify-config -c "$T/verify.conf" >"$T/verify.txt" 2>&1
    check hotkey_conf_passes_hyprland_verify bash -c "! grep -qi 'error' '$T/verify.txt'"
    # The check must be able to fail: a broken line is reported.
    printf 'windowrule = nonsense on, match:class x\nbind = broken\n' >"$T/bad.conf"; printf 'source = %s\n' "$T/bad.conf" >"$T/verify-bad.conf"
    HOME="$h" Hyprland --verify-config -c "$T/verify-bad.conf" >"$T/verify-bad.txt" 2>&1
    check hotkey_verify_detects_errors grep -qi 'error' "$T/verify-bad.txt"
  else rec hotkey_conf_passes_hyprland_verify SKIP "Hyprland not installed"; fi
  hk hotkey install --apply >"$T/again.txt" 2>&1; check hotkey_install_is_idempotent grep -q 'no changes' "$T/again.txt"
  hk hotkey uninstall --apply >/dev/null 2>&1; check hotkey_uninstall_restores_conf_bytes cmp "$T/orig.conf" "$f"
  # Lua flavour (current Omarchy).
  rm -f "$h"/.config/hypr/bindings.conf*
  printf -- '-- user\nhl.bind("SUPER + RETURN", hl.dsp.exec_cmd("kitty"))\n' >"$h/.config/hypr/bindings.lua"; cp "$h/.config/hypr/bindings.lua" "$T/orig.lua"
  hk hotkey install --apply >/dev/null 2>&1
  if command -v Hyprland >/dev/null; then
    printf 'dofile("%s")\n' "$h/.config/hypr/bindings.lua" >"$T/verify.lua"
    HOME="$h" Hyprland --verify-config -c "$T/verify.lua" >"$T/verify-lua.txt" 2>&1
    check hotkey_lua_passes_hyprland_verify bash -c "! grep -qi 'error' '$T/verify-lua.txt'"
  fi
  hk hotkey uninstall --apply >/dev/null 2>&1; check hotkey_uninstall_restores_lua_bytes cmp "$T/orig.lua" "$h/.config/hypr/bindings.lua"
  rec hotkey_hyprland_live SKIP "Hyprland cannot start headless here (needs a KMS device); trigger verified on sway below"

  # The same binding on sway, as `hotkey print` documents it, triggered by a real key press (wtype).
  start_service || { rec hotkey_service FAIL "service did not start"; return 1; }
  local bind; bind="$(hk hotkey print 2>/dev/null | grep -m1 '^bindsym ')"
  [[ -n "$bind" ]] || { rec hotkey_sway_bind FAIL "hotkey print has no bindsym line"; return 1; }
  check hotkey_sway_accepts_print_line bash -c "swaymsg '$bind' | grep -q '\"success\": true'"
  makoctl dismiss -a >/dev/null 2>&1
  # Super+Shift+V. No phone is paired: the command must still run, say why, and leave the clipboard alone.
  printf 'before' | wl-copy
  wtype -M logo -M shift -k v -m shift -m logo
  check hotkey_press_runs_the_command wait_for 15 bash -c "makoctl list 2>/dev/null | grep -q 'GeekBridge'"
  check hotkey_press_reports_in_a_notification wait_for 10 bash -c "grep -qiE 'phone' '$NOTIFY_LOG'"
  grep -A8 'string "Clipboard"' "$NOTIFY_LOG" | head -12 >"$OUTD/hotkey-notification.txt"
  check hotkey_failed_paste_keeps_clipboard bash -c '[[ "$(wl-paste -n)" == before ]]'
}

# ================================================================ apps
sway_rules() { # the for_window lines from `hotkey print`
  env HOME="$HOME" "$CTL" hotkey print 2>/dev/null | grep '^ *for_window \[app_id' | sed 's/^ *//'
}
app_env() { env RUST_BACKTRACE=1 GEEKBRIDGE_STATE_DIR="$PRIV/state" GEEKBRIDGE_CONTROL_PORT=$CTRL "$@"; }
launch() { # app args... -> pid in $LAST (every app but Share takes --api-port)
  local name="$1" api=(--api-port $API); shift
  [[ $name == geekbridge-share ]] && api=()
  (app_env setsid "$TGT/$name" "${api[@]}" "$@" >"$OUTD/app-$name.log" 2>&1 & echo $! >"$XRT/app.pid"); LAST="$(cat "$XRT/app.pid")"; PIDS+=("$LAST")
}
stage_apps() {
  [[ -n "$(pgrep -F "$XRT/svc.pid" 2>/dev/null)" ]] || start_service || { rec apps_service FAIL "no service"; return 1; }
  local rule; rule="$(sway_rules)"
  [[ -n "$rule" ]] || { rec apps_rule FAIL "no for_window line in hotkey print"; return 1; }
  while IFS= read -r line; do swaymsg "$line" | grep -q '"success": true' || rec apps_rule_rejected FAIL "$line"; done <<<"$rule"
  local app id
  # app -> expected app_id and whether the printed rule floats it
  for spec in "geekbridge-calls:dev.geekbridge.Calls:1" "geekbridge-share:dev.geekbridge.Share:1" "geekbridge:dev.geekbridge.GeekBridge:0" \
              "geekbridge-messages:dev.geekbridge.Messages:0" "geekbridge-contacts:dev.geekbridge.Contacts:0" \
              "geekbridge-agenda:dev.geekbridge.Agenda:0" "geekbridge-clipboard:dev.geekbridge.Clipboard:0"; do
    IFS=: read -r app id float <<<"$spec"
    [[ -x "$TGT/$app" ]] || { rec "app_$app" SKIP "binary not built"; continue; }
    launch "$app" --theme dark; local pid=$LAST
    if ! wait_for 15 bash -c "swaymsg -t get_tree | grep -q '\"app_id\": \"$id\"'"; then
      rec "app_${app}_maps_on_wayland" FAIL "no window with app_id $id; got: $(windows | cut -d' ' -f1 | tr '\n' ' '); log: $(head -3 "$OUTD/app-$app.log" | tr '\n' '|')"; FAILS=$((FAILS + 1)); kill "$pid" 2>/dev/null; continue
    fi
    sleep 2
    local line; line="$(windows | grep "^$id ")"
    read -r _ fl st x y w h <<<"$line"
    rec "app_${app}_app_id" PASS "$id"
    if [[ $float == 1 ]]; then
      check "app_${app}_floats_sticky" test "$fl$st" = 11
      # centred: its middle within 2 px of the output's middle (1280x800)
      check "app_${app}_centred" python3 -I -c "import sys; x,y,w,h=map(int,sys.argv[1:]); sys.exit(0 if abs(x+w/2-640)<=2 and abs(y+h/2-400)<=2 else 1)" "$x" "$y" "$w" "$h"
      check "app_${app}_gets_the_rule_size" test "${w}x${h}" = 420x700
      rec "app_${app}_size" INFO "${w}x${h} at $x,$y"
    else
      check "app_${app}_stays_tiled" test "$fl$st" = 00
    fi
    shot "${app#geekbridge-}-$([[ $float == 1 ]] && echo floating || echo tiled)"
    if [[ $app == geekbridge-calls ]]; then
      # Physical size grows with the scale so the logical desktop stays 1280x800 and the window fits.
      for s in 1.5 2 1; do
        swaymsg output HEADLESS-1 mode "$(python3 -I -c "import sys; s=float(sys.argv[1]); print(f'{int(1280*s)}x{int(800*s)}')" "$s")" scale $s >/dev/null; sleep 1.5
        local l2; l2="$(windows | grep "^$id ")"
        shot "calls-scale-${s/./_}x"
        rec "app_calls_scale_$s" INFO "$l2"
        check "app_calls_alive_at_scale_$s" kill -0 "$pid"
      done
    fi
    # No warnings from winit/egui/accesskit at startup (Mesa's software-EGL chatter is the pixman test compositor, not us).
    local noisy; noisy="$(grep -viE '^MESA|^libEGL|^$' "$OUTD/app-$app.log" | grep -iE 'warn|error|panick|failed' | head -3)"
    check "app_${app}_startup_log_clean" test -z "$noisy"
    [[ -n "$noisy" ]] && rec "app_${app}_startup_log" INFO "$noisy"
    kill "$pid" 2>/dev/null; wait_for 5 bash -c "! kill -0 $pid 2>/dev/null"
  done
  # A compositor may configure a window at (nearly) nothing: a tiled window dragged tiny, or a surface's first 0x0
  # configure. Every app must survive that (the Home window used to panic in egui on a negative max width).
  for spec in "geekbridge:GeekBridge" "geekbridge-calls:Calls" "geekbridge-share:Share" "geekbridge-messages:Messages" \
              "geekbridge-contacts:Contacts" "geekbridge-agenda:Agenda" "geekbridge-clipboard:Clipboard"; do
    app="${spec%%:*}"; id="dev.geekbridge.${spec##*:}"; [[ -x "$TGT/$app" ]] || continue
    swaymsg "for_window [app_id=\"$id\"] floating enable" >/dev/null
    launch "$app"; local pid=$LAST
    wait_for 15 bash -c "swaymsg -t get_tree | grep -q '\"app_id\": \"$id\"'"; sleep 1.5
    for sz in "10 10" "1 1" "50 400" "400 30" "420 700"; do swaymsg "[app_id=\"$id\"] resize set $sz" >/dev/null; sleep 0.7; done
    check "app_${app}_survives_tiny_sizes" kill -0 "$pid"
    grep -a -q panicked "$OUTD/app-$app.log" && rec "app_${app}_panic" INFO "$(grep -a -A1 panicked "$OUTD/app-$app.log" | head -2 | tr '\n' ' ')"
    kill "$pid" 2>/dev/null; wait_for 5 bash -c "! kill -0 $pid 2>/dev/null"
  done
  check apps_never_used_x11 bash -c "! grep -qiE 'x11|xcb|cannot open display' '$OUTD'/app-*.log"
}

# ================================================================ notify
stage_notify() {
  swaymsg '[app_id=".*"] kill' >/dev/null 2>&1; sleep 1   # an empty desktop for the picture
  makoctl dismiss -a >/dev/null 2>&1
  local out="$XRT/action.out"; : >"$out"
  # Same arguments the service uses for "file received" (docs/omarchy.md section 3), plus the default action.
  (notify-send --app-name=GeekBridge --wait --category=transfer.complete --action=open=Open --action=folder="Show in folder" \
     "File received" -- "photo.jpg" >"$out" 2>&1 & echo $! >"$XRT/ns.pid")
  check notify_arrives_in_mako wait_for 10 bash -c "makoctl list | grep -q 'File received'"
  local id; id="$(makoctl list | grep -m1 -o '^Notification [0-9]*' | grep -o '[0-9]*$')"
  makoctl list >"$OUTD/notify-list.txt"; shot notification
  check notify_has_actions bash -c "makoctl list | grep -qi 'open'"
  makoctl invoke -n "$id" folder >/dev/null 2>&1
  check notify_action_reaches_the_sender wait_for 5 bash -c "grep -qx folder '$out'"
  # Body click = `default`: the service opens the window, never answers/accepts.
  : >"$out"
  (notify-send --app-name=GeekBridge --wait --urgency=critical --category=call.incoming --action=default=Open --action=answer=Answer --action=decline=Decline "Incoming call" -- "Mum" >"$out" 2>&1 &)
  wait_for 10 bash -c "makoctl list | grep -q 'Incoming call'"
  id="$(makoctl list | grep -B1 -m1 'Incoming call' | grep -o '^Notification [0-9]*' | grep -o '[0-9]*$')"
  [[ -n "$id" ]] || id="$(makoctl list | grep -m1 -o '^Notification [0-9]*' | grep -o '[0-9]*$')"
  makoctl invoke -n "$id" decline >/dev/null 2>&1
  check notify_call_decline_action wait_for 5 bash -c "grep -qx decline '$out'"
  # Hostile text is shown as text.
  makoctl dismiss -a >/dev/null 2>&1
  notify-send --app-name=GeekBridge -- "<b>bold</b> &amp; \$(touch $XRT/pwned)" "x" >/dev/null 2>&1
  sleep 0.5; check notify_text_is_inert test ! -e "$XRT/pwned"
  # Feedback of the hotkey command itself when the service is down (nothing to read a token from).
  makoctl dismiss -a >/dev/null 2>&1
  env GEEKBRIDGE_STATE_DIR="$XRT/nonexistent-state" "$CTL" clipboard paste-from-phone --notify >/dev/null 2>&1
  check notify_paste_without_service_says_so wait_for 5 bash -c "grep -q 'not running' '$NOTIFY_LOG'"
}

# ================================================================ tray
stage_tray() {
  if command -v waybar >/dev/null; then
    mkdir -p "$PRIV/waybar"
    printf '{"layer":"top","height":28,"modules-right":["tray"],"tray":{"icon-size":18,"spacing":8}}\n' >"$PRIV/waybar/config.jsonc"
    : >"$PRIV/waybar/style.css"
    (waybar -c "$PRIV/waybar/config.jsonc" -s "$PRIV/waybar/style.css" >"$OUTD/waybar.log" 2>&1 & echo $! >"$XRT/waybar.pid"); PIDS+=("$(cat "$XRT/waybar.pid")")
    wait_for 15 bash -c "busctl --user list --no-pager | grep -q org.kde.StatusNotifierWatcher" || { rec tray_waybar FAIL "waybar did not own the watcher"; return 1; }
    pkill -F "$XRT/svc.pid" 2>/dev/null; sleep 1; start_service
    check tray_icon_registers_with_waybar wait_for 15 bash -c "busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems | grep -qi geekbridge"
    shot tray-waybar
  else
    rec tray_waybar SKIP "waybar not installed; the StatusNotifierWatcher is emulated by examples/tray-fake-watcher"
    local out; out="$(env -u XDG_RUNTIME_DIR CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}" OUT="$OUTD/tray" "$ROOT/scripts/e2e/tray-headless.sh" 2>&1)"
    echo "$out" >"$OUTD/tray-headless.log"
    if [[ $? == 0 ]] && ! grep -q '^FAIL' <<<"$out"; then rec tray_register_and_menu PASS "tray-headless.sh (register, properties, menu, disable, quit)"; else rec tray_register_and_menu FAIL "$(tail -3 <<<"$out" | tr '\n' '|')"; FAILS=$((FAILS + 1)); fi
  fi
}

for s in clip hotkey apps notify tray; do want "$s" && { echo "== $s"; "stage_$s"; }; done
echo
echo "results: $RES  (failures: $FAILS)"
exit $((FAILS > 0))
