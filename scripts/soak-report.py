#!/usr/bin/env python3
"""Turn a soak run (dist/soak/<ts>/, written by scripts/soak-e2e.sh) into report.md plus PNG charts.

  python3 -I scripts/soak-report.py DIR [--publish]

--publish also writes docs/soak-report.md and docs/img/soak-*.png (image links relative to docs/).
Charts use Pillow when it is importable; without it the report falls back to ASCII sparkline tables (no
matplotlib or gnuplot needed). Stdlib only otherwise.
"""
import csv, os, shutil, statistics, sys

try:
    from PIL import Image, ImageDraw, ImageFont
except Exception:  # pragma: no cover - fallback path
    Image = None

BARS = "▁▂▃▄▅▆▇█"
WARMUP_FRAC = 0.10   # leave the first 10 % (min 5 min) out of the growth fit: caches, JIT and thread pools settle


def num(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def load_samples(d):
    rows = []
    with open(os.path.join(d, "samples.csv")) as f:
        for r in csv.DictReader(f):
            rows.append(r)
    return rows


def series(rows, key, scale=1.0):
    out = []
    for r in rows:
        v = num(r.get(key))
        t = num(r.get("elapsed_s"))
        if v is not None and t is not None:
            out.append((t / 60.0, v * scale))
    return out


def cpu_series(rows):
    out, prev = [], None
    for r in rows:
        t, c = num(r.get("elapsed_s")), num(r.get("desk_cpu_ticks"))
        if t is None or c is None:
            continue
        if prev and t > prev[0] and c >= prev[1]:
            out.append((t / 60.0, 100.0 * (c - prev[1]) / 100.0 / (t - prev[0])))   # USER_HZ is 100
        prev = (t, c)
    return out


def fit(points):
    """Least-squares slope per hour, from (minutes, value)."""
    n = len(points)
    if n < 3:
        return None
    xs, ys = [p[0] for p in points], [p[1] for p in points]
    mx, my = sum(xs) / n, sum(ys) / n
    den = sum((x - mx) ** 2 for x in xs)
    return None if den == 0 else 60.0 * sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den


def analyse(points, abs_floor):
    if not points:
        return None
    t_end = points[-1][0]
    start = max(WARMUP_FRAC * t_end, min(5.0, t_end / 2))
    body = [p for p in points if p[0] >= start] or points
    vals = [p[1] for p in body]
    q = max(1, len(vals) // 4)
    early, late = statistics.median(vals[:q]), statistics.median(vals[-q:])
    slope = fit(body)
    hours = (body[-1][0] - body[0][0]) / 60.0 if len(body) > 1 else 0
    growth = late - early
    rel = growth / early if early else 0
    leak = growth > abs_floor and rel > 0.05 and (slope or 0) > 0
    return dict(first=points[0][1], last=points[-1][1], lo=min(vals), hi=max(vals), early=early, late=late, growth=growth,
                slope=slope, hours=hours, leak=leak, minutes=t_end)


def fmt(v, unit="", digits=1):
    return "-" if v is None else (f"{v:.{digits}f}{unit}")


def pct(vals, p):
    if not vals:
        return None
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(p * (len(s) - 1))))]


def spark(points, width=60):
    if not points:
        return ""
    vals = [p[1] for p in points]
    step = max(1, len(vals) // width)
    cols = [statistics.mean(vals[i:i + step]) for i in range(0, len(vals), step)]
    lo, hi = min(cols), max(cols)
    return "".join(BARS[0 if hi == lo else int((c - lo) / (hi - lo) * (len(BARS) - 1))] for c in cols)


# ---------------------------------------------------------------- charts
def font(size):
    try:
        return ImageFont.load_default(size)
    except Exception:
        return ImageFont.load_default()


COLORS = [(31, 119, 180), (214, 39, 40), (44, 160, 44), (255, 127, 14), (148, 103, 189)]


def chart(path, title, panels, marks=()):
    """panels: [(ylabel, [(label, points)])], one stacked panel each, shared x axis in minutes; marks: [(minute, label)]"""
    W, ph, left, right, top, bottom = 1100, 190, 82, 20, 44, 36
    H = top + ph * len(panels) + bottom + 24 * 0
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    f, fs = font(14), font(11)
    d.text((left, 12), title, fill="black", font=font(17))
    xmax = max((p[0] for _, ss in panels for _, pts in ss for p in pts), default=1) or 1
    for i, (ylabel, ss) in enumerate(panels):
        y0 = top + i * ph
        y1 = y0 + ph - 26
        allv = [p[1] for _, pts in ss for p in pts]
        lo, hi = (min(allv), max(allv)) if allv else (0, 1)
        if hi == lo:
            hi, lo = hi + 1, lo - 1 if lo > 0 else lo
        pad = (hi - lo) * 0.08
        lo, hi = max(0, lo - pad) if min(allv or [0]) >= 0 else lo - pad, hi + pad
        d.rectangle([left, y0, W - right, y1], outline=(180, 180, 180))
        for k in range(5):
            yy = y1 - (y1 - y0) * k / 4
            d.line([left, yy, W - right, yy], fill=(235, 235, 235))
            d.text((4, yy - 6), f"{lo + (hi - lo) * k / 4:.4g}", fill=(80, 80, 80), font=fs)
        for k in range(0, int(xmax) + 1, max(1, int(xmax // 8) or 1)):
            xx = left + (W - left - right) * k / xmax
            d.line([xx, y0, xx, y1], fill=(240, 240, 240))
            if i == len(panels) - 1:
                d.text((xx - 8, y1 + 4), f"{k}", fill=(80, 80, 80), font=fs)
        for m, _label in marks:
            xx = left + (W - left - right) * m / xmax
            d.line([xx, y0, xx, y1], fill=(255, 200, 200))
        d.text((left + 4, y0 + 2), ylabel, fill="black", font=f)
        for j, (label, pts) in enumerate(ss):
            c = COLORS[j % len(COLORS)]
            xy = [(left + (W - left - right) * x / xmax, y1 - (y1 - y0) * (v - lo) / (hi - lo)) for x, v in pts]
            if len(xy) > 1:
                d.line(xy, fill=c, width=2)
            elif xy:
                d.ellipse([xy[0][0] - 2, xy[0][1] - 2, xy[0][0] + 2, xy[0][1] + 2], fill=c)
            d.text((W - right - 190, y0 + 2 + 13 * j), "— " + label, fill=c, font=fs)
    d.text((left, H - 18), "minutes since the loop started   (pink lines: failures)", fill=(80, 80, 80), font=fs)
    img.save(path)


def scatter(path, title, groups, marks=()):
    W, H, left, right, top, bottom = 1100, 360, 82, 20, 44, 36
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    f, fs = font(14), font(11)
    d.text((left, 12), title, fill="black", font=font(17))
    allp = [p for _, pts in groups for p in pts]
    if not allp:
        img.save(path)
        return
    xmax = max(p[0] for p in allp) or 1
    ymax = max(p[1] for p in allp) or 1
    y0, y1 = top, H - bottom
    d.rectangle([left, y0, W - right, y1], outline=(180, 180, 180))
    for k in range(5):
        yy = y1 - (y1 - y0) * k / 4
        d.line([left, yy, W - right, yy], fill=(235, 235, 235))
        d.text((4, yy - 6), f"{ymax * k / 4:.4g} s", fill=(80, 80, 80), font=fs)
    for k in range(0, int(xmax) + 1, max(1, int(xmax // 8) or 1)):
        xx = left + (W - left - right) * k / xmax
        d.text((xx - 8, y1 + 4), f"{k}", fill=(80, 80, 80), font=fs)
    for j, (label, pts) in enumerate(groups):
        c = COLORS[j % len(COLORS)]
        for x, y in pts:
            xx, yy = left + (W - left - right) * x / xmax, y1 - (y1 - y0) * y / ymax
            d.ellipse([xx - 3, yy - 3, xx + 3, yy + 3], fill=c)
        d.text((W - right - 200, y0 + 2 + 13 * j), "● " + label, fill=c, font=fs)
    d.text((left, H - 18), "minutes since the loop started; y = latency in seconds", fill=(80, 80, 80), font=fs)
    img.save(path)


# ---------------------------------------------------------------- report
def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    publish = "--publish" in sys.argv
    if not args:
        sys.exit(__doc__)
    d = args[0].rstrip("/")
    rows = load_samples(d)
    if len(rows) < 2:
        sys.exit("soak-report: fewer than two samples in %s" % d)
    events = []
    p = os.path.join(d, "events.tsv")
    if os.path.exists(p):
        with open(p) as f:
            events = list(csv.DictReader(f, delimiter="\t"))
    fails = open(os.path.join(d, "failures.log")).read() if os.path.exists(os.path.join(d, "failures.log")) else ""
    build = open(os.path.join(d, "build-info.txt")).read() if os.path.exists(os.path.join(d, "build-info.txt")) else ""

    minutes = (num(rows[-1]["elapsed_s"]) or 0) / 60.0
    metrics = [
        ("Desktop RSS", "MiB", series(rows, "desk_rss_kb", 1 / 1024), 2.0),
        ("Desktop threads", "", series(rows, "desk_threads"), 2),
        ("Desktop open fds", "", series(rows, "desk_fds"), 3),
        ("Desktop CPU", "%", cpu_series(rows), 1e9),
        ("Phone PSS (total)", "MiB", series(rows, "ph_pss_kb", 1 / 1024), 5.0),
        ("Phone Java heap", "MiB", series(rows, "ph_java_kb", 1 / 1024), 3.0),
        ("Phone native heap", "MiB", series(rows, "ph_native_kb", 1 / 1024), 3.0),
        ("Phone threads", "", series(rows, "ph_threads"), 3),
        ("Phone open fds", "", series(rows, "ph_fds"), 5),
        ("Phone sockets", "", series(rows, "ph_sockets"), 3),
        ("Phone wake locks held", "", series(rows, "ph_wakelocks"), 1e9),
        ("Phone alarms", "", series(rows, "ph_alarms"), 1e9),
    ]
    fail_min = []
    for e in events:
        if e["ok"] == "0":
            fail_min.append(((num(e["elapsed_s"]) or 0) / 60.0, e["kind"]))

    out = []
    w = out.append
    w("# Stability soak report\n")
    w(f"Run `{os.path.basename(d)}`: loop length {minutes:.0f} min, {len(rows)} samples every ~{(num(rows[1]['elapsed_s']) - num(rows[0]['elapsed_s'])):.0f} s. "
      "Release (R8) APK on an x86_64 emulator, release desktop service, `scripts/soak-e2e.sh`. Emulator numbers are a regression "
      "baseline, not phone numbers.\n")
    if build:
        w("```\n" + build.strip() + "\n```\n")

    charts = []
    if Image:
        def pts(name):
            return next(m[2] for m in metrics if m[0] == name)
        chart(os.path.join(d, "soak-desktop.png"), "Desktop service: RSS, threads, fds, CPU",
              [("RSS MiB", [("rss", pts("Desktop RSS"))]), ("threads", [("threads", pts("Desktop threads"))]),
               ("open fds", [("fds", pts("Desktop open fds"))]), ("CPU %", [("cpu", pts("Desktop CPU"))])], fail_min)
        chart(os.path.join(d, "soak-phone-memory.png"), "Phone app: PSS, Java and native heap",
              [("MiB", [("PSS", pts("Phone PSS (total)")), ("Java", pts("Phone Java heap")), ("native", pts("Phone native heap"))])], fail_min)
        chart(os.path.join(d, "soak-phone-handles.png"), "Phone app: threads, fds, sockets, wake locks, alarms",
              [("threads", [("threads", pts("Phone threads"))]), ("fds / sockets", [("fds", pts("Phone open fds")), ("sockets", pts("Phone sockets"))]),
               ("wake locks / alarms", [("wake locks", pts("Phone wake locks held")), ("alarms", pts("Phone alarms"))])], fail_min)
        lat = {}
        for e in events:
            if e["ok"] == "1" and num(e["latency_ms"]) and e["kind"] in ("clip_d2p", "clip_p2d", "file_d2p", "file_p2d", "reconnect", "call_answer", "sms_inject", "agenda_refresh"):
                lat.setdefault(e["kind"], []).append(((num(e["elapsed_s"]) or 0) / 60.0, num(e["latency_ms"]) / 1000.0))
        scatter(os.path.join(d, "soak-latency.png"), "Operation latency over time (successful operations)", sorted(lat.items()), fail_min)
        charts = ["soak-desktop.png", "soak-phone-memory.png", "soak-phone-handles.png", "soak-latency.png"]
        w("## Charts\n")
        for c in charts:
            w(f"![{c}]({'img/' if publish else ''}{c})\n")

    w("## Resource trends (leak check)\n")
    w("Growth = median of the last quarter minus the median of the first quarter of the samples after the first "
      f"{int(WARMUP_FRAC * 100)} % (warm-up). A series is flagged **GROWTH** when it grew by more than its absolute floor "
      "and more than 5 %, with a positive least-squares slope.\n")
    w("| series | start | end | min | max | early median | late median | growth | slope /h | verdict |")
    w("|---|---|---|---|---|---|---|---|---|---|")
    leaks = []
    for name, unit, points, floor in metrics:
        a = analyse(points, floor)
        if not a:
            continue
        verdict = "**GROWTH**" if a["leak"] else "flat"
        if a["leak"]:
            leaks.append((name, a))
        w(f"| {name} | {fmt(a['first'], unit)} | {fmt(a['last'], unit)} | {fmt(a['lo'], unit)} | {fmt(a['hi'], unit)} | "
          f"{fmt(a['early'], unit)} | {fmt(a['late'], unit)} | {fmt(a['growth'], unit)} | {fmt(a['slope'], unit, 2)} | {verdict} |")
    w("")
    w("```")
    for name, unit, points, _ in metrics:
        if points:
            w(f"{name:<24} {spark(points)}")
    w("```\n")
    w("**Leak verdict:** " + ("; ".join(f"{n} +{a['growth']:.1f} over {a['minutes']:.0f} min ({fmt(a['slope'], '', 2)}/h)" for n, a in leaks)
                           if leaks else "no series grew past its floor in the measured window.") + "\n")

    # budgets from docs/performance.md and docs/android-perf.md (idle budgets; the soak is busier than idle, so the
    # peak is informational and the late median is the comparable number)
    budgets = [("Desktop RSS", 10.0, "MiB", "idle 10 000 kB"), ("Desktop threads", 8, "", "idle budget"), ("Desktop open fds", 20, "", "idle budget"),
               ("Phone PSS (total)", 35.0, "MiB", "idle, app closed"), ("Phone threads", 32, "", "idle, app closed"),
               ("Phone wake locks held", 0, "", "idle"), ("Phone alarms", 0, "", "idle")]
    w("## Against the documented budgets\n")
    w("| series | budget | peak in the soak | late median | verdict |")
    w("|---|---|---|---|---|")
    for name, limit, unit, note in budgets:
        pts = next((m[2] for m in metrics if m[0] == name), [])
        if not pts:
            continue
        a = analyse(pts, 0)
        peak = max(v for _, v in pts)
        late = a["late"] if a else None
        ok = "within" if (late is not None and late <= limit) else "**over** (late median)"
        w(f"| {name} | {limit}{unit} ({note}) | {fmt(peak, unit)} | {fmt(late, unit)} | {ok} |")
    w("")

    # operations
    w("## Operations\n")
    kinds = {}
    for e in events:
        kinds.setdefault(e["kind"], []).append(e)
    w("| operation | runs | ok | success | p50 | p95 | max |")
    w("|---|---|---|---|---|---|---|")
    for k in sorted(kinds):
        es = kinds[k]
        ok = [e for e in es if e["ok"] == "1"]
        lats = [num(e["latency_ms"]) / 1000.0 for e in ok if num(e["latency_ms"]) is not None]
        w(f"| {k} | {len(es)} | {len(ok)} | {100.0 * len(ok) / len(es):.0f} % | {fmt(pct(lats, .5), ' s')} | {fmt(pct(lats, .95), ' s')} | {fmt(max(lats) if lats else None, ' s')} |")
    w("")
    up = [r for r in rows if r.get("link_up") in ("0", "1")]
    if up:
        w(f"**Link uptime:** {100.0 * sum(1 for r in up if r['link_up'] == '1') / len(up):.1f} % of {len(up)} samples saw a connected link "
          "(this includes the deliberate flaps, call/restart gaps and the screen-off periods).")
    dl = [num(e["latency_ms"]) / 1000.0 for e in events if e["kind"] == "dead_link_detect" and e["ok"] == "1"]
    if dl:
        w(f"\n**Dead-link detection** (black-holed link, no FIN/RST): {len(dl)} runs, p50 {pct(dl, .5):.1f} s, max {max(dl):.1f} s.")
    wl = [num(r["ph_wakelocks"]) for r in rows if num(r.get("ph_wakelocks")) is not None]
    if wl:
        w(f"\n**Wake locks:** max {int(max(wl))} held by the app at any sample; {sum(1 for v in wl if v > 0)} of {len(wl)} samples saw one.")
    last = rows[-1]
    w(f"\n**Log error counts at the end** (desktop ERROR/WARN lines: {last.get('desk_err_warn')}, phone logcat W+ lines matching error patterns: {last.get('ph_log_err')}).\n")

    w("## Failures\n")
    bad = [e for e in events if e["ok"] == "0"]
    if not bad:
        w("No operation failed.\n")
    else:
        w("| t+ min | operation | detail |")
        w("|---|---|---|")
        for e in bad:
            w(f"| {(num(e['elapsed_s']) or 0) / 60:.1f} | {e['kind']} | {e['detail']} |")
        w("")
        w("Logs recorded with each failure (`failures.log`, truncated to 12 000 characters):\n")
        w("```\n" + fails[:12000] + ("\n[...]" if len(fails) > 12000 else "") + "\n```\n")

    text = "\n".join(out) + "\n"
    with open(os.path.join(d, "report.md"), "w") as f:
        f.write(text)
    print("wrote", os.path.join(d, "report.md"))
    if publish:
        root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
        os.makedirs(os.path.join(root, "docs", "img"), exist_ok=True)
        for c in charts:
            shutil.copy(os.path.join(d, c), os.path.join(root, "docs", "img", "soak-" + c.replace("soak-", "")))
        with open(os.path.join(root, "docs", "soak-report.md"), "w") as f:
            f.write(text)
        print("published docs/soak-report.md")


if __name__ == "__main__":
    main()
