#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Per-package line and branch coverage from the JaCoCo CSV (Android JVM unit tests). Usage: coverage-android.py CSV [--files N]"""
import collections
import csv
import sys

if len(sys.argv) < 2:
    sys.exit(__doc__)
top = int(sys.argv[sys.argv.index('--files') + 1]) if '--files' in sys.argv else 15
pkg = collections.defaultdict(lambda: [0, 0, 0, 0])
per_class = {}
for r in csv.DictReader(open(sys.argv[1])):
    # Kotlin file-level classes (FooKt) and nested classes are folded into their source file's name.
    name = r['CLASS'].split('$')[0]
    key = (r['PACKAGE'], name)
    v = per_class.setdefault(key, [0, 0, 0, 0])
    for i, col in enumerate(('LINE_MISSED', 'LINE_COVERED', 'BRANCH_MISSED', 'BRANCH_COVERED')):
        v[i] += int(r[col])
for (p, _), v in per_class.items():
    for i in range(4):
        pkg[p][i] += v[i]
print(f"{'package':32} {'lines':>7} {'covered':>8} {'%':>6}  {'branches':>8} {'%':>6}")
t = [0, 0, 0, 0]
for p, v in sorted(pkg.items()):
    lines, br = v[0] + v[1], v[2] + v[3]
    print(f"{p:32} {lines:7} {v[1]:8} {100 * v[1] / max(lines, 1):6.1f}  {br:8} {100 * v[3] / max(br, 1):6.1f}")
    t = [a + b for a, b in zip(t, v)]
lines, br = t[0] + t[1], t[2] + t[3]
print(f"{'TOTAL':32} {lines:7} {t[1]:8} {100 * t[1] / max(lines, 1):6.1f}  {br:8} {100 * t[3] / max(br, 1):6.1f}")
print("\nlargest uncovered files:")
for (p, c), v in sorted(per_class.items(), key=lambda kv: -kv[1][0])[:top]:
    n = v[0] + v[1]
    print(f"  {c:40} {n:6} lines {100 * v[1] / max(n, 1):5.1f}%  uncovered {v[0]}")
