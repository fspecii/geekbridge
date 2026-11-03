#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Software bill of materials (CycloneDX 1.5 JSON) for GeekBridge: the Rust workspace (cargo metadata --locked plus the
Cargo.lock checksums) and the Android build dependencies (android/gradle/verification-metadata.xml).

Deterministic: components sorted by purl, keys sorted, no host paths, the timestamp is SOURCE_DATE_EPOCH (default: the
committer time of HEAD), the serial number is a UUIDv5 of the content. Run through scripts/gen-sbom.sh."""
import argparse, hashlib, json, os, subprocess, sys, tomllib, uuid
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from urllib.parse import quote

# Android artefacts that end up in the APK (releaseRuntimeClasspath, docs/supply-chain.md). Everything else in the
# verification metadata is a build tool or a test dependency and is marked scope "excluded".
ANDROID_RUNTIME = {("org.jetbrains.kotlin", "kotlin-stdlib"), ("org.jetbrains", "annotations")}
NAMESPACE = uuid.UUID("5a1b0c2e-6f0e-4d57-9a53-7e1d2b9c4f10")


def run(cmd, cwd):
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True).stdout


def spdx_expression(lic):
    if not lic:
        return None
    return " OR ".join(p.strip() for p in lic.replace("/", " OR ").split(" OR ") if p.strip())


def rust_components(root):
    meta = json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1"], root))
    with open(os.path.join(root, "Cargo.lock"), "rb") as f:
        lock = tomllib.load(f)
    sums = {(p["name"], p["version"], p.get("source")): p.get("checksum") for p in lock["package"]}
    members = set(meta["workspace_members"])
    comps, ref_of = [], {}
    for p in meta["packages"]:
        ref = f"pkg:cargo/{quote(p['name'])}@{p['version']}"
        assert ref not in ref_of.values(), f"duplicate package {ref}"
        ref_of[p["id"]] = ref
        c = {"type": "library", "bom-ref": ref, "name": p["name"], "version": p["version"], "purl": ref,
             "scope": "required"}
        if p.get("description"):
            c["description"] = " ".join(p["description"].split())
        expr = spdx_expression(p.get("license"))
        if expr:
            c["licenses"] = [{"expression": expr}]
        if p["id"] in members:
            c["properties"] = [{"name": "geekbridge:origin", "value": "workspace"}]
        else:
            h = sums.get((p["name"], p["version"], p.get("source")))
            if h:
                c["hashes"] = [{"alg": "SHA-256", "content": h}]
            if p.get("source") and p["source"].startswith("registry+"):
                c["externalReferences"] = [{"type": "distribution", "url":
                    f"https://crates.io/api/v1/crates/{p['name']}/{p['version']}/download"}]
            c["properties"] = [{"name": "geekbridge:origin", "value": "crates.io" if p.get("source", "").startswith(
                "registry+https://github.com/rust-lang/crates.io-index") else (p.get("source") or "unknown")}]
        comps.append(c)
    deps = []
    for n in meta["resolve"]["nodes"]:
        deps.append({"ref": ref_of[n["id"]], "dependsOn": sorted({ref_of[d] for d in n["dependencies"]})})
    ws_version = next(p["version"] for p in meta["packages"] if p["id"] in members)
    return comps, deps, ws_version


def android_components(root):
    path = os.path.join(root, "android", "gradle", "verification-metadata.xml")
    tree = ET.parse(path)
    ns = {"g": "https://schema.gradle.org/dependency-verification"}
    comps = []
    for e in tree.getroot().findall("g:components/g:component", ns):
        group, name, version = e.get("group"), e.get("name"), e.get("version")
        arts = {a.get("name"): a.find("g:sha256", ns).get("value") for a in e.findall("g:artifact", ns)
                if a.find("g:sha256", ns) is not None}
        # Prefer the binary (jar, aar, klib, module) over the pom; names sorted, so the choice is stable.
        pick = next((n for n in sorted(arts) if not n.endswith((".pom", ".module"))), None) or \
            next((n for n in sorted(arts) if n.endswith(".pom")), None)
        ref = f"pkg:maven/{quote(group)}/{quote(name)}@{version}"
        c = {"type": "library", "bom-ref": ref, "group": group, "name": name, "version": version, "purl": ref,
             "scope": "required" if (group, name) in ANDROID_RUNTIME else "excluded",
             "properties": [{"name": "geekbridge:origin", "value": "gradle-verification-metadata"},
                            {"name": "geekbridge:role", "value":
                                "android-runtime" if (group, name) in ANDROID_RUNTIME else "android-build-or-test"}]}
        if pick:
            c["hashes"] = [{"alg": "SHA-256", "content": arts[pick]}]
            c["properties"].append({"name": "geekbridge:hashed-artifact", "value": pick})
        comps.append(c)
    return comps


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    ap.add_argument("--out", help="output file (default: stdout)")
    a = ap.parse_args()
    root = os.path.abspath(a.root)
    epoch = os.environ.get("SOURCE_DATE_EPOCH") or run(["git", "log", "-1", "--format=%ct", "HEAD"], root).strip()
    if not epoch.isdigit():
        sys.exit("SOURCE_DATE_EPOCH must be an integer")
    stamp = datetime.fromtimestamp(int(epoch), timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    rust, deps, version = rust_components(root)
    android = android_components(root)
    comps = sorted(rust + android, key=lambda c: c["purl"])
    assert len({c["bom-ref"] for c in comps}) == len(comps), "duplicate bom-ref"
    app = {"type": "application", "bom-ref": "geekbridge", "name": "geekbridge", "version": version,
           "licenses": [{"license": {"id": "GPL-3.0-or-later"}}],
           "purl": f"pkg:github/fspecii/geekbridge@{version}"}
    deps.append({"ref": "geekbridge", "dependsOn": sorted(c["bom-ref"] for c in comps
                 if any(p["value"] == "workspace" for p in c.get("properties", [])) or c["scope"] == "required"
                 and c["purl"].startswith("pkg:maven/"))})
    doc = {"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
           "metadata": {"timestamp": stamp, "component": app,
                        "tools": {"components": [{"type": "application", "name": "scripts/gen-sbom.py"}]},
                        "properties": [{"name": "geekbridge:sources", "value":
                                        "cargo metadata --locked + Cargo.lock; android/gradle/verification-metadata.xml"}]},
           "components": comps, "dependencies": sorted(deps, key=lambda d: d["ref"])}
    body = json.dumps(doc, sort_keys=True, indent=1, ensure_ascii=True) + "\n"
    doc["serialNumber"] = "urn:uuid:" + str(uuid.uuid5(NAMESPACE, hashlib.sha256(body.encode()).hexdigest()))
    body = json.dumps(doc, sort_keys=True, indent=1, ensure_ascii=True) + "\n"
    if a.out:
        os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
        with open(a.out, "w", encoding="utf-8", newline="\n") as f:
            f.write(body)
        print(f"wrote {a.out}: {len(rust)} Rust and {len(android)} Android components", file=sys.stderr)
    else:
        sys.stdout.write(body)


if __name__ == "__main__":
    main()
