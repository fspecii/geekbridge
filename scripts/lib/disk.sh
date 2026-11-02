# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Shared by scripts/disk-report.sh and scripts/clean-dev.sh (source it; do not run it).
# Enumerates the disk hogs GeekBridge scripts create and decides, by rule, whether each is safe to delete:
#   safe = owned by us AND regenerable AND idle > GB_IDLE_HOURS AND big enough to matter AND
#          not referenced by any running process AND not protected.
# Nothing here deletes anything.
#
# Inputs (all overridable, which is how the tests run in a scratch tree):
#   GB_ROOT (repo), HOME, GB_SCRATCH, GB_TMP (default $TMPDIR or /tmp), ANDROID_AVD_HOME,
#   GB_IDLE_HOURS (default 12), GB_MIN_ITEM_MB (default 50; smaller items are listed but never cleaned),
#   GB_KEEP_AVDS (space separated, always protected; default "gbbase gbcall gbnothing gbhome"),
#   GB_KEEP_TARGET_DIRS (default "debug release android": the normal builds)
GB_ROOT="${GB_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)}"
GB_SCRATCH="${GB_SCRATCH:-${HOME:?}/.cache/gb-scratch}"
GB_TMP="${GB_TMP:-${TMPDIR:-/tmp}}"
ANDROID_AVD_HOME="${ANDROID_AVD_HOME:-$GB_ROOT/.tools/android-user/avd}"
GB_IDLE_HOURS="${GB_IDLE_HOURS:-12}"
GB_MIN_ITEM_MB="${GB_MIN_ITEM_MB:-50}"
GB_KEEP_AVDS="${GB_KEEP_AVDS:-gbbase gbcall gbnothing gbhome}"
GB_KEEP_TARGET_DIRS="${GB_KEEP_TARGET_DIRS:-debug release android}"

# --- running-process snapshot ---------------------------------------------------------------------
# One line per (cwd | cmdline | environ entry | open fd target) of every process we can read.
# lsof-free: only /proc. Our own pid is skipped so our arguments do not pin what we examine.
gb_snapshot() {   # $1 = output file
    local d pid f
    : >"$1"
    for d in /proc/[0-9]*; do
        pid="${d#/proc/}"
        [[ "$pid" == "$$" || "$pid" == "${BASHPID:-}" ]] && continue
        [[ -r "$d/cmdline" ]] || continue
        {
            readlink -- "$d/cwd" 2>/dev/null
            tr '\0' '\n' <"$d/cmdline" 2>/dev/null
            tr '\0' '\n' <"$d/environ" 2>/dev/null
            for f in "$d"/fd/*; do readlink -- "$f" 2>/dev/null; done
        } 2>/dev/null | sed "s/^/$pid /" >>"$1" || true
        # cmdline words joined too, so "-avd NAME" / "--target-dir X" style pairs stay visible
        { printf '%s %s\n' "$pid" "$(tr '\0' ' ' <"$d/cmdline")"; } 2>/dev/null >>"$1" || true
    done
}

# gb_in_use SNAPSHOT PATH [ALT_REGEX]  -> 0 (true) when some process references PATH (cwd inside it,
# an argument/env/open file naming it or anything below it). ALT_REGEX is an extra extended regex
# (used for relative spellings like target/NAME).
gb_in_use() {
    local snap="$1" p="$2" alt="${3:-}" esc
    esc="$(printf '%s' "$p" | sed 's/[][\.*^$+?(){}|]/\\&/g')"
    grep -Eq "(^[0-9]+ |[^[:alnum:]_./-])${esc}(/|[[:space:]]|\$)" "$snap" && return 0
    [[ -n "$alt" ]] && grep -Eq "$alt" "$snap" && return 0
    return 1
}

gb_item_pid_alive() {   # name ending in -<pid> whose process is alive
    local pid="${1##*-}"
    [[ "$pid" =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null
}

# newest mtime (epoch seconds) within the first levels of a tree; cheap enough for 100 GB cargo dirs
gb_newest() {
    local out
    out="$(find "$1" -xdev -maxdepth 3 -printf '%T@\n' 2>/dev/null | sort -n | tail -1)"
    printf '%s' "${out%.*}"
}

gb_kib() { du -sk --one-file-system -- "$1" 2>/dev/null | cut -f1; }

# gb_enumerate [KINDS]   KINDS = space list of target avd cache tmp (default all). Prints TSV, one item per line:
#   kind  path  kib  idle_hours  verdict(yes|no)  reason
# "yes" means the cleaner may delete it. Report-only categories always say "no (report only)".
gb_enumerate() {
    local kinds="${1:-target avd cache tmp}" snap now
    snap="$(mktemp "${TMPDIR:-/tmp}/gb-snap.XXXXXX")"; now="$(date +%s)"
    # shellcheck disable=SC2064
    trap "rm -f -- '$snap'" RETURN
    gb_snapshot "$snap"
    local nosnap=0; [[ -s "$snap" ]] || nosnap=1   # fail closed: no process view, no deletions

    # emit kind path owned(1|0) protected_reason alt_regex
    local k p kib newest idle verdict reason prot owned alt
    _gb_emit() {
        k="$1" p="$2" owned="$3" prot="$4" alt="${5:-}"
        [[ -e "$p" || -L "$p" ]] || return 0
        kib="$(gb_kib "$p")"; kib="${kib:-0}"
        newest="$(gb_newest "$p")"; newest="${newest:-$now}"
        idle=$(( (now - newest) / 3600 )); ((idle < 0)) && idle=0
        verdict=yes reason="regenerable, idle ${idle}h"
        if [[ -L "$p" ]]; then verdict=no reason="symlink (never followed)"
        elif [[ "$owned" != 1 ]]; then verdict=no reason="report only"
        elif [[ -n "$prot" ]]; then verdict=no reason="protected: $prot"
        elif ((nosnap)); then verdict=no reason="cannot read /proc"
        elif gb_in_use "$snap" "$p" "$alt"; then verdict=no reason="in use by a running process"
        elif ((idle < GB_IDLE_HOURS)); then verdict=no reason="idle ${idle}h < ${GB_IDLE_HOURS}h"
        elif ((kib / 1024 < GB_MIN_ITEM_MB)); then verdict=no reason="small (< ${GB_MIN_ITEM_MB} MB)"
        fi
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$k" "$p" "$kib" "$idle" "$verdict" "$reason"
    }

    for k in $kinds; do
        case "$k" in
        target)
            if [[ -d "$GB_ROOT/target" ]]; then
                for p in "$GB_ROOT"/target/*/; do
                    p="${p%/}"; local b="${p##*/}"
                    [[ -d "$p" || -L "$p" ]] || continue
                    prot=""; [[ " $GB_KEEP_TARGET_DIRS " == *" $b "* ]] && prot="normal build dir"
                    _gb_emit target "$p" 1 "$prot" "(^[0-9]+ |[^[:alnum:]_./-])target/${b//./\\.}(/|[[:space:]]|\$)"
                done
            fi ;;
        avd)
            if [[ -d "$ANDROID_AVD_HOME" ]]; then
                for p in "$ANDROID_AVD_HOME"/*.avd; do
                    [[ -d "$p" || -L "$p" ]] || continue
                    local n="${p##*/}"; n="${n%.avd}"
                    prot=""
                    [[ " $GB_KEEP_AVDS " == *" $n "* ]] && prot="kept AVD (GB_KEEP_AVDS / --keep-avd)"
                    if [[ -z "$prot" ]] && { pgrep -af "(qemu|emulator).*(-avd |@)${n}( |\$)|/${n}\.avd" 2>/dev/null | grep -v 'pgrep' | grep -q .; }; then
                        prot="emulator running"
                    fi
                    _gb_emit avd "$p" 1 "$prot"
                done
            fi ;;
        cache)
            for p in "$HOME"/.cache/gb-*; do
                [[ -e "$p" || -L "$p" ]] || continue
                if [[ "$p" == "$GB_SCRATCH" ]]; then
                    for p in "$GB_SCRATCH"/*; do
                        [[ -e "$p" || -L "$p" ]] || continue
                        owned=0; prot=""
                        if [[ -f "$p/.gb-owned" || "${p##*/}" == target-shared ]]; then owned=1; fi
                        gb_item_pid_alive "${p##*/}" && prot="task pid alive"
                        _gb_emit cache "$p" "$owned" "$prot"
                    done
                    p="$GB_SCRATCH"
                else
                    _gb_emit cache "$p" 1 ""
                fi
            done
            # Gradle caches: report only (slow to re-download, rarely the problem)
            for p in "$GB_ROOT/.tools/gradle-home/caches" "$GB_ROOT/.tools/gradle-home/daemon" "$HOME/.gradle/caches"; do
                if [[ -d "$p" ]]; then _gb_emit gradle "$p" 0 ""; fi
            done ;;
        tmp)
            local g
            for g in "$GB_TMP"/gb-* "$GB_TMP"/geekbridge-emulate-call.*; do
                [[ -e "$g" || -L "$g" ]] || continue
                local b="${g##*/}"
                [[ -O "$g" || -L "$g" ]] || continue
                # our names only: gb-<name>.<6 chars> (lib-tmp.sh), gb-test-*, gb-callui-xdg-<uid>, geekbridge-emulate-call.*
                if [[ "$b" =~ ^gb-[A-Za-z0-9_-]+\.[A-Za-z0-9]{6}$ || "$b" == gb-test-* || "$b" == "gb-callui-xdg-$UID" || "$b" == geekbridge-emulate-call.* ]]; then
                    [[ "$b" == gb-snap.* ]] && continue
                    _gb_emit tmp "$g" 1 ""
                fi
            done ;;
        esac
    done
}

# Report-only extras: dist/ (build outputs, may hold deliverables)
gb_enumerate_extra() {
    local now p; now="$(date +%s)"
    for p in "$GB_ROOT/dist" "$GB_ROOT/.tools/downloads"; do
        [[ -d "$p" ]] || continue
        local kib newest; kib="$(gb_kib "$p")"; newest="$(gb_newest "$p")"
        printf 'other\t%s\t%s\t%s\tno\treport only\n' "$p" "${kib:-0}" "$(( (now - ${newest:-$now}) / 3600 ))"
    done
}
