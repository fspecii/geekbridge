#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verdict for a real-service soak (scripts/soak-real.sh): slope fits over samples.csv.

  python3 -I scripts/soak-fit.py samples.csv [--events events.tsv] [--warmup-frac 0.10]

Series that oscillate with activity (RSS, threads, descriptors, state size) are fitted on their per-window
minimum (a "floor": a leak raises the floor, activity bursts do not). Each fit reports the OLS and the
Theil-Sen slope per hour, the drift over the fitted window, and Kendall's tau (1.0 = strictly monotonic).
A series FAILS when its Theil-Sen drift is above its budget AND tau says the growth is systematic
(tau > 0.3) AND the second half of the window still grows (drift above budget/4 per half window: warm-up
that has levelled off is not a leak), or when a hard cap is broken. Control-API poll failures and a left-over
pairing are reported as anomalies (not failures) when they sit in the first 10 minutes and are explained by
the failed pairings; anything later fails. Prints a markdown report; exit status 1 on any failure.
Stdlib only.
"""
import csv, statistics, sys

BUDGET = {  # series: (unit, drift budget over the whole fitted window)
    "rss_kib": ("KiB", 3072),      # same allowance as scripts/soak.sh SOAK_RSS_GROWTH_KIB
    "threads": ("", 3),
    "fds": ("", 4),                # scripts/soak.sh SOAK_FD_SLACK
    "state_bytes": ("B", 4096),
    "state_files": ("", 2),
    "log_bytes": ("B", 256 * 1024),
}
CAPS = {  # hard caps on the maximum seen
    "events": 256,                 # the event log is a ring (leaks.rs)
    "notifications": 128,          # docs/kdeconnect-interop.md: mirrored notifications bounded to 128
    "state_files": 40,             # documented file set (docs/state-formats.md) with .bak / .lock
    "state_bytes": 256 * 1024,
    "paired": 3,
    "links": 3,
    "challenges": 3,
}


def num(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return 0.0


def ols(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    den = sum((x - mx) ** 2 for x in xs) or 1.0
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den


def theil_sen(xs, ys):
    n = len(xs)
    step = max(1, n // 120)  # keep it O(1e4) pairs
    idx = list(range(0, n, step))
    sl = [(ys[j] - ys[i]) / (xs[j] - xs[i]) for a, i in enumerate(idx) for j in idx[a + 1:] if xs[j] != xs[i]]
    return statistics.median(sl) if sl else 0.0


def kendall(ys):
    n = len(ys)
    step = max(1, n // 120)
    v = ys[::step]
    s = tot = 0
    for a in range(len(v)):
        for b in range(a + 1, len(v)):
            tot += 1
            s += (v[b] > v[a]) - (v[b] < v[a])
    return s / tot if tot else 0.0


def floor_series(ts, ys, window):
    out_t, out_y = [], []
    start = ts[0]
    bucket = []
    for t, y in zip(ts, ys):
        if t - start >= window and bucket:
            out_t.append(statistics.mean(b[0] for b in bucket))
            out_y.append(min(b[1] for b in bucket))
            bucket, start = [], t
        bucket.append((t, y))
    if bucket:
        out_t.append(statistics.mean(b[0] for b in bucket))
        out_y.append(min(b[1] for b in bucket))
    return out_t, out_y


def spark(ys, width=48):
    bars = "▁▂▃▄▅▆▇█"
    if not ys:
        return ""
    step = max(1, len(ys) // width)
    v = [statistics.mean(ys[i:i + step]) for i in range(0, len(ys), step)]
    lo, hi = min(v), max(v)
    return "".join(bars[int((x - lo) / (hi - lo) * 7)] if hi > lo else bars[0] for x in v)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    path = argv[1]
    events = argv[argv.index("--events") + 1] if "--events" in argv else None
    poll_errors = argv[argv.index("--poll-errors") + 1] if "--poll-errors" in argv else None
    wf = float(argv[argv.index("--warmup-frac") + 1]) if "--warmup-frac" in argv else 0.10
    rows = list(csv.DictReader(open(path)))
    run = [r for r in rows if r["tag"] == "run"]
    idle = [r for r in rows if r["tag"] == "idle"]
    if len(run) < 12:
        print(f"too few samples ({len(run)}) for a fit")
        return 1
    ts = [num(r["t_s"]) for r in run]
    span = ts[-1] - ts[0]
    warm = ts[0] + max(wf * span, min(300.0, 0.25 * span))
    sel = [i for i, t in enumerate(ts) if t >= warm]
    win = ts[-1] - warm
    fail = []
    anomalies = []
    lines = []
    L = lines.append
    L(f"Samples: {len(run)} during the run ({span / 60:.1f} min, every {span / max(1, len(run) - 1):.0f} s) + {len(idle)} idle; "
      f"fit window starts at {warm / 60:.1f} min ({len(sel)} samples, {win / 60:.1f} min).\n")
    L("| series | start | min | median | max | end | OLS slope /h | Theil-Sen /h | drift over window | 2nd-half /h | tau | budget | verdict |")
    L("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for key in ["rss_kib", "threads", "fds", "state_bytes", "state_files", "log_bytes", "saves_bytes",
                "transfers", "events", "notifications", "driver_fds", "driver_threads"]:
        ys_all = [num(r[key]) for r in run]
        xs = [ts[i] for i in sel]
        ys = [ys_all[i] for i in sel]
        if key in ("rss_kib", "threads", "fds", "state_bytes", "state_files", "driver_fds", "driver_threads"):
            fx, fy = floor_series(xs, ys, 300.0)
        else:
            fx, fy = xs, ys
        if len(fx) < 4:
            fx, fy = xs, ys
        o = ols(fx, fy) * 3600
        t = theil_sen(fx, fy) * 3600
        drift = t * (fx[-1] - fx[0]) / 3600
        tau = kendall(fy)
        h = len(fx) // 2
        t2 = theil_sen(fx[h:], fy[h:]) * 3600 if len(fx) - h >= 4 else t
        half_drift = t2 * (fx[-1] - fx[h]) / 3600 if len(fx) - h >= 4 else drift
        budget = BUDGET.get(key)
        verdict = "info"
        bt = "-"
        if budget:
            bt = f"{budget[1]}{budget[0]}"
            if drift > budget[1] and tau > 0.3 and half_drift > budget[1] / 4:
                verdict = "FAIL: growth"
                fail.append(f"{key}: Theil-Sen drift {drift:.0f}{budget[0]} over {win / 60:.0f} min (tau {tau:.2f}) > {budget[1]}")
            else:
                verdict = "ok"
        cap = CAPS.get(key)
        if cap is not None:
            bt = (bt + " / " if bt != "-" else "") + f"cap {cap}"
            if max(ys_all) > cap:
                verdict = "FAIL: cap"
                fail.append(f"{key}: max {max(ys_all):.0f} exceeds cap {cap}")
            elif verdict == "info":
                verdict = "ok"
        L(f"| {key} | {ys_all[0]:.0f} | {min(ys_all):.0f} | {statistics.median(ys_all):.0f} | {max(ys_all):.0f} | {ys_all[-1]:.0f} | "
          f"{o:.1f} | {t:.1f} | {drift:.1f} | {t2:.1f} | {tau:.2f} | {bt} | {verdict} |")
    L("")
    # rates: wake-ups (voluntary + involuntary context switches) and CPU
    rate_t, wake, cpu = [], [], []
    for a, b in zip(run, run[1:]):
        dt = num(b["t_s"]) - num(a["t_s"])
        if dt <= 0:
            continue
        rate_t.append(num(b["t_s"]))
        wake.append((num(b["ctx_vol"]) - num(a["ctx_vol"])) / dt)
        cpu.append((num(b["cpu_ticks"]) - num(a["cpu_ticks"])) / dt)  # ticks/s = percent of one core at 100 Hz
    rx = [t for t in rate_t if t >= warm]
    wy = [w for t, w in zip(rate_t, wake) if t >= warm]
    cy = [c for t, c in zip(rate_t, cpu) if t >= warm]
    if len(rx) > 4:
        for name, ys, unit in (("wake-ups (voluntary ctx switches)", wy, "/s"), ("CPU", cy, "% of one core")):
            t = theil_sen(rx, ys) * 3600
            tau = kendall(ys)
            bad = t * (rx[-1] - rx[0]) / 3600 > max(5.0, 0.5 * statistics.median(ys)) and tau > 0.3
            if bad:
                fail.append(f"{name}: rate drifts up {t:.2f}{unit}/h (tau {tau:.2f})")
            L(f"- {name} during the run: median {statistics.median(ys):.1f}{unit}, p95 {sorted(ys)[int(0.95 * (len(ys) - 1))]:.1f}{unit}, "
              f"Theil-Sen {t:+.2f}{unit} per hour, tau {tau:.2f} -> {'FAIL: rising' if bad else 'ok'}")
    # idle phase
    if len(idle) >= 2:
        i0, i1 = idle[0], idle[-1]
        dt = num(i1["t_s"]) - num(i0["t_s"])
        wk = (num(i1["ctx_vol"]) - num(i0["ctx_vol"])) / dt if dt > 0 else 0
        cp = (num(i1["cpu_ticks"]) - num(i0["cpu_ticks"])) / dt if dt > 0 else 0
        base_fd, base_th = num(run[0]["fds"]), num(run[0]["threads"])
        end_fd, end_th = num(i1["fds"]), num(i1["threads"])
        L(f"- idle phase ({dt:.0f} s, driver stopped, phone linked): {wk:.1f} wake-ups/s, {cp:.2f}% CPU, "
          f"RSS {num(i1['rss_kib']):.0f} KiB, fds {end_fd:.0f} (first sample {base_fd:.0f}), threads {end_th:.0f} (first sample {base_th:.0f}), "
          f"links {i1['links']}, paired {i1['paired']}")
        if end_fd > base_fd + 8:
            fail.append(f"fds after quiesce {end_fd:.0f} vs {base_fd:.0f} at start (leak)")
        if end_th > base_th + 4:
            fail.append(f"threads after quiesce {end_th:.0f} vs {base_th:.0f} at start (leak)")
        left = num(i1["paired"]) - 1
        if left > num(run[-1]["pair_fail"]):
            fail.append(f"paired devices at the end: {i1['paired']} (expected 1 plus at most one per failed pairing)")
        elif left > 0:
            anomalies.append(f"{left:.0f} ephemeral pairing left behind, matching {run[-1]['pair_fail']} failed pairing attempt(s)")
    last = run[-1]
    L("")
    L("Driver counters at the end: " + ", ".join(f"{k}={last[k]}" for k in
      ("reconnect_ok", "reconnect_fail", "pairs", "pair_fail", "unpairs", "files_up", "files_down", "file_fail",
       "pkts_sent", "pkts_recv", "polls", "poll_fail", "bytes_moved")))
    ro, rf = num(last["reconnect_ok"]), num(last["reconnect_fail"])
    pa, pf = num(last["pairs"]), num(last["pair_fail"])
    if ro + rf and rf / (ro + rf) > 0.05:
        fail.append(f"reconnect failures {rf:.0f}/{ro + rf:.0f}")
    if pa + pf and pf / (pa + pf) > 0.10:
        fail.append(f"pairing failures {pf:.0f}/{pa + pf:.0f}")
    late = 0
    if poll_errors:
        try:
            for l in open(poll_errors):
                at = float(l.split("s after start")[0])
                late += at > 600
        except (OSError, ValueError):
            pass
    if num(last["poll_fail"]) > 0:
        msg = f"control API poll failures {last['poll_fail']}/{last['polls']} ({late} after minute 10)"
        (fail if late or num(last["poll_fail"]) > 0.005 * max(1, num(last["polls"])) else anomalies).append(msg)
    L("")
    for key in ("rss_kib", "threads", "fds", "state_bytes", "log_bytes"):
        L(f"- {key}: `{spark([num(r[key]) for r in rows])}`")
    if events:
        try:
            ev = [l.rstrip("\n").split("\t") for l in open(events)]
            kinds = {}
            for e in ev:
                if len(e) > 3 and e[1] in ("reconnect", "pair"):
                    k = (e[1], e[2] or "-", e[3])
                    kinds[k] = kinds.get(k, 0) + 1
            L("")
            L("| event | fault profile | result | count |")
            L("|---|---|---|---|")
            for (a, b, c), n in sorted(kinds.items()):
                L(f"| {a} | {b} | {c} | {n} |")
        except OSError:
            pass
    L("")
    for a in anomalies:
        L(f"- anomaly: {a}")
    if anomalies:
        L("")
    if fail:
        L("**VERDICT: FAIL**")
        for f in fail:
            L(f"- {f}")
    else:
        L("**VERDICT: PASS**")
    print("\n".join(lines))
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
