#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Desktop self-update end to end with REAL release artifacts against the GitHub replica
# (scripts/e2e/github_replica.py). Complements update-install.sh, which uses tiny synthetic assets.
#
#   scripts/e2e/updater-real.sh assemble     build $UPD_REAL/run/releases/{v0.1.0,v0.1.1,v0.1.1-test} from the packages
#   scripts/e2e/updater-real.sh replica      replica-vs-schema checks only (no service)
#   scripts/e2e/updater-real.sh desktop      the installed v0.1.0 service: check -> offer -> download -> verify -> manual install
#   scripts/e2e/updater-real.sh all          assemble, replica, desktop
#
# Input, produced by the commands in docs/updater-e2e.md (UPD_REAL defaults to $TMPDIR/upd-real):
#   $UPD_REAL/out/010/{bin/*, geekbridge-0.1.0-linux-x86_64-installer.tar.gz, geekbridge_0.1.0_amd64.deb, geekbridge-debug.apk}
#   $UPD_REAL/out/011/{...}  built from a scratch copy bumped with scripts/release.sh --skip-tests 0.1.1
# Everything runs below $UPD_REAL/run: own ports (replica 18893, service 17780-17783), own state and download dirs.
# Results: dist/android/logs/updater-real-results.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
WHAT="${1:-all}"
S="${UPD_REAL:-${TMPDIR:-/tmp}/upd-real}"
W="$S/run"; REL="$W/releases"; MODE="$W/mode"; RLOG="$W/replica.log"
RP=18893; REPO=acme/geekbridge-releases
DW="$W/desktop"; CTRL=17781
RESULTS="$ROOT/dist/android/logs/updater-real-results.tsv"; mkdir -p "$W" "$(dirname "$RESULTS")"
BINS=(geekbridge geekbridge-service geekbridge-ctl geekbridge-calls geekbridge-messages geekbridge-clipboard geekbridge-agenda geekbridge-contacts geekbridge-share)

log() { printf '[real %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" | tee -a "$RESULTS" >&2; }
check() { if [[ "$2" == 0 ]]; then rec "$1" PASS "${3:-}"; else rec "$1" FAIL "${3:-}"; fi; }
mode() { printf '%s\n' "$@" >"$MODE"; }
rlog() { jq -c "$@" "$RLOG" 2>/dev/null; }   # query the replica's JSON-lines request log
latest_hits() { jq -s '[.[] | select(.path|endswith("/releases/latest"))] | length' "$RLOG" 2>/dev/null || echo 0; }

# ================================================================ assemble (release.yml logic)
assemble() {
  rm -rf "$REL" "$W/stage"; mkdir -p "$REL"
  local v ver dir out
  for pair in "010 0.1.0" "011 0.1.1"; do
    set -- $pair; v=$1; ver=$2; out="$S/out/$v"; dir="$REL/v$ver"
    [[ -x "$out/bin/geekbridge-ctl" ]] || { rec assemble FAIL "missing $out/bin"; return 1; }
    mkdir -p "$dir" "$W/stage/geekbridge-linux-x86_64"
    # "Package" step of .github/workflows/release.yml
    for b in "${BINS[@]}"; do cp "$out/bin/$b" "$W/stage/geekbridge-linux-x86_64/"; done
    cp "$S/src-$v/README.md" "$W/stage/geekbridge-linux-x86_64/" || true
    tar -czf "$dir/geekbridge-linux-x86_64.tar.gz" -C "$W/stage" geekbridge-linux-x86_64
    rm -rf "$W/stage"
    # "Installer" step output + the Android artifact the release job downloads
    cp "$out/geekbridge-$ver-linux-x86_64-installer.tar.gz" "$out/geekbridge_${ver}_amd64.deb" "$dir/"
    [[ -f "$out/geekbridge-debug.apk" ]] && cp "$out/geekbridge-debug.apk" "$dir/geekbridge-android.apk"
    # "SHA256SUMS" step of the release job, verbatim
    ( cd "$dir" && sha256sum -- geekbridge-* $(compgen -G "geekbridge_*" || true) > SHA256SUMS )
    notes "$ver" "$S/src-$v/CHANGELOG.md" >"$dir/NOTES.md"
    printf '{"prerelease": false, "draft": false, "published_at": "%s"}\n' "$([[ $v == 010 ]] && echo 2026-11-02T08:00:00Z || echo 2026-11-02T12:00:00Z)" >"$dir/release.json"
  done
  # a prerelease whose files are those of v0.1.1 (what release.yml would publish for the tag v0.1.1-test)
  mkdir -p "$REL/v0.1.1-test"
  for f in "$REL/v0.1.1/"*; do [[ "$(basename "$f")" == release.json ]] || ln -sf "$f" "$REL/v0.1.1-test/"; done
  printf '{"prerelease": true, "draft": false, "published_at": "2026-11-02T13:00:00Z"}\n' >"$REL/v0.1.1-test/release.json"
  assemble_extras
  rec assemble PASS "$(cd "$REL/v0.1.1" && ls | tr '\n' ' ')"
}
# Two more releases the Android scenarios need. Their publish dates keep v0.1.1 the newest, so /latest still
# answers v0.1.1 unless a mode (only=...) says otherwise.
#   v0.0.9  older than the installed app (downgrade is not offered)
#   v0.1.2  the v0.1.1 APK re-signed with a throwaway key: newer, correctly hashed, but not installable over v0.1.0
assemble_extras() {
  mkdir -p "$REL/v0.0.9" "$REL/v0.1.2"
  cp "$REL/v0.1.0/geekbridge-linux-x86_64.tar.gz" "$REL/v0.0.9/"; ( cd "$REL/v0.0.9" && sha256sum geekbridge-linux-x86_64.tar.gz >SHA256SUMS )
  printf '{"prerelease": false, "draft": false, "published_at": "2026-11-01T08:00:00Z"}\n' >"$REL/v0.0.9/release.json"
  [[ -f "$REL/v0.1.1/geekbridge-android.apk" ]] || return 0
  ( source "$ROOT/scripts/android-env.sh" >/dev/null 2>&1
    ks="$W/wrongkey.jks"; rm -f "$ks"
    keytool -genkeypair -keystore "$ks" -storepass wrongkey1 -keypass wrongkey1 -alias wrong -keyalg RSA -keysize 2048 -validity 3650 \
      -dname "CN=Not The Release Key" >/dev/null 2>&1 &&
    apksigner sign --ks "$ks" --ks-pass pass:wrongkey1 --out "$REL/v0.1.2/geekbridge-android.apk" "$REL/v0.1.1/geekbridge-android.apk" ) \
    && ( cd "$REL/v0.1.2" && sha256sum geekbridge-android.apk >SHA256SUMS ) \
    && printf '## What'"'"'s Changed\n\nSame code as v0.1.1, signed with a different key (test fixture).\n' >"$REL/v0.1.2/NOTES.md" \
    && printf '{"prerelease": false, "draft": false, "published_at": "2026-11-02T10:00:00Z"}\n' >"$REL/v0.1.2/release.json" \
    || { rec assemble-wrongkey FAIL "could not sign the v0.1.2 fixture"; rm -rf "$REL/v0.1.2"; }
}
# The release body: `gh release create --generate-notes` output shape + the signer note, with the CHANGELOG section as the summary.
notes() { # version changelog
  printf '## What'"'"'s Changed\n\n'; awk -v v="$1" '$0 ~ "^## \\["v"\\]" {s=1; next} /^## \[/ {s=0} s' "$2" | head -40
  printf '\n**Full Changelog**: https://github.com/%s/compare/v0.1.0...v%s\n\nAndroid APK is DEBUG-signed (prerelease only; not updatable from or to a release build). Signer SHA-256: `0000`\n' "$REPO" "$1"
}

start_replica() {
  pkill -f "^python3 -I scripts/e2e/github_replica.py --port $RP" 2>/dev/null; sleep 0.3
  mode ""; : >"$RLOG"
  (setsid nohup python3 -I scripts/e2e/github_replica.py --port "$RP" --repo "$REPO" --releases "$REL" --mode-file "$MODE" --log "$RLOG" >"$W/replica.out" 2>&1 &)
  for _ in $(seq 20); do curl -s -o /dev/null "http://127.0.0.1:$RP/" && return 0; sleep 0.3; done; return 1
}
stop_replica() { pkill -f "^python3 -I scripts/e2e/github_replica.py --port $RP" 2>/dev/null; true; }
BASE="http://127.0.0.1:$RP"

# ================================================================ replica vs GitHub's schema and documented behaviour
replica() {
  start_replica || { rec replica-start FAIL "see $W/replica.out"; return 1; }
  local api="$BASE/repos/$REPO/releases/latest" h="$W/hdr" b="$W/body.json" v
  mode "only=v0.1.0"
  curl -s -D "$h" -H 'Accept: application/vnd.github+json' -o "$b" "$api"
  check replica-schema-v0.1.0 $(python3 -I scripts/e2e/validate_release_payload.py "$b" >"$W/schema.out" 2>&1; echo $?) "$(head -3 "$W/schema.out" | tr '\n' ' ')"
  mode ""
  curl -s -D "$h" -H 'Accept: application/vnd.github+json' -o "$b" "$api"
  check replica-schema-v0.1.1 $(python3 -I scripts/e2e/validate_release_payload.py "$b" >"$W/schema.out" 2>&1; echo $?) "$(head -3 "$W/schema.out" | tr '\n' ' ')"
  check replica-latest-is-v0.1.1 $([[ "$(jq -r .tag_name "$b")" == v0.1.1 ]]; echo $?) "prerelease v0.1.1-test (newer) is skipped, as GitHub's /latest does"
  check replica-asset-fields $([[ "$(jq -r '[.assets[] | select(.name=="geekbridge-linux-x86_64.tar.gz")][0] | [.size, .digest, .content_type, .state, (.browser_download_url|test("/releases/download/v0.1.1/"))] | @tsv' "$b")" == "$(stat -c %s "$REL/v0.1.1/geekbridge-linux-x86_64.tar.gz")	sha256:$(sha256sum "$REL/v0.1.1/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)	application/gzip	uploaded	true" ]]; echo $?) "size, digest, content_type, state, browser_download_url of the real tarball"
  check replica-sums-match-digests $([[ "$(jq -r '.assets[] | select(.name!="SHA256SUMS") | (.digest|sub("sha256:";"")) + "  " + .name' "$b" | sort)" == "$(sort "$REL/v0.1.1/SHA256SUMS")" ]]; echo $?) "API digest == SHA256SUMS line for every asset"
  local want='x-ratelimit-limit x-ratelimit-remaining x-ratelimit-reset x-ratelimit-used x-ratelimit-resource etag x-github-api-version-selected cache-control'
  local miss=""; for k in $want; do grep -qi "^$k:" "$h" || miss="$miss $k"; done
  check replica-headers $([[ -z "$miss" ]]; echo $?) "missing:${miss:- none}"
  local etag used; etag="$(grep -i '^etag:' "$h" | cut -d' ' -f2- | tr -d '\r')"; used="$(grep -i '^x-ratelimit-used:' "$h" | cut -d' ' -f2 | tr -d '\r')"
  local code used2; code="$(curl -s -o /dev/null -D "$h" -w '%{http_code}' -H "If-None-Match: $etag" "$api")"; used2="$(grep -i '^x-ratelimit-used:' "$h" | cut -d' ' -f2 | tr -d '\r')"
  check replica-etag-304 $([[ "$code" == 304 && "$used2" == "$used" ]]; echo $?) "If-None-Match $etag -> $code, X-RateLimit-Used stays $used"
  check replica-404-shape $([[ "$(curl -s "$BASE/repos/nobody/none/releases/latest" | jq -r '.message + "|" + .status')" == "Not Found|404" ]]; echo $?) '{"message":"Not Found","documentation_url":...,"status":"404"}'
  # download redirect chain
  local url loc; url="$(jq -r '.assets[] | select(.name=="geekbridge-linux-x86_64.tar.gz") | .browser_download_url' "$b")"
  code="$(curl -s -o /dev/null -D "$h" -w '%{http_code}' "$url")"; loc="$(grep -i '^location:' "$h" | cut -d' ' -f2- | tr -d '\r')"
  check replica-download-302 $([[ "$code" == 302 && "$loc" == */release-assets/github-production-release-asset/*"?"*sig=* ]]; echo $?) "browser_download_url -> 302 ${loc:0:90}..."
  curl -s -D "$h" -o "$W/dl.bin" "$loc"
  check replica-download-bytes $([[ "$(sha256sum "$W/dl.bin" | cut -d' ' -f1)" == "$(sha256sum "$REL/v0.1.1/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)" ]] && grep -qi '^content-disposition: attachment; filename=geekbridge-linux-x86_64.tar.gz' "$h"; echo $?) "200 with Content-Disposition, identical bytes"; rm -f "$W/dl.bin"
  code="$(curl -s -o "$W/part.bin" -w '%{http_code}' -H 'Range: bytes=10-19' "$loc")"
  check replica-download-range $([[ "$code" == 206 && "$(stat -c %s "$W/part.bin")" == 10 ]]; echo $?) "Range bytes=10-19 -> $code"; rm -f "$W/part.bin"
  mode expired; loc="$(curl -s -o /dev/null -D "$h" "$url"; grep -i '^location:' "$h" | cut -d' ' -f2- | tr -d '\r')"
  check replica-expired-signature-403 $([[ "$(curl -s -o /dev/null -w '%{http_code}' "$loc")" == 403 ]]; echo $?) "CDN refuses an expired pre-signed URL"
  mode ratelimit; curl -s -D "$h" -o "$b" "$api"
  check replica-ratelimit-shape $([[ "$(head -1 "$h")" == *403* ]] && grep -qi '^x-ratelimit-remaining: 0' "$h" && jq -e '.message|startswith("API rate limit exceeded")' "$b" >/dev/null; echo $?) "403 + X-RateLimit-Remaining: 0 + documented message"
  mode ""; stop_replica
}

# ================================================================ the installed v0.1.0 service
SVC="" CTLBIN=""
STATE=""
ctl() { timeout 90 "$CTLBIN" --state-dir "$STATE" --control-port "$CTRL" "$@"; }
start_service() { # state-dir [env assignments...]; uses $SVC
  local st="$1"; shift; STATE="$st"
  mkdir -p "$st" "$DW/xdg-state"; chmod 700 "$st"
  (env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u DISPLAY "$@" \
     GEEKBRIDGE_UPDATE_REPO="$REPO" GEEKBRIDGE_UPDATE_BASE_URL="$BASE" XDG_STATE_HOME="$DW/xdg-state" \
     setsid nohup "$SVC" --state-dir "$st" --api-port 17780 --control-port "$CTRL" --discovery-port 17716 \
     --listen-inbound 127.0.0.1:1941 --dev-loopback --no-calling-probe --no-announce --no-tray --name Real-Update >>"$DW/service.log" 2>&1 &)
  for _ in $(seq 40); do ctl update status >/dev/null 2>&1 && return 0; sleep 0.5; done; return 1
}
stop_service() { pkill -f "^$SVC --state-dir" 2>/dev/null; sleep 1; true; }
dl_files() { find "$DW/xdg-state" -type f -path '*geekbridge-update-*' 2>/dev/null; }

desktop() {
  [[ -d "$REL/v0.1.1" ]] || assemble || return 1
  stop_replica; pkill -f "^$DW/prefix-010/bin/geekbridge-service" 2>/dev/null; sleep 0.5
  rm -rf "$DW"; mkdir -p "$DW"; local out
  # ---- install v0.1.0 exactly as a user would: extract the installer tarball, ./install.sh (custom prefix: no unit, no autostart elsewhere)
  mkdir -p "$DW/x010"; tar xzf "$S/out/010/geekbridge-0.1.0-linux-x86_64-installer.tar.gz" -C "$DW/x010"
  ( cd "$DW/x010/geekbridge-0.1.0" && ./install.sh --prefix "$DW/prefix-010" ) >"$DW/install-010.log" 2>&1
  SVC="$DW/prefix-010/bin/geekbridge-service"; CTLBIN="$DW/prefix-010/bin/geekbridge-ctl"
  out="$("$CTLBIN" --version 2>&1)"; check desktop-installed-v0.1.0 $([[ "$out" == *" 0.1.0" ]]; echo $?) "$out ($DW/prefix-010)"
  start_replica || { rec desktop FAIL "replica did not start"; return 1; }

  # ---- state of an existing install: the state-versioning agent's v1 fixtures (identity, trusted peer, consent, ...)
  local fx="$ROOT/crates/bridge-service/tests/fixtures/state-v1"
  mkdir -p "$DW/state"; chmod 700 "$DW/state"
  if [[ -d "$fx" ]]; then cp "$fx"/*.json "$fx"/*.pem "$fx"/device-name "$fx"/ipc-token "$DW/state/" 2>/dev/null; rm -f "$DW/state/update-state.json"; chmod 600 "$DW/state/"* ; fi
  ( cd "$DW/state" && sha256sum identity.pem trusted_peers.json consent.json 2>/dev/null ) >"$DW/state-before.sha"

  # ---- 1. automatic check shortly after start (no manual action); the replica only knows v0.1.0
  mode "only=v0.1.0"
  start_service "$DW/state" || { rec desktop-start FAIL "service did not start; see $DW/service.log"; return 1; }
  out="$(ctl status 2>&1)"; local paired_before; paired_before="$(echo "$out" | grep -ci 'paired\|trusted' || true)"
  echo "$out" >"$DW/ctl-status-before.txt"
  log "waiting for the startup check (~15 s)"; for _ in $(seq 40); do [[ "$(latest_hits)" -ge 1 ]] && break; sleep 1; done
  sleep 2; out="$(ctl update status 2>&1)"
  check desktop-auto-check-ran $([[ "$(latest_hits)" -ge 1 && "$out" == *"up to date"* ]]; echo $?) "startup check hit the replica $(latest_hits)x; $(echo "$out" | grep -E 'latest' | head -1)"
  local req; req="$(rlog -s '[.[] | select(.path|endswith("/releases/latest"))][0]')"
  check desktop-request-headers $([[ "$(echo "$req" | jq -r .ua)" == "geekbridge-updater/0.1.0" && "$(echo "$req" | jq -r .accept)" == "application/vnd.github+json" && "$(echo "$req" | jq -r .api_version)" == 2022-11-28 && "$(echo "$req" | jq -r .auth)" == false ]]; echo $?) "$(echo "$req" | jq -c '{ua,accept,api_version,auth}')"

  # ---- 2. manual re-check: conditional request, 304
  sleep 31; ctl update check >/dev/null 2>&1
  req="$(rlog -s '[.[] | select(.path|endswith("/releases/latest"))][-1]')"
  check desktop-etag-304 $([[ "$(echo "$req" | jq -r .status)" == 304 && "$(echo "$req" | jq -r .if_none_match)" == W/* ]]; echo $?) "$(echo "$req" | jq -c '{status,if_none_match}')"

  # ---- 3. prereleases: /latest never returns one; and a client handed one anyway must refuse it
  mode "only=v0.1.0,v0.1.1-test"; sleep 31; out="$(ctl update check 2>&1)"
  check desktop-prerelease-not-latest $([[ "$out" == *"up to date"* ]]; echo $?) "GitHub's /latest skips v0.1.1-test; the checker stays on v0.1.0"
  mode "tag=v0.1.1-test"; sleep 31; out="$(ctl update check 2>&1)"
  check desktop-prerelease-refused-if-served $([[ "$out" != *"newer"* && "$out" == *"up to date"* ]]; echo $?) "forced prerelease payload: $(echo "$out" | grep -E 'latest' | head -1)"

  # ---- 4. v0.1.1 is published: offered with notes, the right installer and its published digest
  mode ""; sleep 31; out="$(ctl update check 2>&1)"; echo "$out" >"$DW/ctl-check-offer.txt"
  local tarsha; tarsha="$(sha256sum "$REL/v0.1.1/geekbridge-linux-x86_64.tar.gz" | cut -d' ' -f1)"
  check desktop-offer-v0.1.1 $([[ "$out" == *"v0.1.1 - newer"* ]]; echo $?) "$(echo "$out" | grep -E 'latest|installer' | tr '\n' '|')"
  check desktop-offer-selected-asset $([[ "$out" == *"geekbridge-linux-x86_64.tar.gz (SHA-256 $tarsha)"* ]]; echo $?) "the plain release archive, not the -installer tarball or the .deb: $(echo "$out" | grep installer)"
  check desktop-offer-notes $([[ "$(jq -r .release.notes "$DW/state/update-state.json")" == *"What's Changed"*"Tray / menu-bar icon"* ]]; echo $?) "release body kept (cleaned) in update-state.json for the apps' banner: $(jq -r .release.notes "$DW/state/update-state.json" | head -1)"

  # ---- 5. faults while downloading: nothing may be left behind and nothing installed
  local rc
  for m in corrupt truncate expired evil_redirect; do
    mode "$m"; ctl update download >"$DW/dl-$m.out" 2>&1; rc=$?
    check "desktop-download-$m-refused" $([[ $rc != 0 && -z "$(dl_files)" ]]; echo $?) "rc=$rc: $(tail -1 "$DW/dl-$m.out" | cut -c1-110)"
  done
  mode "tamper_sums"; sleep 31; ctl update check >/dev/null 2>&1; ctl update download >"$DW/dl-tamper.out" 2>&1; rc=$?
  check desktop-download-tampered-sums-refused $([[ $rc != 0 && "$(cat "$DW/dl-tamper.out")" == *"does not match"* && -z "$(dl_files)" ]]; echo $?) "rc=$rc: $(tail -1 "$DW/dl-tamper.out" | cut -c1-110)"

  # ---- 6. the real download
  mode ""; sleep 31; ctl update check >/dev/null 2>&1
  local t0 t1; : >"$RLOG"; t0=$(date +%s.%N); out="$(ctl update download 2>&1)"; rc=$?; t1=$(date +%s.%N)
  local path; path="$(echo "$out" | sed -n 's/^verified download: //p')"
  check desktop-download-verified $([[ $rc == 0 && -f "$path" && "$(sha256sum "$path" | cut -d' ' -f1)" == "$tarsha" ]]; echo $?) "$path ($(stat -c %s "$path" 2>/dev/null) bytes in $(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.1f", b-a}') s)"
  check desktop-download-permissions $([[ "$(stat -c %a "$(dirname "$path")")" == 700 && "$(stat -c %a "$path")" == 4?? ]]; echo $?) "dir $(stat -c %a "$(dirname "$path")") file $(stat -c %a "$path")"
  check desktop-download-redirect-chain $([[ "$(rlog -s '[.[] | select(.path|test("releases/download|release-assets")) | .status] | join(",")')" == '"302,200"' ]]; echo $?) "$(rlog -s '[.[] | select(.path|test("releases/download|release-assets")) | .status]' | tr -d ' ')"
  check desktop-no-token-sent $([[ "$(rlog -s '[.[] | select(.auth==true)] | length')" == 0 && "$(rlog -s '[.[] | select(.violation)] | length')" == 0 ]]; echo $?) "no Authorization header on any request"
  out="$(ctl update status 2>&1)"
  check desktop-state-reveal-installer $([[ "$out" == *"verified download: $path"* ]]; echo $?) "status: $(echo "$out" | grep -E 'verified download')"

  # ---- 7. the documented manual install of the new archive into a scratch prefix, then run it
  local x11="$DW/x011"; mkdir -p "$x11"; tar xzf "$path" -C "$x11"
  local top; top="$(ls "$x11")"
  local P11="$DW/prefix-011"; mkdir -p "$P11/bin"
  if [[ -x "$x11/$top/install.sh" ]]; then ( cd "$x11/$top" && ./install.sh --prefix "$P11" ) >"$DW/install-011.log" 2>&1
  else install -m 0755 "$x11/$top"/geekbridge* "$P11/bin/"; fi
  out="$("$P11/bin/geekbridge-ctl" --version 2>&1)"
  check desktop-manual-install-v0.1.1 $([[ "$out" == *" 0.1.1" ]]; echo $?) "$out ($P11), archive layout: $top/ $(ls "$x11/$top" | wc -l) files"

  # ---- 8. state compatibility: new service on the same state dir
  stop_service
  SVC="$P11/bin/geekbridge-service"; CTLBIN="$P11/bin/geekbridge-ctl"
  start_service "$DW/state" GEEKBRIDGE_NO_UPDATE_CHECK=1 || { rec desktop-start-v0.1.1 FAIL "see $DW/service.log"; return 1; }
  out="$(ctl status 2>&1)"; echo "$out" >"$DW/ctl-status-after.txt"
  check desktop-state-pairing-survives $([[ "$(diff <(sed -E 's/[0-9]+ ?(s|ms)? ago//g' "$DW/ctl-status-before.txt" | grep -iv 'uptime\|service_version') <(sed -E 's/[0-9]+ ?(s|ms)? ago//g' "$DW/ctl-status-after.txt" | grep -iv 'uptime\|service_version') >"$DW/status.diff"; echo $?)" == 0 ]]; echo $?) "ctl status of the paired devices identical before/after ($(wc -l <"$DW/status.diff") diff lines)"
  check desktop-state-files-unchanged $(cd "$DW/state" && sha256sum -c "$DW/state-before.sha" >"$DW/state-after.txt" 2>&1; echo $?) "identity.pem trusted_peers.json consent.json: $(tr '\n' ' ' <"$DW/state-after.txt")"
  mode ""; sleep 1; out="$(ctl update check 2>&1)"
  check desktop-after-update-up-to-date $([[ "$out" == *"running:  v0.1.1"* && "$out" == *"up to date"* ]]; echo $?) "$(echo "$out" | head -3 | tr '\n' '|')"
  stop_service

  # ---- 9. failure states on fresh state dirs (the old binary; fresh, so no cooldown applies)
  SVC="$DW/prefix-010/bin/geekbridge-service"; CTLBIN="$DW/prefix-010/bin/geekbridge-ctl"
  local i=0
  for pair in "notfound|No public release" "ratelimit|rate limit" "secondary|rate limit" "error502|HTTP 502"; do
    i=$((i+1)); local m="${pair%%|*}" want="${pair#*|}"
    mode "$m"; rm -rf "$DW/state-f$i"; start_service "$DW/state-f$i" GEEKBRIDGE_NO_UPDATE_CHECK=1 || { rec "desktop-fail-$m" FAIL "no start"; continue; }
    : >"$RLOG"; out="$(ctl update check 2>&1)"; sleep 1; out="$out $(ctl update status 2>&1)"
    local before; before="$(latest_hits)"; sleep 31; ctl update check >/dev/null 2>&1
    local extra=""; [[ $m == ratelimit || $m == secondary ]] && extra=" second manual check made $(( $(latest_hits) - before )) more request(s) (paused)"
    check "desktop-state-$m" $([[ "$out" == *"$want"* ]]; echo $?) "$(echo "$out" | grep -io "$want[^|]*" | head -1 | cut -c1-90);$extra"
    stop_service
  done
  stop_replica
}

case "$WHAT" in
  assemble) : >"$RESULTS"; assemble ;; replica) : >"$RESULTS"; assemble && replica ;; desktop) : >"$RESULTS"; desktop ;;
  all) : >"$RESULTS"; assemble && replica; desktop ;;
  *) echo "usage: $0 [assemble|replica|desktop|all]"; exit 2 ;;
esac
echo; column -t -s $'\t' "$RESULTS"
! grep -q $'\tFAIL\t' "$RESULTS"
