#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Remove GeekBridge-owned scratch left in the temp dir. Dry run by default.
#
#   scripts/clean-tmp.sh [--apply] [--older-than MINUTES] [--emu]
#
# Looks in $GB_TMP (default $TMPDIR, else /tmp) for names our scripts and tests create:
#   gb-<name>.XXXXXX         scripts/lib-tmp.sh (gb_mktemp)
#   gb-test-*                Rust test scratch (GEEKBRIDGE_TEST_TMP)
#   geekbridge-emulate-call.*, gb-callui-xdg-<uid>     emulate-call.sh, e2e/call-ui.sh
# Only entries owned by you, and only those patterns: never anything else in the directory.
# --older-than skips entries modified within the last MINUTES (default 60), so a running script's
# scratch is left alone. --emu also empties the project's emulator temp dirs (.tools/*/tmp), unless
# an emulator is running. Nothing is deleted without --apply.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BASE="${GB_TMP:-${TMPDIR:-/tmp}}"
APPLY=0 AGE=60 EMU=0
while (($#)); do
    case "$1" in
        --apply) APPLY=1 ;;
        --older-than) AGE="$2"; shift ;;
        --emu) EMU=1 ;;
        -h|--help) sed -n '3,16p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
[[ "$AGE" =~ ^[0-9]+$ ]] || { echo "--older-than needs a number of minutes" >&2; exit 2; }
[[ -d "$BASE" ]] || { echo "nothing to do: $BASE does not exist"; exit 0; }

n=0 total=0
consider() {   # $1 = path
    local p="$1"
    [[ -O "$p" ]] || return 0
    [[ -n "$(find "$p" -maxdepth 0 -mmin "-$AGE" 2>/dev/null)" ]] && { echo "keep    $p (touched < ${AGE} min ago)"; return 0; }
    local kib; kib="$(du -sk "$p" 2>/dev/null | cut -f1)"; kib="${kib:-0}"
    n=$((n + 1)); total=$((total + kib))
    if ((APPLY)); then rm -rf -- "$p" && echo "removed $p (${kib} KiB)"; else echo "would remove $p (${kib} KiB)"; fi
}

shopt -s nullglob
for p in "$BASE"/gb-*.?????? "$BASE"/gb-test-* "$BASE"/geekbridge-emulate-call.* "$BASE/gb-callui-xdg-$UID"; do
    [[ -e "$p" || -L "$p" ]] || continue
    b="${p##*/}"
    # gb_mktemp names are gb-<name>.<6 random chars>; anything else starting with gb- is not ours.
    if [[ "$b" == gb-* && "$b" != gb-test-* && "$b" != gb-callui-xdg-* && ! "$b" =~ ^gb-[A-Za-z0-9_-]+\.[A-Za-z0-9]{6}$ ]]; then continue; fi
    consider "$p"
done

if ((EMU)); then
    if pgrep -f 'qemu-system|emulator/emulator' >/dev/null 2>&1; then
        echo "skip    emulator temp dirs: an emulator is running"
    else
        for d in "$ROOT"/.tools/*/tmp; do
            [[ -d "$d" ]] || continue
            for p in "$d"/* "$d"/.[!.]*; do [[ -e "$p" || -L "$p" ]] && consider "$p"; done
        done
    fi
fi

verb="would free"; ((APPLY)) && verb="freed"
echo "$verb $((total / 1024)) MiB in $n entr$([[ $n == 1 ]] && echo y || echo ies) under $BASE$( ((APPLY)) || echo ' (dry run; pass --apply)')"
