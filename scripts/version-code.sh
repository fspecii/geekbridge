#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prints the Android versionName and versionCode derived from [workspace.package] version in Cargo.toml
# (or from an explicit version argument). Same formula as android/app/build.gradle.kts and
# AppVersion.toVersionCode (UpdateModel.kt); docs/builds.md#android-version-code.
#
#   versionCode = (major*10000 + minor*100 + patch) * 100 + slot
#   slot: 99 final; alpha.N 0..24, beta.N 25..49, rc.N 50..74 (N capped at 24); so rc < final.
#
# Debug (dev-channel) builds do not use this formula: versionCode = 20000 + number of commits
# (`git rev-list --count HEAD`), so every new main build is a higher code (docs/updates.md#dev-channel).
#
# Usage: scripts/version-code.sh [VERSION]      prints "NAME CODE"
#        scripts/version-code.sh --code [VERSION]   prints the code only
#        scripts/version-code.sh --dev [COUNT]      the debug/dev code; COUNT defaults to `git rev-list --count HEAD`
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

DEV_CODE_BASE=20000   # keep in step with android/app/build.gradle.kts and bridge-update's DEV_CODE_BASE

dev_code() {
  local n="${1:-$(git rev-list --count HEAD 2>/dev/null || echo 0)}"
  [[ "$n" =~ ^[0-9]+$ ]] || { echo "not a commit count: $n" >&2; return 1; }
  echo $(( DEV_CODE_BASE + 10#$n ))
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
  [[ "$(dev_code 161)" == 20161 && "$(dev_code 0)" == 20000 ]] || { echo "FAIL dev_code"; fail=1; }
  dev_code x >/dev/null 2>&1 && { echo "FAIL dev_code accepts text"; fail=1; }
  [[ $fail == 0 ]] && echo "version-code self-test OK"
  return $fail
}

case "${1:-}" in
  --self-test) self_test ;;
  --dev) dev_code "${2:-}" ;;
  --code) code_for "${2:-$(cargo_version)}" ;;
  *) v="${1:-$(cargo_version)}"; echo "$v $(code_for "$v")" ;;
esac
