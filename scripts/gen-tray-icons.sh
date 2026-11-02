#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Regenerate assets/tray/: dot-matrix SVGs (from the tables in crates/bridge-service/src/tray/icon.rs,
# the same ones the service draws at run time) and PNGs at the usual tray sizes (needs rsvg-convert).
#
#   scripts/gen-tray-icons.sh
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
OUT="$ROOT/assets/tray"
command -v rsvg-convert >/dev/null || { echo "rsvg-convert not found (librsvg)" >&2; exit 2; }
mkdir -p "$OUT/png"
(cd "$ROOT" && cargo run --quiet -p bridge-service --example tray-icons -- svg "$OUT")
for svg in "$OUT"/icon-*.svg; do
    base="$(basename "$svg" .svg)"
    for size in 16 22 32 48; do
        rsvg-convert -w "$size" -h "$size" "$svg" -o "$OUT/png/$base-$size.png"
    done
done
echo "wrote $(ls "$OUT"/*.svg | wc -l) SVGs and $(ls "$OUT"/png/*.png | wc -l) PNGs to $OUT"
