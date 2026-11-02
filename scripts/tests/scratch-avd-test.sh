#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests for scripts/lib/scratch.sh and scripts/lib/avd.sh in a scratch HOME (no real AVDs, no emulator).
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/stest-XXXXXX")"; : >"$T/.gb-owned"
trap 'rm -rf -- "$T"' EXIT
fail=0; ok() { echo "ok   $*"; }; bad() { echo "FAIL $*"; fail=1; }
export HOME="$T/home"; mkdir -p "$HOME"
export GB_SCRATCH="$T/scratch" ANDROID_AVD_HOME="$T/avd"
mkdir -p "$ANDROID_AVD_HOME/gbbase.avd/snapshots"
printf 'path=%s/gbbase.avd\npath.rel=avd/gbbase.avd\ntarget=android-36\n' "$ANDROID_AVD_HOME" >"$ANDROID_AVD_HOME/gbbase.ini"
printf 'AvdId=gbbase\navd.ini.displayname=gbbase\nhw.ramSize=2G\n' >"$ANDROID_AVD_HOME/gbbase.avd/config.ini"
L="source '$src/scripts/lib/scratch.sh'; source '$src/scripts/lib/avd.sh';"

# scratch: removed on success, existing trap still runs
out="$(bash -c "$L trap 'echo prev-trap-ran' EXIT; gb_scratch_init t1; echo \$GB_SCRATCH_DIR >'$T/d1'; touch \$GB_SCRATCH_DIR/x")"
d="$(cat "$T/d1")"
[[ ! -e "$d" ]] && ok "scratch removed on success" || bad "scratch left after success"
[[ "$out" == *prev-trap-ran* ]] && ok "pre-existing EXIT trap preserved" || bad "previous trap lost"
# scratch: kept on failure, kept with GB_KEEP_SCRATCH
bash -c "$L gb_scratch_init t2; echo \$GB_SCRATCH_DIR >'$T/d2'; exit 3" 2>/dev/null || true
[[ -f "$(cat "$T/d2")/.gb-owned" ]] && ok "scratch kept on failure, marked ours" || bad "failed run's scratch not kept"
GB_KEEP_SCRATCH=1 bash -c "$L gb_scratch_init t3; echo \$GB_SCRATCH_DIR >'$T/d3'" 2>/dev/null
[[ -d "$(cat "$T/d3")" ]] && ok "GB_KEEP_SCRATCH=1 keeps" || bad "keep flag ignored"
# preflight
bash -c "$L GB_MIN_FREE_GB=999999; gb_preflight '$T'" 2>"$T/pf" && bad "preflight passed with huge threshold" || { grep -q 'refusing to start' "$T/pf" && ok "preflight refuses with clear message"; }
bash -c "$L GB_MIN_FREE_GB=0; gb_preflight '$T'" && ok "preflight passes with threshold 0"
# avd clone + removal
bash -c "$L gb_avd_ensure gbt1 && ls '$ANDROID_AVD_HOME' >'$T/ls1' && grep -q 'path=$ANDROID_AVD_HOME/gbt1.avd' '$ANDROID_AVD_HOME/gbt1.ini' && grep -q AvdId=gbt1 '$ANDROID_AVD_HOME/gbt1.avd/config.ini'" 2>/dev/null \
  && ok "clone created with rewritten paths" || bad "clone not created correctly"
[[ ! -e "$ANDROID_AVD_HOME/gbt1.avd" && ! -e "$ANDROID_AVD_HOME/gbt1.ini" ]] && ok "clone deleted on exit" || bad "clone left behind"
[[ ! -e "$ANDROID_AVD_HOME/gbt1.avd/snapshots" ]] || true
GB_KEEP_AVD=1 bash -c "$L gb_avd_ensure gbt2" 2>/dev/null
[[ -d "$ANDROID_AVD_HOME/gbt2.avd" ]] && ok "GB_KEEP_AVD=1 keeps clone" || bad "keep flag ignored"
# existing AVD is never removed
bash -c "$L gb_avd_ensure gbt2; echo \$GB_AVD_CLONED >'$T/c'" 2>/dev/null
[[ "$(cat "$T/c")" == 0 && -d "$ANDROID_AVD_HOME/gbt2.avd" ]] && ok "pre-existing AVD untouched" || bad "pre-existing AVD removed"
# non-clone AVD is not removed even if listed
[[ -d "$ANDROID_AVD_HOME/gbbase.avd" ]] && ok "base AVD intact" || bad "base AVD removed"
# on-exit hook runs before removal
bash -c "$L gb_avd_ensure gbt3; gb_avd_on_exit 'echo hook-ran >$T/hook'" 2>/dev/null
[[ -f "$T/hook" && ! -e "$ANDROID_AVD_HOME/gbt3.avd" ]] && ok "on-exit hook ran, clone removed" || bad "hook/removal"
[[ $fail == 0 ]] && echo "ALL PASSED" || { echo "SOME FAILED"; exit 1; }
