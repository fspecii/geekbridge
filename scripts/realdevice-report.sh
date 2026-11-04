#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# One command that collects everything needed to diagnose a real-phone session remotely, redacts it and
# writes one text file (and a .tar.gz of it) under ~/.cache/geekbridge-reports/. Read-only: it changes
# nothing and never asks for messages, notifications, contacts or clipboard content.
#
#   scripts/realdevice-report.sh [--state-dir DIR] [--control-port PORT] [--bin-dir DIR] [--service-log FILE]
#                                [--page URL] [--apk FILE] [--serial SERIAL] [--out DIR] [--no-adb] [--stdout]
#                                [--bt-trace SECONDS]
#
# Sections: versions and SHA-256 of the installed desktop binaries; geekbridge-ctl doctor, status (JSON),
# events, clipboard status; Bluetooth and call audio (adapter, rfkill, bluetoothctl, PipeWire cards and call
# streams, WirePlumber bluez settings and log lines, call audio-devices; BlueZ objects over D-Bus, the dongle's USB id
# and driver, PipeWire bluez nodes, bluetooth.service and WirePlumber journals); for every paired phone phone-state and phone-log (debug builds only; others say
# so); firewall state and listening ports; service log tail; the phone's installed app version and APK hash
# (adb, when a phone is attached); the APK(s) linked from the page at --page URL, hashed and compared.
#
# Redaction (scripts/lib/report_redact.py, tested by scripts/tests/realdevice-kit-test.sh): control token and
# private keys, pairing links, content/name/secret JSON values, long hex/base64 runs, e-mail addresses, phone
# numbers, MACs, the last two IPv4 octets, the user name in paths; certificate fingerprints are shortened.
# Still read the file before attaching it to a public issue.
#
# --bt-trace SECONDS: first records SECONDS of Bluetooth traffic while you reproduce the problem (start the call
# setup, pair, place or answer a call): the org.pipewire.Telephony session-bus traffic (no root needed), and with
# passwordless sudo also BlueZ's system-bus traffic (dbus-monitor) and an HCI capture (btmon -w, kept as a separate
# .snoop file next to the report: it can hold link keys, share it privately only). Without sudo it prints the two
# commands to run by hand in another terminal.
#
# Defaults match scripts/realdevice.sh (state target/realdevice-state, control port 17381, binaries
# target/release). Environment: GEEKBRIDGE_REALDEVICE_STATE, GEEKBRIDGE_CONTROL_PORT, GEEKBRIDGE_BIN_DIR,
# GEEKBRIDGE_ADB.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN="${GEEKBRIDGE_BIN_DIR:-$ROOT/target/release}"
STATE="${GEEKBRIDGE_REALDEVICE_STATE:-$ROOT/target/realdevice-state}"
CTRL_PORT="${GEEKBRIDGE_CONTROL_PORT:-17381}"
OUTDIR="${GEEKBRIDGE_REPORT_DIR:-$HOME/.cache/geekbridge-reports}"
SVC_LOG="" PAGE="" APKFILE="" SERIAL="${ANDROID_SERIAL:-}" USE_ADB=1 TO_STDOUT=0 BT_TRACE=0
PKG=dev.geekbridge.android

usage() { sed -n '3,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
while (($#)); do
  case "$1" in
    --state-dir) shift; STATE="${1:?--state-dir needs a value}" ;;
    --control-port) shift; CTRL_PORT="${1:?--control-port needs a value}" ;;
    --bin-dir) shift; BIN="${1:?--bin-dir needs a value}" ;;
    --service-log) shift; SVC_LOG="${1:?--service-log needs a file}" ;;
    --page) shift; PAGE="${1:?--page needs a URL}" ;;
    --apk) shift; APKFILE="${1:?--apk needs a file}" ;;
    --serial) shift; SERIAL="${1:?--serial needs a value}" ;;
    --out) shift; OUTDIR="${1:?--out needs a directory}" ;;
    --no-adb) USE_ADB=0 ;;
    --stdout) TO_STDOUT=1 ;;
    --bt-trace) shift; BT_TRACE="${1:?--bt-trace needs seconds}"; [[ $BT_TRACE =~ ^[0-9]+$ ]] || { echo "error: --bt-trace needs a number of seconds" >&2; exit 1; } ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument $1 (try --help)" >&2; exit 1 ;;
  esac
  shift
done

command -v python3 >/dev/null || { echo "error: python3 is needed for the redaction filter" >&2; exit 1; }
REDACT=(python3 -I "$ROOT/scripts/lib/report_redact.py")
[[ -f $STATE/ipc-token ]] && REDACT+=(--needle-file "$STATE/ipc-token")

ctl() { timeout 20 "$BIN/geekbridge-ctl" --state-dir "$STATE" --control-port "$CTRL_PORT" "$@" 2>&1; }
sec() { printf '\n## %s\n' "$*"; }
run() { # label command...: prints the command, its output and exit status
  local label="$1"; shift
  sec "$label"; if [[ $1 == ctl ]]; then printf '$ geekbridge-ctl %s\n' "${*:2}"; else printf '$ %s\n' "$*"; fi
  local out rc; out="$("$@" 2>&1)"; rc=$?
  printf '%s\n[exit %s]\n' "$out" "$rc"
}
hash_of() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }

TS="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUTDIR" && chmod 700 "$OUTDIR" 2>/dev/null
WORK="$(mktemp -d "$OUTDIR/.work-XXXXXX")" || exit 1
trap 'rm -rf "$WORK"' EXIT
RAW="$WORK/raw.txt"

# --bt-trace: record while the owner reproduces the problem, before anything else is collected.
trace() {
  local secs="$1" sudo=()
  if [[ $EUID -eq 0 ]]; then sudo=()
  elif sudo -n true 2>/dev/null; then sudo=(sudo -n)
  else sudo=(none); fi
  echo ">>> Recording Bluetooth traffic for $secs s: reproduce the problem NOW (call setup, pairing, a call)." >&2
  if command -v dbus-monitor >/dev/null; then
    timeout "$secs" dbus-monitor --session "sender='org.pipewire.Telephony'" "destination='org.pipewire.Telephony'" \
      >"$WORK/trace-telephony.txt" 2>&1 &
  fi
  if [[ ${sudo[0]-} != none ]]; then
    command -v dbus-monitor >/dev/null && timeout "$secs" "${sudo[@]}" dbus-monitor --system "sender='org.bluez'" \
      "destination='org.bluez'" >"$WORK/trace-bluez.txt" 2>&1 &
    command -v btmon >/dev/null && timeout "$secs" "${sudo[@]}" btmon -w "$OUTDIR/btmon-$TS.snoop" >/dev/null 2>&1 &
  else
    {
      echo "no passwordless sudo: BlueZ's own traffic was not recorded. To record it, run these in another terminal"
      echo "while reproducing, then attach the outputs (privately: the .snoop can hold link keys):"
      echo "  sudo timeout $secs dbus-monitor --system \"sender='org.bluez'\" \"destination='org.bluez'\" > bluez-trace.txt"
      echo "  sudo timeout $secs btmon -w btmon.snoop"
    } >"$WORK/trace-bluez.txt"
  fi
  wait
  echo ">>> Recording done; collecting the rest." >&2
}

bluetooth_details() {
  sec "Bluetooth stack details (host)"
  local c a h
  printf '$ uname -r\n'; uname -r
  for c in "bluetoothctl --version" "busctl --system tree org.bluez" "wpctl status" \
    "journalctl -u bluetooth.service -n 200 --no-pager" "journalctl --user -u wireplumber -n 200 --no-pager"; do
    command -v "${c%% *}" >/dev/null || continue
    printf '$ %s\n' "$c"; timeout 10 $c 2>&1 | head -220
  done
  [[ -x /usr/lib/bluetooth/bluetoothd ]] && { printf '$ bluetoothd --version\n'; /usr/lib/bluetooth/bluetoothd --version 2>&1; }
  # The controller: USB id (which dongle), driver, btusb parameters; mSBC needs USB alt setting 1/6 support.
  for h in /sys/class/bluetooth/hci[0-9]*; do
    [[ -e $h ]] || continue
    printf '$ %s: driver %s, usb %s:%s %s %s\n' "${h##*/}" "$(basename "$(readlink -f "$h/device/driver" 2>/dev/null)" 2>/dev/null)" \
      "$(cat "$h/device/../idVendor" 2>/dev/null)" "$(cat "$h/device/../idProduct" 2>/dev/null)" \
      "$(cat "$h/device/../manufacturer" 2>/dev/null)" "$(cat "$h/device/../product" 2>/dev/null)"
  done
  if [[ -d /sys/module/btusb/parameters ]]; then
    printf '$ btusb parameters\n'; for c in /sys/module/btusb/parameters/*; do printf '  %s=%s\n' "${c##*/}" "$(cat "$c" 2>/dev/null)"; done
  fi
  if command -v busctl >/dev/null; then
    for a in $(busctl --system tree org.bluez --list 2>/dev/null | grep -E '^/org/bluez/hci[0-9]+$'); do
      printf '$ busctl --system introspect org.bluez %s org.bluez.Adapter1 (properties)\n' "$a"
      timeout 5 busctl --system introspect org.bluez "$a" org.bluez.Adapter1 2>&1 | awk '$2=="property"' | head -40
    done
    for a in $(busctl --system tree org.bluez --list 2>/dev/null | grep -E '^/org/bluez/hci[0-9]+/dev_[0-9A-F_]+$'); do
      printf '$ busctl --system introspect org.bluez %s org.bluez.Device1 (properties)\n' "$a"
      timeout 5 busctl --system introspect org.bluez "$a" org.bluez.Device1 2>&1 | awk '$2=="property"' | grep -v -E 'ManufacturerData|ServiceData|AdvertisingData' | head -30
    done
  fi
  if command -v pw-dump >/dev/null; then
    printf '$ pw-dump (bluez devices and nodes: name, class, profile, codec, state)\n'
    timeout 10 pw-dump 2>/dev/null | python3 -I -c 'import json,sys
try: objs=json.load(sys.stdin)
except Exception as e: print("pw-dump unreadable:", e); sys.exit()
for o in objs:
    i=o.get("info") or {}; p=i.get("props") or {}
    if not any(str(k).startswith("api.bluez5") for k in p): continue
    keys=["device.name","node.name","media.class","api.bluez5.profile","api.bluez5.codec","device.profile.name","api.bluez5.connection","bluez5.profile"]
    print(o.get("type","").split(":")[-1], o.get("id"), i.get("state",""), " ".join("%s=%s"%(k,p[k]) for k in keys if k in p))
    for prm in (i.get("params") or {}).get("Profile",[]) or []:
        print("   active profile:", prm.get("name"), prm.get("description",""))'
  fi
  if command -v journalctl >/dev/null; then
    printf '$ WirePlumber/PipeWire SCO and mSBC capability lines (info level; empty unless WIREPLUMBER_DEBUG=spa.bluez5*:4)\n'
    journalctl --user -u wireplumber -u pipewire -n 5000 --no-pager 2>/dev/null | grep -i -E 'alt6|alt1|msbc|esco|sco|codec|RejectSCO|transport' | tail -40
  fi
  for t in "$WORK/trace-telephony.txt" "$WORK/trace-bluez.txt"; do
    [[ -f $t ]] || continue
    sec "Trace: ${t##*/} (--bt-trace)"; head -c 400000 "$t"
  done
  [[ -f $OUTDIR/btmon-$TS.snoop ]] && { sec "HCI capture"; echo "btmon capture kept (NOT in this report; share privately): $OUTDIR/btmon-$TS.snoop ($(stat -c %s "$OUTDIR/btmon-$TS.snoop") bytes; read with: btmon -r FILE)"; }
}

collect() {
  echo "# GeekBridge real-device report"
  echo "collected: $(date '+%F %T %z') by scripts/realdevice-report.sh"
  echo "host: $(uname -srm)"; echo "session: ${XDG_SESSION_TYPE:-?} / ${XDG_CURRENT_DESKTOP:-?}"
  echo "repo: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo n/a)"
  echo "state dir: $STATE   control port: $CTRL_PORT   bin dir: $BIN"

  sec "Installed binaries (version, SHA-256)"
  local list="$ROOT/scripts/release-binaries.txt" name path
  names=$( { grep -v '^#' "$list" 2>/dev/null; echo geekbridge-phone-sim; } | awk 'NF' )
  for name in $names; do
    path="$BIN/$name"; [[ -x $path ]] || path="$(command -v "$name" 2>/dev/null || true)"
    if [[ -n $path && -x $path ]]; then
      printf 'HASH %s  %s' "$(hash_of "$path")" "$path"
      case "$name" in geekbridge-ctl) printf '  (%s)' "$(timeout 5 "$path" --version 2>&1 | head -1)" ;; esac
      printf '  mtime %s\n' "$(date -r "$path" '+%F %T' 2>/dev/null)"
    else echo "not found: $name"; fi
  done

  if [[ ! -x $BIN/geekbridge-ctl ]]; then
    sec "geekbridge-ctl"; echo "missing: $BIN/geekbridge-ctl (build with scripts/build-desktop.sh or pass --bin-dir)"
  else
    run "doctor" ctl doctor
    run "status (JSON)" ctl status
    run "events" ctl events 0
    run "clipboard status (counters only)" ctl clipboard status
    run "calling" ctl calling
    run "calls (call audio lines)" ctl calls
    run "call audio-devices" ctl call audio-devices
    run "update status" ctl update status
    local ids id
    ids="$(ctl status | python3 -I -c 'import json,sys
t=sys.stdin.read()
try:
    d=json.loads(t[t.index("{"):t.rindex("}")+1])
    print("\n".join(p["device_id"] for p in d.get("paired",[])))
except Exception: pass')"
    if [[ -z $ids ]]; then sec "phone-state / phone-log"; echo "no paired phone (or the service is not running)"; fi
    for id in $ids; do
      run "phone-state ${id:0:8}" ctl phone-state "$id"
      run "phone-log ${id:0:8} (last 200 lines)" ctl phone-log "$id" -n 200
    done
  fi

  sec "Bluetooth and call audio (host)"
  local c
  for c in "ls /sys/class/bluetooth" "rfkill list bluetooth" "systemctl is-active bluetooth.service" \
    "systemctl --user is-active pipewire.service pipewire-pulse.service wireplumber.service" \
    "bluetoothctl show" "bluetoothctl devices Paired" "pactl list cards short" \
    "busctl --user tree org.pipewire.Telephony"; do
    command -v "${c%% *}" >/dev/null || continue
    printf '$ %s\n' "$c"; timeout 8 $c 2>&1 | head -40
  done
  if command -v bluetoothctl >/dev/null; then
    for a in $(timeout 5 bluetoothctl devices Paired 2>/dev/null | awk '/^Device/{print $2}'); do
      printf '$ bluetoothctl info (paired device)\n'; timeout 5 bluetoothctl info "$a" 2>&1 | grep -E 'Name|Icon|Paired|Trusted|Connected|UUID' | head -20
    done
  fi
  if command -v lsusb >/dev/null; then printf '$ lsusb | grep -i -E "bluetooth|wireless"\n'; lsusb 2>&1 | grep -i -E 'bluetooth|wireless|btusb|csr|realtek|intel.*bt' | head; fi
  if command -v pactl >/dev/null; then
    printf '$ pactl list cards (bluez only: profiles)\n'
    pactl list cards 2>/dev/null | awk '/^Card/{c=0} /Name: bluez_card/{c=1} c&&/(Name:|Active Profile|audio-gateway|off:)/' | head -20
    printf '$ call streams (sink inputs / source outputs with api.bluez5.profile)\n'
    pactl list sink-inputs 2>/dev/null | grep -E 'Sink Input #|api.bluez5.profile|api.bluez5.codec|Sink:|Mute:' | head -20
    pactl list source-outputs 2>/dev/null | grep -E 'Source Output #|api.bluez5.profile|api.bluez5.codec|Source:|Mute:' | head -20
  fi
  printf '$ WirePlumber bluez5 settings in user/system fragments\n'
  grep -r -n -E 'bluez5\.(roles|hfphsp-backend|auto-connect|telephony|enable-msbc)' \
    "$HOME/.config/wireplumber" /etc/wireplumber /usr/share/wireplumber/wireplumber.conf.d 2>/dev/null | head -20
  if command -v journalctl >/dev/null; then
    printf '$ journalctl --user -u wireplumber -u pipewire (bluez/hfp/sco lines, last 80)\n'
    journalctl --user -u wireplumber -u pipewire -n 2000 --no-pager 2>/dev/null | grep -i -E 'bluez|hfp|sco|telephony|rfcomm' | tail -80
    printf '$ journalctl -k (Bluetooth kernel lines, last 30; may need the systemd-journal group)\n'
    journalctl -k -n 3000 --no-pager 2>/dev/null | grep -i -E 'bluetooth|btusb|hci[0-9]|firmware' | tail -30
  fi

  bluetooth_details

  sec "Firewall and listening ports"
  local t
  for t in "ufw status" "firewall-cmd --state" "firewall-cmd --list-all" "nft list ruleset"; do
    command -v "${t%% *}" >/dev/null || continue
    printf '$ %s\n' "$t"; timeout 8 $t 2>&1 | head -60; echo "[exit ${PIPESTATUS[0]}] (without root some of these only say 'permission denied': doctor above reads what it can)"
  done
  command -v ss >/dev/null && { echo '$ ss -tuln (ports 1716-1764 and the control port)'
    ss -tulnH 2>/dev/null | awk -v c="$CTRL_PORT" '{n=split($5,a,":"); p=a[n]; if ((p>=1716 && p<=1764) || p==c) print}'; }
  run "firewall help" ctl firewall

  sec "Service log (last 300 lines)"
  if [[ -n $SVC_LOG ]]; then tail -n 300 "$SVC_LOG" 2>&1
  elif [[ -f $ROOT/dist/realdevice/service.log ]]; then echo "(dist/realdevice/service.log)"; tail -n 300 "$ROOT/dist/realdevice/service.log"
  elif command -v journalctl >/dev/null; then journalctl --user -u geekbridge-service -n 300 --no-pager 2>&1
  else echo "no service log found; pass --service-log FILE"; fi

  sec "Phone (adb)"
  local adb="${GEEKBRIDGE_ADB:-$(command -v adb || true)}" a
  if ((!USE_ADB)); then echo "skipped (--no-adb)"
  elif [[ -z $adb ]]; then echo "adb not found; the owner can run it on the PC: scripts/realdevice.sh logs"
  else
    a=("$adb"); [[ -n $SERIAL ]] && a+=(-s "$SERIAL")
    if [[ -z "$(timeout 8 "${a[@]}" devices 2>/dev/null | awk 'NR>1 && $2=="device"')" ]]; then echo "no phone attached over adb"
    else
      p() { timeout 10 "${a[@]}" shell "$@" 2>/dev/null | tr -d '\r'; }
      echo "model: $(p getprop ro.product.model)  android: $(p getprop ro.build.version.release) (API $(p getprop ro.build.version.sdk))  build: $(p getprop ro.build.display.id)"
      p dumpsys package $PKG | grep -E 'versionName|versionCode|lastUpdateTime|signatures' | sed 's/^ */  /'
      local apkpath; apkpath="$(p pm path $PKG | head -1 | sed 's/^package://')"
      if [[ -n $apkpath ]]; then
        timeout 120 "${a[@]}" pull "$apkpath" "$WORK/installed.apk" >/dev/null 2>&1 && echo "HASH $(hash_of "$WORK/installed.apk")  installed APK on the phone ($(stat -c %s "$WORK/installed.apk") bytes)"
      else echo "app $PKG is not installed"; fi
      echo "battery exemption (deviceidle whitelist): $(p dumpsys deviceidle whitelist | grep -c "$PKG") entries"
      echo "standby bucket: $(p am get-standby-bucket $PKG)   doze deep: $(p dumpsys deviceidle get deep)   light: $(p dumpsys deviceidle get light)"
      echo "service: $(p dumpsys activity services $PKG | grep -c ServiceRecord) ServiceRecord(s) for the app"
    fi
  fi

  sec "APK on the served page and local files"
  [[ -n $APKFILE ]] && echo "HASH $(hash_of "$APKFILE")  $APKFILE (--apk)"
  local ap
  for ap in "$ROOT"/android/app/build/outputs/apk/*/*.apk "$ROOT"/dist/*.apk; do
    [[ -f $ap ]] && echo "HASH $(hash_of "$ap")  $ap ($(stat -c %s "$ap") bytes, built $(date -r "$ap" '+%F %T'))"
  done
  if [[ -n $PAGE ]]; then
    echo "page: $PAGE"
    if page="$(curl -fsSL --max-time 15 "$PAGE" 2>&1)"; then
      echo "HASH $(printf '%s' "$page" | sha256sum | cut -d' ' -f1)  (page HTML)"
      links="$(printf '%s' "$page" | grep -oE "(href|src)=[\"'][^\"']+\.apk[\"']" | sed -E "s/^[a-z]+=[\"']//; s/[\"']$//" | sort -u)"
      [[ -z $links ]] && echo "no .apk link found on the page"
      for l in $links; do
        case "$l" in http*) u="$l" ;; /*) u="$(printf '%s' "$PAGE" | sed -E 's#^(https?://[^/]+).*#\1#')$l" ;; *) u="${PAGE%/*}/$l" ;; esac
        if curl -fsSL --max-time 180 --max-filesize 209715200 -o "$WORK/page.apk" "$u" 2>/dev/null; then
          echo "HASH $(hash_of "$WORK/page.apk")  served APK $u ($(stat -c %s "$WORK/page.apk") bytes)"
          [[ -f $WORK/installed.apk ]] && { [[ "$(hash_of "$WORK/page.apk")" == "$(hash_of "$WORK/installed.apk")" ]] && echo "  same as the APK installed on the phone" || echo "  DIFFERENT from the APK installed on the phone"; }
        else echo "could not download $u"; fi
      done
    else echo "could not fetch the page: $page"; fi
  else echo "no --page URL given (pass the page the phone downloads the APK from to compare it with the installed one)"; fi
}

((BT_TRACE > 0)) && trace "$BT_TRACE"
{ collect; } >"$RAW" 2>&1
"${REDACT[@]}" <"$RAW" >"$WORK/report.txt"
if ((TO_STDOUT)); then cat "$WORK/report.txt"; exit 0; fi
FINAL="$OUTDIR/geekbridge-report-$TS.txt"
cp "$WORK/report.txt" "$FINAL" && chmod 600 "$FINAL"
cp "$FINAL" "$WORK/geekbridge-report-$TS.txt" 2>/dev/null
tar -C "$OUTDIR" -czf "$FINAL.tar.gz" "geekbridge-report-$TS.txt" && chmod 600 "$FINAL.tar.gz"
printf 'Report written (redacted, %s lines):\n  %s\n  %s\nRead it once before attaching it anywhere public.\n' "$(wc -l <"$FINAL")" "$FINAL" "$FINAL.tar.gz"
