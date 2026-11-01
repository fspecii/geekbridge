#!/usr/bin/env bash
# Install GeekBridge from the extracted installer tarball.
#
#   ./install.sh                 per-user install into ~/.local (no root); enables the service at login
#   sudo ./install.sh --system   install into /usr/local for all users
#   ./install.sh --prefix DIR    custom prefix (no service enabling)
#   ./install.sh --uninstall     remove what this script installed (same mode flags)
#
# The service is enabled through a systemd user unit when systemd is available, and always via an
# XDG autostart entry (which starts the same unit, or the binary directly without systemd).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mode=user prefix="" uninstall=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --system) mode=system; shift;;
    --prefix) prefix="$2"; mode=custom; shift 2;;
    --uninstall) uninstall=1; shift;;
    -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
case "$mode" in
  user) prefix="${HOME}/.local"; unitdir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart";;
  system) prefix=/usr/local; unitdir=/usr/lib/systemd/user; autostart=/etc/xdg/autostart;;
  custom) unitdir="$prefix/lib/systemd/user"; autostart="$prefix/etc/xdg/autostart";;
esac
bindir="$prefix/bin" appdir="$prefix/share/applications" icondir="$prefix/share/icons/hicolor"
bins=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-share geekbridge-service-launch)
desktops=(dev.geekbridge.GeekBridge dev.geekbridge.Calls dev.geekbridge.Messages dev.geekbridge.Agenda dev.geekbridge.Clipboard dev.geekbridge.Share)
iconnames=(dev.geekbridge.GeekBridge dev.geekbridge.Calls dev.geekbridge.Messages dev.geekbridge.Agenda dev.geekbridge.Clipboard dev.geekbridge.Share)

user_systemd() { [[ $mode == user ]] && command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; }

if [[ $uninstall -eq 1 ]]; then
  if user_systemd; then systemctl --user disable --now geekbridge-service.service 2>/dev/null || true; fi
  for b in "${bins[@]}"; do rm -f "$bindir/$b"; done
  for d in "${desktops[@]}"; do rm -f "$appdir/$d.desktop"; done
  for n in "${iconnames[@]}"; do
    rm -f "$icondir/scalable/apps/$n.svg"
    for s in 48 128 256; do rm -f "$icondir/${s}x${s}/apps/$n.png"; done
  done
  rm -f "$unitdir/geekbridge-service.service" "$autostart/geekbridge-service.desktop"
  user_systemd && systemctl --user daemon-reload || true
  echo "Removed. Pairing data in ~/.local/share/geekbridge was left in place."
  exit 0
fi

install -d "$bindir" "$appdir" "$unitdir" "$autostart"
for b in "${bins[@]}"; do
  [[ -f "$here/bin/$b" ]] || { echo "missing $here/bin/$b" >&2; exit 66; }
  install -m 0755 "$here/bin/$b" "$bindir/$b"
done
for d in "${desktops[@]}"; do install -m 0644 "$here/share/applications/$d.desktop" "$appdir/$d.desktop"; done
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
  echo "Service enabled and started (systemd user unit)."
else
  echo "Service will start at your next login (XDG autostart)."
fi
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$icondir" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database -q "$appdir" 2>/dev/null || true
case ":$PATH:" in *":$bindir:"*) ;; *) echo "Note: $bindir is not on your PATH." ;; esac
echo "Installed. Start 'GeekBridge' from your application menu or run: geekbridge"
