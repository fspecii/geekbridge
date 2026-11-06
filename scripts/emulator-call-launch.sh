#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Inner launcher: runs the headless emulator in the foreground (exec). Normally started by
# scripts/emulator-call.sh inside its own transient systemd user unit; run it directly only to debug.
# Extra arguments are passed to `emulator` (for example -wipe-data).
#
# Environment (all optional): GB_EMU_AVD (default gbcall), GB_EMU_PORT (console port, default 5584),
# GB_EMU_ADB_PORT (default 16037), GB_EMU_TMPDIR (default .tools/gbcall/tmp), GB_EMU_EXTRA (extra
# emulator flags, word-split).
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"
export ANDROID_ADB_SERVER_PORT="${GB_EMU_ADB_PORT:-16037}"
# A private TMPDIR keeps emulator temp files out of the (shared, quota-limited) /tmp.
export TMPDIR="${GB_EMU_TMPDIR:-$PWD/.tools/gbcall/tmp}"
mkdir -p "$TMPDIR"
# The emulator keeps its read-only overlay (hundreds of MB, growing) and crash db in ANDROID_TMP, default /tmp/android-$USER:
# on a small tmpfs that fills up and the device goes "offline" mid-run.
export ANDROID_TMP="$TMPDIR"
# shellcheck disable=SC2086
exec emulator -avd "${GB_EMU_AVD:-gbcall}" -port "${GB_EMU_PORT:-5584}" \
  -no-window -no-audio -no-snapshot -no-boot-anim \
  -gpu swiftshader_indirect -no-metrics -verbose ${GB_EMU_EXTRA:-} "$@"
