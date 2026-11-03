#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Long soak of the real geekbridge-service binary in a sandbox (private state dir, scratch HOME, own ports,
# stand-in phone behind the bridge-chaos fault proxy; never the live service, config or ports 1716/17381/8000).
#
#   SECS=3900 scripts/soak-real.sh [--keep]
#
# Driver: crates/bridge-service/tests/soak_real.rs (ignored test). Verdict: scripts/soak-fit.py (slope fits on
# RSS / FDs / threads / wake-ups / state, log and save directory sizes). Scratch goes under $GB_TMP
# (default ~/.cache/gb-tmp) and is deleted afterwards unless --keep. samples.csv and the verdict are copied
# to dist/soak/real-<ts>/. Prints df before and after.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
GB_TMP="${GB_TMP:-$HOME/.cache/gb-tmp}"
source "$ROOT/scripts/lib-tmp.sh"
KEEP=0; [[ "${1:-}" == --keep ]] && KEEP=1
SECS="${SECS:-3900}"
TS="$(date +%Y%m%d-%H%M%S)"
OUT="$(gb_mktemp soak-real -d)"
DIST="$ROOT/dist/soak/real-$TS"
mkdir -p "$DIST"
echo "df before:"; df -h "$GB_TMP" | tail -1
trap '((KEEP)) || rm -rf "$OUT"; echo "df after:"; df -h "$GB_TMP" | tail -1' EXIT
GEEKBRIDGE_SOAK_OUT="$OUT" GEEKBRIDGE_SOAK_SECS="$SECS" \
    cargo test --release --locked -p bridge-service --test soak_real -- --ignored --nocapture
rc=$?
cp "$OUT"/samples.csv "$OUT"/events.tsv "$OUT"/service.log "$OUT"/poll-errors.log "$DIST"/ 2>/dev/null
python3 -I scripts/soak-fit.py "$DIST/samples.csv" --events "$DIST/events.tsv" --poll-errors "$DIST/poll-errors.log" | tee "$DIST/verdict.md"
fit=${PIPESTATUS[0]}
echo "driver exit $rc, verdict exit $fit; results in $DIST"
exit $((rc != 0 || fit != 0))
