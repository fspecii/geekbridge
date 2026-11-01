#!/usr/bin/env bash
# Type-check (and lint) the macOS-only code for both Apple targets with the private toolchain.
#
# What this proves: the objc2/IOBluetooth/CoreAudio bindings are used with correct Rust types and
# the delegate protocol impls match the generated declarations. What it does NOT prove: that the
# binary links against the real frameworks, that macOS delivers callbacks as assumed, TCC behaviour,
# or that any audio flows. `cargo check` stops before the linker, so no Apple SDK is required.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=check-env.sh
source "$here/check-env.sh"
root="$(cd "$here/../../.." && pwd)"
cd "$root"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  echo "== cargo check $target"
  cargo check -p bridge-calls-macos --target "$target" --lib --bins --tests --features mock
done
echo "== cargo clippy aarch64-apple-darwin (-D warnings)"
cargo clippy -p bridge-calls-macos --target aarch64-apple-darwin --lib --bins --tests \
  --features mock -- -D warnings
echo "== cargo clippy host (-D warnings)"
cargo clippy -p bridge-calls-macos --all-targets --features mock -- -D warnings
echo "== cargo fmt --check"
cargo fmt -p bridge-calls-macos -- --check
echo "== cargo test (host)"
cargo test -p bridge-calls-macos --features mock
