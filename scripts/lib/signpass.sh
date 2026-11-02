# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Source it; do not run it.
# gb_signing_pass_file SRC DEST   writes the FIRST line of SRC to DEST (mode 0600) TWICE, newline-terminated.
# scripts/make-keystore.sh writes the password without a newline (so `gh secret set < file` stores it exactly) and
# the store and key password are the same value. keytool -storepass:file accepts that, but apksigner fails with
# "end of file reached" unless the line is newline-terminated, and when --ks-pass and --key-pass name the SAME file it
# reads consecutive lines (store password first, key password second), hence two lines. Found by the first dry run with a throwaway key; see docs/reports/stable-release-readiness.md.
gb_signing_pass_file() {
    local src="$1" dest="$2" line
    [[ -r "$src" ]] || { echo "password file $src is not readable" >&2; return 1; }
    IFS= read -r line < "$src" || [[ -n "$line" ]] || { echo "password file $src is empty" >&2; return 1; }
    ( umask 077; printf '%s\n%s\n' "$line" "$line" > "$dest" )
}
