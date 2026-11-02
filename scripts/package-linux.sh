#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the Linux installers from already-built release binaries.
#
#   scripts/package-linux.sh [--bin-dir DIR] [--out DIR] [--version X.Y.Z]
#
# Produces in --out (default dist/):
#   geekbridge-<version>-linux-x86_64-installer.tar.gz   extract, run ./install.sh (per-user) or sudo ./install.sh --system
#   geekbridge_<version>_amd64.deb                       system install; postinst enables the service for every user
# --bin-dir defaults to target/release (needs geekbridge, geekbridge-service, geekbridge-ctl and the
# five app executables). Icons are rendered from assets/icon.svg with rsvg-convert.
# Uses only ar/tar/gzip/xz/rsvg-convert, no dpkg tooling, so it runs on any Linux CI runner.
# The tarball name deliberately differs from geekbridge-linux-x86_64.tar.gz, which the in-app
# update checker reads.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
bindir="$ROOT/target/release" out="$ROOT/dist" version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir) bindir="$2"; shift 2;;
    --out) out="$2"; shift 2;;
    --version) version="$2"; shift 2;;
    -h|--help) sed -n '3,14p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
mkdir -p "$out"; out="$(cd "$out" && pwd -P)"   # absolute: the .deb is assembled from another directory
[[ -n "$version" ]] || version="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT/Cargo.toml")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "bad version '$version'" >&2; exit 64; }
debversion="$(printf '%s' "$version" | sed 's/-/~/')"   # not ${version/-/~}: bash expands that ~ to $HOME
bins=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share)
for b in "${bins[@]}"; do [[ -x "$bindir/$b" ]] || { echo "missing binary: $bindir/$b" >&2; exit 66; }; done
command -v rsvg-convert >/dev/null || { echo "rsvg-convert is required (librsvg)" >&2; exit 69; }

source "$ROOT/scripts/lib-tmp.sh"
work="$(gb_mktemp package -d)"; trap 'rm -rf "$work"' EXIT
pk="$ROOT/packaging/linux"
desktops=(dev.geekbridge.GeekBridge dev.geekbridge.Calls dev.geekbridge.Messages dev.geekbridge.Agenda dev.geekbridge.Contacts dev.geekbridge.Clipboard dev.geekbridge.Share dev.geekbridge.PasteFromPhone)

# ---- shared payload (installer tarball layout: bin/ share/)
pay="$work/payload"; mkdir -p "$pay/bin" "$pay/share/applications" "$pay/share/icons/hicolor/scalable/apps"
for b in "${bins[@]}"; do install -m 0755 "$bindir/$b" "$pay/bin/$b"; done
install -m 0755 "$pk/geekbridge-service-launch" "$pay/bin/"
for d in "${desktops[@]}"; do install -m 0644 "$pk/$d.desktop" "$pay/share/applications/"; done
install -m 0644 "$pk/geekbridge-service.service" "$pk/geekbridge-service.desktop" "$pay/share/"
install -m 0644 "$ROOT/assets/icon.svg" "$pay/share/icons/hicolor/scalable/apps/geekbridge.svg"
for s in 48 128 256; do
  mkdir -p "$pay/share/icons/hicolor/${s}x${s}/apps"
  rsvg-convert -w "$s" -h "$s" "$ROOT/assets/icon.svg" -o "$pay/share/icons/hicolor/${s}x${s}/apps/geekbridge.png"
done
mkdir -p "$out"

# ---- installer tarball
t="$work/geekbridge-$version"; mkdir -p "$t"
cp -a "$pay/." "$t/"; install -m 0755 "$pk/install.sh" "$t/install.sh"
for doc in README.md LICENSE THIRD_PARTY_NOTICES.md; do cp "$ROOT/$doc" "$t/"; done   # GPL-3.0-or-later: the licence text travels with the binaries
tar --owner=0 --group=0 --numeric-owner -czf "$out/geekbridge-$version-linux-x86_64-installer.tar.gz" -C "$work" "geekbridge-$version"

# ---- .deb (data laid out for /usr)
root="$work/deb"; mkdir -p "$root/usr/bin" "$root/usr/share" "$root/usr/lib/systemd/user" "$root/etc/xdg/autostart" "$root/usr/share/doc/geekbridge"
for b in "${bins[@]}"; do install -m 0755 "$pay/bin/$b" "$root/usr/bin/$b"; done
install -m 0755 "$pk/geekbridge-service-launch" "$root/usr/bin/"
cp -a "$pay/share/applications" "$pay/share/icons" "$root/usr/share/"
# Per-app icon names used by the .desktop files: one copy of the same art per name.
for n in "${desktops[@]}"; do
  install -Dm 0644 "$pay/share/icons/hicolor/scalable/apps/geekbridge.svg" "$root/usr/share/icons/hicolor/scalable/apps/$n.svg"
  for s in 48 128 256; do install -Dm 0644 "$pay/share/icons/hicolor/${s}x${s}/apps/geekbridge.png" "$root/usr/share/icons/hicolor/${s}x${s}/apps/$n.png"; done
done
rm -f "$root/usr/share/icons/hicolor/scalable/apps/geekbridge.svg" "$root"/usr/share/icons/hicolor/*/apps/geekbridge.png
sed "s|@BINDIR@|/usr/bin|" "$pk/geekbridge-service.service" > "$root/usr/lib/systemd/user/geekbridge-service.service"
chmod 0644 "$root/usr/lib/systemd/user/geekbridge-service.service"
install -m 0644 "$pk/geekbridge-service.desktop" "$root/etc/xdg/autostart/"
[[ -f "$ROOT/README.md" ]] && install -m 0644 "$ROOT/README.md" "$root/usr/share/doc/geekbridge/README.md"
install -m 0644 "$pk/copyright" "$root/usr/share/doc/geekbridge/copyright"
install -m 0644 "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$root/usr/share/doc/geekbridge/"

size_kb=$(du -sk "$root" | cut -f1)
ctl="$work/control"; mkdir -p "$ctl"
cat > "$ctl/control" <<EOF
Package: geekbridge
Version: $debversion
Architecture: amd64
Maintainer: GeekBridge <noreply@users.noreply.github.com>
Installed-Size: $size_kb
Depends: libc6, libgcc-s1, libxkbcommon0, libwayland-client0, libgl1, libx11-6, libxcb1
Recommends: wl-clipboard | xclip, xdg-utils, zenity | kdialog
Section: net
Priority: optional
Homepage: https://github.com/fspecii/geekbridge
Description: Bridge between your computer and your Android phone
 Pair a phone, sync the clipboard, send files and links. Includes the
 background service (started at login), a control utility and the
 GeekBridge, Calls, Messages, Agenda, Clipboard and Share windows.
EOF
cat > "$ctl/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    # Enable the unit for every user's systemd user manager; the XDG autostart entry covers the rest.
    if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
        systemctl --global enable geekbridge-service.service >/dev/null 2>&1 || true
    fi
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t /usr/share/icons/hicolor 2>/dev/null || true
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications 2>/dev/null || true
fi
exit 0
EOF
cat > "$ctl/prerm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = remove ] || [ "$1" = purge ]; then
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --global disable geekbridge-service.service >/dev/null 2>&1 || true
    fi
fi
exit 0
EOF
chmod 0755 "$ctl/postinst" "$ctl/prerm"
(cd "$root" && find . -type f -exec md5sum {} + | sed 's| \./| |' | sort -k2) > "$ctl/md5sums"
echo 2.0 > "$work/debian-binary"
tar_opts=(--owner=0 --group=0 --numeric-owner --sort=name)
(cd "$ctl" && tar "${tar_opts[@]}" -cJf "$work/control.tar.xz" ./control ./md5sums ./postinst ./prerm)
(cd "$root" && tar "${tar_opts[@]}" -cJf "$work/data.tar.xz" .)
deb="$out/geekbridge_${debversion}_amd64.deb"
rm -f "$deb"
(cd "$work" && ar rc "$deb" debian-binary control.tar.xz data.tar.xz)
echo "==> $out/geekbridge-$version-linux-x86_64-installer.tar.gz"
echo "==> $deb"
