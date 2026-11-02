#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Read-only disk report: the big things GeekBridge development leaves behind, biggest first, with a
# "safe" verdict per item (regenerable AND owned by our scripts AND idle > GB_IDLE_HOURS AND not used
# by a running process; see scripts/lib/disk.sh). Never deletes anything: scripts/clean-dev.sh does that.
#
#   scripts/disk-report.sh [--idle-hours N] [--all]       (--all also lists items under 1 MB)
# Env: GB_IDLE_HOURS (12), GB_MIN_ITEM_MB (50), GB_SCRATCH, GB_TMP, GB_KEEP_AVDS. See docs/disk-hygiene.md.
set -Eeuo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ALL=0
while (($#)); do
    case "$1" in
        --idle-hours) GB_IDLE_HOURS="${2:?}"; shift ;;
        --all) ALL=1 ;;
        -h|--help) sed -n '3,9p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
[[ "${GB_IDLE_HOURS:-12}" =~ ^[0-9]+$ ]] || { echo "--idle-hours needs a number" >&2; exit 2; }
export GB_IDLE_HOURS
# shellcheck source=lib/disk.sh
source "$here/lib/disk.sh"

human() { awk -v k="$1" 'BEGIN { if (k >= 1048576) printf "%.1f GB", k/1048576; else if (k >= 1024) printf "%.0f MB", k/1024; else printf "%d KB", k }'; }

echo "== free space =="
df -h --output=target,size,used,avail,pcent "$GB_ROOT" "$GB_SCRATCH" "$GB_TMP" 2>/dev/null | awk 'NR==1 || !seen[$1]++' || true
if [[ -d "$GB_TMP" ]]; then
    tmp_used="$(du -sk --one-file-system -- "$GB_TMP" 2>/dev/null | cut -f1 || true)"
    echo "$GB_TMP in use (everything, all owners): $(human "${tmp_used:-0}")"
fi
echo

rows="$( { gb_enumerate; gb_enumerate_extra; } | sort -t$'\t' -k3,3nr )"
printf '%-8s %10s %7s  %-5s %s\n' KIND SIZE IDLE SAFE "PATH  (reason)"
total=0 safe=0
while IFS=$'\t' read -r kind path kib idle verdict reason; do
    [[ -n "$kind" ]] || continue
    ((ALL)) || ((kib >= 1024)) || continue
    total=$((total + kib))
    mark="no"; [[ "$verdict" == yes ]] && { mark="YES"; safe=$((safe + kib)); }
    printf '%-8s %10s %6sh  %-5s %s  (%s)\n' "$kind" "$(human "$kib")" "$idle" "$mark" "${path/#$HOME/\~}" "$reason"
done <<<"$rows"
echo
echo "listed: $(human "$total")   safe to delete now: $(human "$safe")  (idle > ${GB_IDLE_HOURS}h, not in use)"

echo
echo "== running emulators =="
pgrep -a 'qemu|emulator' 2>/dev/null | grep -v pgrep | cut -c1-160 || echo "(none)"
echo
echo "Next: make clean-dev            (dry run)      make clean-dev APPLY=1   (delete the YES rows)"
