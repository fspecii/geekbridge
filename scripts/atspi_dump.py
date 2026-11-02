#!/usr/bin/env python3 -I
# SPDX-License-Identifier: GPL-3.0-or-later
"""Dump the AT-SPI accessibility tree of one running application (used by scripts/a11y-dump.sh).

    python3 -I scripts/atspi_dump.py APP_NAME_SUBSTRING [SECONDS_TO_WAIT]

Prints one line per node: indented role, quoted name, and states worth reading. Needs PyGObject
and libatspi (package `python-gobject` + `at-spi2-core`). Exit 1 when the application never shows up.
"""
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

KEEP = {
    Atspi.StateType.FOCUSED: "focused",
    Atspi.StateType.FOCUSABLE: "focusable",
    Atspi.StateType.CHECKED: "checked",
    Atspi.StateType.SELECTED: "selected",
    Atspi.StateType.EDITABLE: "editable",
    Atspi.StateType.ENABLED: None,
}


def states(node):
    ss = node.get_state_set()
    out = [label for st, label in KEEP.items() if label and ss.contains(st)]
    if not (ss.contains(Atspi.StateType.ENABLED) and ss.contains(Atspi.StateType.SENSITIVE)):
        out.append("disabled")
    return out


def dump(node, depth, out):
    try:
        role = node.get_role_name()
        name = node.get_name() or ""
        text = ""
        try:
            ti = node.get_text_iface()
            if ti is not None and not name:
                text = Atspi.Text.get_text(ti, 0, min(80, Atspi.Text.get_character_count(ti)))
        except Exception:
            pass
        shown = name or text
        line = "  " * depth + role + (f' "{shown}"' if shown else "")
        st = states(node)
        if st:
            line += " [" + ",".join(st) + "]"
        out.append(line)
        for i in range(node.get_child_count()):
            child = node.get_child_at_index(i)
            if child is not None:
                dump(child, depth + 1, out)
    except Exception as e:  # a window closing mid-walk
        out.append("  " * depth + f"<error {e}>")


def main():
    want = sys.argv[1].lower()
    wait = float(sys.argv[2]) if len(sys.argv) > 2 else 15
    Atspi.init()
    deadline = time.time() + wait
    while time.time() < deadline:
        desktop = Atspi.get_desktop(0)
        for i in range(desktop.get_child_count()):
            app = desktop.get_child_at_index(i)
            # AccessKit registers the application with an empty name on some versions, and the
            # private bus holds only the app under test, so an unnamed application counts too.
            if app is not None and want in ((app.get_name() or "") or want).lower():
                time.sleep(1.5)  # let the first full tree arrive
                out = []
                dump(app, 0, out)
                print("\n".join(out))
                return 0
        time.sleep(0.3)
    print(f"no accessible application matching {want!r}", file=sys.stderr)
    return 1


sys.exit(main())
