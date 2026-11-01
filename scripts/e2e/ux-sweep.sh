#!/usr/bin/env bash
# UX sweep of the reskinned app on the emulator: for each configuration (theme, font scale, orientation, RTL) open
# Home, Diagnostics (MainActivity) and the QR-pair screen, walk down each page and run scripts/e2e/a11y-audit.py
# (touch targets < 48dp, unlabeled controls, clipped/off-screen nodes, overlapping controls). Screenshots of the
# first page of each go to target/ux/. Needs ADB (adb -s SERIAL, dedicated server port) in the environment.
#
#   ADB="adb -s emulator-5624" ANDROID_ADB_SERVER_PORT=16070 scripts/e2e/ux-sweep.sh [CONFIG ...]
#   CONFIG: light dark light-f2 dark-f2 light-land light-rtl   (default: all)
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
OUT="$ROOT/target/ux"; mkdir -p "$OUT"
: "${ADB:?set ADB}"
PKG=dev.geekbridge.android
AUDIT="$ROOT/scripts/e2e/a11y-audit.py"
set_cfg() { # night font rotation rtl
  $ADB shell cmd uimode night "$1" >/dev/null
  $ADB shell settings put system font_scale "$2"
  $ADB shell settings put system accelerometer_rotation 0
  $ADB shell settings put system user_rotation "$3"
  # RTL: the developer "force RTL" setting is not picked up on this image; an RTL app locale mirrors the layout.
  if [[ "$4" == true ]]; then $ADB shell cmd locale set-app-locales $PKG --locales ar >/dev/null
  else $ADB shell cmd locale set-app-locales $PKG --locales "" >/dev/null 2>&1; fi
  sleep 2
}
screens() {
  case "$1" in
    home) echo "-n $PKG/.HomeActivity" ;;
    diag) echo "-n $PKG/.HomeActivity" ;;   # MainActivity (Diagnostics) is not exported: open it from Home
    qr)   echo "-n $PKG/.QrPairActivity" ;;
  esac
}
walk() { # cfg screen
  local cfg="$1" scr="$2" prev="" cur page=0
  $ADB shell am force-stop $PKG; sleep 1
  # shellcheck disable=SC2046
  $ADB shell am start -f 0x04000000 $(screens "$scr") >/dev/null; sleep 4
  if [[ "$scr" == diag ]]; then
    python3 "$ROOT/scripts/e2e/adbui.py" scrolltap "Diagnostics & about" >/dev/null; sleep 3
  fi
  while ((page < 9)); do
    "$AUDIT" "$cfg-$scr-p$page"
    ((page == 0)) && $ADB exec-out screencap -p >"$OUT/$cfg-$scr.png"
    cur="$(python3 "$ROOT/scripts/e2e/adbui.py" dump | md5sum)"
    [[ "$cur" == "$prev" ]] && break
    prev="$cur"; ((page++))
    local w h; read -r w h < <($ADB shell wm size | grep -oE '[0-9]+x[0-9]+' | tr x ' ' | tail -1)
    if [[ "$cfg" == *land* ]]; then local t=$w; w=$h; h=$t; fi
    $ADB shell input swipe $((w / 2)) $((h * 3 / 4)) $((w / 2)) $((h / 4)) 400; sleep 1
  done
}
CFGS=("$@"); ((${#CFGS[@]})) || CFGS=(light dark light-f2 dark-f2 light-land light-rtl)
export ADB
for cfg in "${CFGS[@]}"; do
  case "$cfg" in
    light) set_cfg no 1.0 0 false ;; dark) set_cfg yes 1.0 0 false ;;
    light-f2) set_cfg no 2.0 0 false ;; dark-f2) set_cfg yes 2.0 0 false ;;
    light-land) set_cfg no 1.0 1 false ;; light-rtl) set_cfg no 1.0 0 true ;;
  esac
  for scr in home diag qr; do walk "$cfg" "$scr"; done
done
set_cfg no 1.0 0 false
