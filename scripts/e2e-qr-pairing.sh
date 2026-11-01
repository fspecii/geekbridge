#!/usr/bin/env bash
# QR / link pairing end to end: Android emulator app <-> desktop geekbridge-service (see docs/pairing.md).
#
#   scripts/e2e-qr-pairing.sh [--no-restart]
#
# Needs the environment `scripts/e2e-android.sh --only env` prepares (emulator gb36, release binaries in
# target/e2e, debug APK installed). Scenarios, each recorded in dist/android/logs/e2e-qr-results.tsv:
#   expired   a QR code older than its 120 s is refused with plain words; nothing is paired
#   pair      a fresh code opened as a link: confirm screen, "Pair with X?" (no code), paired, next-steps card
#   reuse     the same code from a fresh phone identity is refused ("already used"); nothing is paired
# The emulator cannot scan a screen, so the link is delivered with `am start -d geekbridge://pair?...`, which is
# exactly what the phone's camera app does with the QR code. Screenshots: dist/screenshots/android/qr-*.png
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1
export ANDROID_ADB_SERVER_PORT="${E2E_ADB_PORT:-16038}"
SERIAL="emulator-${E2E_EMU_PORT:-5594}"; export ADB="adb -s $SERIAL"
RUN="${E2E_RUN:-$ROOT/target/e2e-run}" E2E_TARGET="${E2E_TARGET:-$ROOT/target/e2e}"
CTRL_PORT="${E2E_CTRL_PORT:-17481}" API_PORT="${E2E_API_PORT:-17480}" DISC_PORT="${E2E_DISC_PORT:-17416}" LISTEN_PORT="${E2E_LISTEN_PORT:-1730}"
CTL=("$E2E_TARGET/release/geekbridge-ctl" --state-dir "$RUN/state" --control-port "$CTRL_PORT")
SVC="$E2E_TARGET/release/geekbridge-service"
UI="$ROOT/scripts/e2e/adbui.py"; PKG=dev.geekbridge.android
LOGS="$ROOT/dist/android/logs" SHOTS="$ROOT/dist/screenshots/android"; mkdir -p "$LOGS" "$SHOTS"
RESULTS="$LOGS/e2e-qr-results.tsv"; : >"$RESULTS"
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" | tee -a "$RESULTS" >&2; }
shot() { $ADB exec-out screencap -p >"$SHOTS/qr-$1.png" 2>/dev/null; }
ctl() { timeout 20 "${CTL[@]}" "$@"; }
ui() { timeout 90 "$UI" "$@"; }
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
paired_n() { ctl status | jget 'len(d["paired"])'; }

if [[ "${1:-}" != --no-restart ]]; then
  pkill -f "^$SVC --state-dir $RUN/state" 2>/dev/null; sleep 1
  # The emulator reaches the host's loopback listener as 10.0.2.2; the QR code must say so.
  (env -u WAYLAND_DISPLAY GEEKBRIDGE_NO_UPDATE_CHECK=1 GEEKBRIDGE_QR_ADDRESSES="10.0.2.2:$LISTEN_PORT" \
     setsid nohup "$SVC" --state-dir "$RUN/state" --api-port "$API_PORT" --control-port "$CTRL_PORT" \
     --discovery-port "$DISC_PORT" --listen-inbound "127.0.0.1:$LISTEN_PORT" --dev-loopback --no-calling-probe \
     --name E2E-Desktop >>"$LOGS/e2e-service.log" 2>&1 &)
  for _ in $(seq 15); do ctl status >/dev/null 2>&1 && break; sleep 1; done
fi
ctl status >/dev/null 2>&1 || { rec setup FAIL "service not running"; exit 1; }

fresh_phone() { # forget every pairing on the desktop and wipe the phone's identity
  for id in $(ctl status | jget '" ".join(p["device_id"] for p in d["paired"])'); do ctl unpair "$id" >/dev/null; done
  $ADB shell pm clear $PKG >/dev/null; $ADB shell input keyevent KEYCODE_WAKEUP
}
scan() { # payload
  $ADB shell am start -W -a android.intent.action.VIEW -d "'$1'" $PKG >/dev/null
}
payload() { ctl pair-qr 2>/dev/null; }

# ------------------------------------------------------------------ expired
fresh_phone
P_OLD="$(payload)"; [[ "$P_OLD" == geekbridge://pair?* ]] || { rec setup FAIL "no payload: $P_OLD"; exit 1; }
echo "waiting 125 s for the code to expire..." >&2; sleep 125
scan "$P_OLD"; ui wait "Pair with" 15 && shot confirm-expired
ui tap "Continue" >/dev/null
if ui wait "did not accept the QR code" 40; then shot expired; rec expired PASS "refused in plain words; paired=$(paired_n)"; else shot expired-fail; rec expired FAIL "no plain-language refusal"; fi
[[ "$(paired_n)" == 0 ]] || rec expired FAIL "something got paired"

# ------------------------------------------------------------------ pair
fresh_phone
P1="$(payload)"
scan "$P1"
if ui wait "Pair with E2E-Desktop" 15; then shot confirm; else shot confirm-fail; fi
ui tap "Continue" >/dev/null
if ui wait "Computer identity" 40; then
  shot approve
  ui dump | grep -q "Compare\|same code" && rec pair FAIL "a code comparison was shown"
  ui tap "Pair" --exact >/dev/null
  if ui wait "What to do next" 40; then shot done; rec pair PASS "paired=$(paired_n); events: $(ctl events | jget '",".join(e["kind"] for e in d["events"] if e["kind"].startswith("qr_") or e["kind"]=="paired")')"; else shot done-fail; rec pair FAIL "never reached the next-steps card; paired=$(paired_n)"; fi
else
  shot approve-fail; rec pair FAIL "no approval dialog"
fi

# ------------------------------------------------------------------ reuse
fresh_phone
scan "$P1"; ui wait "Pair with" 15 >/dev/null; ui tap "Continue" >/dev/null
if ui wait "did not accept the QR code" 40; then shot reused; rec reuse PASS "second use refused; paired=$(paired_n)"; else shot reused-fail; rec reuse FAIL "no refusal"; fi
[[ "$(paired_n)" == 0 ]] || rec reuse FAIL "something got paired"
ctl events | jget '"; ".join(e["kind"] + ":" + e.get("reason","") for e in d["events"] if e["kind"]=="qr_refused")' >&2
