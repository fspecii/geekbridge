#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Check that build-desktop.sh, package-linux.sh and release.yml all use scripts/release-binaries.txt,
# that no hard-coded binary list crept back in, and that every listed name is a real cargo binary.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
list="$ROOT/scripts/release-binaries.txt"
fail=0
for f in scripts/build-desktop.sh scripts/package-linux.sh .github/workflows/release.yml; do
    grep -q 'release-binaries.txt' "$ROOT/$f" || { echo "FAIL: $f does not read release-binaries.txt" >&2; fail=1; }
    if grep -q 'geekbridge-clipboard geekbridge-agenda' "$ROOT/$f"; then echo "FAIL: $f hard-codes a binary list" >&2; fail=1; fi
done
mapfile -t names < <(grep -v -e '^#' -e '^[[:space:]]*$' "$list")
[[ ${#names[@]} -eq 9 ]] || { echo "FAIL: expected 9 binaries, list has ${#names[@]}" >&2; fail=1; }
if [[ "$(printf '%s\n' "${names[@]}" | sort | uniq -d)" != "" ]]; then echo "FAIL: duplicate names" >&2; fail=1; fi
bins="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1 --offline 2>/dev/null | python3 -I -c '
import json,sys
for p in json.load(sys.stdin)["packages"]:
    for t in p["targets"]:
        if "bin" in t["kind"]: print(t["name"])')"
for n in "${names[@]}"; do
    grep -qx -- "$n" <<<"$bins" || { echo "FAIL: $n is not a cargo binary" >&2; fail=1; }
done
[[ $fail -eq 0 ]] && echo "binary list ok (${#names[@]} binaries)"
exit $fail
