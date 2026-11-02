#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Reclaim disk from GeekBridge development leftovers. DRY RUN unless --apply.
#
#   scripts/clean-dev.sh [--apply] [--older-than HOURS] [--keep-avd NAME]... [--what target|avd|cache|tmp|all]
#
#   --what WHAT         one or more categories (repeat, or comma-separated); default all
#   --older-than HOURS  only items idle at least this long (newest mtime in the tree's top 3 levels); default 12
#   --keep-avd NAME     never delete this AVD (repeatable; also: gbbase gbcall gbnothing gbhome always kept,
#                       override the default list with GB_KEEP_AVDS)
#   --apply             actually delete; without it nothing is touched and "would free X GB" is printed
#
# What it owns (the manifest / naming convention; anything else is never touched):
#   target  <repo>/target/<name>/        except debug, release, android (the normal builds; GB_KEEP_TARGET_DIRS)
#   avd     $ANDROID_AVD_HOME/<name>.avd + <name>.ini   (default <repo>/.tools/android-user/avd)
#   cache   $GB_SCRATCH/* items marked .gb-owned or target-shared (default ~/.cache/gb-scratch), ~/.cache/gb-*
#   tmp     $GB_TMP/gb-<name>.XXXXXX, gb-test-*, gb-callui-xdg-<uid>, geekbridge-emulate-call.*  (scripts/lib-tmp.sh)
# Rules, all of which must hold per item: path is exactly one of the shapes above (checked again right
# before deleting, symlinks refused, no ".."), regenerable, idle longer than --older-than, not
# referenced by a running process (cwd, cmdline, environment or open files of any /proc/<pid>; for AVDs
# also `-avd NAME`), and at least GB_MIN_ITEM_MB (50) big. Gradle caches and dist/ are never touched.
# Details: docs/disk-hygiene.md. scripts/clean-tmp.sh remains for the /tmp-only, minutes-based variant.
set -Eeuo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
APPLY=0 WHAT="" KEEP=""
while (($#)); do
    case "$1" in
        --apply) APPLY=1 ;;
        --older-than) GB_IDLE_HOURS="${2:?--older-than needs HOURS}"; shift ;;
        --keep-avd) KEEP="$KEEP ${2:?--keep-avd needs a name}"; shift ;;
        --what) WHAT="$WHAT ${2:?--what needs a category}"; shift ;;
        -h|--help) sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
[[ "${GB_IDLE_HOURS:-12}" =~ ^[0-9]+$ ]] || { echo "--older-than needs a whole number of hours" >&2; exit 2; }
export GB_IDLE_HOURS="${GB_IDLE_HOURS:-12}"
WHAT="${WHAT//,/ }"; WHAT="${WHAT:-all}"
kinds=""
for w in $WHAT; do
    case "$w" in
        all) kinds="target avd cache tmp" ;;
        target|avd|cache|tmp) kinds="$kinds $w" ;;
        *) echo "unknown --what '$w' (target|avd|cache|tmp|all)" >&2; exit 2 ;;
    esac
done
for n in $KEEP; do [[ "$n" =~ ^[A-Za-z0-9_.-]+$ ]] || { echo "bad AVD name '$n'" >&2; exit 2; }; done
GB_KEEP_AVDS="${GB_KEEP_AVDS:-gbbase gbcall gbnothing gbhome}$KEEP"
export GB_KEEP_AVDS
# shellcheck source=lib/disk.sh
source "$here/lib/disk.sh"
[[ -f "$GB_ROOT/Cargo.toml" && -d "$GB_ROOT/scripts" ]] || { echo "refusing: $GB_ROOT does not look like the GeekBridge repo" >&2; exit 1; }

# The only shapes we will ever delete (second line of defence after the enumerator).
allowed() {   # kind path
    local kind="$1" p="$2" parent b
    [[ "$p" == /* && "$p" != *..* && ! -L "$p" && -e "$p" ]] || return 1
    parent="${p%/*}"; b="${p##*/}"
    case "$kind" in
        target) [[ "$parent" == "$GB_ROOT/target" && -n "$b" ]] ;;
        avd) [[ "$parent" == "$ANDROID_AVD_HOME" && "$b" == *.avd ]] ;;
        cache) [[ ( "$parent" == "$HOME/.cache" && "$b" == gb-* && "$p" != "$GB_SCRATCH" ) || ( "$parent" == "$GB_SCRATCH" && "$b" != .* ) ]] ;;
        tmp) [[ "$parent" == "$GB_TMP" && ( "$b" == gb-* || "$b" == geekbridge-emulate-call.* ) ]] ;;
        *) return 1 ;;
    esac
}

human() { awk -v k="$1" 'BEGIN { printf "%.1f GB", k/1048576 }'; }
plan="$(gb_enumerate "$kinds" | sort -t$'\t' -k3,3nr)"
snap2="$(mktemp "${TMPDIR:-/tmp}/gb-snap.XXXXXX")"; trap 'rm -f -- "$snap2"' EXIT
((APPLY)) && gb_snapshot "$snap2"   # fresh process view right before deleting

total=0 n=0 skipped=0
while IFS=$'\t' read -r kind path kib idle verdict reason; do
    [[ -n "$kind" ]] || continue
    if [[ "$verdict" != yes ]]; then
        ((kib >= 51200)) && { echo "skip    $kind $(human "$kib") $path ($reason)"; skipped=$((skipped + 1)); }
        continue
    fi
    if ! allowed "$kind" "$path"; then echo "REFUSED $path (not a path this tool owns)"; continue; fi
    if ((APPLY)); then
        if gb_in_use "$snap2" "$path"; then echo "skip    $kind $path (started being used)"; continue; fi
        if [[ "$kind" == avd ]]; then
            rm -rf -- "$path" "${path%.avd}.ini"
        else
            rm -rf -- "$path"
        fi
        echo "removed $kind $(human "$kib") $path"
    else
        echo "would remove $kind $(human "$kib") $path (idle ${idle}h)"
    fi
    total=$((total + kib)); n=$((n + 1))
done <<<"$plan"

if ((APPLY)); then
    echo "freed $(human "$total") in $n item(s); $skipped big item(s) kept"
else
    echo "would free $(human "$total") in $n item(s); $skipped big item(s) kept (dry run; pass --apply to delete)"
fi
