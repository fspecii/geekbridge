#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds the test-only "other app" for scripts/e2e/share.sh (a content provider and a trampoline activity) into an APK,
# using only the project-local SDK (javac, aapt2, d8, zipalign, apksigner). Never part of a release.
#   scripts/e2e/share-sender/build.sh OUT_DIR      -> OUT_DIR/share-sender.apk
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
OUT="${1:?usage: build.sh OUT_DIR}"
# shellcheck disable=SC1091
source "$HERE/../../android-env.sh" >/dev/null 2>&1
SDK="${ANDROID_HOME:-$GB_TOOLS/android-sdk}"
BT="$SDK/build-tools/$GB_BUILD_TOOLS"
JAR="$SDK/platforms/$GB_PLATFORM/android.jar"
mkdir -p "$OUT/classes" "$OUT/dex"
rm -rf "$OUT/classes"/* "$OUT/dex"/* "$OUT/base.apk" "$OUT/aligned.apk"
javac -nowarn --release 11 -classpath "$JAR" -d "$OUT/classes" "$HERE"/src/dev/geekbridge/e2e/sender/*.java
(cd "$OUT/classes" && find . -name '*.class' -print0 | xargs -0 "$BT/d8" --lib "$JAR" --min-api 26 --output "$OUT/dex")
"$BT/aapt2" link -o "$OUT/base.apk" --manifest "$HERE/AndroidManifest.xml" -I "$JAR" --min-sdk-version 26 --target-sdk-version 34
(cd "$OUT/dex" && python3 -I -c "import zipfile,sys;z=zipfile.ZipFile(sys.argv[1],'a');z.write('classes.dex');z.close()" "$OUT/base.apk")
"$BT/zipalign" -f 4 "$OUT/base.apk" "$OUT/aligned.apk"
[[ -f "$OUT/debug.keystore" ]] || keytool -genkeypair -keystore "$OUT/debug.keystore" -storepass android -keypass android \
  -alias gb -keyalg RSA -keysize 2048 -validity 3650 -dname "CN=GeekBridge e2e sender" >/dev/null 2>&1
"$BT/apksigner" sign --ks "$OUT/debug.keystore" --ks-pass pass:android --key-pass pass:android --out "$OUT/share-sender.apk" "$OUT/aligned.apk"
echo "$OUT/share-sender.apk"
