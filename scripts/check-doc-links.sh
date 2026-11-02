#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Check that relative links and image paths in README.md, CHANGELOG.md, SECURITY.md, CONTRIBUTING.md, CODE_OF_CONDUCT.md, ARCHITECTURE.md, SUPPORT.md, docs/*.md, docs/reports/*.md and
# third_party/*/README.md exist and are not git-ignored (an ignored target would be a dead link in the public repo).
#   scripts/check-doc-links.sh
# External (http, mailto) links and in-page anchors to other files are not fetched; same-file anchors are not checked.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
exec python3 -I - README.md CHANGELOG.md SECURITY.md CONTRIBUTING.md CODE_OF_CONDUCT.md ARCHITECTURE.md SUPPORT.md docs/*.md docs/reports/*.md third_party/*/README.md <<'PY'
import os, re, subprocess, sys
bad = 0
link = re.compile(r'!?\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)|<img[^>]+src="([^"]+)"')
for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    text = re.sub(r"```.*?```", "", text, flags=re.S)
    text = re.sub(r"`[^`\n]*`", "", text)
    for m in link.finditer(text):
        target = m.group(1) or m.group(2)
        if re.match(r"[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
            continue
        rel = target.split("#")[0].split("?")[0]
        if not rel:
            continue
        full = os.path.normpath(os.path.join(os.path.dirname(path), rel))
        if not os.path.exists(full):
            print(f"{path}: missing {target}")
            bad += 1
        elif subprocess.run(["git", "check-ignore", "-q", full]).returncode == 0:
            print(f"{path}: {target} is git-ignored (dead link once published)")
            bad += 1
print(f"{bad} broken link(s)")
sys.exit(1 if bad else 0)
PY
