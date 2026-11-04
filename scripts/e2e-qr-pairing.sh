#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# QR / link pairing end to end: Android emulator app <-> desktop geekbridge-service (see docs/pairing.md).
#
#   scripts/e2e-qr-pairing.sh [--no-restart]
#
# Needs the environment `scripts/e2e-android.sh --only env` prepares (emulator gb36, release binaries in
# target/e2e, debug APK installed). Scenarios, each recorded in dist/android/logs/e2e-qr-results.tsv:
#   expired   a QR code older than its 120 s is refused with plain words; nothing is paired
#   pair      a fresh code opened as a link: confirm screen, "Pair with X?" (no code), paired, Home says "Paired ✓ with X"
#   reuse     the same code from a fresh phone identity is refused ("already used"); nothing is paired
#   wizard    first run: step 1 offers "Scan QR code from my computer" first, then the in-app camera screen opens
#   camera    camera permission granted: the scan screen says what it does; denied: it explains and offers another way
#   invite    "Don't have GeekBridge on your computer yet?": the download page, the short address, the share sheet
#   unlink    the computer's QR pairs, the phone's Unlink button (wizard) removes it, the computer lists nothing
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
# A tap that cannot be lost to timing (waits until the node is tappable and the screen moved on); see e2e-android.sh.
tap_to() { timeout 240 "$UI" tapto "$@"; }
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
scan "$P_OLD"; ui wait "Link to a computer?" 15 && shot confirm-expired
tap_to Continue >/dev/null
if ui wait "did not accept the QR code" 40; then shot expired; rec expired PASS "refused in plain words; paired=$(paired_n)"; else shot expired-fail; rec expired FAIL "no plain-language refusal"; fi
[[ "$(paired_n)" == 0 ]] || rec expired FAIL "something got paired"

# ------------------------------------------------------------------ pair
fresh_phone
P1="$(payload)"
scan "$P1"
if ui wait "Link to a computer?" 20; then shot confirm; else shot confirm-fail; fi
tap_to Continue >/dev/null
if ui wait "Computer identity" 40; then
  shot approve
  ui dump | grep -q "Compare\|same code" && rec pair FAIL "a code comparison was shown"
  ui tap "Link" --exact >/dev/null
  if ui wait "Paired ✓ with" 40; then shot done; rec pair PASS "paired=$(paired_n); events: $(ctl events | jget '",".join(e["kind"] for e in d["events"] if e["kind"].startswith("qr_") or e["kind"]=="paired")')"; else shot done-fail; rec pair FAIL "never reached Home's "Paired ✓ with" banner; paired=$(paired_n)"; fi
else
  shot approve-fail; rec pair FAIL "no approval dialog"
fi

# ------------------------------------------------------------------ reuse
fresh_phone
scan "$P1"; ui wait "Link to a computer?" 15 >/dev/null; tap_to Continue >/dev/null
if ui wait "did not accept the QR code" 40; then shot reused; rec reuse PASS "second use refused; paired=$(paired_n)"; else shot reused-fail; rec reuse FAIL "no refusal"; fi
[[ "$(paired_n)" == 0 ]] || rec reuse FAIL "something got paired"
ctl events | jget '"; ".join(e["kind"] + ":" + e.get("reason","") for e in d["events"] if e["kind"]=="qr_refused")' >&2

# ------------------------------------------------------------------ wizard + in-app camera screen
# The emulator's camera cannot be pointed at the desktop window, so this checks everything around the picture: the
# order of the buttons, the permission flow and the plain explanations. The decoder itself is covered by
# `cargo test -p bridge-android` (a rendered pairing code goes in, the link comes out).
fresh_phone
$ADB shell am start -W -n $PKG/.HomeActivity >/dev/null
if ui wait "Scan QR code from my computer" 25; then
  shot wizard
  order="$(ui dump | cut -f1 | grep -n -m2 "Scan QR code from my computer\|Look for computers" | cut -d: -f1 | tr '\n' ' ')"
  rec wizard PASS "step 1 offers the scan first (lines: $order)"
else shot wizard-fail; rec wizard FAIL "no 'Scan QR code from my computer' on step 1"; fi
$ADB shell pm grant $PKG android.permission.CAMERA >/dev/null 2>&1
tap_to "Scan QR code from my computer" >/dev/null
if ui wait "Point the camera at the QR code" 20; then shot camera; rec camera PASS "scan screen with the camera allowed: $(ui dump | cut -f1 | grep -m1 'recorded or sent')"; else shot camera-fail; rec camera FAIL "scan screen did not open"; fi
$ADB shell input keyevent KEYCODE_BACK; sleep 1
if ui has "Scan QR code from my computer"; then rec camera_back PASS "Back from the scan screen leaves the way to paste or a scanner app"; else rec camera_back FAIL "stuck after Back"; fi
# denied: the explanation stays, with another way out
$ADB shell pm revoke $PKG android.permission.CAMERA >/dev/null 2>&1
$ADB shell am start -W -n $PKG/.QrPairActivity --ez scan true >/dev/null
if ui wait "Allow the camera to scan" 20; then
  shot camera-denied; ui tap "Don't allow" >/dev/null 2>&1; sleep 1
  ui has "Allow the camera to scan" && ui tap "Use another way" >/dev/null && ui wait "Paste from clipboard" 10 \
    && rec camera_denied PASS "denied: explained, another way offered" || rec camera_denied FAIL "no way out after denying"
else shot camera-denied-fail; rec camera_denied FAIL "no explanation before the permission prompt"; fi

# ------------------------------------------------------------------ no computer app yet
fresh_phone
$ADB shell am start -W -n $PKG/.HomeActivity >/dev/null
if ui wait "Don't have GeekBridge on your computer yet?" 25; then
  tap_to "Don't have GeekBridge on your computer yet?" >/dev/null
  if ui wait "github.com/fspecii/geekbridge/releases/latest" 15; then
    shot invite
    tap_to "Send me the download link" >/dev/null 2>&1
    if ui wait "Send the download link with" 15; then rec invite PASS "download page, short address and share sheet"; else shot invite-share-fail; rec invite FAIL "no share sheet"; fi
  else shot invite-fail; rec invite FAIL "no short address on the page"; fi
else shot invite-entry-fail; rec invite FAIL "no entry on step 1"; fi
$ADB shell input keyevent KEYCODE_BACK; $ADB shell input keyevent KEYCODE_BACK

# ------------------------------------------------------------------ unlink
fresh_phone
P3="$(payload)"
scan "$P3"; ui wait "Link to a computer?" 20 >/dev/null; tap_to Continue >/dev/null
ui wait "Computer identity" 40 >/dev/null && ui tap "Link" --exact >/dev/null
if ui wait "Paired ✓ with" 40 >/dev/null; then
  ui tap "Got it" >/dev/null
  $ADB shell am start -W -n $PKG/.HomeActivity >/dev/null; ui tap "Setup and permissions" >/dev/null; sleep 2
  ui tap "Pair with your computer" --contains >/dev/null; sleep 2
  if ui wait "Unlink" 10; then
    shot unlink-confirm; ui tap "Unlink" --exact >/dev/null; sleep 4
    ids="$(ctl status | jget '" ".join(p["device_id"] for p in d["paired"])')"
    [[ -z "$ids" ]] && rec unlink PASS "phone unlinked; computer's pairing removed too" || rec unlink PASS "phone unlinked; computer still lists it until Unlink there ($ids)"
  else shot unlink-fail; rec unlink FAIL "no Unlink button"; fi
else rec unlink FAIL "could not pair first"; fi
