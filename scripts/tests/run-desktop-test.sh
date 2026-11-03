#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# scripts/run-desktop.sh upgrade safety, with stub binaries (no real service, no phone, no fixed ports, no network):
#   1. --fresh refuses when the state dir holds pairings (the real-device state is the owner's paired phone), changes
#      nothing, and says how to forget on purpose; with --forget-pairings the old state is MOVED aside, not deleted;
#      --fresh on an empty or missing state dir still works as before.
#   2. A running service that is older than the binary on disk is not silently reused: exit 1 with the --restart hint;
#      --restart stops it, keeps the state dir untouched and starts the new binary.
#   3. A running service that is as new as the binary is reused.
# Run: scripts/tests/run-desktop-test.sh   (about 10 s)
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/rundesk-XXXXXX")"; : >"$T/.gb-owned"
trap 'for f in "$T"/out/pids.*; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done; pkill -f "$T/bin/geekbridge-service" 2>/dev/null || true; rm -rf -- "$T"' EXIT
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }

mkdir -p "$T/bin" "$T/out"
# stub service: marks itself up, then idles. stub ctl: "status" succeeds while the marker exists. stub UI: idles.
cat >"$T/bin/geekbridge-service" <<EOF
#!/bin/sh
trap 'rm -f "$T/up"; kill \$p 2>/dev/null; exit 0' TERM
touch "$T/up"; echo started >>"$T/starts"; sleep 300 & p=\$!; wait
EOF
cat >"$T/bin/geekbridge-ctl" <<EOF
#!/bin/sh
for a in "\$@"; do [ "\$a" = status ] && { [ -f "$T/up" ] && exit 0 || exit 1; }; done
[ "\$1" = pair-qr ] && exit 1
exit 0
EOF
cat >"$T/bin/geekbridge" <<'EOF'
#!/bin/sh
exec sleep 300
EOF
chmod +x "$T/bin/"*
export GEEKBRIDGE_BIN_DIR="$T/bin" GEEKBRIDGE_REALDEVICE_OUT="$T/out" GEEKBRIDGE_REALDEVICE_STATE="$T/state"
export GEEKBRIDGE_CONTROL_PORT=39881 GEEKBRIDGE_API_PORT=39880
unset WAYLAND_DISPLAY DISPLAY
RD="$src/scripts/run-desktop.sh"
paired_state() {
  rm -rf "$T/state"; mkdir -p "$T/state"; chmod 700 "$T/state"
  echo '{"version":1,"peers":[{"device_id":"5bc2f5cfb5cfde194d768ce8c1ca3268","name":"SM-F946B"}]}' >"$T/state/trusted_peers.json"
  echo identity >"$T/state/identity.pem"
}

# ---- 1. --fresh
paired_state
rc=0; "$RD" --fresh --no-ui >"$T/o1" 2>&1 || rc=$?
[[ $rc != 0 ]] && ok "--fresh with pairings is refused (exit $rc)" || bad "--fresh with pairings was not refused"
[[ -f "$T/state/trusted_peers.json" && -f "$T/state/identity.pem" ]] && ok "refused --fresh left the state alone" || bad "refused --fresh still touched the state"
grep -q "forget-pairings" "$T/o1" && grep -q "1 paired" "$T/o1" && ok "the refusal counts the pairings and names --forget-pairings" || bad "refusal text: $(cat "$T/o1")"
[[ ! -f "$T/starts" ]] && ok "nothing was started" || bad "a service was started despite the refusal"

paired_state
rc=0; "$RD" --fresh --forget-pairings --no-ui >"$T/o2" 2>&1 || rc=$?
[[ $rc == 0 ]] && ok "--fresh --forget-pairings works" || bad "--fresh --forget-pairings failed: $(cat "$T/o2")"
moved="$(ls -d "$T"/state.before-fresh-* 2>/dev/null | head -1)"
[[ -n "$moved" && -f "$moved/trusted_peers.json" && -f "$moved/identity.pem" ]] && ok "the old state was moved aside, not deleted ($(basename "$moved"))" || bad "old state not kept"
[[ ! -f "$T/state/trusted_peers.json" ]] && ok "the new session state starts empty" || bad "pairings are still in the session state"
"$RD" --stop >/dev/null 2>&1; rm -f "$T/up" "$T/starts"; rm -rf "$T"/state.before-fresh-*

rm -rf "$T/state"
rc=0; "$RD" --fresh --no-ui >"$T/o3" 2>&1 || rc=$?
[[ $rc == 0 && -f "$T/up" ]] && ok "--fresh on a missing state dir works as before" || bad "--fresh with nothing to forget failed: $(cat "$T/o3")"
"$RD" --stop >/dev/null 2>&1; rm -f "$T/up" "$T/starts"

# ---- 2. a stale running service
paired_state
"$RD" --no-ui >"$T/o4" 2>&1; [[ -f "$T/up" ]] && ok "service started" || bad "service did not start: $(cat "$T/o4")"
sleep 3; touch "$T/bin/geekbridge-service"        # a rebuild: the binary on disk is now newer than the process
rc=0; "$RD" --no-ui >"$T/o5" 2>&1 || rc=$?
[[ $rc != 0 ]] && grep -q "older than" "$T/o5" && grep -q -- "--restart" "$T/o5" && ok "a running service older than the binary is not reused (hint: --restart)" || bad "stale service reused (rc $rc): $(cat "$T/o5")"
before="$(sha256sum "$T/state/trusted_peers.json")"
rc=0; "$RD" --restart --no-ui >"$T/o6" 2>&1 || rc=$?
[[ $rc == 0 ]] && [[ "$(wc -l <"$T/starts")" == 2 ]] && ok "--restart started the new binary (2 starts in total)" || bad "--restart (rc $rc, starts $(wc -l <"$T/starts")): $(cat "$T/o6")"
[[ "$before" == "$(sha256sum "$T/state/trusted_peers.json")" ]] && ok "--restart keeps the state dir" || bad "--restart changed the state"

# ---- 3. an up-to-date service is reused
rc=0; "$RD" --no-ui >"$T/o7" 2>&1 || rc=$?
[[ $rc == 0 && "$(wc -l <"$T/starts")" == 2 ]] && grep -q "reusing" "$T/o7" && ok "an up-to-date service is reused" || bad "up-to-date service not reused (rc $rc)"
"$RD" --stop >/dev/null 2>&1

exit "$fail"
