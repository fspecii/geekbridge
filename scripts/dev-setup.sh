#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Check that this machine can build and test GeekBridge, and say exactly what is missing.
# Linux, macOS and Windows (WSL or Git Bash). Installs NOTHING unless you pass --install.
#
#   scripts/dev-setup.sh                 # report only
#   scripts/dev-setup.sh --desktop-only  # skip the Android checks (Rust workspace + desktop apps only)
#   scripts/dev-setup.sh --install       # also install what is missing (asks sudo for system packages)
#
# Checks: rustc >= the workspace rust-version, cargo, rustfmt, clippy, a C linker, pkg-config and the system
# libraries of the desktop apps (names per apt / pacman / dnf / brew), JDK 17, and an Android SDK (ANDROID_HOME,
# android/local.properties or the project-local .tools/android-sdk that scripts/bootstrap-android.sh creates).
# Exit status: 0 = ready, 1 = something required is missing.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

INSTALL=0
ANDROID=1
for arg in "$@"; do
    case "$arg" in
        --install) INSTALL=1 ;;
        --desktop-only|--no-android) ANDROID=0 ;;
        -h|--help) sed -n '3,13p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
    esac
done

if [[ -t 1 ]]; then G=$'\e[32m'; R=$'\e[31m'; Y=$'\e[33m'; N=$'\e[0m'; else G=; R=; Y=; N=; fi
MISSING=0
INSTALLED=0
HINTS=()
ok()   { printf '  %sok%s    %s\n' "$G" "$N" "$*"; }
warn() { printf '  %swarn%s  %s\n' "$Y" "$N" "$*"; }
miss() { printf '  %sMISSING%s %s\n' "$R" "$N" "$*"; MISSING=1; }
hint() { HINTS+=("$*"); }
have() { command -v "$1" >/dev/null 2>&1; }

# ---- platform --------------------------------------------------------------
OS="$(uname -s)"; ARCH="$(uname -m)"; PM=""
case "$OS" in
    Linux)
        if grep -qi microsoft /proc/version 2>/dev/null; then PLATFORM="Windows (WSL)"; else PLATFORM="Linux"; fi
        if   have apt-get; then PM=apt
        elif have pacman;  then PM=pacman
        elif have dnf;     then PM=dnf
        fi ;;
    Darwin) PLATFORM="macOS"; have brew && PM=brew ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM="Windows (Git Bash)" ;;
    *) PLATFORM="$OS" ;;
esac
echo "GeekBridge dev setup: $PLATFORM $ARCH${PM:+, package manager: $PM}"
[[ "$PLATFORM" == "Windows (Git Bash)" ]] && warn "native Windows builds work for the Rust workspace; for the full flow (Android, scripts) use WSL2"

# ---- Rust ------------------------------------------------------------------
need_rust="$(sed -n 's/^rust-version *= *"\([0-9.]*\)".*/\1/p' Cargo.toml | head -n1)"
need_rust="${need_rust:-1.88}"
ver_ge() { [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]; }  # ver_ge have want

echo "Rust"
RUST_OK=1
if have rustc && have cargo; then
    have_rust="$(rustc --version | sed -n 's/^rustc \([0-9][0-9.]*\).*/\1/p')"
    if ver_ge "$have_rust" "$need_rust"; then ok "rustc $have_rust (>= $need_rust), $(cargo --version)"
    else miss "rustc $have_rust is older than $need_rust"; RUST_OK=0; hint "update Rust: rustup update stable"; fi
else
    miss "rustc / cargo not found"; RUST_OK=0
    hint "install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   (or: scripts/dev-setup.sh --install)"
fi
if ((RUST_OK)); then
    cargo fmt --version >/dev/null 2>&1 && ok "rustfmt" || { miss "rustfmt"; hint "rustup component add rustfmt"; }
    cargo clippy --version >/dev/null 2>&1 && ok "clippy" || { miss "clippy"; hint "rustup component add clippy"; }
fi
if have cc || have gcc || have clang; then ok "C compiler/linker"
elif [[ "$OS" == Darwin ]]; then miss "C toolchain"; hint "xcode-select --install"
else miss "C compiler/linker (cc)"; fi

# ---- system libraries for the egui desktop apps (Linux) --------------------
if [[ "$OS" == Linux ]]; then
    echo "System libraries (desktop apps, audio, tests)"
    case "$PM" in
        apt)    PKGS="build-essential pkg-config libxkbcommon-dev libwayland-dev libgl1-mesa-dev libxrandr-dev libxi-dev libpulse-dev dbus"
                pkg_missing() { ! dpkg -s "$1" >/dev/null 2>&1; }
                pkg_install() { sudo apt-get update && sudo apt-get install -y "$@"; } ;;
        pacman) PKGS="base-devel pkgconf libxkbcommon wayland mesa libxrandr libxi libpulse dbus"
                pkg_missing() { ! pacman -Qq "$1" >/dev/null 2>&1; }
                pkg_install() { sudo pacman -S --needed "$@"; } ;;
        dnf)    PKGS="gcc pkgconf-pkg-config libxkbcommon-devel wayland-devel mesa-libGL-devel libXrandr-devel libXi-devel pulseaudio-libs-devel dbus-daemon"
                pkg_missing() { ! rpm -q "$1" >/dev/null 2>&1; }
                pkg_install() { sudo dnf install -y "$@"; } ;;
        *)      PKGS=""
                warn "unknown package manager: install pkg-config plus the development files of libxkbcommon, wayland, GL, Xrandr, Xi, PulseAudio and a D-Bus daemon" ;;
    esac
    if [[ -n "$PKGS" ]]; then
        TO_INSTALL=()
        for p in $PKGS; do pkg_missing "$p" && TO_INSTALL+=("$p"); done
        if ((${#TO_INSTALL[@]} == 0)); then ok "all packages present ($PM)"
        else
            miss "packages: ${TO_INSTALL[*]}"
            if ((INSTALL)); then
                echo "  installing system packages with sudo ($PM)"
                pkg_install "${TO_INSTALL[@]}" || echo "  package installation failed; run the command above yourself"
                INSTALLED=1
            else
                case "$PM" in
                    apt) hint "sudo apt-get install -y ${TO_INSTALL[*]}" ;;
                    pacman) hint "sudo pacman -S --needed ${TO_INSTALL[*]}" ;;
                    dnf) hint "sudo dnf install -y ${TO_INSTALL[*]}" ;;
                esac
            fi
        fi
    fi
    if have wl-copy || have xclip; then ok "clipboard tool (wl-clipboard or xclip)"
    else warn "no wl-clipboard / xclip: clipboard sync cannot run here (tests do not need it)"; fi
elif [[ "$OS" == Darwin ]]; then
    echo "System libraries"
    xcode-select -p >/dev/null 2>&1 && ok "Xcode command line tools" || { miss "Xcode command line tools"; hint "xcode-select --install"; }
    have brew && ok "Homebrew (needed only for JDK 17: brew install openjdk@17)" || warn "Homebrew not found (optional)"
fi

# ---- Android ---------------------------------------------------------------
if ((ANDROID)); then
    echo "Android (only for the phone app: make android, ./gradlew)"
    jdk_major() { "$1" -version 2>&1 | sed -n '1s/.*version "\([0-9]*\).*/\1/p'; }
    JAVA=""
    if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/java" ]]; then JAVA="$JAVA_HOME/bin/java"
    elif have java; then JAVA="$(command -v java)"
    else JAVA="$(ls -d "$ROOT"/.tools/jdk/jdk-*/bin/java 2>/dev/null | head -n1 || true)"; fi
    LOCAL_TOOLS=0
    if [[ -n "$JAVA" ]]; then
        m="$(jdk_major "$JAVA")"
        if [[ "$m" == 17 ]]; then ok "JDK 17 ($JAVA)"
        elif [[ -n "$m" && "$m" -gt 17 ]]; then warn "JDK $m found; the project is built and tested with JDK 17"
        else miss "JDK 17 (found ${m:-unknown} at $JAVA)"; LOCAL_TOOLS=1; fi
    else miss "JDK 17"; LOCAL_TOOLS=1
        case "$PM" in
            apt) hint "sudo apt-get install -y openjdk-17-jdk" ;;
            pacman) hint "sudo pacman -S --needed jdk17-openjdk" ;;
            dnf) hint "sudo dnf install -y java-17-openjdk-devel" ;;
            brew) hint "brew install openjdk@17" ;;
        esac
    fi
    SDK=""
    for c in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$(sed -n 's/^sdk\.dir=//p' android/local.properties 2>/dev/null | head -n1)" "$ROOT/.tools/android-sdk" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
        [[ -n "$c" && -d "$c/platforms" ]] && { SDK="$c"; break; }
    done
    if [[ -n "$SDK" ]]; then
        ok "Android SDK at $SDK"
        [[ -d "$SDK/platforms/android-36" ]] && ok "platform android-36" || { miss "platform android-36"; hint "sdkmanager \"platforms;android-36\" \"build-tools;36.0.0\""; }
        [[ -d "$SDK/ndk/30.0.16248370" ]] && ok "NDK 30.0.16248370 (native library only)" || warn "NDK 30.0.16248370 not installed (needed only to rebuild the Rust library for the phone: sdkmanager \"ndk;30.0.16248370\")"
        [[ -f android/local.properties ]] || hint "echo \"sdk.dir=$SDK\" > android/local.properties   (or export ANDROID_HOME=$SDK)"
    else
        miss "Android SDK (no ANDROID_HOME, android/local.properties sdk.dir or .tools/android-sdk)"; LOCAL_TOOLS=1
    fi
    if ((LOCAL_TOOLS)); then
        if [[ "$OS" == Linux && "$ARCH" == x86_64 ]]; then
            hint "scripts/bootstrap-android.sh   # project-local JDK, Gradle, SDK, NDK and Rust Android targets under .tools/, no sudo; then: source scripts/android-env.sh"
            if ((INSTALL)); then
                echo "  running scripts/bootstrap-android.sh"
                scripts/bootstrap-android.sh || echo "  bootstrap-android.sh failed; see its output"
                INSTALLED=1
            fi
        else
            hint "install Android Studio (or the command-line tools) and set ANDROID_HOME; scripts/bootstrap-android.sh supports Linux x86_64 only"
        fi
    fi
    if ((RUST_OK)) && have rustup; then
        for t in aarch64-linux-android x86_64-linux-android; do
            rustup target list --installed 2>/dev/null | grep -qx "$t" || { warn "Rust target $t missing (needed only for the phone library)"; hint "rustup target add $t"; }
        done
    fi
fi

# ---- install Rust last (so the checks above stay read-only without --install) ---
if ((INSTALL && !RUST_OK)); then
    echo "Installing Rust with rustup (official installer, https://rustup.rs)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default
    echo "  done. Open a new shell (or: source \"\$HOME/.cargo/env\") and run this script again."
    INSTALLED=1
fi

echo
if ((${#HINTS[@]})); then
    echo "To fix:"
    for h in "${HINTS[@]}"; do printf '  %s\n' "$h"; done
    ((INSTALL)) || echo "  (or run: scripts/dev-setup.sh --install)"
fi
if ((INSTALLED)); then echo "Installed what was missing; run scripts/dev-setup.sh again (new shell) to confirm."; exit 0; fi
if ((MISSING)); then echo "${R}Not ready.${N} Items marked MISSING must be fixed (Android ones are optional if you only work on the desktop: --desktop-only)."; exit 1; fi
echo "${G}Ready.${N} Next: make check   (Rust)   |   make android   (phone app)   |   make desktop   (run the desktop apps)"
