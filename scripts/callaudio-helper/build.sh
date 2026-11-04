#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Compile ShellProbe.java to a dex jar: build.sh OUT_DIR   (project-local SDK; nothing global is touched)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "$HERE/../android-env.sh" >/dev/null
OUT="${1:?usage: build.sh OUT_DIR}"; mkdir -p "$OUT/cls"
AJ="$ANDROID_HOME/platforms/$GB_PLATFORM/android.jar"
javac --release 17 -cp "$AJ" -d "$OUT/cls" "$HERE/ShellProbe.java"
d8 --min-api 26 --lib "$AJ" --output "$OUT" $(find "$OUT/cls" -name '*.class')
( cd "$OUT" && zip -q shellprobe.jar classes.dex )
echo "$OUT/shellprobe.jar"
