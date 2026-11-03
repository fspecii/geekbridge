# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Signing-key and version checks for a sideloaded APK (source it; do not run it).
#
# Android installs an update over an existing app only when the signing certificate is the SAME and the versionCode is
# not lower. A different key fails with INSTALL_FAILED_UPDATE_INCOMPATIBLE and the only way out is to uninstall,
# which deletes the pairings. Debug builds are signed with $ANDROID_USER_HOME/debug.keystore; scripts/android-env.sh
# points that at the project's .tools/android-user, so a build made in a shell that did NOT source it (or on another
# machine / a CI runner) is signed with ~/.android/debug.keystore instead and will not update the phone's copy.
#
#   apk_signer_sha256 FILE        signer certificate SHA-256 (lower case hex; several signers: sorted, comma joined)
#   apk_version_code FILE         versionCode from the manifest
#   apk_update_verdict OLD NEW    ok | same-version | downgrade | other-key | unsigned | unknown
#                                 what Android will do with `install -r NEW` over an installed OLD
# GB_APKSIGNER / GB_AAPT2 override the tools (default: apksigner / aapt2 on PATH, else the project SDK).

_gb_apk_tool() { # name override-var
  local name="$1" ov="${!2:-}" root d
  [[ -n "$ov" ]] && { [[ -x "$ov" ]] && { printf '%s\n' "$ov"; return 0; }; return 1; }
  command -v "$name" 2>/dev/null && return 0
  root="${GB_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)}"
  for d in "$root"/.tools/android-sdk/build-tools/*/; do [[ -x "$d$name" ]] && { printf '%s\n' "$d$name"; return 0; }; done
  return 1
}

apk_signer_sha256() {
  local apk="${1:?apk}" tool out
  [[ -f "$apk" ]] || return 1
  tool="$(_gb_apk_tool apksigner GB_APKSIGNER)" || { echo "apk-signer: apksigner not found (source scripts/android-env.sh)" >&2; return 1; }
  out="$("$tool" verify --print-certs "$apk" 2>/dev/null)" || return 1
  out="$(sed -n 's/^Signer #[0-9]* certificate SHA-256 digest: *//p' <<<"$out" | tr 'A-F' 'a-f' | sort | paste -sd, -)"
  [[ -n "$out" ]] || return 1
  printf '%s\n' "$out"
}

apk_version_code() {
  local apk="${1:?apk}" tool
  tool="$(_gb_apk_tool aapt2 GB_AAPT2)" || return 1
  "$tool" dump badging "$apk" 2>/dev/null | sed -n "s/^package:.* versionCode='\([0-9]*\)'.*/\1/p" | head -1
}

apk_update_verdict() {
  local old="${1:?old}" new="${2:?new}" so sn vo vn
  sn="$(apk_signer_sha256 "$new")" || { echo unsigned; return 0; }
  so="$(apk_signer_sha256 "$old")" || { echo unknown; return 0; }
  [[ "$so" == "$sn" ]] || { echo other-key; return 0; }
  vo="$(apk_version_code "$old")"; vn="$(apk_version_code "$new")"
  [[ -n "$vo" && -n "$vn" ]] || { echo unknown; return 0; }
  if ((vn < vo)); then echo downgrade; elif ((vn == vo)); then echo same-version; else echo ok; fi
}
