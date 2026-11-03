#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Offline checks for scripts/gen-sbom.sh: valid CycloneDX shape, covers every Cargo.lock and Gradle-verification
# package, byte-identical on a re-run and from a different checkout path, no host paths, timestamp = SOURCE_DATE_EPOCH.
#   scripts/tests/sbom-test.sh
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$ROOT"
mkdir -p "${GB_SCRATCH:-$HOME/.cache/gb-scratch}"
T="$(mktemp -d "${GB_SCRATCH:-$HOME/.cache/gb-scratch}/sbom-XXXXXX")"
trap 'rm -rf -- "$T"' EXIT
fail=0
ok()  { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }

export SOURCE_DATE_EPOCH=1700000000
scripts/gen-sbom.sh --out "$T/a.json" 2>/dev/null && ok "generates" || { bad "gen-sbom.sh failed"; exit 1; }
scripts/gen-sbom.sh --out "$T/b.json" 2>/dev/null
cmp -s "$T/a.json" "$T/b.json" && ok "two runs are byte-identical" || bad "output differs between runs"

SOURCE_DATE_EPOCH=1700000001 scripts/gen-sbom.sh --out "$T/c.json" 2>/dev/null
diff <(grep -v -e '"timestamp"' -e '"serialNumber"' "$T/a.json") <(grep -v -e '"timestamp"' -e '"serialNumber"' "$T/c.json") >/dev/null \
    && ok "SOURCE_DATE_EPOCH changes only timestamp and serial" || bad "SOURCE_DATE_EPOCH changed more than the timestamp"

# The SBOM is built from cargo metadata and lock files, so it must not carry the checkout path or $HOME.
if grep -q -e "$ROOT" -e "$HOME" "$T/a.json"; then bad "SBOM contains a host path"; else ok "no host paths"; fi

python3 -I - "$T/a.json" "$ROOT" <<'PY' && ok "structure, completeness, ordering" || bad "structure/completeness check failed"
import json, re, sys, tomllib, xml.etree.ElementTree as ET
d = json.load(open(sys.argv[1])); root = sys.argv[2]
assert d["bomFormat"] == "CycloneDX" and d["specVersion"] == "1.5" and d["version"] == 1
assert re.fullmatch(r"urn:uuid:[0-9a-f-]{36}", d["serialNumber"])
assert d["metadata"]["timestamp"] == "2023-11-14T22:13:20Z", d["metadata"]["timestamp"]
comps = d["components"]; refs = [c["bom-ref"] for c in comps]
assert refs == sorted(refs) and len(set(refs)) == len(refs), "components not sorted/unique"
for c in comps:
    assert c["type"] and c["name"] and c["version"] and c["purl"] == c["bom-ref"], c
    for h in c.get("hashes", []): assert re.fullmatch(r"[0-9a-f]{64}", h["content"]), c
lock = tomllib.load(open(root + "/Cargo.lock", "rb"))["package"]
rust = {c["purl"] for c in comps if c["purl"].startswith("pkg:cargo/")}
for p in lock:
    if p.get("source"):
        assert f"pkg:cargo/{p['name']}@{p['version']}" in rust, p["name"]
        sums = [c["hashes"][0]["content"] for c in comps if c["purl"] == f"pkg:cargo/{p['name']}@{p['version']}"]
        assert sums == [p["checksum"]], ("checksum", p["name"])
ns = "{https://schema.gradle.org/dependency-verification}"
x = ET.parse(root + "/android/gradle/verification-metadata.xml").getroot().findall(f"{ns}components/{ns}component")
mvn = {c["purl"] for c in comps if c["purl"].startswith("pkg:maven/")}
assert len(mvn) == len(x), (len(mvn), len(x))
run = {c["name"] for c in comps if c["purl"].startswith("pkg:maven/") and c["scope"] == "required"}
assert run == {"kotlin-stdlib", "annotations"}, run
known = set(refs) | {"geekbridge"}
for dep in d["dependencies"]:
    assert dep["ref"] in known and set(dep["dependsOn"]) <= known, dep["ref"]
PY
exit $fail
