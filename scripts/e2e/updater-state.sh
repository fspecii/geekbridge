#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Desktop update states against the GitHub replica (scripts/e2e/github_replica.py), modelled on the real
# fspecii/geekbridge releases: only a pre-release published (/releases/latest is 404, the list shows
# v0.1.0-rc.1) -> "up to date"; a stable release appears -> offered, downloaded, SHA-256 verified; ETag
# (304); rate limits (403 / 429 + Retry-After); the network going away. Never touches the live service or
# its config: own state dir, own ports, a scratch directory, loopback only, nothing is published.
#
#   scripts/e2e/updater-state.sh [--no-build]
#
# Env: UPD_STATE_TARGET (cargo target dir with debug bins; default $ROOT/target), results go to
# $W/results.tsv (W defaults to $TMPDIR/updater-state). Exit 0 only if every check passed.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
W="${UPD_STATE_W:-${TMPDIR:-/tmp}/updater-state}"; T="${UPD_STATE_TARGET:-${CARGO_TARGET_DIR:-$ROOT/target}}/debug"
REL="$W/releases"; MODE="$W/mode"; RLOG="$W/replica.log"; DRUN="$W/run"; RP=18894; CTRL=17791
SVC="$T/geekbridge-service"; CTLBIN="$T/geekbridge-ctl"; FAIL=0
log() { printf '[updstate %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
check() { if [[ "$2" == 0 ]]; then printf 'PASS\t%s\t%s\n' "$1" "${3:-}"; else printf 'FAIL\t%s\t%s\n' "$1" "${3:-}"; FAIL=1; fi; }
mode() { printf '%s\n' "$@" >"$MODE"; }
ctl() { timeout 60 "$CTLBIN" --state-dir "$DRUN/state" --control-port "$CTRL" "$@"; }
stop_replica() { pkill -f "^python3 -I scripts/e2e/github_replica.py --port $RP " 2>/dev/null; sleep 0.4; true; }
start_replica() {
  stop_replica; : >"$RLOG"
  (setsid nohup python3 -I scripts/e2e/github_replica.py --port "$RP" --repo fspecii/geekbridge --releases "$REL" \
     --mode-file "$MODE" --log "$RLOG" >"$W/replica.out" 2>&1 &)
  sleep 1
}
stop_service() { pkill -f "^$SVC --state-dir $DRUN/state" 2>/dev/null; sleep 0.7; true; }
start_service() {
  mkdir -p "$DRUN/state" "$DRUN/xdg-state"; chmod 700 "$DRUN/state"
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_UPDATE_REPO=fspecii/geekbridge \
     GEEKBRIDGE_UPDATE_BASE_URL="http://127.0.0.1:$RP" XDG_STATE_HOME="$DRUN/xdg-state" \
     setsid nohup "$SVC" --state-dir "$DRUN/state" --api-port 17790 --control-port "$CTRL" --discovery-port 17626 \
     --listen-inbound 127.0.0.1:1940 --dev-loopback --no-calling-probe --no-announce --name E2E-UpdState >>"$W/service.log" 2>&1 &)
  for _ in $(seq 20); do ctl update status >/dev/null 2>&1 && return 0; sleep 0.5; done; return 1
}
n_hits() { jq -s --arg p "$1" --arg s "${2:-}" '[.[] | select(.path==$p and (($s=="") or (.status==($s|tonumber))))] | length' "$RLOG"; }
# A release directory like the real one: the asset names release.yml / release-local.sh publish, tiny contents.
make_release() { # tag prerelease(true|false) published_at
  local d="$REL/$1" f; mkdir -p "$d"
  for f in "geekbridge-${1#v}-linux-x86_64-installer.tar.gz" "geekbridge-linux-x86_64.tar.gz" "geekbridge_${1#v}_amd64.deb" \
           "geekbridge-${1#v}-1-x86_64.pkg.tar.zst" geekbridge-android.apk geekbridge-android-arm64.apk geekbridge-windows-x86_64.zip \
           geekbridge-macos-aarch64.tar.gz geekbridge-macos-x86_64.tar.gz geekbridge-sbom.cdx.json; do
    printf 'stand-in for %s %s\n' "$1" "$f" >"$d/$f"
  done
  (cd "$d" && sha256sum -- geekbridge-* >SHA256SUMS)
  printf '{"prerelease": %s, "draft": false, "published_at": "%s"}\n' "$2" "$3" >"$d/release.json"
}

[[ "${1:-}" == --no-build ]] || cargo build -q -p bridge-service --bins || { echo "build failed" >&2; exit 2; }
[[ -x "$SVC" && -x "$CTLBIN" ]] || { echo "missing $SVC / $CTLBIN" >&2; exit 2; }
rm -rf "$W"; mkdir -p "$W" "$REL"; : >"$MODE"
trap 'stop_service; stop_replica' EXIT
make_release v0.1.0-rc.1 true 2026-11-02T18:00:00Z
start_replica; start_service || { echo "service did not start; see $W/service.log" >&2; exit 2; }

# 1. The real situation today: only a pre-release is published.
out="$(ctl update check 2>&1)"
check only-prerelease-up-to-date $([[ "$out" == *"up to date"* && "$out" != *"No public release"* && "$out" != *"newer"* ]]; echo $?) "$(grep latest <<<"$out")"
check latest-404-then-list $([[ "$(n_hits /repos/fspecii/geekbridge/releases/latest 404)" == 1 && "$(n_hits /repos/fspecii/geekbridge/releases 200)" == 1 ]]; echo $?) "one /latest (404) + one list (200)"

# 2. A stable release is published: offered for this Linux x86_64 machine, then downloaded and verified.
make_release v0.1.1 false 2026-11-03T09:00:00Z
sleep 31; out="$(ctl update check 2>&1)"
sha="$(sha256sum "$REL/v0.1.1/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)"
check stable-offered-right-asset $([[ "$out" == *"v0.1.1 - newer"* && "$out" == *"geekbridge-linux-x86_64.tar.gz (SHA-256 $sha)"* ]]; echo $?) "$(grep -E 'latest|installer' <<<"$out" | tr '\n' '|')"
out="$(ctl update download 2>&1)"; rc=$?; path="$(sed -n 's/^verified download: //p' <<<"$out")"
check download-verified $([[ $rc == 0 && -f "$path" && "$(sha256sum "$path" | cut -d' ' -f1)" == "$sha" ]]; echo $?) "$path"
check download-private $([[ "$(stat -c %a "$(dirname "$path")" 2>/dev/null)" == 700 ]]; echo $?) "0700 directory"

# 3. ETag: with the same release a later check is answered 304 (not counted by GitHub).
sleep 31; ctl update check >/dev/null 2>&1
check etag-304 $([[ "$(n_hits /repos/fspecii/geekbridge/releases/latest 304)" -ge 1 ]]; echo $?) "$(n_hits /repos/fspecii/geekbridge/releases/latest 304) x 304"

# 4. Rate limits.
mode ratelimit; sleep 31; out="$(ctl update check 2>&1)"
check rate-limit-403 $([[ "$out" == *"rate limit"* ]]; echo $?) "$(grep problem <<<"$out")"
before="$(n_hits /repos/fspecii/geekbridge/releases/latest)"; ctl update check >/dev/null 2>&1
check rate-limit-backs-off $([[ "$(n_hits /repos/fspecii/geekbridge/releases/latest)" == "$before" ]]; echo $?) "no request while backing off"
stop_service; mode secondary; start_service; sleep 1; out="$(ctl update check 2>&1)"
check rate-limit-429-retry-after $([[ "$out" == *"rate limit"* && "$out" == *"2 min"* ]]; echo $?) "$(grep problem <<<"$out")"
stop_service; mode ""; start_service

# 5. Offline: the cached offer stays, with a clear problem; it recovers when the network is back.
stop_replica; sleep 31; out="$(ctl update check 2>&1)"
check offline-error-keeps-offer $([[ "$out" == *"Couldn't reach"* && "$out" == *"v0.1.1 - newer"* ]]; echo $?) "$(grep -E 'problem|latest' <<<"$out" | tr '\n' '|')"
start_replica; sleep 31; out="$(ctl update check 2>&1)"
check recovers-when-online $([[ "$out" == *"v0.1.1 - newer"* && "$out" != *"problem:"* ]]; echo $?) "$(grep -E 'problem|latest' <<<"$out" | tr '\n' '|')"

# 6. Private / missing repository stays its own state.
mode notfound; sleep 31; out="$(ctl update check 2>&1)"
check private-is-no-public-release $([[ "$out" == *"No public release found"* ]]; echo $?) "$(grep latest <<<"$out")"
exit "$FAIL"
