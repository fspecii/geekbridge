#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Compare release APK sizes and classes.dex against scripts/android-budgets.env and fail on overflow.
#
#   scripts/android-size-check.sh [--universal APK] [--arm64 APK]
#
# At least one APK is required. The dex budget is checked against the first APK given (the dex does not depend on the ABI split).
# Prints a table (also appended to $GITHUB_STEP_SUMMARY when set). Exit 0: all inside the budgets; 1: at least one over; 2: usage.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck disable=SC1091
source "$ROOT/scripts/android-budgets.env"

uni="" arm=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --universal) uni="${2:?}"; shift 2 ;;
    --arm64) arm="${2:?}"; shift 2 ;;
    *) echo "usage: $0 [--universal APK] [--arm64 APK]" >&2; exit 2 ;;
  esac
done
[[ -n "$uni$arm" ]] || { echo "usage: $0 [--universal APK] [--arm64 APK]" >&2; exit 2; }

fail=0
rows=""
check() {   # name measured budget
  local name="$1" got="$2" max="$3" status=ok left
  left=$((max - got))
  if ((got > max)); then status="OVER"; fail=1; fi
  rows+="$(printf '| %-34s | %10d | %10d | %+9d | %s |' "$name" "$got" "$max" "$left" "$status")"$'\n'
}
dex_bytes() { unzip -lv "$1" | awk '$NF ~ /^classes[0-9]*\.dex$/ { s += $1 } END { print s + 0 }'; }

first=""
if [[ -n "$uni" ]]; then
  [[ -f "$uni" ]] || { echo "no such APK: $uni" >&2; exit 2; }
  check "release APK, universal" "$(stat -c %s "$uni")" "$GB_BUDGET_APK_UNIVERSAL"; first="${first:-$uni}"
fi
if [[ -n "$arm" ]]; then
  [[ -f "$arm" ]] || { echo "no such APK: $arm" >&2; exit 2; }
  check "release APK, arm64-v8a only" "$(stat -c %s "$arm")" "$GB_BUDGET_APK_ARM64"; first="${first:-$arm}"
fi
dex="$(dex_bytes "$first")"
((dex > 0)) || { echo "no classes*.dex in $first" >&2; exit 2; }
check "classes.dex (release, R8)" "$dex" "$GB_BUDGET_DEX"

out="$(printf '| %-34s | %10s | %10s | %9s | %s |\n' "Android size (bytes)" "measured" "budget" "headroom" "status"
       printf '| %-34s | %10s | %10s | %9s | %s |\n' "----------------------------------" "--------" "------" "--------" "------"
       printf '%s' "$rows")"
echo "$out"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then { echo '### Android size budgets (scripts/android-budgets.env)'; echo; echo "$out"; } >> "$GITHUB_STEP_SUMMARY"; fi
if ((fail)); then
  echo "::error::Android size over budget (scripts/android-budgets.env). Find the savings, or raise the budget with the reason in docs/android-perf.md." >&2
  exit 1
fi
