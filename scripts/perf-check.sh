#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Measure the desktop service against the perf budget in docs/performance.md; exit 1 when over.
#
#   scripts/perf-check.sh [--build] [--bin-dir DIR] [--window SECONDS] [--ui]
#
#   --build        cargo build --release --locked first (bridge-service, plus bridge-apps with --ui)
#   --bin-dir DIR  where the release binaries are (default: $CARGO_TARGET_DIR/release or target/release)
#   --window N     seconds of idle observation for the wakeup count (default 20)
#   --ui           also measure geekbridge-messages (needs a display; skipped without DISPLAY/WAYLAND_DISPLAY)
#
# Everything is read from /proc (Linux only): RSS, threads, fds, context switches ("wakeups") and CPU
# ticks. No perf/strace/valgrind needed. Budgets can be overridden through the environment, e.g.
# BUDGET_RSS_KIB=9000. The service runs on private ports in a temp state dir and never touches the
# user's real state, the network beyond loopback/UDP discovery on a private port, or the system bus.
# The tray runs too, on a private session bus with a stand-in StatusNotifierWatcher (see below).
set -Eeuo pipefail
IFS=$'\n\t'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BIN_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/release"
WINDOW=20
BUILD=0
UI=0

# GB_CI=1 (set by .github/workflows/check.yml): shared runners have other CPUs, noisy neighbours and a different
# libc/kernel, so the load-dependent rows get documented slack. Sizes stay strict. Explicit BUDGET_* overrides win (CI values are only defaults, applied before the normal ones).
if [[ ${GB_CI:-} == 1 ]]; then
    : "${BUDGET_RSS_KIB:=8500}" "${BUDGET_THREADS:=10}" "${BUDGET_FDS:=24}" "${BUDGET_WAKEUPS_PER_S:=6}"
    : "${BUDGET_CPU_TICKS:=6}" "${BUDGET_STARTUP_MS:=1000}"
fi

# Budgets (service, idle, no phone linked). Measured baseline in docs/performance.md.
: "${BUDGET_RSS_KIB:=6000}"           # resident set (10000 -> 6000: idle code pages are dropped 2 s after start, 3.4 MB measured, docs/performance.md)
: "${BUDGET_THREADS:=8}"              # OS threads
: "${BUDGET_FDS:=20}"                 # open descriptors
: "${BUDGET_WAKEUPS_PER_S:=3}"        # context switches per second, whole process
: "${BUDGET_CPU_TICKS:=2}"            # CPU ticks (10 ms each) burned over the whole window
: "${BUDGET_STARTUP_MS:=300}"         # exec to "discovery API" line, warm cache
: "${BUDGET_SERVICE_BYTES:=7560000}"  # stripped binary size (lowered 2026-11-06 from 7 580 000 B after the size round: opt-level "z" for the cold crates (zbus, zvariant, zbus_xml, x509-parser, rcgen, webpki-roots: -30.1 kB) and one base64 version (callwifi on 0.23, -9.1 kB), 7 549 976 -> 7 510 808 B locally (CI runs about 17 kB above a local build, so ~7 528 000 B expected, 32 kB headroom); idle RSS, startup, wakeups and the stand-in phone pairing (CPU ticks, RSS) measured equal before and after. Before that 7.56 -> 7.58 MB on 2026-11-06: CI measured 7 567 184 B at a8d98b443 against 7 534 536 B at 149b92589 (+32.6 kB): the update client's happy-eyeballs connect race and HTTPS-proxy CONNECT tunnel (~230 lines, std only, no new dependency) and the BlueZ guard in call setup; +12.8 kB headroom; unchanged 7.56 MB on 2026-11-05: the self-update recovery at start and `--version` cost +21.5 kB, 7 512 296 B locally against 7 490 792 B without; 7.50 -> 7.56 MB on 2026-11-05: CI measured 7 515 424 B with the Messages data work (matching, SMS limits and eviction) and the call-trace writer, no new dependency; 7.42 -> 7.50 MB on 2026-11-04: CI measured 7 370 952 B and growing with the Wi-Fi call audio speak path and ctl setup, no new dependency; 7.36 -> 7.42 MB on 2026-11-04: CI measured 7 357 896 B with the fresh-install and Messages work, no new dependency; 7.30 -> 7.36 MB on 2026-11-04: CI measured 7 303 880 B after the call chain review, +56 kB headroom, no new dependency; 6.67 -> 7.30 MB: 7 247 968 B locally with the experimental Wi-Fi call audio on the computer (docs/reports/call-audio-wifi-integration.md): +578 kB, almost all of it the pure-Rust Opus codec `opus-pure` (BSD-3-Clause, no C): the encoder for "Speak (experimental)" is 266 kB (measured by building without it), the decoder and the rest of the hub (jitter buffer, gate, wire, pw-cat playback) about 310 kB; 52 kB headroom; the codec is only instantiated while a session runs, so idle RSS, threads and wake-ups are unchanged; 6.65 -> 6.67 MB: CI 6 653 584 B after security review 7 (the BlueZ agent confirmed-device and name checks, private state files, QR token grace, bidi-clean text), +8.8 kB over the 6 644 752 B step, docs/reports/security-review-7.md; 6.7 -> 6.65 MB: 6 575 448 B locally after size trim 4, the second base64 copy removed, docs/reports/size-trim-4.md; 6.2 -> 6.7 MB: 6 558 424 B locally (CI 6 609 168 B with an in-process libpulse, since replaced by pactl/parec) at the call-audio routing commit: the BlueZ pairing agent, the call-audio controller, the pactl adapter and the checklist, ~230 kB of own code plus zbus/serde instantiations, no new dependency, docs/performance.md; 6.1 -> 6.2 MB: CI 6 138 064 B at b5633caee, pairing/call-audio setup/calls window, no new dependency; 5.0 -> 5.3 -> 5.6 -> 5.8 -> 6.0 -> 6.1 MB: updater asset selection, Messages and Wi-Fi bind safety net, 6.003 MB measured in CI; phone mirroring, 5.82 MB measured in CI; the tray, then CI-measured, docs/performance.md)
: "${BUDGET_CTL_BYTES:=1745000}"       # 1.79 -> 1.745 MB on 2026-11-06: opt-level "z" for bridge-core (its serde Deserialize impls were the largest block of the ctl), 1 781 328 -> 1 723 848 B locally (-57.5 kB; CI runs about 5 kB above a local build, so ~1 729 000 B expected, ~16 kB headroom), service unchanged (-0.4 kB); before that 1.65 -> 1.79 MB on 2026-11-05: 1 764 712 B locally (+131 kB over 1 634 088 B built from the same tree without it) with `update install|rollback|restart` for the Linux per-user self-update: the streaming gzip and strict tar readers, staging/validation/swap/recovery and the state-file record, ~77 kB of own symbols (unstripped `nm`) plus std `Command`/fs/format instantiations, no new dependency (docs/design/desktop-self-update.md); 1.60 -> 1.65 MB on 2026-11-04: CI measured 1 608 632 B (callwifi setup and test commands, no new dependency); 1.55 -> 1.60 MB on 2026-11-04: CI measured 1 559 960 B (Messages privacy and fresh-install doctor work, no new dependency); 1.54 -> 1.55 MB: 1 545 400 B locally with the ufw source-range check in `doctor` (`ufw_allows_from`, the own-LAN-address read, the doctor verdict text) and the plain-words hotkey state in `clipboard status`; +8.5 kB over the 1 536 872 B step, 6.6 kB headroom; 1.48 -> 1.54 MB: 1 517 848 B locally with `callwifi` (the CallWifi wire types, `callwifi_cli`, the doctor lines; +38 kB over the 1.48 MB step), 22 kB headroom; 1.55 -> 1.48 MB: 1 454 056 B locally after size trim 4 (`ipc::pretty_json` prints the reply body instead of serializing every reply type again, -61 kB; docs/reports/size-trim-4.md); 1 509 632 B locally at the call-audio routing commit (HearCalls/CallAudioDevices wire types, `call setup`/`call audio-devices`; the checklist itself stays in the service), so 1.55 MB; 1 450 704 B on CI at b5633caee (pairing, call-audio setup and calls-window wire types), so 1.5 MB; 1 358 984 B on CI after the share-result commit, so 1.4 MB (docs/performance.md); 1 214 392 B after Response::from_json (1.3 MB budget); 1 318 016 B with the recent-photos requests and `photos` command, so 1.35 MB again (docs/performance.md)
: "${BUDGET_UI_RSS_KIB:=75000}"       # geekbridge-messages, idle, warm GL shader cache (65 MB measured 2026-11-02; 90000 before the memtrim/atlas work)
: "${BUDGET_UI_WAKEUPS_PER_S:=12}"

while (($#)); do
    case "$1" in
        --build) BUILD=1 ;;
        --ui) UI=1 ;;
        --bin-dir) BIN_DIR="$2"; shift ;;
        --window) WINDOW="$2"; shift ;;
        -h|--help) sed -n '3,14p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

if ((BUILD)); then
    pkgs=(-p bridge-service)
    ((UI)) && pkgs+=(-p bridge-apps)
    (cd "$ROOT" && cargo build --release --locked "${pkgs[@]}" --bins --example tray-fake-watcher)
fi
BIN_DIR="$(cd "$BIN_DIR" 2>/dev/null && pwd -P || echo "$BIN_DIR")"
[[ -x "$BIN_DIR/geekbridge-service" ]] || { echo "no release binary in $BIN_DIR (use --build or --bin-dir)" >&2; exit 2; }

source "$ROOT/scripts/lib-tmp.sh"
TMP="$(gb_mktemp perf -d)"
PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    [[ -n "${PERF_KEEP:-}" ]] && echo "kept $TMP" || rm -rf "$TMP"
}
trap cleanup EXIT

FAIL=0
row() { # name value budget unit
    local ok=ok
    if (($2 > $3)); then ok=OVER; FAIL=1; fi
    printf '%-26s %10s %s   (budget %s)  %s\n' "$1" "$2" "$4" "$3" "$ok"
}
# Voluntary context switches summed over every thread: each is a thread that blocked and was woken
# again, i.e. one wakeup. Involuntary (preempted) switches are excluded: they measure machine load.
ctxt() { { cat /proc/"$1"/task/*/status 2>/dev/null || true; } | awk '$1 == "voluntary_ctxt_switches:" {s += $2} END {print s + 0}'; }
ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
status() { awk -v k="$2:" '$1 == k {print $2}' "/proc/$1/status"; }
port() { echo $((20000 + RANDOM % 20000)); }

export GEEKBRIDGE_NO_UPDATE_CHECK=1
# The tray is part of the measured service: run it on a private session bus (never the user's) with
# a stand-in StatusNotifierWatcher when the example is built, so the item is registered and idle,
# which is what a desktop with a bar looks like. Without dbus-daemon the tray simply has no bus.
# PERF_NO_TRAY=1 measures the service without it (A/B).
TRAY_FLAG=()
if [[ -z "${PERF_NO_TRAY:-}" ]] && command -v dbus-daemon >/dev/null; then
    cat > "$TMP/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><listen>unix:path=$TMP/bus</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
    dbus-daemon --config-file="$TMP/bus.conf" --nofork --print-address=1 > "$TMP/bus.addr" 2>/dev/null &
    PIDS+=("$!")
    until [[ -s "$TMP/bus.addr" ]]; do
        kill -0 "${PIDS[-1]}" 2>/dev/null || { echo "dbus-daemon exited before printing its address" >&2; exit 1; }
        sleep 0.02
    done
    export DBUS_SESSION_BUS_ADDRESS="$(head -n1 "$TMP/bus.addr")"
    TRAY_FLAG=(--tray)
    if [[ -x "$BIN_DIR/examples/tray-fake-watcher" ]]; then
        "$BIN_DIR/examples/tray-fake-watcher" >"$TMP/watcher.log" 2>&1 &
        PIDS+=("$!")
        until grep -q 'WATCHER READY' "$TMP/watcher.log" 2>/dev/null; do
            kill -0 "${PIDS[-1]}" 2>/dev/null || { cat "$TMP/watcher.log" >&2; echo "tray-fake-watcher exited" >&2; exit 1; }
            sleep 0.02
        done
    fi
fi
API_PORT=$(port) CONTROL_PORT=$((API_PORT + 1)) DISC_PORT=$((API_PORT + 2))

start_ns=$(date +%s%N)
"$BIN_DIR/geekbridge-service" --state-dir "$TMP/state" --api-port "$API_PORT" \
    --control-port "$CONTROL_PORT" --discovery-port "$DISC_PORT" --no-calling-probe "${TRAY_FLAG[@]}" >"$TMP/service.log" 2>&1 &
SVC=$!
PIDS+=("$SVC")
until grep -q 'discovery API' "$TMP/service.log" 2>/dev/null; do
    kill -0 "$SVC" 2>/dev/null || { cat "$TMP/service.log" >&2; echo "service exited" >&2; exit 1; }
    sleep 0.002
done
startup_ms=$((($(date +%s%N) - start_ns) / 1000000))
# Every change of the resident set with the second it happened (information; printed with the breakdown), to see which job
# faults the code pages back in. Polling at 0.2 s reads /proc only and ends with the service.
(
    last=0
    while kill -0 "$SVC" 2>/dev/null; do
        cur=$(awk '$1 == "VmRSS:" {print $2}' "/proc/$SVC/status" 2>/dev/null || true)
        if [[ -n $cur && $cur != "$last" ]]; then
            printf '%s %s\n' "$((($(date +%s%N) - start_ns) / 100000000))" "$cur" >> "$TMP/rss.series"
            last=$cur
        fi
        sleep 0.2
    done
) &
PIDS+=("$!")

sleep 3 # settle: first-run key generation and allocator warm-up
c0=$(ctxt "$SVC"); t0=$(ticks "$SVC")
# Idle RSS is a sawtooth: the code pages dropped by `codepages::drop_idle` (2 s after start, after each LAN announcement
# every 15 s and after each session-bus probe, at start and on demand) fault back in when the next periodic job runs, and one RSS read lands
# anywhere on that curve (3.5 to 8 MB on the same binary). The budget is therefore on the settled idle figure: the lowest of
# the samples taken every 5 s over the window (at least 4, so at least one follows a trim). Peak is printed for information.
rss=0; rss_max=0; waited=0
while ((waited < WINDOW)); do
    step=$((WINDOW - waited < 5 ? WINDOW - waited : 5)); sleep "$step"; waited=$((waited + step))
    s=$(status "$SVC" VmRSS)
    if ((rss == 0 || s < rss)); then rss=$s; fi
    if ((s > rss_max)); then rss_max=$s; fi
done
c1=$(ctxt "$SVC"); t1=$(ticks "$SVC")
# Where the resident set sits (information only): per mapping kind, Rss/Pss/Anonymous in kB, largest first, plus what
# differs between machines (kernel, libc, page-fault read-around, huge pages), so a CI figure can be compared with a local one.
rss_breakdown() {
    echo "== geekbridge-service resident set by mapping (kB) =="
    [[ -s $TMP/rss.series ]] && echo "RSS changes (tenth of a second since start: kB): $(paste -sd' ' "$TMP/rss.series")"
    echo "kernel $(uname -r), $(getconf GNU_LIBC_VERSION 2>/dev/null || echo libc?), fault_around_bytes $(cat /sys/kernel/debug/fault_around_bytes 2>/dev/null || echo n/a), THP $(sed 's/.*\[\(.*\)\].*/\1/' /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo n/a)"
    awk '
        /^[0-9a-f]+-[0-9a-f]+ / { name = ($6 == "" ? "[anon]" : $6); sub(".*/", "", name); key = $2 " " name; cur = key }
        /^Rss:/ { r[cur] += $2 } /^Pss:/ { p[cur] += $2 } /^Anonymous:/ { a[cur] += $2 }
        /^FilePmdMapped:/ { f[cur] += $2 }
        END { for (k in r) if (r[k] > 0) printf "%7d rss %7d pss %7d anon %7d pmd  %s\n", r[k], p[k], a[k], f[k], k }' \
        "/proc/$SVC/smaps" 2>/dev/null | sort -rn | head -14
    grep -E '^(Rss|Pss|Pss_Anon|Pss_File|Anonymous|Private_Clean|Shared_Clean):' "/proc/$SVC/smaps_rollup" 2>/dev/null | tr -s ' ' | paste -sd' '
    awk '/^(Name|Threads|VmRSS|RssAnon|RssFile|VmStk):/ {printf "%s ", $0} END {print ""}' "/proc/$SVC/status" | tr -s ' \t' ' '
}
threads=$(status "$SVC" Threads); fds=$(find "/proc/$SVC/fd" -mindepth 1 | wc -l)
wakeups=$(((c1 - c0 + WINDOW - 1) / WINDOW))

echo "== geekbridge-service (idle ${WINDOW}s window) =="
row "startup" "$startup_ms" "$BUDGET_STARTUP_MS" ms
row "idle RSS (settled, min)" "$rss" "$BUDGET_RSS_KIB" kB
printf '%-26s %10s kB   (information, not budgeted)\n' "idle RSS (peak sample)" "$rss_max"
row "threads" "$threads" "$BUDGET_THREADS" ""
row "open fds" "$fds" "$BUDGET_FDS" ""
row "wakeups" "$wakeups" "$BUDGET_WAKEUPS_PER_S" "/s"
row "cpu ticks in window" "$((t1 - t0))" "$BUDGET_CPU_TICKS" "x10ms"
row "geekbridge-service size" "$(stat -c %s "$BIN_DIR/geekbridge-service")" "$BUDGET_SERVICE_BYTES" B
[[ -f "$BIN_DIR/geekbridge-ctl" ]] && row "geekbridge-ctl size" "$(stat -c %s "$BIN_DIR/geekbridge-ctl")" "$BUDGET_CTL_BYTES" B
rss_breakdown

if ((UI)); then
    if [[ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" || ! -x "$BIN_DIR/geekbridge-messages" ]]; then
        echo "== geekbridge-messages: skipped (no display or binary) =="
    else
        # Warm-up launch: with a cold GL shader disk cache the NVIDIA driver compiles in-process and
        # keeps ~40 MB (110 MB RSS vs 65 MB); that is the driver, not the app, so measure warm.
        (cd "$TMP" && timeout 5 "$BIN_DIR/geekbridge-messages" --api-port "$API_PORT" >/dev/null 2>&1) || true
        (cd "$TMP" && exec "$BIN_DIR/geekbridge-messages" --api-port "$API_PORT" >"$TMP/ui.log" 2>&1) &
        UIP=$!
        PIDS+=("$UIP")
        sleep 6
        u0=$(ctxt "$UIP"); sleep "$WINDOW"; u1=$(ctxt "$UIP")
        echo "== geekbridge-messages (idle ${WINDOW}s window) =="
        row "idle RSS" "$(status "$UIP" VmRSS)" "$BUDGET_UI_RSS_KIB" kB
        row "wakeups" "$(((u1 - u0 + WINDOW - 1) / WINDOW))" "$BUDGET_UI_WAKEUPS_PER_S" "/s"
    fi
fi

if ((FAIL)); then
    echo "PERF BUDGET EXCEEDED" >&2
    exit 1
fi
echo "within budget"
