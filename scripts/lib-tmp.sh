# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Scratch-space helper for GeekBridge scripts (source it; do not run it).
#   GB_TMP   where scripts put scratch files (default: $TMPDIR, else /tmp). When set, TMPDIR and
#            GEEKBRIDGE_TEST_TMP follow it so child processes (cargo tests, emulators) agree.
#   gb_mktemp NAME [-d]   -> a fresh path named <GB_TMP>/gb-NAME.XXXXXX (file, or directory with -d)
# Every name starts with "gb-" or "geekbridge-" so scripts/clean-tmp.sh can find (only) our leavings.
GB_TMP="${GB_TMP:-${TMPDIR:-/tmp}}"
if [[ -n "${GB_TMP_FORCED:-}" || "${GB_TMP}" != "${TMPDIR:-/tmp}" ]]; then export TMPDIR="$GB_TMP"; fi
export GEEKBRIDGE_TEST_TMP="${GEEKBRIDGE_TEST_TMP:-$GB_TMP}"
mkdir -p "$GB_TMP"
gb_mktemp() { local name="$1"; shift; mktemp "$@" "$GB_TMP/gb-$name.XXXXXX"; }
