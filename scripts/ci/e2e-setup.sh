#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Toolchain for the emulator end-to-end workflow (.github/workflows/emulator-e2e.yml, docs/emulator-ci.md).
#
#   scripts/ci/e2e-setup.sh light   emulator + system image + adb + avdmanager only (the runner's preinstalled
#                                   cmdline-tools and Temurin 17 are reused), then the base AVD gb36
#   scripts/ci/e2e-setup.sh full    scripts/bootstrap-android.sh --with-emulator (JDK, Gradle, Rust, NDK, SDK), then gb36
#
# Everything lands in the git-ignored .tools/ exactly where scripts/android-env.sh expects it, so the e2e scripts run unchanged.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
mode="${1:?light|full}"
runner_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/usr/local/lib/android/sdk}}"
runner_jdk="${JAVA_HOME_17_X64:-}"
# shellcheck disable=SC1091
source scripts/android-env.sh
case "$mode" in
  full) scripts/bootstrap-android.sh --with-emulator ;;
  light)
    [[ -n "$runner_jdk" && -d "$runner_sdk/cmdline-tools/latest" ]] || { echo "runner JDK 17 / cmdline-tools not found" >&2; exit 1; }
    mkdir -p "$ANDROID_HOME" "$GB_TOOLS/jdk"
    [[ -e "$ANDROID_HOME/cmdline-tools" ]] || cp -a "$runner_sdk/cmdline-tools" "$ANDROID_HOME/cmdline-tools"
    [[ -e "$GB_JDK_HOME" ]] || ln -s "$runner_jdk" "$GB_JDK_HOME"
    sm="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
    yes | "$sm" --sdk_root="$ANDROID_HOME" --licenses >/dev/null 2>&1 || true
    yes | "$sm" --sdk_root="$ANDROID_HOME" platform-tools emulator "$GB_EMULATOR_IMAGE" | grep -Ev '^\[=*[ =]*\]' || true
    ;;
  *) echo "usage: $0 light|full" >&2; exit 2 ;;
esac
mkdir -p "$GB_TOOLS/gbcall"
GB_EMU_AVD=gb36 scripts/emulator-call.sh create-avd
[[ -x "$ANDROID_HOME/emulator/emulator" && -x "$ANDROID_HOME/platform-tools/adb" ]]
ls "$ANDROID_AVD_HOME"
