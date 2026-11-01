#!/usr/bin/env bash
# Build GeekBridge.app (entry app + service + apps) and a .dmg. macOS only; written without a Mac.
#
#   packaging/macos/build-app-dmg.sh --bin-dir DIR --out DIR [--version X.Y.Z] [--arch aarch64|x86_64] [--adhoc-sign]
#
# Layout: GeekBridge.app/Contents/MacOS/GeekBridge  (wrapper, see below)
#                                     /MacOS/{geekbridge,geekbridge-service,geekbridge-ctl,geekbridge-calls,
#                                             geekbridge-messages,geekbridge-clipboard,geekbridge-agenda,geekbridge-contacts,geekbridge-share}
# The wrapper is the bundle executable. On every launch it (re)writes the login item
# ~/Library/LaunchAgents/dev.geekbridge.service.plist (RunAtLoad + KeepAlive, pointing at the service inside
# this bundle), loads it with launchctl, then execs the GeekBridge window. So the first launch
# of the app is the first-run setup, and moving the app keeps the login item correct.
# This is not the calling helper bundle (assemble-app.sh) and is not signed, notarized or tested.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
bindir="" out="" version="" arch="$(uname -m)" sign=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir) bindir="$2"; shift 2;;
    --out) out="$2"; shift 2;;
    --version) version="$2"; shift 2;;
    --arch) arch="$2"; shift 2;;
    --adhoc-sign) sign=1; shift;;
    -h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
[[ -n "$bindir" && -n "$out" ]] || { echo "--bin-dir and --out are required" >&2; exit 64; }
[[ -n "$version" ]] || version="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$root/Cargo.toml")"
plist_version="${version%%-*}"
[[ "$plist_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version '$version'" >&2; exit 64; }
[[ "$arch" == aarch64 || "$arch" == x86_64 ]] || { [[ "$arch" == arm64 ]] && arch=aarch64 || { echo "bad arch" >&2; exit 64; }; }
[[ "$(uname -s)" == Darwin ]] || { echo "this script needs macOS (iconutil, hdiutil)" >&2; exit 69; }

bins=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share)
app="$out/GeekBridge.app"; rm -rf "$app"; mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$out"
for b in "${bins[@]}"; do install -m 0755 "$bindir/$b" "$app/Contents/MacOS/$b"; done

cat > "$app/Contents/MacOS/GeekBridge" <<'WRAP'
#!/bin/bash
# Installs/refreshes the login item for the bundled service, then opens the GeekBridge window.
dir="$(cd "$(dirname "$0")" && pwd)"
agents="$HOME/Library/LaunchAgents"
plist="$agents/dev.geekbridge.service.plist"
mkdir -p "$agents"
new="$(cat <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.geekbridge.service</string>
  <key>ProgramArguments</key><array><string>$dir/geekbridge-service</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
</dict></plist>
PL
)"
if [ "$(cat "$plist" 2>/dev/null)" != "$new" ]; then
    printf '%s\n' "$new" > "$plist"
    launchctl bootout "gui/$(id -u)/dev.geekbridge.service" 2>/dev/null
    launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null
fi
exec "$dir/geekbridge" "$@"
WRAP
chmod 0755 "$app/Contents/MacOS/GeekBridge"

# Icon: assets/icon.svg -> icns (needs rsvg-convert or qlmanage; falls back to no icon).
if command -v rsvg-convert >/dev/null; then
  set_dir="$(mktemp -d)/GeekBridge.iconset"; mkdir -p "$set_dir"
  for s in 16 32 128 256 512; do
    rsvg-convert -w $s -h $s "$root/assets/icon.svg" -o "$set_dir/icon_${s}x${s}.png"
    rsvg-convert -w $((s*2)) -h $((s*2)) "$root/assets/icon.svg" -o "$set_dir/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$set_dir" -o "$app/Contents/Resources/GeekBridge.icns"
else
  echo "warning: rsvg-convert not found (brew install librsvg); the app will have no icon" >&2
fi

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>GeekBridge</string>
  <key>CFBundleDisplayName</key><string>GeekBridge</string>
  <key>CFBundleIdentifier</key><string>dev.geekbridge.app</string>
  <key>CFBundleExecutable</key><string>GeekBridge</string>
  <key>CFBundleIconFile</key><string>GeekBridge</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$plist_version</string>
  <key>CFBundleVersion</key><string>$plist_version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
plutil -lint "$app/Contents/Info.plist"
[[ $sign -eq 1 ]] && codesign --force --deep --sign - "$app"

dmg="$out/geekbridge-$version-macos-$arch.dmg"
stage="$(mktemp -d)"; cp -R "$app" "$stage/"; ln -s /Applications "$stage/Applications"
rm -f "$dmg"; hdiutil create -volname "GeekBridge" -srcfolder "$stage" -ov -format UDZO "$dmg"
echo "==> $dmg"
