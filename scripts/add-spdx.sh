#!/usr/bin/env bash
# Add `SPDX-License-Identifier: GPL-3.0-or-later` headers to first-party source files. Idempotent.
#
#   scripts/add-spdx.sh            add the header where missing
#   scripts/add-spdx.sh --check    list files without it and exit 1 (CI / pre-flight)
#
# Covers *.rs *.kt *.kts *.gradle *.sh *.py *.ps1 in git-tracked and untracked-but-not-ignored files.
# Skips generated or third-party material: target/, build/, third_party/, fuzz/corpus/, tests/fixtures/,
# THIRD_PARTY_NOTICES.md, lock files. A shebang line stays first; the header follows it.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
mode=write
case "${1:-}" in "") ;; --check) mode=check ;; *) echo "usage: $0 [--check]" >&2; exit 2 ;; esac
git ls-files -co --exclude-standard -z | python3 -I -c '
import os, re, sys
mode = sys.argv[1]
ID = "SPDX-License-Identifier: GPL-3.0-or-later"
EXT = {".rs": "//", ".kt": "//", ".kts": "//", ".gradle": "//", ".sh": "#", ".py": "#", ".ps1": "#"}
SKIP = re.compile(r"(^|/)(target|build|third_party|\.gradle|node_modules)/|^fuzz/corpus/|/tests?/fixtures/|/resources/")
missing = []
for path in sys.stdin.buffer.read().decode().split("\0"):
    ext = os.path.splitext(path)[1]
    if not path or ext not in EXT or SKIP.search(path) or not os.path.isfile(path):
        continue
    data = open(path, encoding="utf-8", errors="surrogateescape").read()
    if ID in "\n".join(data.split("\n", 6)[:6]):
        continue
    missing.append(path)
    if mode == "write":
        c = EXT[ext]
        lines = data.split("\n", 1)
        if data.startswith("#!"):
            rest = lines[1] if len(lines) > 1 else ""
            data = lines[0] + "\n" + f"{c} {ID}\n" + rest
        else:
            data = f"{c} {ID}\n" + data
        open(path, "w", encoding="utf-8", errors="surrogateescape").write(data)
if mode == "check":
    for p in missing: print("missing SPDX header:", p)
    sys.exit(1 if missing else 0)
print(f"added header to {len(missing)} file(s)")
' "$mode"
