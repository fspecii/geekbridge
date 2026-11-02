#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prints the Android versionName and versionCode derived from [workspace.package] version in Cargo.toml
# (or from an explicit version argument). Same formula as android/app/build.gradle.kts and
# AppVersion.toVersionCode (UpdateModel.kt); docs/builds.md#android-version-code.
#
#   versionCode = (major*10000 + minor*100 + patch) * 100 + slot
#   slot: 99 final; alpha.N 0..24, beta.N 25..49, rc.N 50..74 (N capped at 24); so rc < final.
#
# Usage: scripts/version-code.sh [VERSION]      prints "NAME CODE"
#        scripts/version-code.sh --code [VERSION]   prints the code only
#        scripts/version-code.sh --self-test
set -euo pipefail
cd "$(dirname "$0")/.."

code_for() {
  local v="${1#v}" core pre="" major minor patch slot=99 base=0 n=0
  v="${v%%+*}"
  core="${v%%-*}"
  [[ "$v" == *-* ]] && pre="${v#*-}"
  [[ "$core" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || { echo "not a version: $1" >&2; return 1; }
  major=$((10#${BASH_REMATCH[1]})); minor=$((10#${BASH_REMATCH[2]})); patch=$((10#${BASH_REMATCH[3]}))
  (( major <= 214 && minor <= 99 && patch <= 99 )) || { echo "version component out of range: $1" >&2; return 1; }
  if [[ -n "$pre" ]]; then
    local label="${pre%%.*}" rest=""
    [[ "$pre" == *.* ]] && rest="${pre#*.}"
    case "${label,,}" in beta) base=25 ;; rc) base=50 ;; *) base=0 ;; esac
    rest="${rest%%.*}"
    # Digits only; a number too long for bash arithmetic is simply "large" (capped below), as in build.gradle.kts.
    if [[ "$rest" =~ ^[0-9]+$ ]]; then (( ${#rest} > 9 )) && n=24 || n=$((10#$rest)); fi
    (( n > 24 )) && n=24
    slot=$((base + n))
  fi
  echo $(( (major * 10000 + minor * 100 + patch) * 100 + slot ))
}

cargo_version() {
  awk '/^\[workspace.package\]/{s=1;next} /^\[/{s=0} s && /^version[[:space:]]*=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}' Cargo.toml
}

self_test() {
  local fail=0 pair v want got prev=0
  for pair in 0.1.0=10099 0.1.1=10199 0.2.0-rc.1=20051 0.2.0=20099 1.0.0=1000099 \
              1.0.0-alpha.1=1000001 1.0.0-beta.2=1000027 1.0.0-rc.1=1000051 1.0.0-rc.99999999999=1000074 1.0.0-alpha.0007=1000007 214.99.99=214999999 v0.1.0+build5=10099; do
    v="${pair%%=*}"; want="${pair##*=}"; got="$(code_for "$v")"
    [[ "$got" == "$want" ]] || { echo "FAIL $v: want $want got $got"; fail=1; }
  done
  # Strictly increasing in semver order.
  for v in 0.1.0 0.1.1 0.2.0-rc.1 0.2.0-rc.2 0.2.0 1.0.0-alpha.1 1.0.0-beta.1 1.0.0-rc.1 1.0.0 1.0.1 1.1.0; do
    got="$(code_for "$v")"
    (( got > prev )) || { echo "FAIL $v ($got) is not above the previous version ($prev)"; fail=1; }
    prev=$got
  done
  for v in 215.0.0 1.100.0 1.0.100 abc 1.0; do
    code_for "$v" >/dev/null 2>&1 && { echo "FAIL $v should be rejected"; fail=1; }
  done
  [[ $fail == 0 ]] && echo "version-code self-test OK"
  return $fail
}

case "${1:-}" in
  --self-test) self_test ;;
  --code) code_for "${2:-$(cargo_version)}" ;;
  *) v="${1:-$(cargo_version)}"; echo "$v $(code_for "$v")" ;;
esac
