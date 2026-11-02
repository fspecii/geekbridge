#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Fuzz GeekBridge's network-facing parsers.
#
#   scripts/fuzz.sh [target|all] [seconds]          coverage-guided libFuzzer run (ASAN), default 300 s each
#   scripts/fuzz.sh --det [target|all] [seconds]    deterministic structure-aware mutation fuzzer
#   scripts/fuzz.sh --seed                          regenerate fuzz/corpus from the committed fixtures
#   scripts/fuzz.sh --list
#
# One job at a time and niced, because emulators share the machine. Logs and crash files go to
# dist/fuzz/<timestamp>/. libFuzzer needs `cargo-fuzz`; without network, run --det (the same
# targets) or `cargo test -p bridge-fuzz`. See docs/fuzzing.md.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib-tmp.sh
[ -f scripts/lib-tmp.sh ] && . scripts/lib-tmp.sh || true
export TMPDIR="${TMPDIR:-$HOME/.cache/gb-tmp}"; mkdir -p "$TMPDIR"

mode=lib
case "${1:-}" in
  --det) mode=det; shift ;;
  --seed) exec cargo run -q -p bridge-fuzz --bin gb-fuzz -- seed ;;
  --list) exec cargo run -q -p bridge-fuzz --bin gb-fuzz -- list ;;
esac
target="${1:-all}"; secs="${2:-300}"
ts="$(date +%Y%m%d-%H%M%S)"; out="dist/fuzz/$ts"; mkdir -p "$out"
echo "logs: $out"

if [ "$mode" = det ]; then
  nice -n 10 cargo run -q --release -p bridge-fuzz --bin gb-fuzz -- run "$target" --seconds "$secs" --seed "${FUZZ_SEED:-1}" --out "$out" 2>&1 | tee "$out/det.log"
  exit "${PIPESTATUS[0]}"
fi

if ! command -v cargo-fuzz >/dev/null; then
  CF="${GB_FUZZ_CARGO_HOME:-$HOME/.cache/gb-fuzz-cargo}"
  [ -x "$CF/bin/cargo-fuzz" ] || CARGO_HOME="$CF" cargo install cargo-fuzz --locked
  PATH="$CF/bin:$PATH"
fi
# The distribution toolchain is stable; RUSTC_BOOTSTRAP lets it accept the sanitizer flags.
export RUSTC_BOOTSTRAP=1
if [ "$target" = all ]; then
  targets=$(cargo run -q -p bridge-fuzz --bin gb-fuzz -- list)
else targets="$target"; fi
rc=0
for t in $targets; do
  echo "== $t ($secs s)"
  ( cd fuzz && nice -n 10 cargo fuzz run "$t" --sanitizer address -- \
      -max_total_time="$secs" -timeout=10 -rss_limit_mb=2048 -max_len=65536 \
      -artifact_prefix="../$out/" 2>&1 ) | tee "$out/$t.log" | tail -n 4 || rc=1
done
ls "$out" | grep -E '^(crash|timeout|oom|leak)-' && rc=1 || true
exit $rc
