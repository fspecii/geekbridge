#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Compile the non-Linux code paths on a Linux machine with no extra rustup targets: copy the tracked tree to a
# scratch directory, rename the `target_os = "linux"` cfg value to one nothing matches, and run clippy with
# warnings denied. That reproduces the dead-code, unused-import and unresolved-import errors the Windows and
# macOS jobs report when Linux-only code (self-update, restart, Do Not Disturb, PipeWire) is not fully gated.
# scripts/xcheck.sh is the faithful check (real targets); this one needs only the host toolchain.
#
#   scripts/sim-nonlinux.sh [extra cargo clippy args]
#
# Known false positive: bridge-kde/src/keepalive.rs is left as is, because its macOS/Windows split is by
# target_os values other than linux that this simulation cannot provide.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source "scripts/lib-tmp.sh"
sim="$(gb_mktemp sim-nonlinux -d)"
trap 'rm -rf "$sim"' EXIT
git ls-files -z | while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done | xargs -0 cp --parents -t "$sim"
cd "$sim"
grep -rlZ 'target_os = "linux"' crates --include=*.rs | grep -zv 'bridge-kde/src/keepalive.rs' \
  | xargs -0 -r sed -i 's/target_os = "linux"/target_os = "gb_not_linux"/g'
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$sim/target}"
RUSTFLAGS="${RUSTFLAGS:-} -A unexpected_cfgs" cargo clippy --workspace --all-targets --keep-going "$@" -- -D warnings
echo "sim-nonlinux: clean"
