#!/usr/bin/env python3
"""Tiny uiautomator driver for the e2e script (stdlib only).

  adbui.py dump                      print visible texts / descriptions / ids, one per line
  adbui.py tap TEXT [--contains]     tap the first node whose text/content-desc equals (or contains) TEXT
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
    return subprocess.run(ADB + list(a), capture_output=True, text=True, check=check, timeout=25).stdout

def nodes():
    for _ in range(4):
        sh("shell", "uiautomator", "dump", "/sdcard/gb-ui.xml", check=False)
        x = sh("exec-out", "cat", "/sdcard/gb-ui.xml", check=False)
        try:
            return list(ET.fromstring(x).iter("node"))
        except ET.ParseError:
            time.sleep(1)
    return []

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

cmd = sys.argv[1]
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
