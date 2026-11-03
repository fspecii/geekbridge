#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Write the CycloneDX 1.5 SBOM release asset (Rust workspace + Android build dependencies), offline, deterministic.
#
#   scripts/gen-sbom.sh [--out FILE] [--root DIR]       default FILE: dist/geekbridge-sbom.cdx.json
#
# Needs python3 (3.11+) and cargo; reads Cargo.lock and android/gradle/verification-metadata.xml, no network.
# The timestamp is SOURCE_DATE_EPOCH, else the committer time of HEAD. No cargo-cyclonedx, cargo-sbom or syft is
# needed (none is a project dependency). The name starts with "geekbridge-" so SHA256SUMS (geekbridge-*) covers it.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
out="" root="$ROOT"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="${2:?--out needs a file}"; shift 2;;
    --root) root="${2:?--root needs a directory}"; shift 2;;
    -h|--help) sed -n '3,9p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
[[ -n "$out" ]] || out="$root/dist/geekbridge-sbom.cdx.json"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 69; }
exec python3 -I "$ROOT/scripts/gen-sbom.py" --root "$root" --out "$out"
