#!/usr/bin/env bash
# Source this file (do not execute) to put the project-local Android/Rust/Java
# toolchain on PATH for the current shell only. Nothing global is modified.
#
#   source scripts/android-env.sh
#
# Intentionally does not call `set -e`/`set -u` so it cannot change the caller's
# shell options.

_gb_env_src="${BASH_SOURCE[0]:-$0}"
GB_ROOT="$(cd "$(dirname "$_gb_env_src")/.." && pwd -P)"
unset _gb_env_src
export GB_ROOT

# ---- version pins (keep in sync with docs/android-contract.md) -------------
export GB_JDK_VERSION="17.0.21+9"
export GB_GRADLE_VERSION="8.13"
export GB_RUST_VERSION="1.99.1"
export GB_RUSTUP_VERSION="1.29.2"
export GB_CMDLINE_TOOLS_REV="16111833"        # cmdline-tools;23.0 ("latest")
export GB_PLATFORM="android-36"
export GB_BUILD_TOOLS="36.0.0"
export GB_NDK_VERSION="30.0.16248370"
export GB_ANDROID_API="26"                    # minSdk == NDK clang API level
export GB_EMULATOR_IMAGE="system-images;android-36;google_apis;x86_64"

# ---- project-local layout --------------------------------------------------
export GB_TOOLS="$GB_ROOT/.tools"
export GB_JDK_HOME="$GB_TOOLS/jdk/jdk-$GB_JDK_VERSION"
export GB_GRADLE_HOME="$GB_TOOLS/gradle/gradle-$GB_GRADLE_VERSION"

export JAVA_HOME="$GB_JDK_HOME"
export ANDROID_HOME="$GB_TOOLS/android-sdk"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export ANDROID_USER_HOME="$GB_TOOLS/android-user"
export ANDROID_AVD_HOME="$ANDROID_USER_HOME/avd"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$GB_NDK_VERSION"
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
export GRADLE_USER_HOME="$GB_TOOLS/gradle-home"
export RUSTUP_HOME="$GB_TOOLS/rustup"
export CARGO_HOME="$GB_TOOLS/cargo"
export RUSTUP_TOOLCHAIN="$GB_RUST_VERSION-x86_64-unknown-linux-gnu"
# Keep Android artifacts apart from the distro-rustc desktop build in target/.
export CARGO_TARGET_DIR="${GB_ANDROID_TARGET_DIR:-$GB_ROOT/target/android}"

GB_NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export GB_NDK_BIN

# Per-target cargo configuration (env only; no .cargo/config.toml is written).
# max-page-size=16384 yields 16 KiB-aligned ELF segments for Android 15+ devices.
_gb_flags="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$GB_NDK_BIN/aarch64-linux-android${GB_ANDROID_API}-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$GB_NDK_BIN/x86_64-linux-android${GB_ANDROID_API}-clang"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="$_gb_flags"
export CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS="$_gb_flags"
unset _gb_flags
# For any `cc`-based build scripts in dependencies.
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$GB_NDK_BIN/llvm-ar"
export AR_x86_64_linux_android="$GB_NDK_BIN/llvm-ar"

export PATH="$CARGO_HOME/bin:$JAVA_HOME/bin:$GB_GRADLE_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/build-tools/$GB_BUILD_TOOLS:$GB_NDK_BIN:$PATH"
