#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Test coverage report (docs/coverage.md). Thresholds there are targets, not gates: this exits 0 whenever the
# tests pass and the report was produced.
#
#   scripts/coverage.sh [rust|android|all]      default: all
#
# Rust needs cargo-llvm-cov (`cargo install cargo-llvm-cov --locked`) and the llvm-tools component
# (`rustup component add llvm-tools-preview`); with a distro rustc point LLVM_COV / LLVM_PROFDATA at llvm-cov and
# llvm-profdata of the same LLVM major version as `rustc -vV`. Android needs the JDK 17 + SDK that `make android`
# uses and downloads JaCoCo (version pinned in android/app/build.gradle.kts, hashes in verification-metadata.xml).
# Output: target/llvm-cov/html (Rust), android/app/build/reports/jacoco/jacocoDebugUnitTestReport (Android).
# CARGO_TARGET_DIR / TMPDIR are honoured, so a scratch target dir keeps your normal build cache untouched.
set -euo pipefail
cd "$(dirname "$0")/.."
what="${1:-all}"
# shellcheck disable=SC1091
source scripts/lib/scratch.sh; gb_preflight "$PWD" || exit 1   # instrumented builds are huge; GB_MIN_FREE_GB, default 15

rust() {
  command -v cargo-llvm-cov >/dev/null || { echo "cargo-llvm-cov is not installed (see the header of $0)" >&2; return 1; }
  # Debug info is not needed for line coverage and makes the instrumented build several times larger.
  export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
  cargo llvm-cov clean --workspace
  # --test-threads=4: instrumented code is slow enough that the cross-process lock tests time out on a busy machine.
  GEEKBRIDGE_REQUIRE_DBUS="${GEEKBRIDGE_REQUIRE_DBUS:-0}" cargo llvm-cov --workspace --locked --no-report --no-fail-fast -- --test-threads=4
  cargo llvm-cov report --summary-only
  cargo llvm-cov report --html
  cargo llvm-cov report --lcov --output-path "${TMPDIR:-/tmp}/geekbridge-cov.lcov"
  echo; echo "== production code only (inline test modules excluded) =="
  python3 -I scripts/coverage-summary.py "${TMPDIR:-/tmp}/geekbridge-cov.lcov" --files 15
}

android() {
  (cd android && ./gradlew --console=plain --no-daemon -Pgb.coverage=true :app:jacocoDebugUnitTestReport)
  python3 -I scripts/coverage-android.py android/app/build/reports/jacoco/jacocoDebugUnitTestReport/jacocoDebugUnitTestReport.csv
}

case "$what" in
  rust) rust ;;
  android) android ;;
  all) rust; android ;;
  *) echo "usage: $0 [rust|android|all]" >&2; exit 2 ;;
esac
