#!/usr/bin/env bash
# Build the Linux desktop binaries (release) and copy them to dist/linux/.
#
#   scripts/build-desktop.sh [--locked]
#
# Builds only `bridge-service` and `bridge-apps` (not the whole workspace) with
# the host cargo. Produces Linux x86_64 executables only; no Windows/macOS
# binaries are built or implied.
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
DIST="$ROOT/dist/linux"

BINARIES=(
    geekbridge-service
    geekbridge-ctl
    geekbridge-calls
    geekbridge-messages
    geekbridge-clipboard
    geekbridge-agenda
    geekbridge-contacts
)

CARGO_FLAGS=()
for arg in "$@"; do
    case "$arg" in
        --locked) CARGO_FLAGS+=(--locked) ;;
        -h|--help) sed -n '2,8p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

command -v cargo >/dev/null || { echo "error: cargo not found on PATH" >&2; exit 1; }

cd "$ROOT"
# Use the default target dir so desktop artifacts do not mix with target/android.
unset CARGO_TARGET_DIR
TARGET_DIR="${ROOT}/target"

echo "==> $(cargo -V) / $(rustc -V)"
cargo build --release -p bridge-service -p bridge-apps "${CARGO_FLAGS[@]}"

rm -rf "$DIST"
mkdir -p "$DIST"
for bin in "${BINARIES[@]}"; do
    src="$TARGET_DIR/release/$bin"
    [[ -x "$src" ]] || { echo "error: expected binary missing: $src" >&2; exit 1; }
    install -m 0755 "$src" "$DIST/$bin"
done

(cd "$DIST" && sha256sum -- "${BINARIES[@]}" | tee SHA256SUMS)
tar -czf "$ROOT/dist/geekbridge-linux-x86_64.tar.gz" -C "$DIST" .
echo "==> Linux x86_64 binaries in $DIST"
echo "==> Archive: $ROOT/dist/geekbridge-linux-x86_64.tar.gz"
