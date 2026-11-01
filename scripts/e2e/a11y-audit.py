#!/usr/bin/env python3
"""Per-screen accessibility / layout audit from one uiautomator dump (stdlib only).

  a11y-audit.py [NAME]     audit the current screen; NAME tags the findings (e.g. home-light-font2)

Flags (one line each, `FLAG<TAB>NAME<TAB>detail`):
  SMALL    clickable node under 48dp in width or height (density from `wm density`)
  NOLABEL  clickable node with neither text nor content-desc and no labelled descendant
  CLIP     node whose bounds leave the screen on the right/left (content cut off), ignoring scroll containers
  OVERLAP  two clickable leaf nodes whose bounds overlap by more than 25 % of the smaller one
Prints OK when nothing is flagged. Uses $ADB (full adb command incl. -s serial) from the environment.
"""
import os, re, subprocess, sys, xml.etree.ElementTree as ET

ADB = os.environ["ADB"].split()
name = sys.argv[1] if len(sys.argv) > 1 else "screen"

def sh(*a):
    return subprocess.run(ADB + list(a), capture_output=True, text=True, timeout=30).stdout

dens = 420
m = re.search(r"(\d+)", sh("shell", "wm", "density"))
if m:
    dens = int(m.group(1))
size = re.search(r"(\d+)x(\d+)", sh("shell", "wm", "size"))
sw, sh_ = (int(size.group(1)), int(size.group(2))) if size else (1080, 2400)
if "land" in name:
    sw, sh_ = max(sw, sh_), min(sw, sh_)
dp48 = 48 * dens / 160.0

root = None
for _ in range(4):
    sh("shell", "uiautomator", "dump", "/sdcard/gb-a11y.xml")
    try:
        root = ET.fromstring(sh("exec-out", "cat", "/sdcard/gb-a11y.xml"))
        break
    except ET.ParseError:
        continue
if root is None:
    print(f"ERROR\t{name}\tno dump"); sys.exit(2)

def box(n):
    l, t, r, b = map(int, re.findall(r"-?\d+", n.get("bounds")))
    return l, t, r, b

def labelled(n):
    if (n.get("text") or n.get("content-desc") or "").strip():
        return True
    return any(labelled(c) for c in n)

bad = []
leaves = []
def walk(n, parent_scroll):
    if n.get("bounds") is None:
        for c in n:
            walk(c, parent_scroll)
        return
    scroll = parent_scroll or n.get("scrollable") == "true"
    l, t, r, b = box(n)
    # Switches that only draw (row is the control) report checkable but not clickable: not a target.
    act = n.get("clickable") == "true"
    # skip system chrome
    if n.get("package") not in ("dev.geekbridge.android",):
        return
    if act and n.get("enabled") == "true" and r > l and b > t and (r - l) < sw and (b - t) < sh_:
        w, h = r - l, b - t
        lab = (n.get("text") or n.get("content-desc") or "").strip()[:40]
        if (w < dp48 - 1 or h < dp48 - 1) and t > 0 and b < sh_ - 1:   # skip nodes cut by the screen edge
            bad.append(("SMALL", f"{w/dens*160:.0f}x{h/dens*160:.0f}dp '{lab}' {n.get('bounds')}"))
        if not labelled(n):
            bad.append(("NOLABEL", f"{n.get('class')} {n.get('resource-id')} {n.get('bounds')}"))
        if not any(True for _ in n):
            leaves.append((l, t, r, b, lab))
    if (r > sw + 2 or l < -2) and n.get("class") not in ("android.widget.ScrollView", "android.widget.HorizontalScrollView") and (r - l) > 0:
        bad.append(("CLIP", f"{n.get('class')} '{(n.get('text') or n.get('content-desc') or '')[:30]}' {n.get('bounds')}"))
    for c in n:
        walk(c, scroll)
walk(root, False)
for i, a in enumerate(leaves):
    for b in leaves[i + 1:]:
        ix = max(0, min(a[2], b[2]) - max(a[0], b[0])); iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
        small = min((a[2] - a[0]) * (a[3] - a[1]), (b[2] - b[0]) * (b[3] - b[1]))
        if small and ix * iy > 0.25 * small:
            bad.append(("OVERLAP", f"'{a[4]}' x '{b[4]}'"))
if not bad:
    print(f"OK\t{name}")
for f, d in bad:
    print(f"{f}\t{name}\t{d}")
