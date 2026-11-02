#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install a PRIVATE Rust toolchain with the Apple std targets for type-checking bridge-calls-macos
# on a non-Mac host. Everything lives under <repo>/.tools/macos-calls-check/ (git-ignored):
# its own RUSTUP_HOME and CARGO_HOME, so the shared Android rustup under .tools/rustup and the
# distro rustc are never touched. No sudo, no shell-profile edits.
#
# Needs a rustup-init binary. By default it reuses the one the Android bootstrap already downloaded
# and verified (.tools/downloads/rustup-init-1.29.2, SHA-256 pinned below); pass RUSTUP_INIT=path
# to use another, in which case the same checksum is enforced.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
d="$root/.tools/macos-calls-check"
toolchain="${GB_MACOS_CHECK_RUST:-1.99.1}"
init="${RUSTUP_INIT:-$root/.tools/downloads/rustup-init-1.29.2}"
want_sha="5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4"
[[ -x "$init" ]] || { echo "rustup-init not found at $init (run scripts/bootstrap-android.sh first or set RUSTUP_INIT)" >&2; exit 66; }
got="$(sha256sum "$init" | cut -d' ' -f1)"
[[ "$got" == "$want_sha" ]] || { echo "rustup-init checksum mismatch ($got)" >&2; exit 65; }
mkdir -p "$d/rustup" "$d/cargo" "$d/target"
RUSTUP_HOME="$d/rustup" CARGO_HOME="$d/cargo" "$init" -y --no-modify-path --profile minimal \
  --default-toolchain "$toolchain" -t aarch64-apple-darwin -t x86_64-apple-darwin \
  -c clippy -c rustfmt
echo "private toolchain ready in $d (source crates/bridge-calls-macos/scripts/check-env.sh)"
