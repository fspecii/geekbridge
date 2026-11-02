#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Assemble "GeekBridge Calls.app" around an already-built geekbridge-macos-calls executable.
#
#   assemble-app.sh --binary PATH --out DIR [--bundle-id ID] [--version X.Y.Z] [--build N]
#                   [--adhoc-sign]
#
# * Fills Info.plist.template and lays out Contents/{Info.plist,MacOS/<exe>}.
# * Refuses a binary that is not a Mach-O file (guards against bundling a Linux build).
# * --adhoc-sign runs `codesign --sign -` with the Hardened Runtime and the Developer ID
#   entitlements. That is an ad hoc DEVELOPMENT signature only: it is not Developer ID signing,
#   not notarization, and carries no distribution claim. Only available on macOS.
# Nothing here has been run on a Mac.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="" out="" bundle_id="dev.geekbridge.calls.macos" version="0.1.0" build="1" sign=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) bin="$2"; shift 2;;
    --out) out="$2"; shift 2;;
    --bundle-id) bundle_id="$2"; shift 2;;
    --version) version="$2"; shift 2;;
    --build) build="$2"; shift 2;;
    --adhoc-sign) sign=1; shift;;
    -h|--help) sed -n '3,16p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
[[ -n "$bin" && -n "$out" ]] || { echo "--binary and --out are required" >&2; exit 64; }
[[ -f "$bin" ]] || { echo "binary not found: $bin" >&2; exit 66; }
[[ "$bundle_id" =~ ^[A-Za-z0-9.-]+$ ]] || { echo "invalid bundle id" >&2; exit 64; }
[[ "$version" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || { echo "invalid version" >&2; exit 64; }
[[ "$build" =~ ^[0-9]+$ ]] || { echo "invalid build number" >&2; exit 64; }

magic="$(od -An -tx1 -N4 "$bin" | tr -d ' \n')"
case "$magic" in
  cffaedfe|feedfacf|cafebabe|bebafeca) ;;  # 64-bit Mach-O (either endianness) or universal
  *) echo "not a Mach-O executable (magic $magic): $bin" >&2; exit 65;;
esac

app="$out/GeekBridge Calls.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
sed -e "s|@BUNDLE_ID@|$bundle_id|g" -e "s|@VERSION@|$version|g" -e "s|@BUILD@|$build|g" \
  "$here/Info.plist.template" > "$app/Contents/Info.plist"
install -m 0755 "$bin" "$app/Contents/MacOS/geekbridge-macos-calls"
mkdir -p "$app/Contents/Resources"
install -m 0644 "$here/../../LICENSE" "$here/../../THIRD_PARTY_NOTICES.md" "$app/Contents/Resources/"

if [[ "$(uname -s)" == "Darwin" ]]; then
  plutil -lint "$app/Contents/Info.plist"
  if [[ $sign -eq 1 ]]; then
    codesign --force --sign - --options runtime \
      --entitlements "$here/entitlements.developer-id.plist" "$app"
    echo "ad hoc signed (development only; not Developer ID, not notarized)"
  fi
else
  [[ $sign -eq 0 ]] || { echo "--adhoc-sign needs macOS codesign" >&2; exit 69; }
  echo "assembled on a non-macOS host; plutil/codesign were not run"
fi
echo "$app"
