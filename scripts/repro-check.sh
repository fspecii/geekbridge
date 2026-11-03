#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Reproducibility audit of the Linux release tarball: build the same commit twice, each in its own clean git worktree
# at a DIFFERENT path with its own target dir, with the settings scripts/release-local.sh uses (locked deps,
# SOURCE_DATE_EPOCH, --remap-path-prefix, strip), and compare every binary and the packaged tarball byte for byte.
#
#   scripts/repro-check.sh [--rev REV] [--jobs N] [--keep]       exit 0 = identical, 1 = differs
#
#   --rev REV   commit to build (default HEAD)       --jobs N   cargo parallelism (default 4)
#   --keep      keep the build trees (default: delete them; the result files are printed first)
# Scratch: $GB_REPRO_DIR, default ~/.cache/gb-repro (never /tmp). Needs about 6 GB per build; refuses below 20 GB free.
# Method, result and what is NOT reproducible (APK signing, Gradle, the Arch package): docs/reproducible-builds.md.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
rev=HEAD jobs=4 keep=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rev) rev="${2:?}"; shift 2;;
    --jobs) jobs="${2:?}"; shift 2;;
    --keep) keep=1; shift;;
    -h|--help) sed -n '3,12p' "${BASH_SOURCE[0]}"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
source "$ROOT/scripts/lib/repro.sh"
BASE="${GB_REPRO_DIR:-$HOME/.cache/gb-repro}"
case "$BASE" in /|"$HOME"|"$HOME/") echo "unsafe GB_REPRO_DIR" >&2; exit 64;; esac
mkdir -p "$BASE"
free_gb=$(( $(df -Pk "$BASE" | awk 'NR==2{print $4}') / 1024 / 1024 ))
(( free_gb >= 20 )) || { echo "only $free_gb GB free under $BASE; need 20" >&2; exit 75; }
sha="$(git -C "$ROOT" rev-parse --verify "$rev^{commit}")"
export SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct "$sha")"
run="$BASE/run-$$"; mkdir -p "$run"
cleanup() {
  if (( keep )); then echo "kept: $run"; return; fi
  for d in "$run"/*/src; do [[ -d "$d" ]] && git -C "$ROOT" worktree remove --force "$d" 2>/dev/null || true; done
  git -C "$ROOT" worktree prune; rm -rf -- "$run"
}
trap cleanup EXIT
mapfile -t bins < <(grep -v -e '^#' -e '^[[:space:]]*$' "$ROOT/scripts/release-binaries.txt")

build() { # build NAME  -> $run/NAME/{src,out/geekbridge-linux-x86_64.tar.gz,out/plain.tar.gz}
  local n="$1" src="$run/$1/src"
  mkdir -p "$run/$n/out"
  git -C "$ROOT" worktree add -q --detach "$src" "$sha"
  (
    cd "$src"
    export RUSTFLAGS="$(gb_repro_rustflags "$src")"
    CARGO_TARGET_DIR="$src/target" CARGO_BUILD_JOBS="$jobs" cargo build --release --locked -p bridge-apps -p bridge-service
    stage="$run/$n/stage/geekbridge-linux-x86_64"; mkdir -p "$stage"
    for b in "${bins[@]}"; do cp "$src/target/release/$b" "$stage/"; done
    cp README.md "$stage/"
    # what release-local.sh did before the audit: no ordering, mtime of the build, gzip name+time header
    tar --owner=0 --group=0 --numeric-owner -czf "$run/$n/out/plain.tar.gz" -C "$run/$n/stage" geekbridge-linux-x86_64
    gb_repro_tar_gz "$run/$n/out/geekbridge-linux-x86_64.tar.gz" "$run/$n/stage" geekbridge-linux-x86_64
    # installer tarball + .deb, using the packaging script of the tree that runs this check
    "$ROOT/scripts/package-linux.sh" --bin-dir "$src/target/release" --out "$run/$n/out" --version 0.0.0-repro >/dev/null
  )
}
echo "==> commit $sha, SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH, scratch $run"
build one-a
sleep 2   # make sure build-time mtimes differ
build two-b
fail=0
echo "==> binaries"
for b in "${bins[@]}"; do
  a="$(sha256sum <"$run/one-a/src/target/release/$b" | cut -c1-64)"; c="$(sha256sum <"$run/two-b/src/target/release/$b" | cut -c1-64)"
  if [[ "$a" == "$c" ]]; then echo "same  $b $a"; else echo "DIFF  $b $a $c"; fail=1; fi
done
echo "==> tarballs"
for f in geekbridge-linux-x86_64.tar.gz geekbridge-0.0.0-repro-linux-x86_64-installer.tar.gz geekbridge_0.0.0~repro_amd64.deb plain.tar.gz; do
  a="$(sha256sum <"$run/one-a/out/$f" | cut -c1-64)"; c="$(sha256sum <"$run/two-b/out/$f" | cut -c1-64)"
  if [[ "$a" == "$c" ]]; then echo "same  $f $a"; elif [[ "$f" == plain.tar.gz ]]; then echo "differs (expected, old method) $f"; else echo "DIFF  $f $a $c"; fail=1; fi
done
exit $fail
