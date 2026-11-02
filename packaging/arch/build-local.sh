#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the Arch package from the working tree, entirely inside a scratch directory.
#
#   packaging/arch/build-local.sh [--out DIR] [--offline]
#
# * Packs the tracked and untracked-but-not-ignored files into geekbridge-<ver>.tar.gz (directory
#   geekbridge-<ver>/), the same layout as a GitHub tag archive.
# * Copies the PKGBUILD + install script + that tarball into a scratch build dir and runs makepkg there.
# * Leaves the finished geekbridge-<ver>-<rel>-x86_64.pkg.tar.zst in --out (default: <scratch>/out).
# It never runs pacman, never installs anything and writes only below the scratch directory
# ($GB_TMP, else $TMPDIR, else /tmp) and --out. --offline builds with cargo's local cache only.
# Dependencies: makepkg (pacman), cargo, rsvg-convert, git, tar.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
source "$ROOT/scripts/lib-tmp.sh"
out="" offline=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="$2"; shift 2;;
    --offline) offline=1; shift;;
    -h|--help) sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
for c in makepkg cargo rsvg-convert git tar sha256sum; do
  command -v "$c" >/dev/null || { echo "missing tool: $c" >&2; exit 69; }
done

cargo_ver="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$ROOT/Cargo.toml")"
pkg_ver="$(sed -n "s/^pkgver=//p" "$ROOT/packaging/arch/PKGBUILD" | head -n1)"
# pkgver may not contain '-', so a prerelease 0.2.0-rc.1 is packaged as 0.2.0_rc.1 (the Arch convention).
[[ -n "$cargo_ver" && "${cargo_ver//-/_}" == "$pkg_ver" ]] \
  || { echo "PKGBUILD pkgver '$pkg_ver' != Cargo.toml version '$cargo_ver' (a '-' becomes '_')" >&2; exit 65; }

work="$(gb_mktemp arch -d)"
[[ -n "$out" ]] || out="$work/out"
mkdir -p "$out"
echo "scratch: $work"
build="$work/build"; mkdir -p "$build"

echo "== source tarball geekbridge-$pkg_ver.tar.gz"
( cd "$ROOT" && git ls-files -z --cached --others --exclude-standard \
    | while IFS= read -r -d '' f; do [[ -e "$f" ]] && printf '%s\0' "$f"; done \
    | tar --null -T - --transform "s|^|geekbridge-$pkg_ver/|" --owner=0 --group=0 --numeric-owner \
        -czf "$build/geekbridge-$pkg_ver.tar.gz" )
cp "$ROOT/packaging/arch/PKGBUILD" "$ROOT/packaging/arch/geekbridge.install" "$build/"
sum="$(sha256sum "$build/geekbridge-$pkg_ver.tar.gz" | cut -d' ' -f1)"
sed -i "s|^sha256sums=.*|sha256sums=('$sum')|" "$build/PKGBUILD"
echo "sha256 $sum"

# cargo's download cache lives in the scratch dir too (seeded from the user's, so a rebuild needs no network).
export CARGO_HOME="$work/cargo-home"; mkdir -p "$CARGO_HOME"
user_cargo="${GB_USER_CARGO_HOME:-$HOME/.cargo}"
for d in registry git; do [[ -d "$user_cargo/$d" ]] && cp -a --reflink=auto "$user_cargo/$d" "$CARGO_HOME/$d"; done
[[ $offline -eq 1 ]] && export CARGO_NET_OFFLINE=true
# The nine release binaries link big dependency trees; many parallel rustc jobs can exhaust RAM.
export CARGO_BUILD_JOBS="${GB_JOBS:-3}"
echo "== makepkg (cargo jobs: $CARGO_BUILD_JOBS)"
cd "$build"
PKGDEST="$out" BUILDDIR="$work/makepkg-build" SRCDEST="$work/srcdest" \
  makepkg --force --cleanbuild --nodeps --noconfirm --nocolor
ls -l "$out"/*.pkg.tar.* 
echo "package: $(ls "$out"/*.pkg.tar.zst)"
