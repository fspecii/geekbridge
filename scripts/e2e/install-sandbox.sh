#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install / upgrade / uninstall / update end to end in a throw-away sandbox. Never touches the real HOME,
# ~/.config or the real systemd user manager: HOME and every XDG dir point into the sandbox, XDG_RUNTIME_DIR
# is unusable, and `systemctl` is a stub on PATH that only records its arguments.
#
#   scripts/e2e/install-sandbox.sh [--bin-dir DIR] [--keep]
#
# Needs release binaries (default target/release), rsvg-convert, desktop-file-validate, systemd-analyze, python3.
# Sandbox: $GB_TMP/gb-install-sandbox (default ~/.cache/gb-tmp). Results: sandbox/results.tsv, exit 1 on any FAIL.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
BIN="$ROOT/target/release"; KEEP=0
while [[ $# -gt 0 ]]; do case "$1" in --bin-dir) BIN="$2"; shift 2;; --keep) KEEP=1; shift;; *) echo "unknown: $1" >&2; exit 64;; esac; done
export GB_TMP="${GB_TMP:-$HOME/.cache/gb-tmp}"; mkdir -p "$GB_TMP"
S="$GB_TMP/gb-install-sandbox"; rm -rf "$S"; mkdir -p "$S"
RES="$S/results.tsv"; : >"$RES"
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" | tee -a "$RES" >&2; }
check() { if [[ "$2" == 0 ]]; then rec "$1" PASS "${3:-}"; else rec "$1" FAIL "${3:-}"; fi; }
ok() { "$@" >/dev/null 2>&1; echo $?; }
cleanup() { pkill -f "gb-install-sandbox/home/.*geekbridge-service" 2>/dev/null; pkill -f "mock_release_server.py $MOCK_PORT" 2>/dev/null
  [[ $KEEP == 1 ]] || rm -rf "$S"; }
MOCK_PORT=18893
trap cleanup EXIT

# ---- sandbox environment (the only place anything is written)
export HOME="$S/home"; export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share" XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$S/no-runtime-dir"   # nonexistent: a stray systemctl --user / dbus call fails instead of reaching the real manager
export TMPDIR="$S/tmp"; unset DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE
mkdir -p "$HOME" "$TMPDIR" "$S/stubs"
REAL_HOME_CFG="$(getent passwd "$(id -u)" | cut -d: -f6)/.config"
cfg_before="$(find "$REAL_HOME_CFG/systemd" "$REAL_HOME_CFG/autostart" -maxdepth 2 2>/dev/null | sort | sha256sum)"
cat >"$S/stubs/systemctl" <<STUB
#!/bin/sh
echo "\$*" >>"$S/systemctl.log"
exit 0
STUB
chmod +x "$S/stubs/systemctl"; export PATH="$S/stubs:$PATH"

# ---- 1. build the packages from the given binaries
out="$S/out"; mkdir -p "$out"
VER="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT/Cargo.toml")"
"$ROOT/scripts/package-linux.sh" --bin-dir "$BIN" --out "$out" >"$S/package.log" 2>&1; check package-build $? "$(tail -2 "$S/package.log" | tr '\n' ' ')"
TGZ="$out/geekbridge-$VER-linux-x86_64-installer.tar.gz"; DEB="$out/geekbridge_${VER}_amd64.deb"

# ---- 2. .deb contents
d="$S/deb"; mkdir -p "$d/ctl" "$d/data"
(cd "$d" && ar x "$DEB" && tar -xJf control.tar.xz -C ctl && tar -xJf data.tar.xz -C data)
want=(usr/bin/geekbridge usr/bin/geekbridge-service usr/bin/geekbridge-ctl usr/bin/geekbridge-service-launch
  usr/lib/systemd/user/geekbridge-service.service etc/xdg/autostart/geekbridge-service.desktop
  usr/share/applications/dev.geekbridge.GeekBridge.desktop usr/share/applications/dev.geekbridge.PasteFromPhone.desktop
  usr/share/doc/geekbridge/copyright usr/share/doc/geekbridge/LICENSE usr/share/geekbridge/package-managed)
miss=""; for f in "${want[@]}"; do [[ -e "$d/data/$f" ]] || miss+=" $f"; done
check deb-expected-files $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"
miss=""; for n in GeekBridge Calls Messages Agenda Contacts Clipboard Share PasteFromPhone; do
  for p in scalable/apps/dev.geekbridge.$n.svg 48x48/apps/dev.geekbridge.$n.png 128x128/apps/dev.geekbridge.$n.png 256x256/apps/dev.geekbridge.$n.png; do
    [[ -s "$d/data/usr/share/icons/hicolor/$p" ]] || miss+=" $p"; done; done
check deb-icons $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"
bad=0; for f in "$d"/data/usr/share/applications/*.desktop "$d/data/etc/xdg/autostart/geekbridge-service.desktop"; do desktop-file-validate "$f" || bad=1; done
check deb-desktop-validate $bad
# every Icon= resolves; every Exec= program is shipped
bad=""; for f in "$d"/data/usr/share/applications/*.desktop; do
  ic="$(sed -n 's/^Icon=//p' "$f")"; [[ -e "$d/data/usr/share/icons/hicolor/scalable/apps/$ic.svg" ]] || bad+=" Icon:$ic"
  while read -r ex; do [[ -x "$d/data/usr/bin/$ex" ]] || bad+=" Exec:$ex"; done < <(sed -n 's/^Exec=\([^ ]*\).*/\1/p' "$f"); done
check deb-icon-exec-resolve $([[ -z $bad ]]; echo $?) "${bad:-ok}"
sed "s|/usr/bin/|$d/data/usr/bin/|" "$d/data/usr/lib/systemd/user/geekbridge-service.service" >"$S/unit-check.service"
mkdir -p "$S/rt"; chmod 700 "$S/rt"
XDG_RUNTIME_DIR="$S/rt" systemd-analyze verify --user "$S/unit-check.service" >"$S/unit.log" 2>&1; check deb-unit-verify $? "$(head -c 200 "$S/unit.log")"
check deb-unit-mode $([[ "$(stat -c %a "$d/data/usr/lib/systemd/user/geekbridge-service.service")" == 644 ]]; echo $?)
check deb-no-setuid $([[ -z "$(find "$d/data" -perm /6000)" ]]; echo $?)
check deb-no-files-outside-usr-etc $([[ -z "$(cd "$d/data" && find . -mindepth 1 -maxdepth 1 ! -name usr ! -name etc)" ]]; echo $?)
check deb-md5sums $( (cd "$d/data" && md5sum -c --quiet "$d/ctl/md5sums") >/dev/null 2>&1; echo $?)
# maintainer scripts: syntax, and purge/remove never touch a home directory
for s in postinst prerm; do sh -n "$d/ctl/$s"; check "deb-$s-syntax" $?; done
check deb-scripts-leave-home-alone $([[ -z "$(grep -E '\$HOME|~/|/home/' "$d"/ctl/postinst "$d"/ctl/prerm)" ]]; echo $?)
# runtime libraries the binaries dlopen must be declared
miss=""; for l in libxkbcommon-x11-0 libxcursor1 libxi6 libxrandr2; do grep -q "^Depends:.*$l" "$d/ctl/control" || miss+=" $l"; done
check deb-depends-complete $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"

# control file fields: parse as RFC-822 stanzas, runtime Recommends/Suggests list the programs the apps shell out to
cf="$d/ctl/control"
field() { awk -v f="$1" 'BEGIN{FS=": "} $0 ~ "^"f":" {sub("^"f": *",""); print; exit}' "$cf"; }
bad=""; for f in Package Version Architecture Maintainer Depends Recommends Suggests Section Priority Homepage Description; do [[ -n "$(field "$f")" ]] || bad+=" $f"; done
check deb-control-fields-present $([[ -z $bad ]]; echo $?) "missing:${bad:- none}"
check deb-control-no-bad-lines $([[ -z "$(grep -vE '^([A-Za-z][A-Za-z0-9-]*: .+| .*)$' "$cf")" ]]; echo $?)
miss=""; for p in libnotify-bin wl-clipboard xclip xdg-utils bluez pipewire wireplumber pipewire-pulse; do field Recommends | grep -qw -- "$p" || miss+=" $p"; done
check deb-recommends-complete $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"
check deb-suggests-notification-daemon $(field Suggests | grep -q 'mako-notifier'; echo $?) "$(field Suggests)"
# the real tool, when this machine has it
if command -v dpkg-deb >/dev/null; then
  check deb-dpkg-deb-info $(dpkg-deb -I "$DEB" control >"$S/dpkg-control.txt" 2>&1; echo $?) "$(grep -E '^(Depends|Recommends)' "$S/dpkg-control.txt" | cut -c1-60 | tr '\n' '|')"
else
  rec deb-dpkg-deb-info SKIP "dpkg-deb not installed"
fi
check deb-ships-licence-and-notices $([[ -s "$d/data/usr/share/doc/geekbridge/THIRD_PARTY_NOTICES.md" && -s "$d/data/usr/share/doc/geekbridge/LICENSE" ]]; echo $?)
check deb-readme-links-absolute $([[ -s "$d/data/usr/share/doc/geekbridge/README.md" ]] && ! grep -q -E '(\]\((docs|assets)/|(src|href)="(docs|assets)/)' "$d/data/usr/share/doc/geekbridge/README.md"; echo $?)
# the Arch package rewrites the README with the same helper (it cannot be built here: that compiles the whole workspace)
check arch-readme-links-absolute $(grep -q 'scripts/lib/readme.sh' "$ROOT/packaging/arch/PKGBUILD" && ( source "$ROOT/scripts/lib/readme.sh"; ! gb_absolute_readme "$ROOT/README.md" | grep -q -E '(\]\((docs|assets)/|(src|href)="(docs|assets)/)' ); echo $?)

# ---- 2b. Arch package, statically (a full makepkg build takes ~10 min: packaging/arch/build-local.sh)
pk="$ROOT/packaging/arch"
apps_pkgbuild="$(bash -c 'source "$1" >/dev/null 2>&1; printf "%s\n" "${_apps[@]}" | sort' _ "$pk/PKGBUILD")"
apps_files="$(cd "$ROOT/packaging/linux" && ls dev.geekbridge.*.desktop | sed 's/^dev\.geekbridge\.//;s/\.desktop$//' | sort)"
check arch-apps-match-desktop-files $([[ "$apps_pkgbuild" == "$apps_files" ]]; echo $?) "$(echo $apps_pkgbuild)"
check arch-install-script-syntax $(bash -n "$pk/geekbridge.install"; echo $?)
check arch-install-enables-nothing $([[ -z "$(grep -E '^[^#]*systemctl[^"]*(enable|start)' "$pk/geekbridge.install" | grep -v -e 'echo' -e '^ ')" ]]; echo $?)
check arch-remove-names-real-state-dir $(grep -q '\.local/state/geekbridge' "$pk/geekbridge.install"; echo $?)
pkg_opt="$(bash -c 'source "$1" >/dev/null 2>&1; printf "%s\n" "${optdepends[@]}"' _ "$pk/PKGBUILD")"
miss=""; for p in libnotify wl-clipboard xclip bluez pipewire wireplumber pipewire-pulse xdg-utils; do grep -q "^$p: ." <<<"$pkg_opt" || miss+=" $p"; done
check arch-optdepends-explained $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"

# ---- 3. installer tarball contents
t="$S/tgz"; mkdir -p "$t"; tar -xzf "$TGZ" -C "$t"; T="$t/geekbridge-$VER"
check tarball-has-installer $([[ -x "$T/install.sh" && -f "$T/LICENSE" && -f "$T/README.md" ]]; echo $?)
check tarball-readme-links-absolute $(! grep -q -E '(\]\((docs|assets)/|(src|href)="(docs|assets)/)' "$T/README.md"; echo $?)   # no relative doc or image links next to the binaries
check tarball-no-absolute-or-dotdot-paths $([[ -z "$(tar -tzf "$TGZ" | grep -E '^/|\.\./')" ]]; echo $?)

# ---- 4. fresh per-user install
STATE="$XDG_DATA_HOME/geekbridge"; mkdir -p "$STATE/service" "$XDG_STATE_HOME/geekbridge"; echo x >"$STATE/service/trust.json"; echo x >"$XDG_STATE_HOME/geekbridge/update.json"
"$T/install.sh" >"$S/install1.log" 2>&1; check install-user-exit $? "$(tail -1 "$S/install1.log")"
L="$HOME/.local"; U="$XDG_CONFIG_HOME/systemd/user/geekbridge-service.service"; A="$XDG_CONFIG_HOME/autostart/geekbridge-service.desktop"
miss=""; for b in geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share geekbridge-service-launch; do [[ -x "$L/bin/$b" ]] || miss+=" $b"; done
check install-binaries $([[ -z $miss ]]; echo $?) "missing:${miss:- none}"
check install-unit-points-at-home $(grep -qx "ExecStart=$L/bin/geekbridge-service" "$U"; echo $?) "$(grep ExecStart "$U")"
check install-autostart-absolute $(grep -qx "Exec=$L/bin/geekbridge-service-launch" "$A"; echo $?)
bad=""; for f in "$L"/share/applications/dev.geekbridge.*.desktop; do grep -q "^Exec=$L/bin/geekbridge" "$f" || bad+=" $(basename "$f")"; done
check install-menu-entries-absolute $([[ -z $bad ]]; echo $?) "relative Exec:${bad:- none}"
check install-launcher-absolute $(grep -q "^exec \"$L/bin/geekbridge-service\"" "$L/bin/geekbridge-service-launch"; echo $?)
check install-service-enabled-via-systemd $(grep -q 'enable --now geekbridge-service.service' "$S/systemctl.log"; echo $?)
check install-docs-for-about $([[ -s "$L/share/doc/geekbridge/LICENSE" && -s "$L/share/doc/geekbridge/THIRD_PARTY_NOTICES.md" ]]; echo $?)
ab="$("$L/bin/geekbridge-ctl" about 2>&1)"
check install-ctl-about-finds-installed-docs $([[ "$ab" == *"Licence: GPL-3.0-or-later"* && "$ab" == *"Third-party notices: $L/share/doc/geekbridge/THIRD_PARTY_NOTICES.md"* && "$ab" == *"Licence text: $L/share/doc/geekbridge/LICENSE"* ]]; echo $?) "$(echo "$ab" | grep -E 'notices|text' | tr '\n' '|')"
if command -v notify-send >/dev/null; then note_ok=0; else grep -q 'notify-send' "$S/install1.log"; note_ok=$?; fi
check install-optional-programs-note $note_ok "missing optional programs are named, never fatal"
check install-desktop-entries $([[ "$(ls "$L/share/applications"/*.desktop | wc -l)" == 8 ]]; echo $?)
check install-icons $([[ "$(find "$L/share/icons" -type f \( -name '*.png' -o -name '*.svg' \) | wc -l)" == 32 ]]; echo $?)
bad=0; for f in "$L"/share/applications/*.desktop "$A"; do desktop-file-validate "$f" || bad=1; done; check install-desktop-validate $bad
sed "s|ExecStart=.*|ExecStart=$L/bin/geekbridge-service|" "$U" >"$S/unit-home.service"; XDG_RUNTIME_DIR="$S/rt" systemd-analyze verify --user "$S/unit-home.service" >/dev/null 2>&1; check install-unit-verify $?
check install-ctl-runs $("$L/bin/geekbridge-ctl" --version >/dev/null 2>&1; echo $?) "$("$L/bin/geekbridge-ctl" --version 2>&1)"
# waybar module: offline JSON line, exit 0, valid JSON, no state created
wb="$("$L/bin/geekbridge-ctl" status --waybar 2>&1)"; wrc=$?
check waybar-offline-json $([[ $wrc == 0 ]] && printf '%s' "$wb" | python3 -I -c 'import json,sys; d=json.loads(sys.stdin.read()); assert "text" in d and "class" in d' 2>/dev/null; echo $?) "$wb"
check install-writes-nothing-in-real-config $([[ "$(find "$REAL_HOME_CFG/systemd" "$REAL_HOME_CFG/autostart" -maxdepth 2 2>/dev/null | sort | sha256sum)" == "$cfg_before" ]]; echo $?)

# ---- 5. user data, then upgrade over the top
FIX="$ROOT/crates/bridge-service/tests/fixtures/state-v1"
mkdir -p "$STATE"; chmod 700 "$STATE"; cp -a "$FIX"/. "$STATE"/; rm -f "$STATE/README.md"; chmod 700 "$STATE"; find "$STATE" -type f -exec chmod 600 {} +   # git keeps no 0600: a fresh clone has 0644 fixtures
state_sum() { (cd "$STATE" && find . -type f | sort | xargs sha256sum | sha256sum); }
s_before="$(state_sum)"
# mark installed binaries so the upgrade is observable
echo old >"$L/bin/.old-marker"; : >"$S/systemctl.log"
"$T/install.sh" >"$S/install2.log" 2>&1; check upgrade-exit $? "$(tail -1 "$S/install2.log")"
check upgrade-state-untouched $([[ "$(state_sum)" == "$s_before" ]]; echo $?)
check upgrade-restarts-running-service $(grep -Eq '(try-)?restart geekbridge-service' "$S/systemctl.log"; echo $?) "$(tr '\n' '|' <"$S/systemctl.log")"
check upgrade-no-duplicate-files $([[ "$(ls "$L/share/applications"/*.desktop | wc -l)" == 8 && "$(ls "$XDG_CONFIG_HOME/autostart" | wc -l)" == 1 ]]; echo $?)
# a trust store the new build can read: the real service on the v1 state, via ctl
SVC="$L/bin/geekbridge-service"; CTL="$L/bin/geekbridge-ctl"; P=(--api-port 17780 --control-port 17781 --discovery-port 17716 --listen-inbound 127.0.0.1:1931)
(env GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_UPDATE_BASE_URL="http://127.0.0.1:$MOCK_PORT" setsid nohup "$SVC" --state-dir "$STATE" "${P[@]}" --dev-loopback --no-calling-probe --no-announce --name Sandbox >"$S/service.log" 2>&1 &)
ctl() { timeout 60 "$CTL" --state-dir "$STATE" --control-port 17781 "$@"; }
up=1; for _ in $(seq 30); do ctl status >/dev/null 2>&1 && { up=0; break; }; sleep 0.5; done
check upgrade-service-starts-on-old-state $up "$(tail -2 "$S/service.log" | tr '\n' '|')"
peer="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["peers"][0]["device_id"])' "$FIX/trusted_peers.json" 2>/dev/null)"
check upgrade-pairing-still-listed $(ctl status 2>&1 | grep -q "$peer"; echo $?) "peer ${peer:0:8}"

# ---- 6. in-app updater against a locally served fake release (GEEKBRIDGE_UPDATE_BASE_URL), checksum failure, rollback
R="$S/release"; mkdir -p "$R/geekbridge-linux-x86_64"
cp "$BIN"/geekbridge{,-service,-ctl,-calls,-messages,-clipboard,-agenda,-contacts,-share} "$R/geekbridge-linux-x86_64/" 2>/dev/null
cp "$ROOT/README.md" "$R/geekbridge-linux-x86_64/"
(cd "$R" && tar -czf geekbridge-linux-x86_64.tar.gz geekbridge-linux-x86_64)
SUM="$(sha256sum "$R/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)"
echo "geekbridge-android.apk" >"$R/apk"   # the mock insists on a primary asset
MODE="$S/mock-mode"; : >"$MODE"
(MOCK_MODE_FILE="$MODE" MOCK_EXTRA="geekbridge-linux-x86_64.tar.gz=$R/geekbridge-linux-x86_64.tar.gz" setsid nohup python3 -I "$ROOT/scripts/e2e/mock_release_server.py" "$MOCK_PORT" "$R/apk" v9.9.9 >"$S/mock.log" 2>&1 &)
sleep 1
out="$(ctl update check 2>&1)"
check updater-offers-newer $([[ "$out" == *"v9.9.9 - newer"* && "$out" == *"$SUM"* ]]; echo $?) "$(echo "$out" | grep -E 'latest|installer' | tr '\n' '|')"
echo corrupt >"$MODE"; ctl update download >"$S/dl-bad.out" 2>&1; rc=$?
check updater-checksum-failure-refused $([[ $rc != 0 && "$(cat "$S/dl-bad.out")" == *"does not match"* ]]; echo $?) "rc=$rc"
check updater-checksum-failure-leaves-nothing $([[ -z "$(find "$S" "$TMPDIR" -name 'geekbridge-update-*' -type f 2>/dev/null)" ]]; echo $?)
check updater-failure-leaves-install-intact $([[ "$(sha256sum "$L/bin/geekbridge-service" | cut -d' ' -f1)" == "$(sha256sum "$T/bin/geekbridge-service" | cut -d' ' -f1)" ]]; echo $?)
: >"$MODE"; out="$(ctl update download 2>&1)"; rc=$?; path="$(echo "$out" | sed -n 's/^verified download: //p')"
check updater-good-download-verified $([[ $rc == 0 && -f "$path" && "$(sha256sum "$path" | cut -d' ' -f1)" == "$SUM" ]]; echo $?) "$path"
check updater-does-not-install $([[ "$(sha256sum "$L/bin/geekbridge-service" | cut -d' ' -f1)" == "$(sha256sum "$T/bin/geekbridge-service" | cut -d' ' -f1)" ]]; echo $?) "no files replaced by the updater"
same=0; for f in identity.pem trusted_peers.json consent.json share.json device-name clipboard-settings.json; do cmp -s "$STATE/$f" "$FIX/$f" || same=1; done
check updater-state-untouched-by-download $same "pairing, consent, identity and settings files byte-identical to the v1 fixture"
# Applying a verified archive is manual (docs/updates.md). The documented procedure with rollback on a bad build:
apply() { # archive-dir  -> installs over $L/bin, restores the previous binaries if the smoke test fails
  local src="$1" bk="$S/backup"; rm -rf "$bk"; mkdir -p "$bk"
  for b in "$src"/geekbridge*; do cp -p "$L/bin/$(basename "$b")" "$bk/" 2>/dev/null; install -m 0755 "$b" "$L/bin/$(basename "$b")"; done
  if ! "$L/bin/geekbridge-ctl" --version >/dev/null 2>&1; then cp -p "$bk"/* "$L/bin/"; return 1; fi
}
pkill -f "gb-install-sandbox/home/.*geekbridge-service" 2>/dev/null; sleep 0.5
mkdir -p "$S/ex"; tar -xzf "$path" -C "$S/ex"
apply "$S/ex/geekbridge-linux-x86_64"; check apply-good-archive $?
check apply-keeps-state $([[ "$(state_sum | cut -c1-8)" != "" && -f "$STATE/trusted_peers.json" && -f "$STATE/identity.pem" ]]; echo $?)
# bad archive: a truncated ctl; rollback must put the previous binary back
mkdir -p "$S/bad"; cp "$S/ex/geekbridge-linux-x86_64/"geekbridge* "$S/bad/"; printf '#!/bin/sh\nexit 3\n' >"$S/bad/geekbridge-ctl"
good_ctl="$(sha256sum "$L/bin/geekbridge-ctl" | cut -d' ' -f1)"
apply "$S/bad"; rc=$?
check apply-bad-archive-detected $([[ $rc != 0 ]]; echo $?)
check apply-rollback-restores-binaries $([[ "$(sha256sum "$L/bin/geekbridge-ctl" | cut -d' ' -f1)" == "$good_ctl" ]]; echo $?)
# the upgraded service still reads the pre-upgrade state
(env GEEKBRIDGE_NO_UPDATE_CHECK=1 setsid nohup "$SVC" --state-dir "$STATE" "${P[@]}" --dev-loopback --no-calling-probe --no-announce --name Sandbox >"$S/service2.log" 2>&1 &)
up=1; for _ in $(seq 30); do ctl status >/dev/null 2>&1 && { up=0; break; }; sleep 0.5; done
check apply-service-restarts-with-pairing $([[ $up == 0 ]] && ctl status 2>&1 | grep -q "$peer"; echo $?)
pkill -f "gb-install-sandbox/home/.*geekbridge-service" 2>/dev/null; sleep 0.5

# ---- 7. uninstall keeps user data unless asked
: >"$S/systemctl.log"; s_before="$(state_sum)"
"$T/install.sh" --uninstall >"$S/uninstall.log" 2>&1; check uninstall-exit $? "$(tail -1 "$S/uninstall.log")"
left="$(find "$L" "$XDG_CONFIG_HOME" -type f ! -path "$XDG_CONFIG_HOME/geekbridge/*" 2>/dev/null | grep -v -e "^$STATE/" -e "^$XDG_STATE_HOME/geekbridge/" -e '/\.old-marker$' -e '/mimeinfo.cache$' -e '/icon-theme.cache$')"
check uninstall-removes-everything-installed $([[ -z "$left" ]]; echo $?) "${left:-clean}"
check uninstall-disables-service $(grep -q 'disable --now geekbridge-service.service' "$S/systemctl.log"; echo $?)
check uninstall-keeps-user-data $([[ "$(state_sum)" == "$s_before" ]]; echo $?)
check uninstall-message-names-real-state-dir $(grep -q "$STATE\|XDG_DATA_HOME\|.local/share/geekbridge" "$S/uninstall.log"; echo $?) "$(tail -1 "$S/uninstall.log")"
"$T/install.sh" --uninstall >"$S/uninstall2.log" 2>&1; check uninstall-idempotent $?
"$T/install.sh" >/dev/null 2>&1
"$T/install.sh" --uninstall --purge-data >"$S/purge.log" 2>&1; check purge-exit $?
check purge-removes-state $([[ ! -e "$STATE" && ! -e "$XDG_STATE_HOME/geekbridge" ]]; echo $?)
check purge-only-sandbox $([[ -d "$HOME" ]]; echo $?)
check real-config-untouched-at-end $([[ "$(find "$REAL_HOME_CFG/systemd" "$REAL_HOME_CFG/autostart" -maxdepth 2 2>/dev/null | sort | sha256sum)" == "$cfg_before" ]]; echo $?)

fails="$(grep -c $'\tFAIL' "$RES")"; pass="$(grep -c $'\tPASS' "$RES")"
echo "install-sandbox: $pass passed, $fails failed (results: $RES)" >&2
[[ $KEEP == 1 ]] || cp "$RES" "$GB_TMP/install-sandbox-results.tsv"
[[ "$fails" == 0 ]]
