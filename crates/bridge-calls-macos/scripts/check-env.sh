# Source to use the PRIVATE macOS-check toolchain (does not touch the Android rustup).
# Everything lives under the git-ignored .tools/macos-calls-check/.
_gb_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export RUSTUP_HOME="$_gb_root/.tools/macos-calls-check/rustup"
export CARGO_HOME="$_gb_root/.tools/macos-calls-check/cargo"
export CARGO_TARGET_DIR="$_gb_root/.tools/macos-calls-check/target"
export PATH="$CARGO_HOME/bin:$PATH"
unset _gb_root
