#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the Android companion debug APK with the project-local toolchain.
#
#   scripts/build-android.sh [--skip-tests] [--skip-lint] [--skip-host-tests]
#
# Steps:
#   1. cargo build --profile android (opt-level z, fat LTO, stripped; see Cargo.toml) -p bridge-android for aarch64/x86_64-linux-android
#      (NDK API 26 clang linker, 16 KiB ELF page alignment)
#   2. copy libgeekbridge_android.so to android/app/src/main/jniLibs/<abi>/
#   3. gradle assembleDebug (+ testDebugUnitTest, lintDebug unless skipped)
#   4. copy the APK to dist/android/geekbridge-debug.apk and verify it
#
# Requires scripts/bootstrap-android.sh to have been run once.
set -Eeuo pipefail
IFS=$'\n\t'

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=android-env.sh
source "$HERE/android-env.sh"

RUN_TESTS=1 RUN_LINT=1 RUN_HOST_TESTS=1
for arg in "$@"; do
    case "$arg" in
        --skip-tests) RUN_TESTS=0 ;;
        --skip-lint) RUN_LINT=0 ;;
        --skip-host-tests) RUN_HOST_TESTS=0 ;;
        -h|--help) sed -n '3,14p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

ANDROID_DIR="${GB_ANDROID_DIR:-$GB_ROOT/android}"   # override only for scratch-copy validation
DIST_DIR="${GB_ANDROID_DIST_DIR:-$GB_ROOT/dist/android}"
LIB_NAME="libgeekbridge_android.so"
# rust triple : android ABI
TARGETS=("aarch64-linux-android:arm64-v8a" "x86_64-linux-android:x86_64")

[[ -x "$GB_JDK_HOME/bin/java" && -x "$GB_GRADLE_HOME/bin/gradle" && -x "$CARGO_HOME/bin/cargo" \
   && -d "$ANDROID_NDK_HOME" ]] \
    || die "toolchain missing; run scripts/bootstrap-android.sh first"
[[ -f "$ANDROID_DIR/settings.gradle.kts" || -f "$ANDROID_DIR/settings.gradle" ]] \
    || die "no Gradle project in $ANDROID_DIR"

cd "$GB_ROOT"
log "rustc: $(rustc -V); NDK $GB_NDK_VERSION; JDK $(java -version 2>&1 | head -1)"

# ---- 1. native libraries -----------------------------------------------------
if (( RUN_HOST_TESTS )); then
    log "host unit tests: bridge-android"
    cargo test -p bridge-android --target x86_64-unknown-linux-gnu
fi

for entry in "${TARGETS[@]}"; do
    triple="${entry%%:*}"
    log "cargo build --profile android -p bridge-android --target $triple"
    cargo build --profile android -p bridge-android --target "$triple"
done

# ---- 2. stage into jniLibs (ignored by git) -----------------------------------
JNI_DIR="$ANDROID_DIR/app/src/main/jniLibs"
for entry in "${TARGETS[@]}"; do
    triple="${entry%%:*}" abi="${entry##*:}"
    src="$CARGO_TARGET_DIR/$triple/android/$LIB_NAME"
    [[ -f "$src" ]] || die "missing build output $src (is bridge-android a cdylib?)"
    mkdir -p "$JNI_DIR/$abi"
    # Strip here: AGP 8.13 cannot strip NDK r30 output ("Unable to strip ...").
    "$GB_NDK_BIN/llvm-strip" --strip-unneeded -o "$JNI_DIR/$abi/$LIB_NAME" "$src"
    chmod 0644 "$JNI_DIR/$abi/$LIB_NAME"
done

# Strict ELF checks: right machine, LOAD segments aligned to >= 16 KiB.
elf_check() { # <file> <expected machine substring>
    local f="$1" machine="$2" hdr
    hdr="$(llvm-readelf -h "$f")"
    grep -q "Machine:.*$machine" <<<"$hdr" || die "$f: unexpected ELF machine (wanted $machine)"
    local bad
    bad="$(llvm-readelf -lW "$f" | awk '$1=="LOAD" { if (strtonum($NF) < 16384) print $NF }')"
    [[ -z "$bad" ]] || die "$f: LOAD segment alignment below 16 KiB: $bad"
}
elf_check "$JNI_DIR/arm64-v8a/$LIB_NAME" "AArch64"
elf_check "$JNI_DIR/x86_64/$LIB_NAME" "Advanced Micro Devices X86-64"
log "native libs OK (machine + 16 KiB LOAD alignment)"

# JNI symbol must be exported or System.loadLibrary would succeed yet calls fail.
for abi in arm64-v8a x86_64; do
    llvm-nm -D --defined-only "$JNI_DIR/$abi/$LIB_NAME" \
        | grep -q 'Java_dev_geekbridge_android_NativeBridge_nativeRequest' \
        || die "$abi: JNI symbol Java_dev_geekbridge_android_NativeBridge_nativeRequest not exported"
done

# ---- 3. Gradle -----------------------------------------------------------------
printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$ANDROID_DIR/local.properties"

GRADLE_TASKS=(assembleDebug)
(( RUN_TESTS )) && GRADLE_TASKS+=(testDebugUnitTest)
(( RUN_LINT )) && GRADLE_TASKS+=(lintDebug)
log "gradle ${GRADLE_TASKS[*]}"
gradle -p "$ANDROID_DIR" --console=plain --no-daemon "${GRADLE_TASKS[@]}"

# ---- 4. collect + verify APK ---------------------------------------------------
APK_DIR="$ANDROID_DIR/app/build/outputs/apk/debug"
mapfile -t apks < <(find "$APK_DIR" -maxdepth 1 -name '*.apk' -type f 2>/dev/null)
(( ${#apks[@]} == 1 )) || die "expected exactly one APK in $APK_DIR, found ${#apks[@]}"

mkdir -p "$DIST_DIR"
APK="$DIST_DIR/geekbridge-debug.apk"
install -m 0644 "${apks[0]}" "$APK"

listing="$(unzip -Z1 "$APK")"
source "$HERE/lib-tmp.sh"
VERIFY="$(gb_mktemp build-android -d)"
trap 'rm -rf "$VERIFY"' EXIT
for abi in arm64-v8a x86_64; do
    grep -qx "lib/$abi/$LIB_NAME" <<<"$listing" || die "APK is missing lib/$abi/$LIB_NAME"
    unzip -q -o "$APK" "lib/$abi/$LIB_NAME" -d "$VERIFY"
    want="$(sha256sum "$JNI_DIR/$abi/$LIB_NAME" | cut -d' ' -f1)"
    got="$(sha256sum "$VERIFY/lib/$abi/$LIB_NAME" | cut -d' ' -f1)"
    [[ "$want" == "$got" ]] || die "packaged $abi lib differs from the jniLibs copy"
done
elf_check "$VERIFY/lib/arm64-v8a/$LIB_NAME" "AArch64"
elf_check "$VERIFY/lib/x86_64/$LIB_NAME" "Advanced Micro Devices X86-64"

zipalign -c -P 16 -v 4 "$APK" >/dev/null || die "APK failed 16 KiB zipalign check"
apksigner verify "$APK" >/dev/null || die "APK signature verification failed"

badging="$(aapt2 dump badging "$APK")"
grep -q "^package: name='dev.geekbridge.android'" <<<"$badging" || die "unexpected application ID"
grep -q "^minSdkVersion:'26'" <<<"$badging" || die "minSdk is not 26"
grep -q "^targetSdkVersion:'36'" <<<"$badging" || die "targetSdk is not 36"

(cd "$DIST_DIR" && sha256sum geekbridge-debug.apk | tee SHA256SUMS)
log "APK: $APK ($(du -h "$APK" | cut -f1)); abis: $(grep -o '^native-code:.*' <<<"$badging")"
log "debug-signed development build; unsigned release/Play distribution not produced"
