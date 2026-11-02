#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Generate the "What's new" texts shown once after an update, from CHANGELOG.md:
#   android/app/src/main/assets/whats_new.json   (Android: read by WhatsNewModel)
#   crates/bridge-core/assets/whats_new.txt      (desktop: include_str! by bridge_core::whats_new)
# Both are checked in; `--check` (run by a Rust and a Gradle unit test, and by release.sh) fails when they differ
# from what this script would write, so they cannot drift from CHANGELOG.md or from the Cargo.toml version.
#
#   scripts/gen-whats-new.sh [--check] [--version X.Y.Z[-pre]] [--changelog FILE] [--stdout]
#
#   --check       write nothing; exit 1 when a generated file is out of date
#   --version     section to extract (default: [workspace.package] version in Cargo.toml)
#   --changelog   input (default: CHANGELOG.md)
#   --stdout      print the desktop text instead of writing files (used by the extraction tests)
#
# Plain text only: markdown (links, emphasis, code, headings, html) and control characters are removed, each
# highlight is one line of at most 140 characters, and there are at most 5. A version with no section in the
# changelog gets the single line "See the release notes".
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

CHECK=0 STDOUT=0 VERSION="" CHANGELOG="CHANGELOG.md"
while (($#)); do
    case "$1" in
        --check) CHECK=1 ;;
        --stdout) STDOUT=1 ;;
        --version) VERSION="${2:?--version needs a value}"; shift ;;
        --changelog) CHANGELOG="${2:?--changelog needs a file}"; shift ;;
        -h|--help) sed -n '3,17p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [[ -z "$VERSION" ]]; then
    VERSION="$(awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml)"
fi
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "not a version: $VERSION" >&2; exit 2; }
[[ -r "$CHANGELOG" ]] || { echo "cannot read $CHANGELOG" >&2; exit 1; }

# One highlight per line (already sanitised and capped), at most 5. Empty output means "no section / no bullets".
highlights_pass() {
    local pref="$1"
    LC_ALL=C.UTF-8 awk -v ver="$VERSION" -v pref="$pref" '
    function clean(s,   t) {
        gsub(/[\001-\010\013-\037\177]/, " ", s)            # control characters (tab and newline are handled below)
        gsub(/\t/, " ", s)
        gsub(/<[^>]*>/, "", s)                              # html tags
        while (match(s, /!?\[[^]]*\]\([^)]*\)/)) {          # [text](url) and ![alt](url) -> text
            t = substr(s, RSTART, RLENGTH)
            sub(/^!?\[/, "", t); sub(/\]\(.*$/, "", t)
            s = substr(s, 1, RSTART - 1) t substr(s, RSTART + RLENGTH)
        }
        gsub(/[`*_~]+/, "", s)                              # code, emphasis, strike (no intra-word underscores kept)
        gsub(/[][#>|\\]/, "", s)                            # brackets, heading, quote, table and escape characters
        gsub(/ +/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s)
        return s
    }
    function cap(s,   i, cut, k, head) {
        # Prefer the first sentence; a long one is cut at the last clause break (", " " (" "; " ": ") that keeps
        # at least 30 characters, else at a word boundary with an ellipsis. Never more than 140 characters.
        i = index(s, ". ")
        if (i > 0 && i <= 140) s = substr(s, 1, i - 1)
        if (length(s) > 110) {
            head = substr(s, 1, 110); cut = 0
            for (k = 30; k <= 109; k++) {
                if (substr(head, k, 2) == ", " || substr(head, k, 2) == "; " || substr(head, k, 2) == ": " || substr(head, k, 2) == " (") cut = k
            }
            if (cut > 0) s = substr(head, 1, cut - 1)
            else { s = substr(s, 1, 109); sub(/ [^ ]*$/, "", s); s = s "…" }
        }
        sub(/[.:;,]+$/, "", s)
        return s
    }
    function flush() {
        if (cur != "" && n < 5) {
            h = cap(clean(cur))
            if (h != "") { out[++n] = h }
        }
        cur = ""
    }
    BEGIN { insec = 0; keep = 0; n = 0; head = "## [" ver "]" }
    /^## / {
        flush()
        insec = (index($0, head) == 1 && (substr($0, length(head) + 1, 1) == "" || substr($0, length(head) + 1, 1) == " ")) ? 1 : 0
        keep = 0
        next
    }
    insec && /^### / {
        flush()
        if (pref == 1) keep = ($0 ~ /^### Highlights[ \t]*$/) ? 1 : 0
        else keep = ($0 ~ /^### (Added|Changed|Fixed|Security)[ \t]*$/) ? 1 : 0
        next
    }
    insec && keep && /^- / { flush(); cur = substr($0, 3); next }
    insec && keep && /^  +[^ ]/ && cur != "" { cur = cur " " $0; next }
    insec && keep && /^[ \t]*$/ { flush(); next }
    END { flush(); for (i = 1; i <= n; i++) print out[i] }
    ' "$CHANGELOG"
}

# A "### Highlights" subsection (plain-language lines written for users) wins; otherwise the first bullets of the
# Added / Changed / Fixed / Security sections are used.
highlights() {
    local o
    o="$(highlights_pass 1)"
    if [[ -n $o ]]; then printf '%s\n' "$o"; else highlights_pass 0; fi
}

mapfile -t LINES < <(highlights)
FALLBACK=0
if ((${#LINES[@]} == 0)); then
    FALLBACK=1
    LINES=("See the release notes")
fi

# Desktop text: "version: X", then one "- highlight" per line. Android: the same as JSON.
desktop_text() {
    printf 'version: %s\n' "$VERSION"
    printf -- '- %s\n' "${LINES[@]}"
}
json_escape() { local s="$1"; s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; printf '%s' "$s"; }
android_json() {
    printf '{\n  "version": "%s",\n  "fallback": %s,\n  "highlights": [\n' "$VERSION" "$([[ $FALLBACK == 1 ]] && echo true || echo false)"
    local i last=$((${#LINES[@]} - 1))
    for i in "${!LINES[@]}"; do
        printf '    "%s"%s\n' "$(json_escape "${LINES[$i]}")" "$([[ $i == "$last" ]] || echo ,)"
    done
    printf '  ]\n}\n'
}

if ((STDOUT)); then desktop_text; exit 0; fi

DESK="crates/bridge-core/assets/whats_new.txt"
ANDROID="android/app/src/main/assets/whats_new.json"
if ((CHECK)); then
    bad=0
    for pair in "$DESK:desktop_text" "$ANDROID:android_json"; do
        f="${pair%%:*}"; fn="${pair##*:}"
        if [[ ! -f "$f" ]] || ! diff -q <("$fn") "$f" >/dev/null; then
            echo "out of date: $f (run scripts/gen-whats-new.sh)" >&2; bad=1
        fi
    done
    exit "$bad"
fi
mkdir -p "$(dirname "$DESK")" "$(dirname "$ANDROID")"
desktop_text > "$DESK"
android_json > "$ANDROID"
echo "wrote $DESK and $ANDROID for $VERSION (${#LINES[@]} highlight(s)$([[ $FALLBACK == 1 ]] && echo ', fallback'))"
