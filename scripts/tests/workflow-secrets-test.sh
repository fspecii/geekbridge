#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Policy checks on the workflows that hold signing secrets (docs/reports/security-review-7.md S7-5, S7-6):
#   * no workflow uses pull_request_target, and none checks out a pull request head with secrets in reach;
#   * dev-release.yml: the job that signs runs only from main, and no step that runs Gradle (build scripts and plugins
#     from the repository) has the dev keystore secrets in its environment or writes the keystore to disk.
# Run: scripts/tests/workflow-secrets-test.sh
set -Eeuo pipefail
src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
exec python3 -I - "$src" <<'PY'
import sys, glob, os, re
import yaml
src = sys.argv[1]
fail = 0
def bad(msg):
    global fail
    print("FAIL", msg); fail = 1
def ok(msg):
    print("ok  ", msg)

for path in sorted(glob.glob(os.path.join(src, ".github/workflows/*.yml"))):
    text = open(path).read()
    name = os.path.basename(path)
    if "pull_request_target" in text:
        bad(f"{name} uses pull_request_target")
    else:
        ok(f"{name}: no pull_request_target")

wf = yaml.safe_load(open(os.path.join(src, ".github/workflows/dev-release.yml")))
job = wf["jobs"]["android"]
if "github.ref == 'refs/heads/main'" in str(job.get("if", "")):
    ok("dev-release android job runs from main only")
else:
    bad("dev-release android job has no main-only guard")
if job.get("environment") == "dev-signing":
    ok("dev-release android job names the dev-signing environment")
else:
    bad("dev-release android job does not name environment: dev-signing")
if "concurrency" in wf:
    bad("dev-release has a workflow-level concurrency group (it would cancel the publish job)")
pc = wf["jobs"]["publish"].get("concurrency", {})
if isinstance(pc, dict) and pc.get("cancel-in-progress") is False:
    ok("dev-release publish job queues instead of being cancelled")
else:
    bad("dev-release publish job must have its own concurrency with cancel-in-progress: false")
for step in job["steps"]:
    run = step.get("run", "") or ""
    env = step.get("env", {}) or {}
    uses_gradle = "gradlew" in run or "gradle " in run
    has_secret = any("secrets." in str(v) and "DEV_KEY" in str(v) for v in env.values())
    if uses_gradle and (has_secret or "base64 -d" in run):
        bad(f"step '{step.get('name')}' runs Gradle with the dev keystore in reach")
    if has_secret and uses_gradle:
        bad(f"step '{step.get('name')}' has dev secrets and runs Gradle")
ok("no step runs Gradle with the dev keystore in reach") if not fail else None
sys.exit(fail)
PY
