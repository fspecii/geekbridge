#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install the project-local JDK 17, Gradle, Rust (rustup + Android targets) and
# Android SDK/NDK under ./.tools. Idempotent. No sudo, no global changes.
#
#   scripts/bootstrap-android.sh [--with-emulator]
#
# Provenance: every archive downloaded directly is verified against a pinned
# SHA-256 (JDK: Adoptium API; Gradle: services.gradle.org/*.sha256; rustup:
# static.rust-lang.org/*.sha256) or SHA-1 (Android cmdline-tools: Google's
# repository2-3.xml). SDK packages, platforms, build-tools and the NDK are
# fetched by Google's sdkmanager, which verifies them against that same manifest.
set -Eeuo pipefail
IFS=$'\n\t'

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=android-env.sh
source "$HERE/android-env.sh"

WITH_EMULATOR=0
for arg in "$@"; do
    case "$arg" in
        --with-emulator) WITH_EMULATOR=1 ;;
        -h|--help) sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# ---- pinned artifacts ------------------------------------------------------
JDK_URL="https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.21%2B9/OpenJDK17U-jdk_x64_linux_hotspot_17.0.21_9.tar.gz"
JDK_SHA256="8b9b611d917e3dfd8419d3566b5949226f69b052fed7e109c1368add1dfe8711"

GRADLE_URL="https://services.gradle.org/distributions/gradle-${GB_GRADLE_VERSION}-bin.zip"
GRADLE_SHA256="20f1b1176237254a6fc204d8434196fa11a4cfb387567519c61556e8710aed78"

RUSTUP_URL="https://static.rust-lang.org/rustup/archive/${GB_RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init"
RUSTUP_SHA256="5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4"

CMDLINE_URL="https://dl.google.com/android/repository/commandlinetools-linux-${GB_CMDLINE_TOOLS_REV}_latest.zip"
CMDLINE_SHA1="e025545c62a8e64c7559119566a569fb1dec5f60"

DL="$GB_TOOLS/downloads"
STAGE="$GB_TOOLS/staging"

log() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

for c in curl tar unzip sha256sum sha1sum; do
    command -v "$c" >/dev/null || die "required host tool missing: $c"
done

mkdir -p "$DL" "$STAGE" "$ANDROID_USER_HOME"
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT
rm -rf "$STAGE"; mkdir -p "$STAGE"

# fetch <url> <dest-file> <algo: sha256|sha1> <expected>
fetch() {
    local url="$1" dest="$2" algo="$3" want="$4" got
    if [[ -f "$dest" ]]; then
        got="$("${algo}sum" "$dest" | cut -d' ' -f1)"
        [[ "$got" == "$want" ]] && return 0
        log "cached $(basename "$dest") has wrong hash; re-downloading"
        rm -f "$dest"
    fi
    log "downloading $(basename "$dest")"
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 --retry-delay 2 \
         --continue-at - --silent --show-error --output "$dest.part" "$url"
    got="$("${algo}sum" "$dest.part" | cut -d' ' -f1)"
    if [[ "$got" != "$want" ]]; then
        rm -f "$dest.part"
        die "$algo mismatch for $url: expected $want, got $got"
    fi
    mv "$dest.part" "$dest"
}

# ---- JDK 17 (Eclipse Temurin) ----------------------------------------------
if [[ ! -x "$GB_JDK_HOME/bin/java" ]]; then
    fetch "$JDK_URL" "$DL/jdk-$GB_JDK_VERSION.tar.gz" sha256 "$JDK_SHA256"
    mkdir -p "$STAGE/jdk" "$GB_TOOLS/jdk"
    tar -xzf "$DL/jdk-$GB_JDK_VERSION.tar.gz" -C "$STAGE/jdk"
    [[ -d "$STAGE/jdk/jdk-$GB_JDK_VERSION" ]] || die "unexpected JDK archive layout"
    rm -rf "$GB_JDK_HOME"
    mv "$STAGE/jdk/jdk-$GB_JDK_VERSION" "$GB_JDK_HOME"
fi
log "JDK: $("$GB_JDK_HOME/bin/java" -version 2>&1 | head -1)"

# ---- Gradle ------------------------------------------------------------------
if [[ ! -x "$GB_GRADLE_HOME/bin/gradle" ]]; then
    fetch "$GRADLE_URL" "$DL/gradle-${GB_GRADLE_VERSION}-bin.zip" sha256 "$GRADLE_SHA256"
    mkdir -p "$STAGE/gradle" "$GB_TOOLS/gradle"
    unzip -q "$DL/gradle-${GB_GRADLE_VERSION}-bin.zip" -d "$STAGE/gradle"
    [[ -d "$STAGE/gradle/gradle-$GB_GRADLE_VERSION" ]] || die "unexpected Gradle archive layout"
    rm -rf "$GB_GRADLE_HOME"
    mv "$STAGE/gradle/gradle-$GB_GRADLE_VERSION" "$GB_GRADLE_HOME"
fi
log "Gradle: $("$GB_GRADLE_HOME/bin/gradle" --version --offline 2>/dev/null | grep -m1 '^Gradle' || echo "$GB_GRADLE_VERSION")"

# ---- Rust toolchain (rustup, project-local) ---------------------------------
# The distro rustc has no Android std, so install a matching toolchain locally.
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    export RUSTUP_INIT_SKIP_PATH_CHECK=yes
    fetch "$RUSTUP_URL" "$DL/rustup-init-$GB_RUSTUP_VERSION" sha256 "$RUSTUP_SHA256"
    chmod +x "$DL/rustup-init-$GB_RUSTUP_VERSION"
    "$DL/rustup-init-$GB_RUSTUP_VERSION" -y --no-modify-path --profile minimal \
        --default-toolchain "$GB_RUST_VERSION" \
        -t aarch64-linux-android -t x86_64-linux-android
fi
"$CARGO_HOME/bin/rustup" toolchain install "$GB_RUST_VERSION" --profile minimal \
    -t aarch64-linux-android -t x86_64-linux-android >/dev/null
log "Rust: $("$CARGO_HOME/bin/rustc" -V) with $("$CARGO_HOME/bin/rustup" target list --installed | paste -sd' ')"

# ---- Android command-line tools ---------------------------------------------
SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
if [[ ! -x "$SDKMANAGER" ]]; then
    fetch "$CMDLINE_URL" "$DL/commandlinetools-linux-${GB_CMDLINE_TOOLS_REV}.zip" sha1 "$CMDLINE_SHA1"
    mkdir -p "$STAGE/cmdline" "$ANDROID_HOME/cmdline-tools"
    unzip -q "$DL/commandlinetools-linux-${GB_CMDLINE_TOOLS_REV}.zip" -d "$STAGE/cmdline"
    [[ -d "$STAGE/cmdline/cmdline-tools" ]] || die "unexpected cmdline-tools archive layout"
    rm -rf "$ANDROID_HOME/cmdline-tools/latest"
    mv "$STAGE/cmdline/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
fi

SDK_PACKAGES=(
    "platform-tools"
    "platforms;$GB_PLATFORM"
    "build-tools;$GB_BUILD_TOOLS"
    "ndk;$GB_NDK_VERSION"
)
if (( WITH_EMULATOR )); then
    SDK_PACKAGES+=("emulator" "$GB_EMULATOR_IMAGE")
fi

# Accepting the Android SDK licenses is part of the requested dev setup.
log "accepting Android SDK licenses (project-local SDK only)"
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses < <(yes) >/dev/null 2>&1 || true
log "installing SDK packages: $(IFS=,; echo "${SDK_PACKAGES[*]}")"
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" "${SDK_PACKAGES[@]}" < <(yes) | grep -Ev '^\[=*[ =]*\]' || true

# ---- verify -------------------------------------------------------------------
[[ -x "$GB_NDK_BIN/aarch64-linux-android${GB_ANDROID_API}-clang" ]] || die "NDK clang missing"
[[ -x "$GB_NDK_BIN/x86_64-linux-android${GB_ANDROID_API}-clang" ]] || die "NDK clang missing"
[[ -x "$ANDROID_HOME/build-tools/$GB_BUILD_TOOLS/aapt2" ]] || die "build-tools $GB_BUILD_TOOLS missing"
[[ -f "$ANDROID_HOME/platforms/$GB_PLATFORM/android.jar" ]] || die "platform $GB_PLATFORM missing"
log "NDK: $(grep -m1 Pkg.Revision "$ANDROID_NDK_HOME/source.properties")"
log "bootstrap complete; use: source scripts/android-env.sh"
