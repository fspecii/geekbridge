#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Fails if a release APK carries any debug-build extra: debug components, extra permissions, cleartext
# traffic, the debug-only classes or the debug log tag. Usage: scripts/check-release-isolation.sh [APK]
# (default android/app/build/outputs/apk/release/app-release-unsigned.apk). Needs aapt2 and unzip (build-tools).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
apk="${1:-$here/android/app/build/outputs/apk/release/app-release-unsigned.apk}"
[ -f "$apk" ] || { echo "no APK at $apk" >&2; exit 2; }
# `|| true`: with pipefail a missing build-tools directory (ls fails) would otherwise abort silently, before the message below.
aapt2="${AAPT2:-$(ls -d "${ANDROID_HOME:-$here/.tools/android-sdk}"/build-tools/*/aapt2 2>/dev/null | sort -V | tail -1 || true)}"
[ -x "$aapt2" ] || { echo "aapt2 not found (set AAPT2)" >&2; exit 2; }
for tool in unzip strings; do
  command -v "$tool" >/dev/null || { echo "$tool not found (unzip and binutils are required)" >&2; exit 2; }
done
work="$(mktemp -d "${TMPDIR:-/tmp}/gb-isolation.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }

manifest="$("$aapt2" dump xmltree --file AndroidManifest.xml "$apk")"
printf '%s\n' "$manifest" | grep -c 'debuggable' >/dev/null && bad "android:debuggable is present"
printf '%s\n' "$manifest" | grep -cE 'usesCleartextTraffic|networkSecurityConfig' >/dev/null && bad "cleartext or a network security config is declared"
for name in DebugControlReceiver ScaleSeedReceiver DebugBootProvider DesignGalleryActivity DebugPinWidgetActivity CallWifiActivity CallWifiReceiver CallWifiStopReceiver; do
  printf '%s\n' "$manifest" | grep -c "$name" >/dev/null && bad "manifest declares $name"
done
perms="$("$aapt2" dump permissions "$apk")"
for p in WRITE_CONTACTS READ_LOGS DUMP; do
  printf '%s\n' "$perms" | grep -c "android.permission.$p'" >/dev/null && bad "release requests $p"
done

unzip -q -o "$apk" 'classes*.dex' -d "$work"
for dex in "$work"/classes*.dex; do
  if strings -n 4 "$dex" | grep -cE 'DebugDiagnostics|DebugScroll|DebugLogStreamer|DebugRedactor|DebugControlReceiver|DebugNetHold|DebugBootProvider|ScaleSeedReceiver|DesignGallery|DebugStateCollector|CallAudioHelper|CallAudioProbe|HelperSession|HelperConnection|CallWifi|SelfTestVerdict|UplinkTest|UplinkRoutes|PcmInjector|TrackInjector|tx_interception|tx_music|getCallUplinkInjectionAudioTrack|dev/geekbridge/android/callaudio/|dev\.geekbridge\.callwifi|geekbridge\.callaudio|geekbridge-debug-|geekbridge\.debug\.(log|state)|dev\.geekbridge\.debug\.' >/dev/null; then
    bad "$(basename "$dex") contains debug-only code or packet names"
  fi
done
# The native library must not carry the experimental call audio hub either (the release build omits the `callwifi` feature).
unzip -q -o "$apk" 'lib/*/libgeekbridge_android.so' -d "$work" 2>/dev/null || true
for so in "$work"/lib/*/libgeekbridge_android.so; do
  [ -f "$so" ] || continue
  if strings -n 8 "$so" | grep -cE 'geekbridge\.callaudio|transport_callwifi' >/dev/null; then
    bad "$(basename "$(dirname "$so")")/libgeekbridge_android.so contains the experimental call audio hub"
  fi
done
[ "$fail" = 0 ] && echo "ok: $apk has no debug diagnostics, receivers, cleartext or debug permissions"
exit "$fail"
