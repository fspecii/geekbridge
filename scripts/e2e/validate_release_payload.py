#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check a JSON document against the vendored GitHub 'Get the latest release' response schema.

    validate_release_payload.py PAYLOAD.json|-            validate a file (or stdin)
    validate_release_payload.py --url http://127.0.0.1:PORT/repos/o/n/releases/latest

Dependency-free validator for the subset of OpenAPI 3.0 the schema uses (type, nullable, required,
properties, items, enum, format date-time/uri). Exit 0 and "OK" when valid, 1 and the list of violations otherwise.
Property names the schema does not list are fine (GitHub adds fields); missing required ones are not.
"""
import json, os, re, sys, urllib.request

SCHEMA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "github-latest-release.schema.json")
DATE = re.compile(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$")

def check(v, s, path, errs):
    if v is None:
        if not s.get("nullable"):
            errs.append(f"{path}: null not allowed")
        return
    t = s.get("type")
    ok = {"string": isinstance(v, str), "integer": isinstance(v, int) and not isinstance(v, bool),
          "number": isinstance(v, (int, float)) and not isinstance(v, bool), "boolean": isinstance(v, bool),
          "object": isinstance(v, dict), "array": isinstance(v, list), None: True}.get(t, True)
    if not ok:
        errs.append(f"{path}: expected {t}, got {type(v).__name__}")
        return
    if "enum" in s and v not in s["enum"]:
        errs.append(f"{path}: {v!r} not in {s['enum']}")
    if t == "string":
        f = s.get("format")
        if f == "date-time" and not DATE.match(v): errs.append(f"{path}: {v!r} is not an RFC 3339 date-time")
        if f in ("uri", "uri-template") and not re.match(r"^[a-z][a-z0-9+.-]*://", v): errs.append(f"{path}: {v!r} is not a URI")
    if t == "object":
        for r in s.get("required", []):
            if r not in v: errs.append(f"{path}.{r}: required property missing")
        for k, sub in s.get("properties", {}).items():
            if k in v: check(v[k], sub, f"{path}.{k}", errs)
    if t == "array" and "items" in s:
        for i, x in enumerate(v): check(x, s["items"], f"{path}[{i}]", errs)

def validate(doc):
    errs = []
    check(doc, json.load(open(SCHEMA))["schema"], "$", errs)
    return errs

if __name__ == "__main__":
    a = sys.argv[1:]
    if len(a) == 2 and a[0] == "--url":
        raw = urllib.request.urlopen(urllib.request.Request(a[1], headers={"Accept": "application/vnd.github+json"})).read()
    elif len(a) == 1:
        raw = sys.stdin.buffer.read() if a[0] == "-" else open(a[0], "rb").read()
    else:
        sys.exit(__doc__)
    errs = validate(json.loads(raw))
    print("OK: payload conforms to the GitHub schema" if not errs else "\n".join(errs))
    sys.exit(1 if errs else 0)
