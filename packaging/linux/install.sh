#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install GeekBridge from the extracted installer tarball.
#
#   ./install.sh                 per-user install into ~/.local (no root); enables the service at login
#   sudo ./install.sh --system   install into /usr/local for all users
#   ./install.sh --prefix DIR    custom prefix (no service enabling)
#   ./install.sh --uninstall     remove what this script installed (same mode flags); keeps pairing data
#   ./install.sh --uninstall --purge-data
#                                also delete the per-user state directory (pairings, identity, settings)
#
# The service is enabled through a systemd user unit when systemd is available, and always via an
# XDG autostart entry (which starts the same unit, or the binary directly without systemd).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mode=user prefix="" uninstall=0 purge=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --system) mode=system; shift;;
    --prefix) prefix="${2:?--prefix needs a directory}"; mode=custom; shift 2;;
    --uninstall) uninstall=1; shift;;
    --purge-data) purge=1; shift;;
    -h|--help) sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
if [[ $purge -eq 1 && $uninstall -ne 1 ]]; then echo "--purge-data only makes sense with --uninstall" >&2; exit 64; fi
# The service keeps pairings, identity and settings under $XDG_DATA_HOME/geekbridge; the updater keeps its
# notes under $XDG_STATE_HOME/geekbridge.
statedir="${XDG_DATA_HOME:-$HOME/.local/share}/geekbridge"
updatedir="${XDG_STATE_HOME:-$HOME/.local/state}/geekbridge"
if [[ $mode == custom && ( "$prefix" != /* || "$prefix" == / ) ]]; then
  echo "--prefix must be an absolute directory other than / (got '$prefix')" >&2; exit 64
fi
case "$mode" in
  user) prefix="${HOME}/.local"; unitdir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart";;
  system) prefix=/usr/local; unitdir=/usr/lib/systemd/user; autostart=/etc/xdg/autostart;;
  custom) unitdir="$prefix/lib/systemd/user"; autostart="$prefix/etc/xdg/autostart";;
esac
bindir="$prefix/bin" docdir="$prefix/share/doc/geekbridge" appdir="$prefix/share/applications" icondir="$prefix/share/icons/hicolor"
bins=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share geekbridge-service-launch)
desktops=(dev.geekbridge.GeekBridge dev.geekbridge.Calls dev.geekbridge.Messages dev.geekbridge.Agenda dev.geekbridge.Contacts dev.geekbridge.Clipboard dev.geekbridge.Share dev.geekbridge.PasteFromPhone)
iconnames=(dev.geekbridge.GeekBridge dev.geekbridge.Calls dev.geekbridge.Messages dev.geekbridge.Agenda dev.geekbridge.Contacts dev.geekbridge.Clipboard dev.geekbridge.Share dev.geekbridge.PasteFromPhone)

user_systemd() { [[ $mode == user ]] && command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; }

if [[ $uninstall -eq 1 ]]; then
  if user_systemd; then systemctl --user disable --now geekbridge-service.service 2>/dev/null || true; fi
  for b in "${bins[@]}"; do rm -f "$bindir/$b"; done
  # Versions kept by `geekbridge-ctl update install` (docs/design/desktop-self-update.md).
  if [[ "$prefix" == /* && "$prefix" != / && -d "$prefix/lib/geekbridge" ]]; then rm -rf -- "$prefix/lib/geekbridge"; rmdir "$prefix/lib" 2>/dev/null || true; fi
  for d in "${desktops[@]}"; do rm -f "$appdir/$d.desktop"; done
  for n in "${iconnames[@]}"; do
    rm -f "$icondir/scalable/apps/$n.svg"
    for s in 48 128 256; do rm -f "$icondir/${s}x${s}/apps/$n.png"; done
  done
  rm -f "$unitdir/geekbridge-service.service" "$autostart/geekbridge-service.desktop"
  rm -f "$docdir/LICENSE" "$docdir/THIRD_PARTY_NOTICES.md"; rmdir "$docdir" 2>/dev/null || true
  user_systemd && systemctl --user daemon-reload || true
  # Let the desktop forget the removed menu entries and icons now rather than at its next cache rebuild.
  command -v gtk-update-icon-cache >/dev/null && [[ -f "$icondir/icon-theme.cache" ]] && gtk-update-icon-cache -q -t "$icondir" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && [[ -d "$appdir" ]] && update-desktop-database -q "$appdir" 2>/dev/null || true
  # Same for the icon cache when no icon is left in the theme directory.
  if [[ -f "$icondir/icon-theme.cache" && -z "$(find "$icondir" -type f ! -name icon-theme.cache -print -quit)" ]]; then rm -f "$icondir/icon-theme.cache"; fi
  # The tool leaves a header-only index behind when the last entry goes; that file is ours only if it is that empty.
  if [[ -f "$appdir/mimeinfo.cache" && "$(tr -d '\n' <"$appdir/mimeinfo.cache")" == "[MIME Cache]" ]]; then rm -f "$appdir/mimeinfo.cache"; fi
  if [[ $purge -eq 1 ]]; then
    # Only ever the one directory this program owns; never an empty or relative path.
    if [[ "$updatedir" == /*/geekbridge && -d "$updatedir" ]]; then rm -rf -- "$updatedir"; fi
    if [[ "$statedir" == /*/geekbridge && -d "$statedir" ]]; then rm -rf -- "$statedir"; echo "Removed. Pairing data and settings in $statedir were deleted (--purge-data)."
    else echo "Removed. No pairing data found in $statedir."; fi
  else
    if [[ -d "$statedir" ]]; then echo "Removed. Pairing data and settings in $statedir were left in place (add --purge-data to delete them)."
    else echo "Removed. No pairing data found in $statedir."; fi
  fi
  # Files you received are yours; the uninstaller never deletes them.
  [[ $mode == user && -d "$HOME/Downloads/GeekBridge" ]] && echo "Files you received stay in $HOME/Downloads/GeekBridge (delete that folder yourself if you want them gone)."
  exit 0
fi

install -d "$bindir" "$appdir" "$unitdir" "$autostart"
for b in "${bins[@]}"; do
  [[ -f "$here/bin/$b" ]] || { echo "missing $here/bin/$b" >&2; exit 66; }
  install -m 0755 "$here/bin/$b" "$bindir/$b"
done
# These files are now the truth: drop the versions `geekbridge-ctl update install` kept (its bin/ links were just replaced).
if [[ "$prefix" == /* && "$prefix" != / && -d "$prefix/lib/geekbridge/versions" ]]; then rm -rf -- "$prefix/lib/geekbridge"; fi
for d in "${desktops[@]}"; do install -m 0644 "$here/share/applications/$d.desktop" "$appdir/$d.desktop"; done
# A per-user install lives in ~/.local/bin, which the desktop's application launcher does not always have on its
# PATH (a fresh ~/.local/bin only joins PATH at the next login), so the menu entries name the programs absolutely.
for d in "${desktops[@]}"; do sed -i "s|^Exec=geekbridge|Exec=$bindir/geekbridge|" "$appdir/$d.desktop"; done
# The licence text and third-party notices, where the About section looks for them (<prefix>/share/doc/geekbridge).
for doc in LICENSE THIRD_PARTY_NOTICES.md; do [[ -f "$here/$doc" ]] && install -Dm 0644 "$here/$doc" "$docdir/$doc"; done
for n in "${iconnames[@]}"; do
  install -Dm 0644 "$here/share/icons/hicolor/scalable/apps/geekbridge.svg" "$icondir/scalable/apps/$n.svg"
  for s in 48 128 256; do install -Dm 0644 "$here/share/icons/hicolor/${s}x${s}/apps/geekbridge.png" "$icondir/${s}x${s}/apps/$n.png"; done
done
sed "s|@BINDIR@|$bindir|" "$here/share/geekbridge-service.service" > "$unitdir/geekbridge-service.service"
chmod 0644 "$unitdir/geekbridge-service.service"
install -m 0644 "$here/share/geekbridge-service.desktop" "$autostart/geekbridge-service.desktop"
# The autostart entry finds the launcher through PATH; make it absolute for per-user installs.
sed -i "s|^Exec=geekbridge-service-launch|Exec=$bindir/geekbridge-service-launch|" "$autostart/geekbridge-service.desktop"
sed -i "s|^exec geekbridge-service\$|exec \"$bindir/geekbridge-service\"|" "$bindir/geekbridge-service-launch"

if user_systemd; then
  systemctl --user daemon-reload
  systemctl --user enable --now geekbridge-service.service
  # An upgrade replaced the binary under a running service: restart it (a no-op when it was not running).
  systemctl --user try-restart geekbridge-service.service || true
  echo "Service enabled and started (systemd user unit)."
else
  echo "Service will start at your next login (XDG autostart)."
fi
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$icondir" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database -q "$appdir" 2>/dev/null || true
# Nothing below is needed to start; each line names what stops working without it.
missing_note=""
add_note() { missing_note+="  - $1"$'\n'; }
command -v notify-send >/dev/null || add_note "desktop notifications: install libnotify (notify-send; Debian: libnotify-bin)"
command -v wl-copy >/dev/null || command -v xclip >/dev/null || add_note "clipboard sync: install wl-clipboard (Wayland) or xclip (X11)"
command -v xdg-open >/dev/null || add_note "opening links and files: install xdg-utils"
command -v bluetoothctl >/dev/null || add_note "call audio: install bluez (Bluetooth)"
command -v pipewire >/dev/null && command -v wpctl >/dev/null || add_note "call audio: install pipewire, wireplumber and pipewire-pulse"
if [[ -n "$missing_note" ]]; then printf 'Optional programs not found (GeekBridge still starts without them):\n%s' "$missing_note"; fi
run="geekbridge"
case ":$PATH:" in *":$bindir:"*) ;; *) run="$bindir/geekbridge"
  echo "Note: $bindir is not on your PATH, so a terminal will not find geekbridge or geekbridge-ctl until you add it"
  echo "      (or log out and in again, if your system adds ~/.local/bin to PATH once it exists). The application menu entries work now." ;; esac
echo "Installed. Start 'GeekBridge' from your application menu or run: $run"
echo "Next: click 'Link a phone' in GeekBridge, and allow TCP and UDP 1716-1764 in your firewall (geekbridge-ctl firewall prints the commands)."
