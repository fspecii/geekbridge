#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# EXPERIMENTAL, OPTIONAL. Starts the call-audio helper on a phone from this computer, for "Call audio over Wi-Fi
# (ADB helper)" (docs/design/call-audio-over-wifi.md). Bluetooth stays the primary call-audio path; skip this if it
# works for you. Needs a GeekBridge DEBUG build on the phone and `adb` on this computer.
#
#   scripts/callaudio-enable.sh                    start the helper on the only connected phone (USB or Wi-Fi adb)
#   scripts/callaudio-enable.sh --serial SERIAL    pick a phone when several are connected
#   scripts/callaudio-enable.sh --pair HOST:PORT CODE   first pair over Wi-Fi (Developer options > Wireless debugging >
#                                                  "Pair device with pairing code"), then continue; add --connect HOST:PORT
#                                                  (the port shown on the Wireless debugging screen itself)
#   scripts/callaudio-enable.sh --status           is a helper running? (changes nothing)
#   scripts/callaudio-enable.sh --stop             stop the helper and forget its token
#   scripts/callaudio-enable.sh --yes              do not ask the confirmation question
#
# What it does, in order, and nothing else:
#   1. checks the phone has the GeekBridge debug build (`run-as` works only for debuggable apps),
#   2. stops an older helper, then asks the app (one broadcast only adb may send, permission DUMP) for a loopback port
#      and a one-time token; the app listens on 127.0.0.1 and accepts only a peer that presents the token,
#   3. starts the helper with `app_process` as the shell user (uid 2000), detached, output discarded; it connects OUT
#      to the app (an app cannot connect to a shell socket under SELinux).
# Nothing is installed and no file is written on the phone; the helper opens no listening port.
#
# Exit codes: 0 done, 1 failed (the message says what to do), 2 usage.
set -uo pipefail

PKG=dev.geekbridge.android
HELPER=dev.geekbridge.android.callaudio.CallAudioHelper
RECEIVER="$PKG/.callaudio.CallWifiReceiver"
ACTION_OPEN=dev.geekbridge.callwifi.OPEN
ACTION_STOP=dev.geekbridge.callwifi.STOP

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ADB="${ADB:-}"
if [ -z "$ADB" ]; then
  if command -v adb >/dev/null 2>&1; then ADB=adb
  elif [ -x "$ROOT/.tools/android-sdk/platform-tools/adb" ]; then ADB="$ROOT/.tools/android-sdk/platform-tools/adb"
  else echo "adb not found. Install Android platform-tools (Arch: pacman -S android-tools; Debian: apt install adb)." >&2; exit 1; fi
fi

serial="" pair="" code="" connect="" mode=start yes=0
while [ $# -gt 0 ]; do
  case "$1" in
    --serial) serial="${2:-}"; shift 2 ;;
    --pair) pair="${2:-}"; code="${3:-}"; shift 3 ;;
    --connect) connect="${2:-}"; shift 2 ;;
    --status) mode=status; shift ;;
    --stop) mode=stop; shift ;;
    --yes) yes=1; shift ;;
    -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done

die() { echo "error: $*" >&2; exit 1; }
adbs() { if [ -n "$serial" ]; then "$ADB" -s "$serial" "$@"; else "$ADB" "$@"; fi; }

if [ -n "$pair" ]; then
  [[ "$pair" =~ ^[A-Za-z0-9._:-]+:[0-9]{1,5}$ ]] || die "--pair wants HOST:PORT from the phone's pairing dialog"
  [[ "$code" =~ ^[0-9]{6}$ ]] || die "the pairing code is the 6 digits shown on the phone"
  "$ADB" pair "$pair" "$code" || die "pairing failed; the dialog on the phone must stay open and the code must be fresh"
fi
if [ -n "$connect" ]; then
  [[ "$connect" =~ ^[A-Za-z0-9._:-]+:[0-9]{1,5}$ ]] || die "--connect wants HOST:PORT from the Wireless debugging screen"
  "$ADB" connect "$connect" || die "could not connect to $connect"
  [ -n "$serial" ] || serial="$connect"
fi

n=$("$ADB" devices | awk 'NR>1 && $2=="device"' | wc -l)
if [ -z "$serial" ]; then
  [ "$n" -ge 1 ] || die "no phone connected. USB: enable Developer options > USB debugging and allow this computer. Wi-Fi: use --pair and --connect."
  [ "$n" -le 1 ] || die "several devices are connected; pick one with --serial (adb devices lists them)"
fi
state="$(adbs get-state 2>/dev/null | tr -d '\r')"
[ "$state" = device ] || die "the phone is not ready (adb state: ${state:-none}); unlock it and accept the debugging prompt"

ash() { adbs shell "$@" 2>&1 | tr -d '\r'; }

if ! adbs shell pm path "$PKG" 2>/dev/null | grep -q '^package:'; then
  die "GeekBridge is not installed on this phone"
fi
dbg="$(ash "run-as $PKG id -u")"
case "$dbg" in
  *"not debuggable"*|*"Unknown package"*|*"package not found"*)
    die "this GeekBridge is a release build; the Wi-Fi call audio helper exists only in debug builds (scripts/build-android.sh builds one)" ;;
esac

running() { ash "ps -A -o PID,ARGS" | grep -F "$HELPER" | grep -v grep | awk '{print $1}'; }

case "$mode" in
  status)
    pids="$(running)"
    if [ -n "$pids" ]; then echo "helper running (pid $(echo "$pids" | tr '\n' ' '))"; else echo "helper not running"; fi
    exit 0 ;;
  stop)
    for p in $(running); do ash "kill $p" >/dev/null; done
    ash "am broadcast -n $RECEIVER -a $ACTION_STOP" | grep -E 'result=' || true
    sleep 1
    [ -z "$(running)" ] && echo "helper stopped" || die "the helper is still running; reboot the phone or run: adb shell kill <pid>"
    exit 0 ;;
esac

if [ "$yes" != 1 ]; then
  cat <<'TXT'
This starts a small helper on your phone that runs with the powers of Android's "shell" user (the same as `adb shell`).
It lets GeekBridge read the voice of the OTHER person in a phone call and send it to your computer, but only
while you press "Listen here" on a call, and only with every switch on. Nothing is recorded or stored.
Recording or sharing calls may be illegal where you live without the other person's consent.
Stop it any time with: scripts/callaudio-enable.sh --stop   (rebooting the phone also ends it)
TXT
  printf 'Start the helper now? [y/N] '
  read -r answer
  case "$answer" in y|Y|yes|YES) ;; *) echo "not started"; exit 0 ;; esac
fi

# Replace an older helper so two never run.
for p in $(running); do ash "kill $p" >/dev/null; done

hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }
uid="$(ash "pm list packages -U $PKG" | sed -n "s/.*uid:\([0-9][0-9]*\).*/\1/p" | head -1)"
[[ "$uid" =~ ^[0-9]+$ ]] || die "could not read the app's uid"

# 1. Ask the app where the helper should connect. An app cannot connect to a shell-owned socket (SELinux), so the app
#    listens on 127.0.0.1 and the helper connects out. The app makes the one-time token; only adb may ask (DUMP).
out="$(ash "am broadcast -n $RECEIVER -a $ACTION_OPEN")"
case "$out" in
  *"result=1"*) ;;
  *"result=2"*) die "the app refused: turn the Experimental switch on first (GeekBridge > Settings > Experimental), then run this again" ;;
  *) die "the GeekBridge app did not answer (${out##*$'\n'}). Is this the debug build, and is it up to date?" ;;
esac
port="$(printf '%s' "$out" | sed -n 's/.*port=\([0-9][0-9]*\).*/\1/p' | head -1)"
token="$(printf '%s' "$out" | sed -n 's/.*token=\([A-Za-z0-9_-]*\).*/\1/p' | head -1)"
[[ "$port" =~ ^[0-9]+$ && -n "$token" ]] || die "the app gave no port or token"

# 2. Start the helper, detached: `setsid nohup ... &` keeps it alive after adb disconnects. Output discarded.
adbs shell "CLASSPATH=\$(pm path $PKG | sed s/^package://) setsid nohup app_process /system/bin $HELPER --connect $port --token $token --app-uid $uid >/dev/null 2>&1 &" >/dev/null 2>&1
ok=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  sleep 0.5
  [ -n "$(running)" ] && { ok=1; break; }
done
[ "$ok" = 1 ] || die "the helper did not start (rebuild the APK with scripts/build-android.sh and reinstall it)"
echo "helper started and connecting to GeekBridge. Now open GeekBridge > Settings > Experimental > Call audio over Wi-Fi and run the self-test."
