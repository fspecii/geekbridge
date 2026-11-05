#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# GeekBridge and the STOCK KDE Connect Android app on one emulator (docs/coexistence.md, part 3).
#
#   scripts/e2e/coexist-android.sh GEEKBRIDGE_APK STOCK_APK OUTDIR [--keep-avd]
#
# Own AVD clone (gbcoex, console port 5634), own adb server (16058). The stock APK is untrusted: it is only
# ever installed into the emulator, never opened or executed on the host. Results: OUTDIR/results.txt
set -uo pipefail
GBAPK=$(realpath "${1:?usage: $0 GEEKBRIDGE_APK STOCK_APK OUTDIR}"); KCAPK=$(realpath "${2:?}"); OUT=$(realpath -m "${3:?}")
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"; cd "$ROOT"
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1
mkdir -p "$OUT"
export ANDROID_ADB_SERVER_PORT=16058
AVD=gbcoex PORT=5634 SERIAL=emulator-5634
ADB="adb -s $SERIAL"; export ADB
GB=dev.geekbridge.android KC=org.kde.kdeconnect_tp
UI="python3 -I $ROOT/scripts/e2e/adbui.py"
R=$OUT/results.txt; : > "$R"
say() { echo "$*" | tee -a "$R"; }
check() { local n=$1; shift; if "$@" >/dev/null 2>&1; then say "PASS  $n"; else say "FAIL  $n"; fi; }
shot() { $ADB exec-out screencap -p > "$OUT/$1.png" 2>/dev/null; }

start_emulator() {
    [ "$($ADB get-state 2>/dev/null)" = device ] && { REUSED=1; return 0; }
    if [ ! -d "$ANDROID_AVD_HOME/$AVD.avd" ]; then
        cp -a --reflink=auto "$ANDROID_AVD_HOME/gb36.avd" "$ANDROID_AVD_HOME/$AVD.avd"
        rm -f "$ANDROID_AVD_HOME/$AVD.avd"/*.lock; rm -rf "$ANDROID_AVD_HOME/$AVD.avd/snapshots"
        sed -e "s|^path=.*|path=$ANDROID_AVD_HOME/$AVD.avd|" -e "s|^path.rel=.*|path.rel=avd/$AVD.avd|" \
            "$ANDROID_AVD_HOME/gb36.ini" > "$ANDROID_AVD_HOME/$AVD.ini"
        sed -i 's/^AvdId=.*/AvdId=gbcoex/;s/^avd.ini.displayname=.*/avd.ini.displayname=gbcoex/' "$ANDROID_AVD_HOME/$AVD.avd/config.ini"
    fi
    mkdir -p "$OUT/emu-tmp"
    (GB_EMU_AVD=$AVD GB_EMU_PORT=$PORT GB_EMU_ADB_PORT=$ANDROID_ADB_SERVER_PORT GB_EMU_TMPDIR="$OUT/emu-tmp" \
        GB_EMU_EXTRA="-wipe-data" setsid nohup scripts/emulator-call-launch.sh > "$OUT/emulator.log" 2>&1 &)
    local i; for i in $(seq 300); do
        [ "$($ADB shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] && return 0; sleep 1
    done; return 1
}
stop_emulator() { $ADB emu kill >/dev/null 2>&1; sleep 3; adb kill-server >/dev/null 2>&1; }
REUSED=0
cleanup_emu() { [ "${4:-}" = --keep-avd ] || [ "$REUSED" = 1 ] || stop_emulator; }
trap cleanup_emu EXIT

# /proc/net listeners as "proto port uid" (hex port decoded). shell may read /proc/net on this image.
listeners() {
    $ADB shell 'for f in tcp tcp6 udp udp6; do awk -v p=$f "NR>1 {split(\$2,a,\":\"); print p, a[2], \$4, \$8}" /proc/net/$f; done' 2>/dev/null |
        tr -d '\r' | while read -r proto hexport st uid; do
            port=$((16#$hexport)); [ "$port" -ge 1716 ] && [ "$port" -le 1764 ] || continue
            case $proto in tcp*) [ "$st" = 0A ] || continue ;; esac
            echo "$proto $port $uid"
        done | sort -u
}
uid_of() { $ADB shell pm list packages -U "$1" | tr -d '\r' | sed -n "s/^package:$1 uid:\([0-9]*\)$/\1/p"; }
crashed() { $ADB logcat -b crash -d 2>/dev/null | grep -E "Process: ($GB|$KC)|FATAL EXCEPTION" | head -3; }
launch() { $ADB shell monkey -p "$1" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1; sleep 4; }

say "== A0: emulator + both APKs"
start_emulator && say "PASS  emulator booted" || { say "FAIL  emulator did not boot"; exit 1; }
$ADB shell svc power stayon true; $ADB shell input keyevent KEYCODE_WAKEUP; $ADB shell wm dismiss-keyguard >/dev/null 2>&1
check "install GeekBridge" bash -c "$ADB install -r '$GBAPK' 2>&1 | grep -q Success"
check "install stock KDE Connect (emulator only)" bash -c "$ADB install -r -g '$KCAPK' 2>&1 | grep -q Success"
GBUID=$(uid_of $GB); KCUID=$(uid_of $KC); say "uids: geekbridge=$GBUID stock=$KCUID"
$ADB logcat -b crash -c; $ADB logcat -c

walk_wizard() { # GeekBridge's first-run wizard: Skip/Next through every step, Overview, Finish
    local i; for i in $(seq 20); do
        $UI has "Step " >/dev/null 2>&1 || break
        $UI tap Allow >/dev/null 2>&1
        $UI tap Overview >/dev/null 2>&1 || $UI tap Skip >/dev/null 2>&1 || $UI tap Next >/dev/null 2>&1 || $UI tap Finish >/dev/null 2>&1
        $UI tap Finish >/dev/null 2>&1; sleep 1.5
    done
}
# "Look for computers" starts the link with UDP discovery listening (shared 1716) and the TCP listener.
start_gb_link() {
    $ADB shell am force-stop $GB; sleep 1
    $ADB shell am start -f 0x04000000 -n "$GB/.HomeActivity" >/dev/null; sleep 3
    $UI tap "Pair a computer" >/dev/null 2>&1; sleep 2
    $UI tap "Other ways to link" >/dev/null 2>&1; sleep 1
    $UI tap "Look for computers" >/dev/null 2>&1; sleep 5
}
# Diagnostics > "Start link": the foreground-service link survives leaving the app (TCP listener only; the
# "Look for computers" link, which also listens on UDP, is stopped when GeekBridge leaves the foreground).
start_gb_service() {
    $ADB shell am force-stop $GB; sleep 1
    $ADB shell am start -f 0x04000000 -n "$GB/.HomeActivity" >/dev/null; sleep 3
    $UI scrolltap "Diagnostics & about" >/dev/null 2>&1; sleep 2
    local which; which=$($UI scrollto "Start link" "Stop link" 2>/dev/null)
    [ "$which" = "Start link" ] && { $UI tap "Start link" >/dev/null 2>&1; sleep 4; }
}
start_kc() { launch $KC; $UI tap Allow >/dev/null 2>&1; $UI tap "Allow" >/dev/null 2>&1; sleep 2; }

$ADB shell am start -f 0x04000000 -n "$GB/.HomeActivity" >/dev/null; sleep 3; walk_wizard
gb_port() { echo "$1" | sed -n "s/^tcp[0-9]* \\(17[0-9][0-9]\\) $GBUID\$/\\1/p" | head -1; }
shares_udp() { echo "$1" | grep -q "^udp[0-9]* 1716 $GBUID\$" && echo "$1" | grep -q "^udp[0-9]* 1716 $KCUID\$"; }
nocrash() { [ -z "$(crashed)" ]; }

say "== A1: stock KDE Connect app first, then GeekBridge"
start_kc; shot a1-stock
L0=$(listeners); say "listeners after stock start: $(echo "$L0" | tr '\n' ';')"
check "A1 stock app listens on TCP 1716 and UDP 1716" bash -c "echo '$L0' | grep -q '^tcp[0-9]* 1716 $KCUID\$' && echo '$L0' | grep -q '^udp[0-9]* 1716 $KCUID\$'"
start_gb_link; shot a1-geekbridge
L=$(listeners); say "listeners with both: $(echo "$L" | tr '\n' ';')"
P=$(gb_port "$L"); say "GeekBridge TCP port: ${P:-none}"
check "A1 GeekBridge TCP listener fell back to a port in 1717-1764" test -n "$P"
check "A1 GeekBridge UDP discovery shares 1716 with the stock app (both uids bound)" shares_udp "$L"
check "A1 stock app still listens on TCP 1716" bash -c "echo '$L' | grep -q '^tcp[0-9]* 1716 $KCUID\$'"
check "A1 GeekBridge lists the stock app's device (heard its broadcast on the shared port)" bash -c "$UI dump | grep -q emulator"
check "A1 no crash in either app" nocrash

say "== A2: another program (a shell-owned socket) holds TCP 1716, then GeekBridge starts"
# GeekBridge stops its link when it leaves the foreground with nothing paired, so "GeekBridge first, then the stock
# app" cannot be held on a phone; the stock-first order is A1 and the other order is covered on the desktop (coexist.sh P2).
$ADB shell am force-stop $GB; $ADB shell am force-stop $KC; sleep 2
($ADB shell "toybox nc -s 0.0.0.0 -p 1716 -L sleep 600" >/dev/null 2>&1 &); sleep 2
L1=$(listeners); say "listeners with the squatter: $(echo "$L1" | tr '\n' ';')"
check "A2 squatter holds TCP 1716" bash -c "echo '$L1' | grep -q '^tcp[0-9]* 1716 '"
start_gb_link; L2=$(listeners); say "listeners after GeekBridge: $(echo "$L2" | tr '\n' ';')"
P2=$(gb_port "$L2"); say "GeekBridge TCP port: ${P2:-none}"
check "A2 GeekBridge starts anyway and listens on a fallback port" test -n "$P2"
check "A2 GeekBridge UDP discovery bound on 1716" bash -c "echo '$L2' | grep -q '^udp[0-9]* 1716 $GBUID\$'"
check "A2 no crash" nocrash
$ADB shell "pkill -f 'nc -s 0.0.0.0'" >/dev/null 2>&1
shot a2-squatter

say "== A3: entry points do not collide"
$ADB shell dumpsys package $GB > "$OUT/dumpsys-geekbridge.txt" 2>&1; $ADB shell dumpsys package $KC > "$OUT/dumpsys-stock.txt" 2>&1
$ADB shell cmd package query-activities --brief -a android.intent.action.SEND -t text/plain > "$OUT/send-targets.txt" 2>&1
$ADB shell cmd package query-services --brief -a android.service.quicksettings.action.QS_TILE > "$OUT/qs-tiles.txt" 2>&1
check "A3 package names differ" test "$GB" != "$KC"
check "A3 both are SEND share targets" bash -c "grep -q $GB '$OUT/send-targets.txt' && grep -q $KC '$OUT/send-targets.txt'"
say "SEND targets: $(grep -E "$GB|$KC" "$OUT/send-targets.txt" | tr '\n' ' ')"
say "Quick Settings tiles: $(grep -E "$GB|$KC" "$OUT/qs-tiles.txt" | tr '\n' ' ')"
check "A3 tile service class names are package-scoped (no shared component)" bash -c "! grep -q '$GB' <(grep '$KC' '$OUT/qs-tiles.txt')"
for p in $GB $KC; do $ADB shell cmd package resolve-activity --brief -a android.intent.action.SEND -t text/plain $p 2>/dev/null | tail -1 | sed "s/^/chooser entry for $p: /" >> "$R"; done
check "A3 still no crash" bash -c "[ -z \"\$($(declare -f crashed); ADB='$ADB' GB=$GB KC=$KC crashed)\" ]"
$ADB logcat -d -b all 2>/dev/null | grep -E "AndroidRuntime|FATAL" | head -5 > "$OUT/fatal.txt"
say "== done; $(grep -c '^PASS' "$R") passed, $(grep -c '^FAIL' "$R") failed"
