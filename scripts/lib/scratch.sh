# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Disk-hygiene helpers for GeekBridge scripts (source it; do not run it). Documented in docs/disk-hygiene.md.
#
#   GB_SCRATCH       the ONE root for per-task scratch (default $HOME/.cache/gb-scratch)
#   GB_MIN_FREE_GB   gb_preflight refuses below this much free space (default 15)
#   GB_TARGET_CAP_GB gb_shared_target warns when the shared cargo target dir exceeds this (default 40)
#   GB_KEEP_SCRATCH=1  keep the per-task scratch dir on exit (default: removed when the script succeeds,
#                      kept on failure for debugging; scripts/clean-dev.sh reaps it once idle)
#
# Ownership convention (what scripts/clean-dev.sh may delete):
#   $GB_SCRATCH/<task>-<pid>/   contains the marker file .gb-owned (written by gb_scratch)
#   $GB_SCRATCH/target-shared/  shared cargo target dir for ad-hoc builds (gb_shared_target)
#   $HOME/.cache/gb-*           other GeekBridge caches
#   <repo>/target/<name>/       per-task cargo dirs of older scripts (never debug, release, android)
#   /tmp/gb-*.XXXXXX etc.       scripts/lib-tmp.sh names
GB_SCRATCH="${GB_SCRATCH:-${HOME:?}/.cache/gb-scratch}"
GB_MIN_FREE_GB="${GB_MIN_FREE_GB:-15}"
GB_TARGET_CAP_GB="${GB_TARGET_CAP_GB:-40}"

# gb_free_gb [PATH]  -> whole GB free on the filesystem holding PATH (nearest existing parent)
gb_free_gb() {
    local p="${1:-$GB_SCRATCH}"
    while [[ ! -e "$p" && "$p" != / && "$p" != . ]]; do p="$(dirname -- "$p")"; done
    df -Pk -- "$p" 2>/dev/null | awk 'NR==2 {print int($4 / 1048576)}'
}

# gb_preflight [PATH...]  Refuse (return 1, message on stderr) when any filesystem holding the given
# paths (default: GB_SCRATCH and the current directory) has < GB_MIN_FREE_GB free. Use before
# emulators and big builds:  gb_preflight || exit 1
gb_preflight() {
    local p free rc=0
    (($#)) || set -- "$GB_SCRATCH" "$PWD"
    for p in "$@"; do
        free="$(gb_free_gb "$p")"; free="${free:-0}"
        if ((free < GB_MIN_FREE_GB)); then
            echo "refusing to start: only ${free} GB free under $p (need GB_MIN_FREE_GB=${GB_MIN_FREE_GB})." >&2
            echo "  free space with:  make disk   then   make clean-dev   (dry run; add APPLY=1 to delete)" >&2
            echo "  or lower the bar: GB_MIN_FREE_GB=<n>" >&2
            rc=1
        fi
    done
    return "$rc"
}

# Chain a command in front of the EXIT trap without clobbering an existing one.
_gb_trap_prepend() {   # $1 = function name to call (receives the exit status)
    local fn="$1" prev=""
    eval "set -- $(trap -p EXIT)"
    prev="${3:-}"
    # shellcheck disable=SC2064
    trap "$fn \$?; $prev" EXIT
}

_gb_scratch_dirs=()
_gb_scratch_exit() {
    local rc="${1:-0}" d
    for d in "${_gb_scratch_dirs[@]}"; do
        [[ -d "$d" && -f "$d/.gb-owned" && "$d" == "$GB_SCRATCH"/* ]] || continue
        if [[ "${GB_KEEP_SCRATCH:-0}" == 1 ]]; then echo "scratch kept: $d" >&2
        elif ((rc == 0)); then rm -rf -- "$d"
        else echo "scratch kept for debugging (run failed): $d" >&2; fi
    done
    return "$rc"
}

# gb_scratch_init TASK  -> sets GB_SCRATCH_DIR=$GB_SCRATCH/<task>-<pid>, creating it (mode 700, marked as ours)
# and arranges for it to be removed on a successful exit. (Command substitution runs in a subshell, so
# the trap would not stick: call it directly.)
#   gb_scratch_init e2e; SCRATCH="$GB_SCRATCH_DIR"
gb_scratch_init() {
    local task="${1:?task name}"
    [[ "$task" =~ ^[A-Za-z0-9_-]+$ ]] || { echo "gb_scratch_init: bad task name '$task'" >&2; return 1; }
    GB_SCRATCH_DIR="$GB_SCRATCH/$task-$$"
    mkdir -p -m 700 "$GB_SCRATCH_DIR" && : >"$GB_SCRATCH_DIR/.gb-owned" || return 1
    [[ ${#_gb_scratch_dirs[@]} -eq 0 ]] && _gb_trap_prepend _gb_scratch_exit
    _gb_scratch_dirs+=("$GB_SCRATCH_DIR")
    export GB_SCRATCH_DIR
}

# gb_shared_target  -> point CARGO_TARGET_DIR at the shared ad-hoc build dir (unless already set) and warn
# when it has outgrown GB_TARGET_CAP_GB. Agents' and contributors' one-off builds should use this
# instead of inventing target/<name> dirs.
gb_shared_target() {
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$GB_SCRATCH/target-shared}"
    mkdir -p "$CARGO_TARGET_DIR"
    gb_target_cap_warn "$CARGO_TARGET_DIR"
}

gb_target_cap_warn() {   # $1 = dir
    local kib
    kib="$(du -sk --one-file-system -- "$1" 2>/dev/null | cut -f1)"; kib="${kib:-0}"
    if ((kib / 1048576 >= GB_TARGET_CAP_GB)); then
        echo "warning: $1 is $((kib / 1048576)) GB (cap ${GB_TARGET_CAP_GB} GB). Reclaim with: make clean-dev WHAT=target APPLY=1" >&2
    fi
}
