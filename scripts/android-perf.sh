#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Android footprint measurements on an emulator (docs/android-perf.md). Emulator numbers are a regression baseline,
# not phone numbers: x86_64 code, software GPU, a different ART/zygote, no real radio or battery.
#
#   scripts/android-perf.sh [STAGE ...]      stages run in this order when none is given (idle is 30 min):
#     size       build debug + release (R8) APKs from a scratch copy and report APK/dex/lib/resource sizes
#     setup      start the dedicated emulator + desktop service, install the signed APK, pair (via e2e-android.sh)
#     startup    cold start (am start -W after force-stop), median of N
#     reconnect  50 desktop-initiated reconnects; PSS/threads/fds before, during and after (leak check)
#     transfer   100 MB desktop -> phone file; PSS/threads/fds before, during, after (+ forced GC)
#     bigxfer    100 MB + 500 MB (BIG_MB) both ways, PSS/threads/fds/desktop RSS every few s, 10 s after, plus a
#                cancel mid-transfer each way (memory returned, partial file gone, no wake lock / extra service)
#     idle       app closed (task swiped away), link service paired + connected; PSS/RSS/threads/sockets at 0 and
#                after IDLE_MIN minutes; alarms, jobs, wake locks, Doze whitelist, batterystats wakeups
#     nopair     unpair on the desktop (app closed) and check that LinkService / the listener stop on the phone
#     nopair_phone  the same through the phone's own Unpair button
#     report     print the results table
#
#   VARIANT=debug|release  which APK to install for the runtime stages (default release = R8 build, signed with a
#                          throwaway key under target/perf-run). IDLE_MIN (30), RECONNECTS (50), XFER_MB (100),
#                          STARTS (7), BIG_MB ("100 500"), BIG_DIRS ("d2p p2d"), KEEP_EMU=1 (leave the emulator running). RUST_SRC=head builds the Rust
#                          crates (phone library + desktop service) from `git archive HEAD` instead of the working
#                          tree, to measure committed code while other work is half-edited.
#
# Isolation: its own AVD (gb36d, console 5604), adb server port 16041, desktop service on control port 17581, Xvfb :96;
# scratch copies under target/perf-android and target/perf-run. Never touches android/app/build or other agents'
# emulators. Results: target/perf-run/results.tsv and target/perf-run/raw/*.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
# shellcheck disable=SC1091
source scripts/android-env.sh >/dev/null 2>&1

PKG=dev.geekbridge.android
P="${PERF_DIR:-$ROOT/target/perf-run}"; RAW="$P/raw"; SCRATCH="${PERF_SCRATCH:-$ROOT/target/perf-android}"; RESULTS="$P/results.tsv"
mkdir -p "$RAW" "$SCRATCH/dist"
VARIANT="${VARIANT:-release}" IDLE_MIN="${IDLE_MIN:-30}" RECONNECTS="${RECONNECTS:-50}" XFER_MB="${XFER_MB:-100}" STARTS="${STARTS:-7}" BIG_MB="${BIG_MB:-100 500}" BIG_DIRS="${BIG_DIRS:-d2p p2d}"

# Dedicated ports/names (same knobs scripts/e2e-android.sh reads).
export E2E_AVD="${PERF_AVD:-gb36d}" E2E_EMU_PORT="${PERF_EMU_PORT:-5604}" E2E_ADB_PORT="${PERF_ADB_PORT:-16041}" E2E_RUN="$P/desk" E2E_SCRATCH="$SCRATCH" E2E_CTRL_PORT="${PERF_CTRL_PORT:-17581}"
export E2E_API_PORT="${PERF_API_PORT:-17580}" E2E_DISC_PORT="${PERF_DISC_PORT:-17516}" E2E_LISTEN_PORT="${PERF_LISTEN_PORT:-1731}" E2E_XDISP="${PERF_XDISP:-:96}" E2E_REDIR_PORT="${PERF_REDIR_PORT:-17261}" E2E_TAG="${PERF_TAG:-perf}"
export E2E_RESULTS="$P/e2e-results.tsv" E2E_TARGET="${PERF_DESKTOP:-$ROOT/target/perf-desktop}"   # own desktop build: same tree as the phone library
export ANDROID_ADB_SERVER_PORT="$E2E_ADB_PORT"
SERIAL="emulator-$E2E_EMU_PORT"
A() { adb -s "$SERIAL" "$@"; }
CTL=("$E2E_TARGET/release/geekbridge-ctl" --state-dir "$E2E_RUN/state" --control-port "$E2E_CTRL_PORT")
ctl() { timeout 30 "${CTL[@]}" "$@"; }
jget() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
log() { printf '[perf %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
rec() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$RESULTS" >&2; }   # metric, value, detail
wait_for() { local n="$1" i; shift; for ((i = 0; i < n; i++)); do "$@" && return 0; sleep 1; done; return 1; }

phone_id() { cat "$P/desk/phone-id" 2>/dev/null; }
pid_app() { A shell pidof "$PKG" 2>/dev/null | tr -d '\r' | awk '{print $1}'; }
is_connected() { [[ "$(ctl status | jget '" ".join(l["phase"] for l in d["links"])')" == *connected* ]]; }
dial() {   # desktop dials the phone through the emulator's port redirection (as the e2e script does)
  A emu redir add "tcp:$E2E_REDIR_PORT:1716" >/dev/null 2>&1
  ctl connect "$(phone_id)" 127.0.0.1 "$E2E_REDIR_PORT" >/dev/null 2>&1
}
# Start every stage from a fresh dial so a stage's numbers do not depend on what the previous one left behind.
ensure_connected() {
  ctl disconnect "$(phone_id)" >/dev/null 2>&1; sleep 1
  A shell am start -n "$PKG/.HomeActivity" >/dev/null 2>&1; sleep 3
  dial; wait_for 20 is_connected
  A shell am start -f 0x04000000 -n "$PKG/.HomeActivity" >/dev/null 2>&1; sleep 2
}
# Phone -> desktop payloads: the phone listens on the first free port of 1739-1764 and the desktop dials the same
# port on 127.0.0.1, so the host port must be redirected to this emulator. Another emulator on the host may already
# own the lowest ones (E2E-11): keep those busy inside this guest, so the phone takes the first one free on both sides.
p2d_ports() {
  local p
  A shell "pkill -f 'nc -l -[p] 17' 2>/dev/null; true" >/dev/null 2>&1
  for ((p = 1739; p <= 1764; p++)); do
    if A emu redir add "tcp:$p:$p" 2>&1 | grep -q '^OK'; then log "phone -> desktop payload port $p"; return 0; fi
    # Already redirected by this emulator from an earlier run: reuse it.
    if A emu redir list 2>/dev/null | grep -q "tcp:$p  *=> *$p"; then log "phone -> desktop payload port $p (kept)"; return 0; fi
    A shell "nohup nc -l -p $p >/dev/null 2>&1 </dev/null &" >/dev/null 2>&1
  done
  return 1
}

# snap LABEL -> "pss_kb java_kb native_kb rss_kb threads fds sockets hwm_kb" appended to raw/snaps.tsv and echoed
snap() {
  local label="$1" pid mi
  pid="$(pid_app)"; [[ -n "$pid" ]] || { echo "$label	no-process" | tee -a "$RAW/snaps.tsv"; return 1; }
  mi="$(A shell dumpsys meminfo "$pid")"
  local pss java nat rss thr fds socks hwm
  pss="$(awk '/TOTAL PSS:/ {print $3}' <<<"$mi")"; rss="$(awk '/TOTAL PSS:/ {print $6}' <<<"$mi")"
  java="$(awk '/Java Heap:/ {print $3}' <<<"$mi")"; nat="$(awk '/Native Heap:/ {print $3}' <<<"$mi")"
  thr="$(A shell "grep -m1 Threads /proc/$pid/status" | awk '{print $2}')"
  hwm="$(A shell "grep -m1 VmHWM /proc/$pid/status" | awk '{print $2}')"
  fds="$(A shell "ls /proc/$pid/fd | wc -l" | tr -d '\r')"
  socks="$(A shell "ls -l /proc/$pid/fd | grep -c socket:" | tr -d '\r')"
  printf '%s\t%s\n' "$label" "pss=$pss java=$java native=$nat rss=$rss threads=$thr fds=$fds sockets=$socks hwm=$hwm" | tee -a "$RAW/snaps.tsv"
}
snapval() { awk -F'\t' -v l="$1" -v k="$2" '$1==l {n=split($2,a," "); for(i=1;i<=n;i++){split(a[i],kv,"="); if(kv[1]==k) v=kv[2]}} END{print v}' "$RAW/snaps.tsv"; }
gc_app() { A shell am send-trim-memory "$PKG" RUNNING_CRITICAL >/dev/null 2>&1; A shell kill -10 "$(pid_app)" >/dev/null 2>&1; sleep 3; }  # SIGUSR1 = ART GC

# ---------------------------------------------------------------- size
stage_size() {
  local S="$SCRATCH/android" B
  log "scratch copy + native libs (cargo --profile android, separate target dir) + gradle debug/release"
  mkdir -p "$SCRATCH"; cp "$ROOT/Cargo.toml" "$SCRATCH/Cargo.toml"
  local RS="$ROOT"
  if [[ "${RUST_SRC:-tree}" == head ]]; then
    RS="$ROOT/target/perf-head"; rm -rf "$RS"; mkdir -p "$RS"; git archive HEAD | tar -x -C "$RS"
  fi
  rsync -a --delete --exclude build --exclude .gradle --exclude .kotlin --exclude local.properties android/ "$S/"
  for e in aarch64-linux-android:arm64-v8a x86_64-linux-android:x86_64; do
    local t="${e%%:*}" a="${e##*:}"
    (cd "$RS" && CARGO_TARGET_DIR="$ROOT/target/perf-cargo" cargo build --profile android -p bridge-android --target "$t") >"$RAW/cargo-$a.log" 2>&1 \
      || { log "cargo failed, see $RAW/cargo-$a.log"; return 1; }
    mkdir -p "$S/app/src/main/jniLibs/$a"
    llvm-strip --strip-unneeded -o "$S/app/src/main/jniLibs/$a/libgeekbridge_android.so" "$ROOT/target/perf-cargo/$t/android/libgeekbridge_android.so"
  done
  # The desktop service/ctl from the same tree (a service built from another revision can fail the payload TLS).
  (cd "$RS" && CARGO_TARGET_DIR="$E2E_TARGET" cargo build --release -p bridge-service --bins >"$RAW/cargo-desktop.log" 2>&1) \
    || { log "desktop build failed, see $RAW/cargo-desktop.log"; return 1; }
  echo "sdk.dir=$ANDROID_HOME" >"$S/local.properties"
  (cd "$S" && gradle --no-daemon --console=plain :app:assembleDebug :app:assembleRelease >"$RAW/gradle-build.log" 2>&1) \
    || { log "gradle failed, see $RAW/gradle-build.log"; return 1; }
  B="$S/app/build/outputs/apk"
  [[ -f "$P/perf.jks" ]] || keytool -genkeypair -keystore "$P/perf.jks" -storepass perfperf -keypass perfperf -alias perf \
    -keyalg RSA -keysize 2048 -validity 3650 -dname "CN=perf (throwaway)" >/dev/null 2>&1
  sign() { zipalign -f -P 16 4 "$1" "$P/tmp-aligned.apk" && apksigner sign --ks "$P/perf.jks" --ks-pass pass:perfperf \
    --key-pass pass:perfperf --out "$2" "$P/tmp-aligned.apk" && rm -f "$2.idsig" "$P/tmp-aligned.apk"; }
  sign "$B/debug/app-debug.apk" "$P/geekbridge-debug.apk"
  sign "$B/release/app-release-unsigned.apk" "$P/geekbridge-release.apk"
  cp "$P/geekbridge-debug.apk" "$SCRATCH/dist/geekbridge-debug.apk"   # where e2e-android.sh looks
  for v in debug release; do
    local f="$P/geekbridge-$v.apk"
    rec "size.apk.$v" "$(stat -c %s "$f")" "bytes; $(unzip -lv "$f" | awk '/classes[0-9]*\.dex/ {s+=$3; c+=$1} END{printf "dex %d B stored %d B", c, s}')"
  done
  rec size.lib.arm64 "$(stat -c %s "$S/app/src/main/jniLibs/arm64-v8a/libgeekbridge_android.so")" bytes
  rec size.lib.x86_64 "$(stat -c %s "$S/app/src/main/jniLibs/x86_64/libgeekbridge_android.so")" bytes
  rec size.res "$(unzip -lv "$P/geekbridge-release.apk" | awk '/resources.arsc|res\//{s+=$1} END{print s}')" "bytes uncompressed (resources.arsc + res/)"
}

# ---------------------------------------------------------------- setup
stage_setup() {
  mkdir -p "$E2E_RUN"
  [[ -f "$P/geekbridge-$VARIANT.apk" ]] || { log "no APK, run the size stage first"; return 1; }
  # e2e-android.sh installs $SCRATCH/dist/geekbridge-debug.apk; the release APK is signed with the same throwaway
  # key, so it installs over it afterwards with the pairing kept.
  A uninstall "$PKG" >/dev/null 2>&1   # an APK signed by another key would make the e2e install fail
  # a file, not a pipe: the emulator/service it leaves running would hold the pipe open and block `tail` forever
  scripts/e2e-android.sh --no-build --only env,battery,pair >"$RAW/e2e-setup.log" 2>&1; tail -6 "$RAW/e2e-setup.log"
  A root >/dev/null 2>&1; sleep 2; wait_for 30 bash -c "adb -s $SERIAL shell id 2>/dev/null | grep -q uid=0"
  if [[ "$VARIANT" == release ]]; then
    A install -r "$P/geekbridge-release.apk" 2>&1 | tail -1
  fi
  A shell cmd deviceidle whitelist +"$PKG" >/dev/null 2>&1
  A shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 4
  ensure_connected
  rec setup.paired "$(ctl status | jget 'len(d["paired"])')" "variant=$VARIANT; connected=$(is_connected && echo yes || echo no)"
}

close_app() {   # remove the task (swipe it away in Recents) so no activity is alive; the foreground service stays
  A shell input keyevent KEYCODE_HOME; sleep 1
  A shell input keyevent KEYCODE_APP_SWITCH; sleep 2; A shell input swipe 540 1300 540 200 200; sleep 2
  A shell input keyevent KEYCODE_HOME; sleep 1
}

# ---------------------------------------------------------------- startup
stage_startup() {
  local t=() i
  for ((i = 0; i < STARTS; i++)); do
    A shell am force-stop "$PKG"; sleep 2
    A shell input keyevent KEYCODE_HOME; sleep 1
    t+=("$(A shell am start -W -n "$PKG/.HomeActivity" | awk '/TotalTime/ {print $2}' | tr -d '\r')")
    sleep 3
  done
  local med; med="$(printf '%s\n' "${t[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')"
  rec startup.cold.median_ms "$med" "TotalTime over $STARTS cold starts after force-stop: ${t[*]}"
  A shell input keyevent KEYCODE_HOME; sleep 2
  rec startup.warm_ms "$(A shell am start -W -n "$PKG/.HomeActivity" | awk '/TotalTime/ {print $2}' | tr -d '\r')" "app still resident"
  ensure_connected
}

# ---------------------------------------------------------------- reconnect leak check
stage_reconnect() {
  ensure_connected || { rec reconnect.error 1 "not connected"; return 1; }
  close_app; sleep 5; gc_app
  snap reconnect.before
  local i
  for ((i = 1; i <= RECONNECTS; i++)); do
    ctl disconnect "$(phone_id)" >/dev/null 2>&1; sleep 1
    dial; wait_for 15 is_connected || log "reconnect $i did not come back"
    ((i % 10 == 0)) && snap "reconnect.$i"
  done
  sleep 5; gc_app
  snap reconnect.after
  rec reconnect.pss_growth_kb "$(( $(snapval reconnect.after pss) - $(snapval reconnect.before pss) ))" "$RECONNECTS reconnects, after GC"
  rec reconnect.threads "$(snapval reconnect.before threads) -> $(snapval reconnect.after threads)" "thread count"
  rec reconnect.fds "$(snapval reconnect.before fds) -> $(snapval reconnect.after fds)" "open fds (sockets $(snapval reconnect.before sockets) -> $(snapval reconnect.after sockets))"
}

# ---------------------------------------------------------------- 100 MB transfer
stage_transfer() {
  ensure_connected || { rec transfer.error 1 "not connected"; return 1; }
  local f="$P/xfer-$XFER_MB.bin" dev="/sdcard/Download/GeekBridge/xfer-$XFER_MB.bin"
  [[ -f "$f" ]] || head -c "$((XFER_MB * 1048576))" /dev/urandom >"$f"
  A shell rm -f "$dev"
  A shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 3
  gc_app; snap transfer.before
  export ADB="adb -s $SERIAL"; UI="$ROOT/scripts/e2e/adbui.py"
  timeout 30 "$UI" tap Decline >/dev/null 2>&1   # a leftover offer dialog would answer the wrong transfer
  ctl send-file "$(phone_id)" "$f" >/dev/null
  local acc=0 k
  for k in 1 2 3 4 5 6; do   # uiautomator is slow when the host is loaded: poll until the offer dialog is really accepted
    if timeout 60 "$UI" has "wants to send a file"; then timeout 60 "$UI" tap Accept && { acc=1; break; }; fi
    sleep 2
  done
  log "offer accepted: $acc"
  local t0=$SECONDS v
  while ((SECONDS - t0 < 240)); do
    sleep 4
    v="$(A shell stat -c %s "$dev" 2>/dev/null | tr -d '\r')"; v="${v:-0}"
    snap "transfer.mid.$((SECONDS - t0))s" >/dev/null
    ((v >= XFER_MB * 1048576)) && break
  done
  local secs=$((SECONDS - t0))
  close_app; sleep 5; gc_app
  snap transfer.after
  local have want; have="$(A shell sha256sum "$dev" 2>/dev/null | cut -d' ' -f1)"; want="$(sha256sum "$f" | cut -d' ' -f1)"
  rec transfer.sha_ok "$([[ -n "$have" && "$have" == "$want" ]] && echo yes || echo NO)" "$XFER_MB MB desktop -> phone in ~${secs}s"
  rec transfer.peak_pss_kb "$(awk -F'\t' '$1 ~ /^transfer\.mid/ {split($2,a," "); sub("pss=","",a[1]); if (a[1]+0>m) m=a[1]+0} END{print m}' "$RAW/snaps.tsv")" "highest PSS sampled during the transfer"
  rec transfer.pss_growth_kb "$(( $(snapval transfer.after pss) - $(snapval transfer.before pss) ))" "after GC, app closed vs. before"
  rec transfer.fds "$(snapval transfer.before fds) -> $(snapval transfer.after fds)" "threads $(snapval transfer.before threads) -> $(snapval transfer.after threads)"
  A shell rm -f "$dev"
}

# ---------------------------------------------------------------- big transfers both ways + cancel
desk_pid() { pgrep -f "geekbridge-service.*$E2E_RUN/state" | head -1; }
desk_rss() { awk '/VmRSS/ {print $2}' "/proc/$(desk_pid)/status" 2>/dev/null; }
desk_snap() { local d; d="$(desk_pid)"; echo "rss=$(awk '/VmRSS/ {print $2}' /proc/$d/status) hwm=$(awk '/VmHWM/ {print $2}' /proc/$d/status) threads=$(awk '/Threads/ {print $2}' /proc/$d/status) fds=$(ls /proc/$d/fd | wc -l)"; }
wl_count() { A shell dumpsys power | grep -iE "WAKE_LOCK|PARTIAL_WAKE" | grep -c "$PKG"; }
fgs_count() { A shell dumpsys activity services "$PKG" | grep -c "isForeground=true"; }
last_tid() { ctl share | grep -oE '^ *#?[0-9]+' | tr -d ' #' | tail -1; }
SAVE="${HOME}/Downloads/GeekBridge"
# xfer_run TAG DIRECTION(d2p|p2d) MB CANCEL(0|1): samples the phone and the desktop service until the file is complete (or
# 8 s after the cancel), then 10 s after the end.
xfer_run() {
  local tag="$1" dir="$2" mb="$3" cancel="$4" name="xfer-$3.bin" f="$P/xfer-$3.bin" dev="/sdcard/Download/GeekBridge/xfer-$3.bin"
  [[ -f "$f" ]] || head -c "$((mb * 1048576))" /dev/urandom >"$f"
  export ADB="adb -s $SERIAL"; local UI="$ROOT/scripts/e2e/adbui.py"
  ensure_connected || { rec "$tag.error" 1 "not connected"; return 1; }
  A shell rm -f "$dev"; rm -f "$SAVE/xfer-$mb"*.bin "$SAVE"/.gb-*xfer-"$mb"*.part
  A shell am start -n "$PKG/.HomeActivity" >/dev/null; sleep 3
  timeout 30 "$UI" tap Decline >/dev/null 2>&1
  gc_app; snap "$tag.before"; echo "$tag.before	$(desk_snap)" >>"$RAW/desk.tsv"
  local wl0 fg0; wl0="$(wl_count)"; fg0="$(fgs_count)"
  local want="$(( mb * 1048576 ))" have_fn t0 acc=0 k
  if [[ "$dir" == d2p ]]; then
    ctl send-file "$(phone_id)" "$f" >/dev/null
    for k in 1 2 3 4 5 6; do
      if timeout 60 "$UI" has "wants to send a file"; then timeout 60 "$UI" tap Accept && { acc=1; break; }; fi; sleep 2
    done
    # The phone writes a pending MediaStore row, so the size only appears at the end: progress comes from the desktop.
    have_fn() { if ((cancel)); then ctl share | grep "$name" | tail -1 | grep -oE '[0-9]+/[0-9]+ bytes' | cut -d/ -f1; else A shell stat -c %s "$dev" 2>/dev/null | tr -d '\r'; fi; }
  else
    p2d_ports || { rec "$tag.error" 1 "no payload port free on both the host and the guest"; return 1; }
    ctl share allow "$(phone_id)" on >/dev/null; ctl share auto-accept "$(phone_id)" on >/dev/null
    A push "$f" "/sdcard/Download/xfer-$mb.png" >/dev/null
    A shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d "file:///sdcard/Download/xfer-$mb.png" >/dev/null; sleep 3
    local id; id="$(A shell "content query --uri content://media/external/images/media --projection _id --where \"_display_name='xfer-$mb.png'\"" | sed -n 's/.*_id=\([0-9]*\).*/\1/p' | head -1)"
    [[ -n "$id" ]] || { rec "$tag.error" 1 "MediaStore did not index xfer-$mb.png"; return 1; }
    # Through the chooser by label (its row order changes, so coordinates can open another app). Starting
    # ShareActivity directly would lose the URI grant (SecurityException).
    # As the shell user: a grant made by root does not let the app open the media file.
    A shell su shell am start -a android.intent.action.SEND -t image/png --eu android.intent.extra.STREAM "content://media/external/images/media/$id" --grant-read-uri-permission >/dev/null
    sleep 4; timeout 60 "$UI" tap "Send to computer" >/dev/null 2>&1; sleep 1; timeout 60 "$UI" tap "Just once" >/dev/null 2>&1; sleep 2
    A shell dumpsys window | grep -m1 mCurrentFocus | grep -q "$PKG" || { rec "$tag.error" 1 "the chooser did not open the app"; return 1; }
    acc=1
    have_fn() { stat -c %s "$SAVE"/xfer-"$mb"*.png "$SAVE"/.gb-*xfer-"$mb"*.part 2>/dev/null | sort -n | tail -1; }
  fi
  log "$tag: started (accepted=$acc)"
  t0=$SECONDS; local v tid="" cancelled=0 done_at=
  while ((SECONDS - t0 < 600)); do
    # A snapshot takes seconds (dumpsys); before a cancel only the size is sampled, or the file finishes first.
    if ((cancel && !cancelled)); then sleep 0.3; else sleep 3; fi
    v="$(have_fn)"; v="${v:-0}"
    if ((!cancel || cancelled)); then snap "$tag.mid.$((SECONDS - t0))s" >/dev/null; echo "$tag.mid.$((SECONDS - t0))s	$(desk_snap)" >>"$RAW/desk.tsv"; fi
    if ((cancel && !cancelled && v >= want / 4)) || ((cancel && !cancelled && SECONDS - t0 > 120)); then
      tid="$(ctl share | grep -oE '^ *#?[0-9]+' | tr -d ' #' | tail -1)"; ctl share cancel "$tid" >/dev/null 2>&1; cancelled=1; done_at=$SECONDS
      log "$tag: cancelled transfer $tid at $((v / 1048576)) MB"
    fi
    ((cancelled)) && ((SECONDS - done_at >= 8)) && break
    ((!cancel)) && ((v >= want)) && break
  done
  local secs=$((SECONDS - t0))
  sleep 10; snap "$tag.after10" >/dev/null; echo "$tag.after10	$(desk_snap)" >>"$RAW/desk.tsv"
  close_app; sleep 3; gc_app; snap "$tag.gc"
  local peak mid_thr mid_fds peak_rss
  peak="$(awk -F'\t' -v t="$tag" 'index($1,t".mid.")==1 {split($2,a," "); sub("pss=","",a[1]); if (a[1]+0>m) m=a[1]+0} END{print m+0}' "$RAW/snaps.tsv")"
  mid_thr="$(awk -F'\t' -v t="$tag" 'index($1,t".mid.")==1 {n=split($2,a," "); for(i=1;i<=n;i++) if (a[i] ~ /^threads=/) {sub("threads=","",a[i]); if (a[i]+0>m) m=a[i]+0}} END{print m+0}' "$RAW/snaps.tsv")"
  mid_fds="$(awk -F'\t' -v t="$tag" 'index($1,t".mid.")==1 {n=split($2,a," "); for(i=1;i<=n;i++) if (a[i] ~ /^fds=/) {sub("fds=","",a[i]); if (a[i]+0>m) m=a[i]+0}} END{print m+0}' "$RAW/snaps.tsv")"
  peak_rss="$(awk -F'\t' -v t="$tag" 'index($1,t".mid.")==1 {split($2,a," "); sub("rss=","",a[1]); if (a[1]+0>m) m=a[1]+0} END{print m+0}' "$RAW/desk.tsv")"
  local ok="n/a"
  if ((!cancel)); then
    local h w; w="$(sha256sum "$f" | cut -d' ' -f1)"
    if [[ "$dir" == d2p ]]; then h="$(A shell sha256sum "$dev" 2>/dev/null | cut -d' ' -f1)"; else h="$(sha256sum "$SAVE"/xfer-"$mb"*.png 2>/dev/null | head -1 | cut -d' ' -f1)"; fi
    ok="$([[ -n "$h" && "$h" == "$w" ]] && echo yes || echo NO)"
  else
    local left
    if [[ "$dir" == d2p ]]; then left="$(A shell "ls -la /sdcard/Download/GeekBridge/ | grep -c xfer-$mb" | tr -d '\r')"; else left="$(ls -A "$SAVE" | grep -c "xfer-$mb")"; fi
    ok="partial_left=$left"
  fi
  rec "$tag.result" "$ok" "$mb MB $dir cancel=$cancel in ~${secs}s"
  rec "$tag.phone.pss_kb" "$(snapval "$tag.before" pss) -> peak $peak -> +10s $(snapval "$tag.after10" pss) -> gc $(snapval "$tag.gc" pss)" "growth after GC: $(( $(snapval "$tag.gc" pss) - $(snapval "$tag.before" pss) )) kB"
  rec "$tag.phone.threads_fds" "threads $(snapval "$tag.before" threads) -> peak $mid_thr -> $(snapval "$tag.gc" threads); fds $(snapval "$tag.before" fds) -> peak $mid_fds -> $(snapval "$tag.gc" fds)" "sockets $(snapval "$tag.before" sockets) -> $(snapval "$tag.gc" sockets)"
  rec "$tag.desktop.rss_kb" "$(awk -F'\t' -v t="$tag.before" '$1==t {split($2,a," "); sub("rss=","",a[1]); print a[1]}' "$RAW/desk.tsv") -> peak $peak_rss -> +10s $(awk -F'\t' -v t="$tag.after10" '$1==t {split($2,a," "); sub("rss=","",a[1]); print a[1]}' "$RAW/desk.tsv")" "desktop service $(awk -F'\t' -v t="$tag.after10" '$1==t {print $2}' "$RAW/desk.tsv")"
  rec "$tag.wakelocks_fgs" "wake locks $wl0 -> $(wl_count); foreground services $fg0 -> $(fgs_count)" "held by the package (dumpsys power / activity services)"
  A shell rm -f "$dev" "/sdcard/Download/xfer-$mb.png"; rm -f "$SAVE"/xfer-"$mb"*.png "$SAVE"/xfer-"$mb"*.bin
}
stage_bigxfer() {
  local mb d
  for mb in $BIG_MB; do
    for d in $BIG_DIRS; do xfer_run "big$mb.$d" "$d" "$mb" 0; done
  done
  mb="${BIG_MB##* }"
  for d in $BIG_DIRS; do xfer_run "cancel$mb.$d" "$d" "$mb" 1; done
}
stage_bigcancel() {   # only the mid-transfer cancels of bigxfer
  local d mb="${BIG_MB##* }"
  for d in $BIG_DIRS; do xfer_run "cancel$mb.$d" "$d" "$mb" 1; done
}

# ---------------------------------------------------------------- idle + power
stage_idle() {
  ensure_connected || log "warning: not connected; measuring the listening service only"
  close_app
  A shell svc power stayon false; A shell input keyevent KEYCODE_SLEEP
  A shell dumpsys battery unplug >/dev/null
  A shell dumpsys batterystats --reset >/dev/null
  sleep 20; gc_app; snap idle.t0
  local pid; pid="$(pid_app)"
  A shell "awk '{print \$14+\$15}' /proc/$pid/stat" >"$RAW/cpu0.txt"
  A shell "grep ctxt /proc/$pid/status" | tr '\n' ' ' >"$RAW/ctxt0.txt"
  log "idle for $IDLE_MIN min (screen off, unplugged, Doze eligible)"
  sleep "$((IDLE_MIN * 60))"
  [[ "$(pid_app)" == "$pid" ]] || log "process restarted during idle: pid $pid -> $(pid_app)"
  snap idle.end
  A shell "awk '{print \$14+\$15}' /proc/$pid/stat" >"$RAW/cpu1.txt"
  A shell "grep ctxt /proc/$pid/status" | tr '\n' ' ' >"$RAW/ctxt1.txt"
  rec idle.cpu_ticks "$(( $(tr -d '\r\n' <"$RAW/cpu1.txt") - $(tr -d '\r\n' <"$RAW/cpu0.txt") ))" "utime+stime ticks (1/100 s) over $IDLE_MIN min"
  rec idle.ctxt_switches "$(tr -s ' \t\r\n' ' ' <"$RAW/ctxt0.txt")=> $(tr -s ' \t\r\n' ' ' <"$RAW/ctxt1.txt")" "voluntary/involuntary"
  A shell dumpsys battery reset >/dev/null
  A shell dumpsys alarm >"$RAW/alarm.txt"; A shell dumpsys jobscheduler >"$RAW/jobs.txt"; A shell dumpsys power >"$RAW/power.txt"
  A shell dumpsys deviceidle >"$RAW/deviceidle.txt"; A shell dumpsys batterystats "$PKG" >"$RAW/batterystats.txt"
  A shell dumpsys batterystats --checkin >"$RAW/batterystats-checkin.txt"
  rec idle.alarms "$(grep -c "$PKG" "$RAW/alarm.txt")" "lines mentioning the package in dumpsys alarm (0 = none registered)"
  rec idle.jobs "$(grep -c "$PKG" "$RAW/jobs.txt")" "lines mentioning the package in dumpsys jobscheduler"
  rec idle.wakelocks "$(grep -iE "WAKE_LOCK|PARTIAL" "$RAW/power.txt" | grep -c "$PKG")" "held by the package at the end (dumpsys power)"
  rec idle.deviceidle_whitelist "$(grep -c "$PKG" "$RAW/deviceidle.txt")" "package lines in dumpsys deviceidle"
  rec idle.batterystats "see raw/batterystats.txt" "wakeups, wake locks, CPU time and network for the package over the idle window"
  A shell svc power stayon true; A shell input keyevent KEYCODE_WAKEUP
}

# ---------------------------------------------------------------- unpaired => service stops
svc_count() { A shell dumpsys activity services "$PKG" | grep -c "ServiceRecord.*LinkService"; }
stage_nopair() {
  # The desktop's unpair sends {pair:false}: the phone forgets the computer and its service stops with the app closed.
  ensure_connected
  close_app; sleep 3
  rec nopair.desktop_service_before "$(svc_count)" "LinkService records while paired and the app is closed (1 expected)"
  ctl unpair "$(phone_id)" >/dev/null; sleep 5
  rec nopair.desktop_service_after "$(svc_count)" "after geekbridge-ctl unpair, app closed (0 expected)"
  rec nopair.desktop_paired_count "$(A shell cat "/data/data/$PKG/shared_prefs/geekbridge_state.xml" | grep -o 'paired_count" value="[0-9]*' | grep -o '[0-9]*$')" "paired computers the app remembers (0 expected)"
  rec nopair.desktop_listeners "$(A shell "cat /proc/net/tcp" | awk '$2 ~ /:06B4$/ && $4 == "0A"' | wc -l)" "sockets listening on 1716 (0 expected)"
}
stage_nopair_phone() {
  # The phone's own "Unpair" button (Diagnostics) must stop the service too. Needs a paired phone: run setup first.
  export ADB="adb -s $SERIAL"; local UI="$ROOT/scripts/e2e/adbui.py"
  A shell am start -f 0x04000000 -n "$PKG/.HomeActivity" >/dev/null; sleep 3
  rec nopair.service_before "$(svc_count)" "LinkService records while paired (1 expected)"
  timeout 90 "$UI" scrolltap "Diagnostics & about" && sleep 3
  timeout 90 "$UI" scrolltap "Unpair" --contains >/dev/null 2>&1; sleep 2
  timeout 60 "$UI" tap Unpair; sleep 8
  rec nopair.service_after_unpair_app_open "$(svc_count)" "after the phone's Unpair, Diagnostics still open"
  close_app; sleep 8
  rec nopair.service_after_unpair_closed "$(svc_count)" "after the phone's Unpair, app closed (0 expected)"
  rec nopair.fgs_notifications "$(A shell dumpsys notification --noredact | grep -c "pkg=$PKG.*LinkService\|channel=link")" "foreground-service notification records (0 expected)"
  A shell am broadcast -a android.intent.action.MY_PACKAGE_REPLACED -n "$PKG/.BootReceiver" >/dev/null 2>&1; sleep 5
  rec nopair.service_after_boot_broadcast "$(svc_count)" "after a simulated MY_PACKAGE_REPLACED with nothing paired (0 expected)"
}

stage_report() { column -t -s$'\t' "$RESULTS" 2>/dev/null || cat "$RESULTS"; }

ALL=(size setup startup reconnect transfer bigxfer idle nopair setup nopair_phone report)
STAGES=("$@"); ((${#STAGES[@]})) || STAGES=("${ALL[@]}")
for s in "${STAGES[@]}"; do
  declare -F "stage_$s" >/dev/null || { echo "unknown stage $s" >&2; exit 2; }
  log "== stage $s"
  "stage_$s" || log "stage $s reported a failure"
done
[[ "${KEEP_EMU:-1}" == 1 ]] || { A emu kill >/dev/null 2>&1; }
