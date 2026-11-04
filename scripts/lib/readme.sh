# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# gb_absolute_readme README.md -> stdout: the README with docs/ and assets/ links rewritten to absolute GitHub URLs,
# for packages that ship it away from the repository (tarball, .deb, Arch package).
gb_absolute_readme() {
  local repo="https://github.com/fspecii/geekbridge" raw="https://raw.githubusercontent.com/fspecii/geekbridge/main"
  sed -E \
    -e "s#(src|href)=\"(assets|docs)/#\1=\"$raw/\2/#g" \
    -e "s#\]\((assets|docs)/([^)]*\.(png|svg|jpg|gif))\)#]($raw/\1/\2)#g" \
    -e "s#\]\(([A-Za-z0-9_][A-Za-z0-9_./-]*)(\#[^)]*)?\)#]($repo/blob/main/\1\2)#g" \
    "$1"
}
