#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build, verify and (only on request) publish a GeekBridge release from THIS machine, without GitHub Actions.
#
#   scripts/release-local.sh vX.Y.Z[-pre] [--prerelease] [--dry-run] [--publish] [--skip-tests] [options]
#
#   --prerelease    required for a tag with a '-' (v0.2.0-rc.1), refused for a stable tag (mirrors release.yml)
#   --dry-run       build a COPY of the working tree whose version is patched to the tag, so a dirty tree, a branch
#                   other than main and a version mismatch are warnings instead of errors. Nothing is published or
#                   tagged: `gh` and the mutating `git` calls are replaced by printing stubs. Overrides --publish.
#   --publish       after building and verifying, create the GitHub release (draft, upload, then undraft, as release.yml)
#                   AND nothing else. Without it the script stops after verifying and prints the exact commands.
#   --tag           create the annotated git tag locally (plain message, no trailers)
#   --push-tag      push that tag to the remote (needs --tag or an existing local tag)
#   --skip-tests    skip cargo fmt/clippy/test (only when they already passed on this commit)
#   --skip-arch     skip the Arch package (the longest single step)
#   --with-windows  also cross-build an EXPERIMENTAL Windows x86_64 (GNU) zip with zig; see docs/release-local.md
#   --notes-file F  use the file F as the release notes body instead of the CHANGELOG.md '## [X.Y.Z]' section (the signer
#                   line and asset note are still appended). Must exist and be non-empty; also works with --dry-run
#   --print-notes   print the notes body that would be used and exit right after that check (builds nothing)
#   --prune         delete build trees (target dirs, source copy) after success; the finished assets stay
#   -h, --help      this text
#
# Environment (all optional unless noted):
#   GB_JOBS                cargo/gradle parallelism, default 3 (unlimited jobs exhausted 31 GB of RAM once)
#   GB_RELEASE_DIR         scratch + output root, default ~/.cache/gb-release   (assets: $GB_RELEASE_DIR/<tag>/out)
#   GB_KEYSTORE            release keystore (PKCS12), GB_KEYSTORE_PASS_FILE its password file, GB_KEY_ALIAS (geekbridge-release)
#   ANDROID_SIGNER_SHA256  expected signer SHA-256 (hex, colons optional); else read from GB_SIGNER_FILE
#                          (default ~/.config/geekbridge/android-signer-sha256). Required for stable tags.
#   GB_RELEASE_REPO        owner/name to publish to (default: the origin remote, else fspecii/geekbridge)
#   GB_REMOTE              git remote, default origin
#   GB_MIN_FREE_GB         refuse to start with less free disk than this, default 10
#   GB_CHECK_TARGET_DIR    cargo target dir for fmt/clippy/test, default <repo>/target
# Full procedure, runtime and disk use: docs/release-local.md.
set -Eeuo pipefail
IFS=$'\n\t'

# Bash reads a script lazily, so editing this file during a 30-minute run could corrupt it. Run a private copy.
if [[ -z "${GB_RL_SELF:-}" ]]; then
    self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/$(basename "${BASH_SOURCE[0]}")"
    copy="$(mktemp "${TMPDIR:-/tmp}/gb-release-local.XXXXXX")"
    cp "$self" "$copy"
    GB_RL_CWD="$PWD" GB_RL_SELF="$self" GB_RL_COPY="$copy" exec bash "$copy" "$@"
fi
[[ -z "${GB_RL_COPY:-}" ]] || rm -f "$GB_RL_COPY"

ROOT="$(cd "$(dirname "$GB_RL_SELF")/.." && pwd -P)"
cd "$ROOT"
source "$ROOT/scripts/lib/signpass.sh"
source "$ROOT/scripts/lib/repro.sh"

log()  { printf '\n==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf '!!! WARNING: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- arguments
TAG="" PRERELEASE=0 DRY=0 PUBLISH=0 TESTS=1 ARCH=1 WINDOWS=0 MKTAG=0 PUSHTAG=0 PRUNE=0 NOTES_FILE="" PRINT_NOTES=0
args=("$@")
for (( i = 0; i < ${#args[@]}; i++ )); do
    arg="${args[i]}"
    case "$arg" in
        --notes-file) (( i + 1 < ${#args[@]} )) || die "--notes-file needs a path"; NOTES_FILE="${args[++i]}" ;;
        --notes-file=*) NOTES_FILE="${arg#--notes-file=}" ;;
        --print-notes) PRINT_NOTES=1 ;;
        --prerelease) PRERELEASE=1 ;;
        --dry-run) DRY=1 ;;
        --publish) PUBLISH=1 ;;
        --tag) MKTAG=1 ;;
        --push-tag) PUSHTAG=1 ;;
        --skip-tests) TESTS=0 ;;
        --skip-arch) ARCH=0 ;;
        --with-windows) WINDOWS=1 ;;
        --prune) PRUNE=1 ;;
        -h|--help) sed -n '2,34p' "$GB_RL_SELF"; exit 0 ;;
        -*) die "unknown option: $arg (try --help)" ;;
        *) [[ -z "$TAG" ]] || die "only one tag expected"; TAG="$arg" ;;
    esac
done
[[ -n "$TAG" ]] || die "usage: scripts/release-local.sh vX.Y.Z [--prerelease] [--dry-run] [--publish] [--skip-tests]"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "tag '$TAG' is not vMAJOR.MINOR.PATCH[-pre]"
VERSION="${TAG#v}"
if [[ "$VERSION" == *-* && $PRERELEASE == 0 ]]; then die "$TAG has a pre-release suffix; pass --prerelease"; fi
if [[ "$VERSION" != *-* && $PRERELEASE == 1 ]]; then die "--prerelease given but $TAG looks stable"; fi
(( DRY )) && PUBLISH=0
# Android refuses versionCode 0 (so v0.0.0 and v0.0.0-alpha.0 cannot be built); fail now, not after 25 minutes.
[[ "$(scripts/version-code.sh --code "$VERSION")" =~ ^[1-9][0-9]*$ ]] \
    || die "$TAG gives Android versionCode 0, which Gradle rejects; use e.g. v0.0.1-dryrun for rehearsals"

JOBS="${GB_JOBS:-3}"
[[ "$JOBS" =~ ^[1-9][0-9]*$ ]] || die "GB_JOBS must be a positive integer"
REMOTE="${GB_REMOTE:-origin}"
BASE="${GB_RELEASE_DIR:-$HOME/.cache/gb-release}"
WORK="$BASE/$TAG"
OUT="$WORK/out"
LOGS="$WORK/logs"
case "$WORK" in /|"$HOME"|"$HOME/") die "refusing unsafe scratch directory $WORK" ;; esac

cargo_version() {
    awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' "$1/Cargo.toml"
}

# Problems that are fatal for a real run but only reported for --dry-run (which builds a patched copy).
problems=0
soft() {
    if (( DRY )); then warn "dry run: a real release would refuse here: $*"; else die "$*"; fi
    problems=$((problems + 1))
}

# ---------------------------------------------------------------- preflight
log "Preflight for $TAG ($([[ $PRERELEASE == 1 ]] && echo prerelease || echo stable)$([[ $DRY == 1 ]] && echo ', DRY RUN'))"
for c in git cargo tar gzip sha256sum awk sed ar xz; do command -v "$c" >/dev/null || die "missing tool: $c"; done
(( ARCH )) && for c in makepkg rsvg-convert zstd; do command -v "$c" >/dev/null || die "missing tool for the Arch package: $c (or pass --skip-arch)"; done
[[ -f "$ROOT/.tools/jdk/jdk-17.0.21+9/bin/java" || -n "$(command -v java || true)" ]] \
    || die "no JDK: run scripts/bootstrap-android.sh"
mkdir -p "$BASE"
free_gb=$(( $(df -Pk "$BASE" | awk 'NR==2{print $4}') / 1024 / 1024 ))
min_free="${GB_MIN_FREE_GB:-10}"
(( free_gb >= min_free )) || die "only ${free_gb} GB free under $BASE (need ${min_free}; set GB_MIN_FREE_GB to override, see docs/release-local.md#disk-use)"
note "free disk: ${free_gb} GB, parallelism: $JOBS jobs"

branch="$(git rev-parse --abbrev-ref HEAD)"
[[ "$branch" == "main" ]] || soft "on branch '$branch', not main"
if [[ -n "$(git status --porcelain)" ]]; then
    soft "working tree is not clean ($(git status --porcelain | wc -l) changed paths); commit them first"
fi
HEAD_SHA="$(git rev-parse HEAD)"
# Timestamps in every artefact (tar mtimes, SBOM) come from the commit, not the clock: docs/reproducible-builds.md.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct HEAD)}"
cargo_ver="$(cargo_version "$ROOT")"
[[ -n "$cargo_ver" ]] || die "no [workspace.package] version in Cargo.toml"
if [[ "$cargo_ver" != "$VERSION" ]]; then
    soft "Cargo.toml version is $cargo_ver but the tag is $TAG; run scripts/release.sh $VERSION (this script never bumps)"
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    [[ "$(git rev-parse "refs/tags/$TAG^{commit}")" == "$HEAD_SHA" ]] || soft "tag $TAG exists but points at another commit"
fi
if (( ! DRY )); then
    git fetch --no-tags -q "$REMOTE" main 2>/dev/null || warn "could not fetch $REMOTE/main; the ancestry check uses the local ref"
    if git rev-parse -q --verify "refs/remotes/$REMOTE/main" >/dev/null; then
        git merge-base --is-ancestor "$HEAD_SHA" "refs/remotes/$REMOTE/main" \
            || die "HEAD is not on $REMOTE/main; push main first (release.yml has the same rule)"
    fi
fi

# Release notes source: --notes-file, else the CHANGELOG.md section for this version.
if [[ -n "$NOTES_FILE" ]]; then
    [[ "$NOTES_FILE" == /* ]] || NOTES_FILE="${GB_RL_CWD:-$ROOT}/$NOTES_FILE"
    [[ -f "$NOTES_FILE" && -r "$NOTES_FILE" ]] || die "--notes-file '$NOTES_FILE' is not a readable file"
    notes_body="$(cat -- "$NOTES_FILE")"
    [[ -n "${notes_body//[$' \t\n']/}" ]] || die "--notes-file '$NOTES_FILE' is empty"
    note "release notes: $NOTES_FILE ($(wc -l <"$NOTES_FILE") lines); CHANGELOG.md is not used for them"
else
notes_body="$(awk -v v="$VERSION" '
    $0 ~ "^## \\[" v "\\]" {on=1; next}
    on && /^## \[/ {exit}
    on {print}' CHANGELOG.md | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')"
if [[ -z "${notes_body//[$' \t\n']/}" ]]; then
    if (( PRERELEASE )); then
        warn "CHANGELOG.md has no '## [$VERSION]' section; the notes will say only that this is a prerelease build."
        notes_body="Prerelease build $VERSION."
    else
        soft "CHANGELOG.md has no '## [$VERSION]' section (scripts/release.sh creates it)"
        notes_body="(no changelog section)"
    fi
fi
fi
if (( PRINT_NOTES )); then printf -- "----- release notes -----\n%s\n" "$notes_body"; exit 0; fi

# Android toolchain (project-local; scripts/bootstrap-android.sh). Run commands inside it with with_android.
with_android() { ( set +u; source "$ROOT/scripts/android-env.sh"; "$@" ); }
with_android bash -c 'for c in apksigner zipalign aapt2 gradle keytool; do command -v "$c" >/dev/null || { echo "missing Android tool: $c (run scripts/bootstrap-android.sh)" >&2; exit 1; }; done'

# Signing key and pin.
KS="${GB_KEYSTORE:-}" PASS_FILE="${GB_KEYSTORE_PASS_FILE:-}" ALIAS="${GB_KEY_ALIAS:-geekbridge-release}"
SIGNER_FILE="${GB_SIGNER_FILE:-$HOME/.config/geekbridge/android-signer-sha256}"
want_pin="${ANDROID_SIGNER_SHA256:-}"
[[ -n "$want_pin" || ! -r "$SIGNER_FILE" ]] || want_pin="$(head -n1 "$SIGNER_FILE")"
want_pin="$(printf '%s' "$want_pin" | tr -d ': \r\n' | tr 'A-F' 'a-f')"
RELEASE_SIGNED=0
if [[ -n "$KS" ]]; then
    [[ -f "$KS" ]] || die "GB_KEYSTORE=$KS does not exist"
    [[ -f "$PASS_FILE" ]] || die "GB_KEYSTORE_PASS_FILE is not set or does not exist"
    [[ -z "$(find "$PASS_FILE" "$KS" -perm /077 2>/dev/null)" ]] || warn "keystore or password file is readable by other users (chmod 600)"
    if [[ -n "$(git -C "$(dirname "$KS")" rev-parse --show-toplevel 2>/dev/null || true)" ]]; then
        warn "the keystore lives inside a git work tree; keep keys outside any repository"
    fi
    got_pin="$(with_android keytool -list -v -keystore "$KS" -storepass:file "$PASS_FILE" -alias "$ALIAS" 2>/dev/null \
        | awk -F'SHA256: ' '/SHA256:/ {print $2; exit}' | tr -d ': ' | tr 'A-F' 'a-f')"
    [[ "$got_pin" =~ ^[0-9a-f]{64}$ ]] || die "cannot read alias '$ALIAS' from $KS (wrong password file or alias? GB_KEY_ALIAS)"
    if [[ -n "$want_pin" ]]; then
        [[ "$want_pin" =~ ^[0-9a-f]{64}$ ]] || die "the pinned signer is not 64 hex digits"
        [[ "$got_pin" == "$want_pin" ]] \
            || die "keystore signer $got_pin does not match the pinned $want_pin: wrong keystore; phones would refuse this update"
        note "signer pin OK: $got_pin"
    elif (( PRERELEASE )); then
        warn "no signer pin (ANDROID_SIGNER_SHA256 / $SIGNER_FILE); the key is $got_pin"
    else
        die "stable release without a pinned signer. Put the fingerprint scripts/make-keystore.sh printed into ANDROID_SIGNER_SHA256 or $SIGNER_FILE (this key: $got_pin)"
    fi
    RELEASE_SIGNED=1
else
    if (( PRERELEASE )); then
        printf '\n'
        warn "NO RELEASE KEYSTORE (GB_KEYSTORE unset): the APKs will be DEBUG-SIGNED."
        warn "A debug-signed APK cannot be updated to or from a release-signed one; phones must uninstall first. Prerelease only."
        printf '\n'
    else
        die "a stable release needs the release keystore: set GB_KEYSTORE, GB_KEYSTORE_PASS_FILE (and GB_KEY_ALIAS); create one with scripts/make-keystore.sh. Debug-signed APKs are allowed for --prerelease only."
    fi
fi

# ---------------------------------------------------------------- source tree and scratch
if [[ -d "$WORK" ]]; then
    # Only ever remove things below the scratch root.
    [[ "$WORK" == "$BASE"/* ]] || die "unsafe scratch path"
    rm -rf "$WORK/out" "$WORK/logs" "$WORK/stage" "$WORK/tmp" "$WORK/arch-out" "$WORK/src"
fi
mkdir -p "$OUT" "$LOGS" "$WORK/stage" "$WORK/tmp"
export TMPDIR="$WORK/tmp"

if (( DRY )); then
    SRC="$WORK/src"
    log "Dry run: copying the working tree to $SRC and patching its version to $VERSION"
    mkdir -p "$SRC"
    git ls-files -z --cached --others --exclude-standard \
        | while IFS= read -r -d '' f; do if [[ -e "$f" || -L "$f" ]]; then printf '%s\0' "$f"; fi; done \
        | tar --null -T - -cf - | tar -x -C "$SRC"
    if [[ "$cargo_ver" != "$VERSION" ]]; then
        awk -v v="$VERSION" '/^\[workspace.package\]/{s=1} /^\[/ && !/^\[workspace.package\]/{s=0}
            s && /^version[[:space:]]*=/ && !done {print "version = \"" v "\""; done=1; next} {print}' "$ROOT/Cargo.toml" > "$SRC/Cargo.toml"
    fi
    # Arch forbids '-' in pkgver; the convention (and packaging/arch/build-local.sh) maps it to '_'.
    sed -i -e "s/^pkgver=.*/pkgver=${VERSION//-/_}/" -e 's/^pkgrel=.*/pkgrel=1/' "$SRC/packaging/arch/PKGBUILD"
    ( cd "$SRC" && cargo update --workspace --offline >/dev/null 2>&1 || true
      git init -q . && git add -A )
else
    SRC="$ROOT"
fi
export CARGO_BUILD_JOBS="$JOBS"

# ---------------------------------------------------------------- helpers
step() { # step NAME cmd...  -> log to $LOGS/NAME.log, show elapsed time, tail the log on failure
    local name="$1" t0=$SECONDS; shift
    log "$name   (log: $LOGS/$name.log)"
    # Run as a background job and wait: inside `if`/`||` bash would ignore `set -e` in the called functions.
    local rc=0
    "$@" >"$LOGS/$name.log" 2>&1 &
    wait "$!" || rc=$?
    if (( rc != 0 )); then
        tail -n 40 "$LOGS/$name.log" >&2
        die "step '$name' failed; full log: $LOGS/$name.log"
    fi
    note "done in $(( (SECONDS - t0) / 60 )) min $(( (SECONDS - t0) % 60 )) s"
}
BINS=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share)

# ---------------------------------------------------------------- checks
check_all() {
    cd "$SRC"
    export CARGO_TARGET_DIR="${GB_CHECK_TARGET_DIR:-$ROOT/target}"
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
}
if (( TESTS )); then step checks check_all; else warn "--skip-tests: fmt/clippy/tests were NOT run by this script"; fi

# ---------------------------------------------------------------- Linux desktop
DESK_TARGET="$WORK/target-desktop"
build_desktop() {
    cd "$SRC"
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$(gb_repro_rustflags "$SRC")" \
        CARGO_TARGET_DIR="$DESK_TARGET" cargo build --release --locked -p bridge-apps -p bridge-service
}
package_linux_archives() {
    local dir="$WORK/stage/geekbridge-linux-x86_64" b
    rm -rf "$dir"; mkdir -p "$dir"
    for b in "${BINS[@]}"; do cp "$DESK_TARGET/release/$b" "$dir/"; done
    cp "$SRC/README.md" "$dir/" || true
    gb_repro_tar_gz "$OUT/geekbridge-linux-x86_64.tar.gz" "$WORK/stage" geekbridge-linux-x86_64
    "$SRC/scripts/package-linux.sh" --bin-dir "$DESK_TARGET/release" --out "$OUT" --version "$VERSION"
}
step build-linux build_desktop
step package-linux package_linux_archives

# ---------------------------------------------------------------- SBOM (CycloneDX 1.5; SHA256SUMS below covers it)
step sbom "$SRC/scripts/gen-sbom.sh" --root "$SRC" --out "$OUT/geekbridge-sbom.cdx.json"

# ---------------------------------------------------------------- Arch package
build_arch() {
    local arch_out="$WORK/arch-out"
    mkdir -p "$arch_out"
    GB_TMP="$WORK/tmp" "$SRC/packaging/arch/build-local.sh" --out "$arch_out"
    # makepkg also emits geekbridge-debug-*; only the main package is a release asset.
    cp "$arch_out/geekbridge-${VERSION//-/_}-1-x86_64.pkg.tar.zst" "$OUT/"
}
arch_pkgver="$(sed -n 's/^pkgver=//p' "$SRC/packaging/arch/PKGBUILD" | head -n1)"
if (( ARCH )); then
    if [[ "$arch_pkgver" != "${VERSION//-/_}" ]]; then
        warn "packaging/arch/PKGBUILD pkgver is '$arch_pkgver', expected '${VERSION//-/_}': skipping the Arch package (scripts/release.sh leaves a prerelease pkgver to you)."
        ARCH=0
    else
        step build-arch build_arch
    fi
fi

# ---------------------------------------------------------------- Android
APK_UNSIGNED_DIR="$SRC/android/app/build/outputs/apk/release"
build_android_native() {
    set -e
    export CARGO_TARGET_DIR="$WORK/target-android"
    cd "$SRC"
    local entry triple abi
    for entry in "aarch64-linux-android:arm64-v8a" "x86_64-linux-android:x86_64"; do
        triple="${entry%%:*}"; abi="${entry##*:}"
        cargo build --profile android --locked -p bridge-android --target "$triple"
        mkdir -p "$SRC/android/app/src/main/jniLibs/$abi"
        "$GB_NDK_BIN/llvm-strip" --strip-unneeded -o "$SRC/android/app/src/main/jniLibs/$abi/libgeekbridge_android.so" \
            "$CARGO_TARGET_DIR/$triple/android/libgeekbridge_android.so"
    done
}
gradle_release() { # gradle_release ABIS dest.apk
    set -e
    printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$SRC/android/local.properties"
    local tasks=(:app:assembleRelease)
    [[ -n "${GB_GRADLE_TEST:-}" ]] && tasks=(:app:testDebugUnitTest "${tasks[@]}")
    local extra=()
    [[ -n "${GB_RELEASE_REPO:-}" ]] && extra+=("-Pgb.releaseRepo=$GB_RELEASE_REPO")
    rm -rf "$APK_UNSIGNED_DIR"
    gradle -p "$SRC/android" --console=plain --no-daemon "--max-workers=$JOBS" "-Pgb.abis=$1" "${extra[@]}" "${tasks[@]}"
    local apks=("$APK_UNSIGNED_DIR"/*.apk)
    [[ ${#apks[@]} -eq 1 && -f "${apks[0]}" ]] || { echo "expected exactly one release APK in $APK_UNSIGNED_DIR" >&2; exit 1; }
    cp "${apks[0]}" "$2"
    cp "$SRC/android/app/build/outputs/mapping/release/mapping.txt" "$WORK/r8-mapping-${1//,/+}.txt" 2>/dev/null || true
}
sign_apk() { # sign_apk unsigned.apk signed.apk
    set -e
    local ks="$KS" pass="file:$PASS_FILE" alias="$ALIAS" apass=""
    if (( ! RELEASE_SIGNED )); then
        ks="${ANDROID_USER_HOME:-$HOME/.android}/debug.keystore"
        [[ -f "$ks" ]] || ks="$HOME/.android/debug.keystore"
        if [[ ! -f "$ks" ]]; then
            ks="$BASE/debug.keystore"
            [[ -f "$ks" ]] || keytool -genkeypair -keystore "$ks" -storetype PKCS12 -storepass android -keypass android \
                -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
        fi
        pass="pass:android"; alias="androiddebugkey"
    else
        # apksigner needs a newline-terminated password line; make-keystore.sh writes none (see scripts/lib/signpass.sh).
        apass="$(mktemp "$TMPDIR/ks-pass.XXXXXX")"
        gb_signing_pass_file "$PASS_FILE" "$apass"
        pass="file:$apass"
    fi
    zipalign -f -P 16 4 "$1" "$1.aligned"
    # v1 + v2 + v3, exactly as release.yml (minSdk 26: v2/v3 are what the platform checks).
    local rc=0
    apksigner sign --ks "$ks" --ks-pass "$pass" --ks-key-alias "$alias" --key-pass "$pass" \
        --v1-signing-enabled true --v2-signing-enabled true --v3-signing-enabled true --v4-signing-enabled false --out "$2" "$1.aligned" || rc=$?
    rm -f "$1.aligned" ${apass:+"$apass"}
    return "$rc"
}
android_all() {
    set -e
    build_android_native
    mkdir -p "$WORK/apk"
    gradle_release "arm64-v8a,x86_64" "$WORK/apk/universal-unsigned.apk"
    sign_apk "$WORK/apk/universal-unsigned.apk" "$OUT/geekbridge-android.apk"
    # Phone-only build: the native lib for the emulator ABI is simply not packaged.
    gradle_release "arm64-v8a" "$WORK/apk/arm64-unsigned.apk"
    sign_apk "$WORK/apk/arm64-unsigned.apk" "$OUT/geekbridge-android-arm64.apk"
}
[[ $TESTS == 1 ]] && export GB_GRADLE_TEST=1
step build-android with_android android_all

# ---------------------------------------------------------------- Windows (experimental)
WIN_ZIP="geekbridge-windows-x86_64-gnu-experimental.zip"
build_windows() {
    set -e
    local zv="$BASE/zig-venv" w="$BASE/zig-wrappers" py
    if [[ ! -x "$zv/bin/python" ]]; then python3 -m venv "$zv"; "$zv/bin/pip" install -q "ziglang==${GB_ZIG_VERSION:-0.17.0}"; fi
    py="$zv/bin/python"
    mkdir -p "$w"
    cat > "$w/zcc" <<EOF
#!/bin/bash
# zig cc as the linker driver for x86_64-pc-windows-gnu. Import libraries that the windows-targets crates ship as
# lib<name>.a are passed by path (zig's lld only looks for *.dll under -Bdynamic); the MinGW runtime libs rustc asks for
# are provided by zig itself, and -lgcc_eh becomes zig's libunwind.
dirs=(); for ((i=1;i<=\$#;i++)); do [[ "\${!i}" == -L ]] && { j=\$((i+1)); dirs+=("\${!j}"); }; done
args=()
for a in "\$@"; do
  case "\$a" in
    --target=*|-m64|-nodefaultlibs|-l:libpthread.a|-lmsvcrt|-lmingwex|-lmingw32|-lgcc) continue;;
    -lgcc_eh) args+=("-lunwind"); continue;;
    -l?*) n="\${a#-l}"; hit=""; for d in "\${dirs[@]}"; do [[ -f "\$d/lib\$n.a" ]] && { hit="\$d/lib\$n.a"; break; }; done
          [[ -n "\$hit" ]] && { args+=("\$hit"); continue; };;
  esac
  args+=("\$a")
done
exec "$py" -m ziglang cc -target x86_64-windows-gnu "\${args[@]}"
EOF
    printf '#!/bin/bash\nexec "%s" -m ziglang ar "$@"\n' "$py" > "$w/zar"
    printf '#!/bin/bash\nexec "%s" -m ziglang dlltool "$@"\n' "$py" > "$w/x86_64-w64-mingw32-dlltool"
    chmod +x "$w/zcc" "$w/zar" "$w/x86_64-w64-mingw32-dlltool"
    # The rust-std for the target comes from the project-local rustup (the one scripts/xcheck.sh uses).
    export RUSTUP_HOME="${GB_WIN_RUSTUP_HOME:-$ROOT/.tools/windows-check/rustup}" CARGO_HOME="${GB_WIN_CARGO_HOME:-$ROOT/.tools/cargo}"
    export RUSTUP_TOOLCHAIN="${GB_WIN_TOOLCHAIN:-1.99.1-x86_64-unknown-linux-gnu}"
    export PATH="$w:$CARGO_HOME/bin:$PATH"
    rustup target list --installed | grep -qx x86_64-pc-windows-gnu || rustup target add x86_64-pc-windows-gnu
    export CARGO_TARGET_DIR="$WORK/target-windows"
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$w/zcc" CC_x86_64_pc_windows_gnu="$w/zcc" AR_x86_64_pc_windows_gnu="$w/zar"
    cd "$SRC"
    cargo build --release --locked --target x86_64-pc-windows-gnu -p bridge-apps -p bridge-service
    local dir="$WORK/stage/geekbridge-windows-x86_64" b
    rm -rf "$dir"; mkdir -p "$dir"
    for b in "${BINS[@]}"; do cp "$CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release/$b.exe" "$dir/"; done
    cp "$SRC/README.md" "$dir/" || true
    ( cd "$WORK/stage" && rm -f "$OUT/$WIN_ZIP" && zip -qr "$OUT/$WIN_ZIP" geekbridge-windows-x86_64 )
}
if (( WINDOWS )); then
    warn "--with-windows: the Windows zip is cross-built with zig, UNSIGNED and NEVER RUN on Windows (no Wine here). Experimental."
    step build-windows build_windows
fi

# ---------------------------------------------------------------- assets: SHA256SUMS, notes
log "SHA256SUMS and release notes"
SIGNER_NOTE=""
verify_apk() { # verify_apk file -> prints signer sha256 on stdout, checks name/code/schemes
    local apk="$1" out signer badge want_code
    out="$(with_android apksigner verify --verbose --print-certs "$apk")" || die "apksigner verify failed for $apk"
    signer="$(sed -n 's/^Signer #1 certificate SHA-256 digest: *//p' <<<"$out" | head -n1 | tr 'A-F' 'a-f')"
    [[ "$signer" =~ ^[0-9a-f]{64}$ ]] || die "could not read the signer of $apk"
    grep -q "Verified using v2 scheme (APK Signature Scheme v2): true" <<<"$out" || die "$apk is not v2-signed"
    grep -q "Verified using v3 scheme (APK Signature Scheme v3): true" <<<"$out" || die "$apk is not v3-signed"
    if (( RELEASE_SIGNED )) && [[ -n "$want_pin" && "$signer" != "$want_pin" ]]; then
        die "$apk signer $signer does not match the pinned $want_pin"
    fi
    badge="$(with_android aapt2 dump badging "$apk")"
    want_code="$("$SRC/scripts/version-code.sh" --code "$VERSION")"
    grep -q "^package: name='dev.geekbridge.android' versionCode='$want_code' versionName='$VERSION'" <<<"$badge" \
        || die "$apk: versionCode/versionName is not $want_code/$VERSION: $(grep '^package:' <<<"$badge")"
    grep -q "^minSdkVersion:'26'" <<<"$badge" || die "$apk: minSdk is not 26"
    printf '%s\n' "$signer"
}
for apk in geekbridge-android.apk geekbridge-android-arm64.apk; do
    s="$(verify_apk "$OUT/$apk")"; note "$apk: versionName $VERSION, versionCode $("$SRC/scripts/version-code.sh" --code "$VERSION"), signer $s"
    SIGNER="$s"
done
if (( RELEASE_SIGNED )); then
    SIGNER_NOTE="Android APK signer certificate SHA-256: \`$SIGNER\`"
else
    SIGNER_NOTE="Android APK is DEBUG-signed (prerelease only; not updatable from or to a release build). Signer SHA-256: \`$SIGNER\`"
fi
no_assets="Linux (x86_64 tarball, installer tarball, .deb, Arch package), Android and a CycloneDX SBOM (geekbridge-sbom.cdx.json) only."
(( WINDOWS )) && no_assets="Linux, Android, a CycloneDX SBOM and an EXPERIMENTAL, untested Windows zip (geekbridge-windows-x86_64-gnu-experimental.zip; the in-app updater never offers it)."
{
    printf '%s\n\n%s\n\n' "$notes_body" "$SIGNER_NOTE"
    printf 'Assets: %s Windows and macOS builds are not part of this release (build from source). ' "$no_assets"
    printf 'Built locally from commit %s; verify downloads against `SHA256SUMS`.\n' "${HEAD_SHA:0:12}"
} > "$WORK/release-notes.md"
(
    cd "$OUT"
    shopt -s nullglob
    files=(geekbridge-* geekbridge_*)
    sha256sum -- "${files[@]}" > SHA256SUMS
)
cat "$OUT/SHA256SUMS"

# ---------------------------------------------------------------- verification before any upload
log "Verifying the artifacts"
fail=0
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }
ok()  { note "ok: $*"; }

# 1. SHA256SUMS matches the files.
( cd "$OUT" && sha256sum -c --quiet SHA256SUMS ) && ok "SHA256SUMS matches" || bad "SHA256SUMS does not match the files"

# 1b. The SBOM is there, is CycloneDX 1.5 for this version, and is listed in SHA256SUMS.
python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["bomFormat"]=="CycloneDX" and d["specVersion"]=="1.5"; assert d["metadata"]["component"]["version"]==sys.argv[2]; assert len(d["components"])>100' \
    "$OUT/geekbridge-sbom.cdx.json" "$VERSION" 2>/dev/null \
    && grep -q ' geekbridge-sbom\.cdx\.json$' "$OUT/SHA256SUMS" \
    && ok "geekbridge-sbom.cdx.json: CycloneDX 1.5 for $VERSION, in SHA256SUMS" || bad "SBOM missing, malformed, wrong version or not in SHA256SUMS"

# 2. The tarball runs, and says the tag's version.
vdir="$(mktemp -d "$WORK/tmp/verify.XXXXXX")"
tar -xzf "$OUT/geekbridge-linux-x86_64.tar.gz" -C "$vdir"
for b in "${BINS[@]}"; do [[ -x "$vdir/geekbridge-linux-x86_64/$b" ]] || bad "tarball lacks $b"; done
ctl_out="$("$vdir/geekbridge-linux-x86_64/geekbridge-ctl" --version 2>&1 || true)"
[[ "$ctl_out" == "geekbridge-ctl $VERSION" ]] && ok "geekbridge-ctl --version = '$ctl_out'" || bad "geekbridge-ctl --version printed '$ctl_out', wanted 'geekbridge-ctl $VERSION'"
rm -rf "$vdir"

# 3. Installer tarball and .deb.
inst="$OUT/geekbridge-$VERSION-linux-x86_64-installer.tar.gz"
listing="$(tar -tzf "$inst")" && grep -q 'install\.sh$' <<<"$listing" && ok "installer tarball has install.sh ($(wc -l <<<"$listing") entries)" || bad "installer tarball lacks install.sh"
debv="$(sed 's/-/~/' <<<"$VERSION")"
deb="$OUT/geekbridge_${debv}_amd64.deb"
if [[ -f "$deb" ]]; then
    ddir="$(mktemp -d "$WORK/tmp/deb.XXXXXX")"
    ( cd "$ddir" && ar x "$deb" )
    members="$(ar t "$deb" | tr '\n' ' ')"
    ctl_ver="$(tar -xJOf "$ddir"/control.tar.xz ./control | sed -n 's/^Version: //p')"
    dlist="$(tar -tJf "$ddir"/data.tar.xz)"
    missing=""; for b in "${BINS[@]}"; do grep -qx "./usr/bin/$b" <<<"$dlist" || missing+="$b "; done
    [[ -z "$missing" && "$ctl_ver" == "$debv" && "$members" == *debian-binary* ]] \
        && ok ".deb: Version $ctl_ver, ${#BINS[@]} binaries under usr/bin ($(wc -l <<<"$dlist") paths)" \
        || bad ".deb problem (version '$ctl_ver', missing: ${missing:-none})"
    rm -rf "$ddir"
else bad "the .deb is missing"; fi

# 4. Arch package.
if (( ARCH )); then
    pkg="$OUT/geekbridge-${VERSION//-/_}-1-x86_64.pkg.tar.zst"
    if [[ -f "$pkg" ]]; then
        plist="$(tar --zstd -tf "$pkg")"
        pinfo="$(tar --zstd -xOf "$pkg" .PKGINFO)"
        missing=""; for b in "${BINS[@]}"; do grep -qx "usr/bin/$b" <<<"$plist" || missing+="$b "; done
        grep -q "^pkgver = ${VERSION//-/_}-1\$" <<<"$pinfo" && [[ -z "$missing" ]] \
            && ok "Arch package $(basename "$pkg"): $(grep -c . <<<"$plist") entries, all ${#BINS[@]} binaries, pkgver ${VERSION//-/_}-1" \
            || bad "Arch package problem (pkgver line: $(grep '^pkgver' <<<"$pinfo"); missing: ${missing:-none})"
    else bad "the Arch package is missing"; fi
fi

# 5. Windows zip: PE files, and only system DLL imports.
if (( WINDOWS )); then
    wdir="$(mktemp -d "$WORK/tmp/win.XXXXXX")"
    unzip -q "$OUT/$WIN_ZIP" -d "$wdir"
    for b in "${BINS[@]}"; do
        f="$wdir/geekbridge-windows-x86_64/$b.exe"
        file "$f" | grep -q 'PE32+ executable.*x86-64' || bad "$b.exe is not a PE32+ x86-64 file"
        objdump -p "$f" 2>/dev/null | awk '/DLL Name/{print tolower($3)}' | grep -Eq '^(libgcc|libwinpthread|libstdc|libunwind|libc\+\+)' && bad "$b.exe imports a non-system runtime DLL"
    done
    ok "Windows zip: ${#BINS[@]} PE32+ x86-64 executables with system-DLL imports only (never executed)"
    rm -rf "$wdir"
fi
(( fail == 0 )) || die "verification failed; nothing was tagged or published"
ls -l "$OUT"

# ---------------------------------------------------------------- tag, publish (only on request)
# run: git tag/push, only ever called when --tag/--push-tag was given. grun: gh, executes only with --publish.
if (( DRY )); then
    run()  { local IFS=' '; printf '[dry-run stub] %s\n' "$*"; }
    grun() { local IFS=' '; printf '[dry-run stub] %s\n' "$*"; }
else
    run()  { ( local IFS=' '; printf '+ %s\n' "$*" ); "$@"; }
    if (( PUBLISH )); then grun() { ( local IFS=' '; printf '+ %s\n' "$*" ); "$@"; }; else grun() { local IFS=' '; printf '  %s\n' "$*"; }; fi
fi
repo_default="$(git remote get-url "$REMOTE" 2>/dev/null | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##' || true)"
REPO="${GB_RELEASE_REPO:-${repo_default:-fspecii/geekbridge}}"
[[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "release repository '$REPO' is not owner/name"

log "Git tag"
if (( MKTAG )); then
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then note "tag $TAG already exists locally"
    else run git -c tag.gpgSign=false tag -a "$TAG" -m "GeekBridge $TAG" "$HEAD_SHA"; fi
elif ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    note "no local tag yet; create it with --tag (or: git tag -a $TAG -m \"GeekBridge $TAG\")"
fi
if (( PUSHTAG )); then
    (( DRY )) || git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "--push-tag: no local tag $TAG (use --tag)"
    run git push "$REMOTE" "refs/tags/$TAG"
fi

publish_cmds() { # the exact commands, in the order release.yml uses
    local flags=(--repo "$REPO" --title "GeekBridge $VERSION" --verify-tag --notes-file "$WORK/release-notes.md")
    (( PRERELEASE )) && flags+=(--prerelease)
    grun gh release create "$TAG" "$OUT"/* --draft "${flags[@]}"
    grun gh release edit "$TAG" --repo "$REPO" --draft=false
    grun gh release view "$TAG" --repo "$REPO" --json url --jq .url
}
if (( DRY )); then
    log "Publish (dry run: gh is a printing stub)"
    publish_cmds
elif (( PUBLISH )); then
    log "Publishing $TAG to $REPO"
    command -v gh >/dev/null || die "gh not found"
    gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
    git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null 2>&1 \
        || die "tag $TAG is not on $REMOTE; run again with --tag --push-tag (never created implicitly)"
    if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then die "a release $TAG already exists in $REPO"; fi
    # Draft first, undraft only once every asset is attached: the in-app updaters read releases/latest.
    publish_cmds
else
    log "Not published (no --publish). To publish exactly this build:"
    printf '\n'
    (( MKTAG )) || { echo "  git tag -a $TAG -m \"GeekBridge $TAG\""; }
    (( PUSHTAG )) || { echo "  git push $REMOTE refs/tags/$TAG"; }
    publish_cmds
    printf '\n  (or re-run this script with --tag --push-tag --publish; rebuilding takes the full time again)\n'
fi

if (( PRUNE )); then
    log "Pruning build trees (assets in $OUT are kept)"
    rm -rf "$WORK/target-desktop" "$WORK/target-android" "$WORK/target-windows" "$WORK/src" "$WORK/stage" "$WORK/tmp" "$WORK/arch-out" "$WORK/apk"
fi
log "Done. Assets: $OUT   notes: $WORK/release-notes.md   logs: $LOGS"
du -sh "$WORK" 2>/dev/null | sed 's/^/    scratch size: /'
(( problems == 0 )) || warn "dry run: $problems problem(s) above would have stopped a real release"
