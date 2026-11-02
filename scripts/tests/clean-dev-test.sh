#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Scratch-tree test for scripts/clean-dev.sh and scripts/disk-report.sh: fake repo, fake HOME, fake
# processes. Never touches the real tree. Run: scripts/tests/clean-dev-test.sh
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# Under GB_SCRATCH, not /tmp: /tmp is often quota-limited, and the marker keeps it ours.
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/cleantest-XXXXXX")"; : >"$T/.gb-owned"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf -- "$T"; }
trap cleanup EXIT
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }

R="$T/repo" H="$T/home" TMPD="$T/tmp" OUT="$T/outside"
mkdir -p "$R/scripts/lib" "$R/target" "$H/.cache/gb-scratch" "$TMPD" "$OUT" "$R/.tools/android-user/avd"
cp "$src"/scripts/clean-dev.sh "$src"/scripts/disk-report.sh "$R/scripts/"; cp "$src"/scripts/lib/disk.sh "$R/scripts/lib/"
echo '[workspace]' >"$R/Cargo.toml"
mk() { mkdir -p "$1"; dd if=/dev/zero of="$1/blob" bs=1M count="${2:-3}" status=none; }
old() { touch -d '3 days ago' "$1"/blob; [[ -f "$1/.gb-owned" ]] && touch -d '3 days ago' "$1/.gb-owned"; touch -d '3 days ago' "$1"; return 0; }

for d in debug release android; do mk "$R/target/$d"; old "$R/target/$d"; done
mk "$R/target/stale"; old "$R/target/stale"                       # should go
mk "$R/target/young"                                               # idle too short
mk "$R/target/busy/sub"; old "$R/target/busy/sub"; touch -d '3 days ago' "$R/target/busy"   # cwd of a process
mk "$R/target/fdheld"; old "$R/target/fdheld"                      # open file of a process
mk "$R/target/tiny" 0; old "$R/target/tiny"                        # below size floor
mk "$R/target/argref"; old "$R/target/argref"                      # named in a process cmdline
ln -s "$OUT" "$R/target/link"                                      # symlink out of tree
echo precious >"$OUT/precious"
mk "$H/.cache/gb-scratch/task-99999"; : >"$H/.cache/gb-scratch/task-99999/.gb-owned"; old "$H/.cache/gb-scratch/task-99999"
mk "$H/.cache/gb-scratch/unmarked"; old "$H/.cache/gb-scratch/unmarked"       # not ours: no marker
mk "$H/.cache/gb-old"; old "$H/.cache/gb-old"
mk "$H/.cache/other-app"; old "$H/.cache/other-app"                           # not gb-*
for a in gb36x gbcall runavd keepme; do mk "$R/.tools/android-user/avd/$a.avd"; echo "path=x" >"$R/.tools/android-user/avd/$a.ini"; old "$R/.tools/android-user/avd/$a.avd"; done
mk "$TMPD/gb-foo.abc123"; old "$TMPD/gb-foo.abc123"
mk "$TMPD/gb-notmine"; old "$TMPD/gb-notmine"                                 # gb- but not our pattern
mk "$TMPD/random"; old "$TMPD/random"

# fake processes
(cd "$R/target/busy/sub" && exec sleep 300) & pids+=($!)
(exec 9<"$R/target/fdheld/blob"; exec sleep 300) & pids+=($!)
(exec -a "fake-builder --target-dir $R/target/argref" sleep 300) & pids+=($!)
(exec -a "qemu-system-x86_64 -avd runavd -port 5554" sleep 300) & pids+=($!)
sleep 0.5

run() { env -i PATH="$PATH" HOME="$H" TMPDIR="$TMPD" GB_TMP="$TMPD" GB_ROOT="$R" GB_MIN_ITEM_MB=1 "$R/scripts/clean-dev.sh" "$@"; }

dry="$(run --older-than 12 --keep-avd keepme)"
echo "$dry" | sed 's/^/    /'
exists() { [[ -e "$1" ]]; }
for x in "$R/target/debug" "$R/target/stale"; do exists "$x" || bad "dry run removed $x"; done
echo "$dry" | grep -q 'would free' && ok "dry run prints would free" || bad "no 'would free' line"
echo "$dry" | grep -q "would remove target .*target/stale" && ok "stale target planned" || bad "stale target not planned"
for x in busy fdheld argref young tiny debug release android link; do
    echo "$dry" | grep -q "would remove .*target/$x " && bad "target/$x wrongly planned"
done; ok "busy/fd/arg/young/tiny/normal/symlink dirs not planned"
echo "$dry" | grep -q "would remove avd .*gb36x.avd" && ok "idle AVD planned" || bad "gb36x not planned"
for a in gbcall runavd keepme; do echo "$dry" | grep -q "would remove avd .*$a.avd" && bad "AVD $a wrongly planned"; done
echo "$dry" | grep -q "would remove cache .*task-99999" && ok "marked scratch planned" || bad "scratch not planned"
echo "$dry" | grep -q "unmarked\|other-app\|gb-notmine\|random" && echo "$dry" | grep -q "would remove.*\(unmarked\|other-app\|gb-notmine\|random\)" && bad "foreign path planned"
echo "$dry" | grep -q "would remove tmp .*gb-foo.abc123" && ok "tmp gb_mktemp name planned" || bad "tmp not planned"

run --apply --older-than 12 --keep-avd keepme >"$T/apply.out" 2>&1 || { cat "$T/apply.out"; bad "apply failed"; }; cat "$T/apply.out" | sed "s/^/    /"
for x in "$R/target/stale" "$R/.tools/android-user/avd/gb36x.avd" "$R/.tools/android-user/avd/gb36x.ini" "$H/.cache/gb-scratch/task-99999" "$H/.cache/gb-old" "$TMPD/gb-foo.abc123"; do
    exists "$x" && bad "not removed: $x"
done
for x in debug release android young busy/sub fdheld tiny argref link; do exists "$R/target/$x" || bad "wrongly removed target/$x"; done
for x in "$R/.tools/android-user/avd/gbcall.avd" "$R/.tools/android-user/avd/runavd.avd" "$R/.tools/android-user/avd/keepme.avd" "$H/.cache/gb-scratch/unmarked" "$H/.cache/other-app" "$TMPD/gb-notmine" "$TMPD/random" "$OUT/precious"; do
    exists "$x" || bad "wrongly removed $x"
done
exists "$R/target/busy/sub/blob" && ok "process-cwd dir survived --apply"
echo "$dry" >/dev/null
rep="$(env -i PATH="$PATH" HOME="$H" TMPDIR="$TMPD" GB_TMP="$TMPD" GB_ROOT="$R" GB_MIN_ITEM_MB=1 "$R/scripts/disk-report.sh" 2>&1)" || bad "report failed"
echo "$rep" | grep -q 'safe to delete now' && ok "report runs" || bad "report output"
[[ $fail == 0 ]] && echo "ALL PASSED" || { echo "SOME FAILED"; exit 1; }
