#!/usr/bin/env bash
# Type-check the whole workspace (all targets, tests included) for the Windows and macOS targets
# from a Linux machine, so Linux-only code that slipped into shared crates is caught without a
# Windows or Mac at hand.
#
#   scripts/xcheck.sh [windows] [macos-arm] [macos-x86]     (default: all three)
#
# Needs the rust-std for the targets (`rustup target add x86_64-pc-windows-msvc aarch64-apple-darwin
# x86_64-apple-darwin`) and a host `gcc`. `cargo check` never links, but `ring`'s build script wants
# to run a C compiler/assembler/archiver for the target; those are replaced here by stubs that emit
# empty objects, which is fine because nothing is linked. This proves that the code compiles, not
# that it runs: see docs/release-checklist.md for what still needs a real Windows/macOS machine.
# Set XCHECK_CLIPPY=1 to also deny clippy warnings.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

source "scripts/lib-tmp.sh"
stubs="$(gb_mktemp xcheck -d)"
trap 'rm -rf "$stubs"' EXIT
cat > "$stubs/fakecc" <<'STUB'
#!/bin/bash
out=""; prev=""
for a in "$@"; do
  case "$a" in
    /Fo*) out="${a#/Fo}";;
    /OUT:*|/out:*|-out:*|-OUT:*) out="${a#*:}";;
  esac
  [ "$prev" = "-o" ] && out="$a"
  prev="$a"
done
[ -z "$out" ] && exit 0
case "$(basename "$0")" in
  lib.exe|ar) : > "$out";;
  *) echo 'int fakecc_dummy;' | gcc -c -x c - -o "$out";;
esac
STUB
chmod +x "$stubs/fakecc"
for n in cl.exe lib.exe nasm nasm.exe ar; do ln -s fakecc "$stubs/$n"; done
export PATH="$stubs:$PATH"
export CC_x86_64_pc_windows_msvc="$stubs/cl.exe" AR_x86_64_pc_windows_msvc="$stubs/lib.exe"
export CC_aarch64_apple_darwin="$stubs/fakecc" AR_aarch64_apple_darwin="$stubs/ar"
export CC_x86_64_apple_darwin="$stubs/fakecc" AR_x86_64_apple_darwin="$stubs/ar"

want=("$@")
[ ${#want[@]} -eq 0 ] && want=(windows macos-arm macos-x86)
for name in "${want[@]}"; do
  case "$name" in
    windows) target=x86_64-pc-windows-msvc;;
    macos-arm) target=aarch64-apple-darwin;;
    macos-x86) target=x86_64-apple-darwin;;
    *) echo "unknown target '$name'" >&2; exit 2;;
  esac
  export CARGO_TARGET_DIR="${XCHECK_TARGET_DIR:-target}/xcheck-$name"
  echo "== cargo check $target"
  RUSTFLAGS="${RUSTFLAGS:-} -D warnings" cargo check --workspace --all-targets --locked --target "$target"
  if [ "${XCHECK_CLIPPY:-}" = 1 ]; then
    echo "== cargo clippy $target"
    cargo clippy --workspace --all-targets --locked --target "$target" -- -D warnings
  fi
done
echo "xcheck OK: ${want[*]}"
