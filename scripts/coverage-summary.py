#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Per-crate and per-file line coverage from an LCOV file, counting production code only.

Inline `#[cfg(test)] mod ...` blocks (the last block of a source file by convention) are excluded: a test's own
lines are always "covered" and would otherwise inflate the numbers. Files under tests/, benches/ and examples/ are
already left out by cargo-llvm-cov; `test_support.rs` and `*_tests.rs` are skipped here. Usage: coverage-summary.py LCOV [--files N] [--crate NAME]
"""
import collections
import os
import re
import sys

root = os.path.realpath(os.path.join(os.path.dirname(__file__), '..')) + '/'
args = sys.argv[1:]
if not args:
    sys.exit(__doc__)
lcov = args[0]
top = int(args[args.index('--files') + 1]) if '--files' in args else 0
only = args[args.index('--crate') + 1] if '--crate' in args else None

files = {}
cur = None
for line in open(lcov):
    line = line.strip()
    if line.startswith('SF:'):
        cur = line[3:].replace(root, '')
        files[cur] = {}
    elif line.startswith('DA:') and cur is not None:
        n, hits = line[3:].split(',')[:2]
        files[cur][int(n)] = int(hits)


def first_test_line(path):
    try:
        src = open(root + path).read().split('\n')
    except OSError:
        return 10**9
    for i, s in enumerate(src):
        if s.strip() == '#[cfg(test)]' and i + 1 < len(src) and re.match(r'\s*(pub )?mod \w+', src[i + 1]):
            return i + 1
    return 10**9


per = collections.defaultdict(lambda: [0, 0])
rows = []
for path, lines in files.items():
    if not path.startswith('crates/'):
        continue
    if re.search(r'(^|/)(test_support|[a-z0-9_]+_tests)\.rs$', path):
        continue  # test-only files: always "covered", never production code
    cut = first_test_line(path)
    prod = {n: h for n, h in lines.items() if n < cut}
    covered = sum(1 for h in prod.values() if h > 0)
    crate = path.split('/')[1]
    per[crate][0] += covered
    per[crate][1] += len(prod)
    if only is None or only == crate:
        rows.append((len(prod) - covered, path, len(prod), covered))

print(f"{'crate':22} {'lines':>7} {'covered':>8} {'%':>6}")
tc = tn = 0
for crate, (c, n) in sorted(per.items()):
    print(f"{crate:22} {n:7} {c:8} {100 * c / max(n, 1):6.1f}")
    tc += c
    tn += n
print(f"{'TOTAL':22} {tn:7} {tc:8} {100 * tc / max(tn, 1):6.1f}")
if top:
    print(f"\nlargest uncovered production line counts{' in ' + only if only else ''}:")
    for unc, path, n, c in sorted(rows, reverse=True)[:top]:
        print(f"  {path:62} {n:6} lines {100 * c / max(n, 1):5.1f}%  uncovered {unc}")
