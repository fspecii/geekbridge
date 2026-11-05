#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Generates the Android RELEASE keystore for GeekBridge (once, ever) and prints what to do next.
#
#   scripts/make-keystore.sh                       interactive: asks for the two output paths
#   scripts/make-keystore.sh --non-interactive --out ~/secrets/geekbridge-release.p12 \
#                            --password-file ~/secrets/geekbridge-release.pass
#
# Options:
#   --out FILE             keystore to create (PKCS12). Refuses to overwrite an existing file.
#   --password-file FILE   where the random password is written (mode 0600; never printed). Refuses to overwrite.
#   --alias NAME           key alias (default: geekbridge-release)
#   --keyalg rsa|ec        rsa = RSA 4096 (default), ec = P-384 (smaller, needs Android 7+ for v2/v3: fine, minSdk is 26)
#   --years N              validity in years, at least 25 (default 30; Play-style rule of thumb: past 2033)
#   --dname DN             certificate subject (default: CN=GeekBridge release, O=GeekBridge)
#   --repo OWNER/NAME      adds -R OWNER/NAME to the printed `gh` commands
#   --non-interactive      never prompt; --out and --password-file are then required
#
# This script never runs `gh secret set`, never uploads anything and never writes inside the repository
# unless you point --out/--password-file there (it refuses: a keystore must not live in the working tree).
set -euo pipefail

out="" passfile="" alias_="geekbridge-release" keyalg="rsa" years=30 repo=""
dname="CN=GeekBridge release, O=GeekBridge" interactive=1

die() { echo "error: $*" >&2; exit 1; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="${2:?}"; shift 2 ;;
    --password-file) passfile="${2:?}"; shift 2 ;;
    --alias) alias_="${2:?}"; shift 2 ;;
    --keyalg) keyalg="${2:?}"; shift 2 ;;
    --years) years="${2:?}"; shift 2 ;;
    --dname) dname="${2:?}"; shift 2 ;;
    --repo) repo="${2:?}"; shift 2 ;;
    --non-interactive) interactive=0; shift ;;
    -h|--help) sed -n '3,21p' "$0"; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

command -v keytool >/dev/null || die "keytool not found (install a JDK 17)"
[[ "$keyalg" == rsa || "$keyalg" == ec ]] || die "--keyalg must be rsa or ec"
[[ "$years" =~ ^[0-9]+$ ]] && (( years >= 25 )) || die "--years must be a number >= 25 (the key must outlive the project)"
[[ "$alias_" =~ ^[A-Za-z0-9_.-]+$ ]] || die "--alias may only contain letters, digits, _ . -"
[[ -z "$repo" || "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "--repo must be owner/name"

if [[ $interactive == 1 ]]; then
  [[ -n "$out" ]] || read -r -p "Keystore file to create (outside the repository, e.g. ~/secrets/geekbridge-release.p12): " out
  [[ -n "$passfile" ]] || read -r -p "File to write the generated password to (0600): " passfile
fi
[[ -n "$out" && -n "$passfile" ]] || die "--out and --password-file are required"
out="${out/#\~/$HOME}"; passfile="${passfile/#\~/$HOME}"

[[ ! -e "$out" && ! -L "$out" ]] || die "$out already exists; refusing to overwrite a keystore (losing the old one would end updates for every user). Choose another path."
[[ ! -e "$passfile" && ! -L "$passfile" ]] || die "$passfile already exists; refusing to overwrite it."
[[ "$out" != "$passfile" ]] || die "keystore and password file must differ"
for f in "$out" "$passfile"; do
  d="$(cd "$(dirname "$f")" 2>/dev/null && pwd)" || die "directory of $f does not exist"
  repo_root="$(git -C "$d" rev-parse --show-toplevel 2>/dev/null || true)"
  [[ -z "$repo_root" ]] || die "$f would be inside the git work tree $repo_root; keep keys outside any repository"
done

umask 077
# 32 random characters from [A-Za-z0-9] (192 bits); written only to the chosen file.
head -c 512 /dev/urandom | LC_ALL=C tr -dc 'A-Za-z0-9' | head -c 32 > "$passfile" || true   # head closes the pipe early: pipefail may report SIGPIPE; the length check below is the real test
chmod 600 "$passfile"
[[ "$(wc -c < "$passfile")" -eq 32 ]] || { rm -f "$passfile"; die "could not generate a password"; }

if [[ "$keyalg" == rsa ]]; then keyargs=(-keyalg RSA -keysize 4096 -sigalg SHA384withRSA)
else keyargs=(-keyalg EC -groupname secp384r1 -sigalg SHA384withECDSA); fi

if ! keytool -genkeypair -keystore "$out" -storetype PKCS12 -alias "$alias_" "${keyargs[@]}" \
      -validity $(( years * 366 )) -dname "$dname" \
      -storepass:file "$passfile" -keypass:file "$passfile" >/dev/null 2>&1; then
  rm -f "$out" "$passfile"
  die "keytool failed; nothing was kept"
fi
chmod 600 "$out"

fp_colon="$(keytool -list -v -keystore "$out" -storepass:file "$passfile" -alias "$alias_" 2>/dev/null \
  | awk -F'SHA256: ' '/SHA256:/ {print $2; exit}' | tr -d ' ' || true)"
fp="$(echo "$fp_colon" | tr -d ':' | tr 'A-F' 'a-f')"
[[ "$fp" =~ ^[0-9a-f]{64}$ ]] || die "could not read the certificate fingerprint"

r=(); [[ -n "$repo" ]] && r=(-R "$repo")
rflag="${r[*]:-}"; [[ -n "$rflag" ]] && rflag=" $rflag"

cat <<MSG

Created:
  keystore       $out   (PKCS12, ${keyalg^^}, alias "$alias_", valid $years years)
  password file  $passfile   (0600; the store and key password are the same value)

Signer certificate SHA-256:
  $fp_colon
  (repo variable form: $fp)

!!! BACK UP THE KEYSTORE AND THE PASSWORD FILE NOW !!!
    Copy both to at least two separate offline places (password manager attachment, encrypted USB).
    If this key is lost, Android will refuse every future update of GeekBridge: every user must uninstall
    (and lose app data) and reinstall. It cannot be recovered or reset. If it leaks, anyone can ship updates
    that phones accept. Never commit it; never paste the password into a chat, issue or log.

Next, review and run these yourself (this script did not run them):

  base64 -w0 "$out" | gh secret set ANDROID_KEYSTORE_B64$rflag
  gh secret set ANDROID_KEYSTORE_PASSWORD$rflag < "$passfile"
  gh secret set ANDROID_KEY_PASSWORD$rflag < "$passfile"
  gh secret set ANDROID_KEY_ALIAS$rflag --body '$alias_'
  gh variable set ANDROID_SIGNER_SHA256$rflag --body '$fp'

The last line pins the expected signer: the release job fails if the APK is ever signed by another key.
MSG
