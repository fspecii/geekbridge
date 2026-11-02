#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# GeekBridge <-> REAL KDE Connect desktop daemon interop run (no phone, no root, no LAN traffic).
#
#   scripts/interop-kdeconnect.sh /path/to/extracted/kdeconnect/root OUTDIR
#
# KC_ROOT is a pacman package tree extracted with bsdtar (kdeconnect + the KDE libraries that are
# not installed system-wide), e.g.:
#   pacman -Sp kdeconnect | xargs -n1 curl -fsSLO ; for f in *.zst; do bsdtar -xf $f -C ROOT; done
# The daemon runs in a throw-away user+network namespace (loopback only, so its UDP broadcasts go
# nowhere), on a private D-Bus, with a private HOME. Nothing under the real $HOME is touched.
set -u
KC_ROOT=${1:?usage: $0 KC_ROOT OUTDIR}
OUT=$(realpath -m "${2:?usage: $0 KC_ROOT OUTDIR}")
REPO=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "${GB_INNER:-}" ]; then
    mkdir -p "$OUT"
    export GB_INNER=1 KC_ROOT OUT REPO
    exec unshare -Urn "$0" "$@"
fi

ip link set lo up
# The daemon ignores UDP identities from loopback senders, so give the namespace one private address.
ip link add dummy0 type dummy && ip addr add 10.77.0.2/24 dev dummy0 && ip link set dummy0 up
export HOME=$OUT/home XDG_CONFIG_HOME=$OUT/home/.config XDG_DATA_HOME=$OUT/home/.local/share \
    XDG_CACHE_HOME=$OUT/home/.cache XDG_RUNTIME_DIR=$OUT/home/rt
rm -rf "$HOME"; mkdir -p "$HOME"/{.config,.local/share,.cache,rt,Downloads}; chmod 700 "$HOME/rt"
export LD_LIBRARY_PATH=$KC_ROOT/usr/lib QT_PLUGIN_PATH=$KC_ROOT/usr/lib/qt6/plugins:/usr/lib/qt6/plugins
export XDG_DATA_DIRS=$KC_ROOT/usr/share:/usr/share XDG_CONFIG_DIRS=/etc/xdg
export QT_QPA_PLATFORM=offscreen GTK_USE_PORTAL=0 QT_NO_XDG_DESKTOP_PORTAL=1 GIO_USE_PORTALS=0
export QT_LOGGING_RULES='kdeconnect.*=true' QT_FORCE_STDERR_LOGGING=1
unset QT_QPA_PLATFORMTHEME GTK_MODULES XDG_CURRENT_DESKTOP WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS

mkdir -p "$OUT/bus"
cat > "$OUT/bus/session.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=$OUT/bus</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
exec dbus-run-session --config-file="$OUT/bus/session.conf" -- "$REPO/scripts/interop-kdeconnect-scenarios.sh"
