#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Tiny uiautomator driver for the e2e script (stdlib only).

  adbui.py dump                      print visible texts / descriptions / ids, one per line
  adbui.py tap TEXT [--contains]     tap the first node whose text/content-desc equals (or contains) TEXT
  adbui.py tapto TEXT [--contains] [--expect TEXT2] [--tries N] [--verify SECONDS]
                                     tap TEXT once it is really tappable (enabled, clickable, its window focused, not
                                     moving), then check the screen changed (TEXT gone or TEXT2 visible); else retry
  adbui.py has TEXT                  exit 0 if some visible node's text contains TEXT
  adbui.py wait TEXT [SECONDS]       wait until TEXT is visible (default 20 s), exit 1 on timeout
  adbui.py scrolltap TEXT            scroll down up to 12 times until TEXT is visible, then tap it
  adbui.py scrollto TEXT             scroll down until a node with exactly TEXT is visible
  adbui.py type TEXT                 type into the focused field
  adbui.py fields A B C              fill the last three EditTexts on screen (clearing them first)
  adbui.py code                      print the first 8-char upper-case hex token on screen (pairing code)
Uses $ADB (full adb command incl. -s serial) from the environment.
"""
import os, re, subprocess, sys, time, xml.etree.ElementTree as ET

ADB = os.environ["ADB"].split()

def sh(*a, check=True):
    return subprocess.run(ADB + list(a), capture_output=True, text=True, check=check, timeout=int(os.environ.get("ADBUI_TIMEOUT", "25"))).stdout

def root():
    """The current UI hierarchy, or None. A failed dump never falls back to the previous dump's file (that would be
    a stale screen), and an adb hiccup is a failed dump, not a crash."""
    for _ in range(4):
        try:
            sh("shell", "rm -f /sdcard/gb-ui.xml; uiautomator dump /sdcard/gb-ui.xml", check=False)
            x = sh("exec-out", "cat", "/sdcard/gb-ui.xml", check=False)
            return ET.fromstring(x)
        except (ET.ParseError, subprocess.SubprocessError):
            time.sleep(1)
    return None

def nodes():
    r = root()
    return [] if r is None else list(r.iter("node"))

def label(n):
    return (n.get("text") or n.get("content-desc") or "").strip()

def center(n):
    l, t, r, b = map(int, re.findall(r"\d+", n.get("bounds")))
    return (l + r) // 2, (t + b) // 2

def find(text, contains):
    for n in nodes():
        s = label(n)
        if s and ((text in s) if contains else (s == text)) and n.get("enabled") == "true":
            return n
    return None

def tap(text, contains=False):
    n = find(text, contains)
    if n is None:
        return False
    x, y = center(n)
    sh("shell", "input", "tap", str(x), str(y))
    return True

def focused_package():
    """Package of the window that has input focus right now ('' when none: an animation, a dialog being added)."""
    try:
        out = sh("shell", "dumpsys window | grep -E 'mCurrentFocus'", check=False)
    except subprocess.SubprocessError:
        return ""
    m = re.search(r"mCurrentFocus=Window\{\S+ \S+ ([\w.]+)[/ }]", out)
    return m.group(1) if m else ""

def candidates(text, contains):
    """(node, tappable, why) for every node labelled TEXT; tappable = enabled and itself or an ancestor clickable."""
    r = root()
    if r is None:
        return []
    parent = {c: p for p in r.iter() for c in p}
    out = []
    for n in r.iter("node"):
        s = label(n)
        if not s or not ((text in s) if contains else (s == text)):
            continue
        chain, a = [n], n
        while a in parent:
            a = parent[a]; chain.append(a)
        clickable = any(c.get("clickable") == "true" for c in chain)
        enabled = n.get("enabled") == "true"
        out.append((n, clickable and enabled, "clickable=%s enabled=%s" % (clickable, enabled)))
    return out

def tapto(text, contains, expect, tries, verify):
    """Tap TEXT robustly. Returns True when the screen moved on (TEXT gone, or EXPECT visible)."""
    log = lambda m: print("tapto[%s]: %s" % (text, m), file=sys.stderr, flush=True)
    for attempt in range(1, tries + 1):
        # 1. wait for a tappable node in the focused window that holds still for two dumps in a row
        end = time.time() + 30
        last, node, logged = None, None, False
        while time.time() < end:
            c = candidates(text, contains)
            ok = [x for x in c if x[1]]
            pkg, foc = (ok[0][0].get("package") if ok else None), focused_package()
            if ok and foc and foc == pkg:
                b = ok[0][0].get("bounds")
                if b == last:
                    node = ok[0][0]; break
                last = b
            else:
                last = None
                if time.time() > end - 25 and not logged:
                    logged = True
                    log("not tappable yet: %s, focus=%r" % (c[0][2] if c else "not on screen", foc))
            time.sleep(0.3)
        if node is None:
            log("never became tappable"); return False
        x, y = center(node)
        sh("shell", "input", "tap", str(x), str(y))
        # 2. the tap counts when the screen changed: the node is gone/moved, or the expected text shows up
        end = time.time() + verify
        while time.time() < end:
            time.sleep(0.5)
            if expect and find(expect, True) is not None:
                return True
            c = [n for n, _, _ in candidates(text, contains) if n.get("bounds") == node.get("bounds") and label(n) == label(node)]
            if not c:
                return True
        log("attempt %d: the screen did not change after the tap at %d,%d (focus=%r); trying again" % (attempt, x, y, focused_package()))
    return False

cmd = sys.argv[1]
if cmd == "tapto":
    a = sys.argv[2:]
    opt = lambda k, d: a[a.index(k) + 1] if k in a else d
    sys.exit(0 if tapto(a[0], "--contains" in a, opt("--expect", None), int(opt("--tries", 3)), float(opt("--verify", 8))) else 1)
if cmd == "dump":
    for n in nodes():
        s = label(n)
        if s:
            print(f"{s}\t{n.get('resource-id')}\t{n.get('bounds')}")
elif cmd == "tap":
    sys.exit(0 if tap(sys.argv[2], "--contains" in sys.argv) else 1)
elif cmd == "has":
    sys.exit(0 if find(sys.argv[2], True) is not None else 1)
elif cmd == "wait":
    end = time.time() + float(sys.argv[3] if len(sys.argv) > 3 else 20)
    while time.time() < end:
        if find(sys.argv[2], True) is not None:
            sys.exit(0)
        time.sleep(1)
    sys.exit(1)
elif cmd == "scrolltap":
    for _ in range(12):
        if tap(sys.argv[2], "--exact" not in sys.argv):
            sys.exit(0)
        sh("shell", "input", "swipe", "540", "1800", "540", "700", "300")
        time.sleep(0.6)
    sys.exit(1)
elif cmd == "fields":
    eds = [n for n in nodes() if n.get("class") == "android.widget.EditText"][-3:]
    for n, val in zip(eds, sys.argv[2:5]):
        x, y = center(n)
        sh("shell", "input", "tap", str(x), str(y))
        sh("shell", "input", "keyevent", "KEYCODE_MOVE_END")
        for _ in range(3):
            sh("shell", "input", "keyevent", *(["KEYCODE_DEL"] * 20))
        sh("shell", "input", "text", val)
    sh("shell", "input", "keyevent", "KEYCODE_BACK")
elif cmd == "code":
    for n in nodes():
        if re.fullmatch(r"[0-9A-F]{8}", label(n)):
            print(label(n)); sys.exit(0)
    sys.exit(1)
elif cmd == "scrollto":
    # scrollto A [B]: from the top of the page, scroll down until A (or B) is visible; prints the one found.
    want = sys.argv[2:4]
    for _ in range(3):
        sh("shell", "input", "swipe", "540", "700", "540", "2000", "150")
    for _ in range(16):
        for w in want:
            if find(w, False) is not None:
                print(w); sys.exit(0)
        sh("shell", "input", "swipe", "540", "1800", "540", "900", "300")
        time.sleep(0.4)
    sys.exit(1)
elif cmd == "taprow":
    # taprow TEXT X: tap at horizontal position X on the row of the node labelled TEXT (for unlabeled switches)
    n = find(sys.argv[2], False)
    if n is None:
        sys.exit(1)
    sh("shell", "input", "tap", sys.argv[3], str(center(n)[1]))
elif cmd == "type":
    sh("shell", "input", "text", sys.argv[2].replace(" ", "%s"))
