#!/usr/bin/env bash
# Structural checks for the macOS packaging templates. Runs on any host (no Apple tools needed):
# parses each plist with Python's plistlib and asserts the keys and purpose strings the helper needs.
# This proves the templates are well-formed and complete; it does NOT prove macOS accepts them.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -I - "$here" <<'PY'
import plistlib, sys, pathlib
d = pathlib.Path(sys.argv[1])
fail = []
def load(name):
    with open(d / name, "rb") as f:
        return plistlib.load(f)

info = load("Info.plist.template")
for k in ["CFBundleExecutable", "CFBundleIdentifier", "CFBundlePackageType", "LSMinimumSystemVersion",
          "NSBluetoothAlwaysUsageDescription", "NSMicrophoneUsageDescription"]:
    if not info.get(k):
        fail.append(f"Info.plist.template: missing or empty {k}")
for k in ["NSBluetoothAlwaysUsageDescription", "NSMicrophoneUsageDescription"]:
    v = info.get(k, "")
    if len(v) < 40 or "phone" not in v.lower():
        fail.append(f"Info.plist.template: {k} is not a real purpose string")
for banned in ["AI ", "agent", "API key", "iPhone", "Continuity"]:
    for k, v in info.items():
        if isinstance(v, str) and banned.lower() in v.lower():
            fail.append(f"Info.plist.template: '{banned}' must not appear ({k})")
for ph in ["@BUNDLE_ID@", "@VERSION@", "@BUILD@"]:
    if ph not in (d / "Info.plist.template").read_text():
        fail.append(f"Info.plist.template: placeholder {ph} missing")

dev = load("entitlements.developer-id.plist")
if dev.get("com.apple.security.device.audio-input") is not True:
    fail.append("developer-id entitlements: audio-input missing")
if "com.apple.security.app-sandbox" in dev:
    fail.append("developer-id entitlements: must not enable the sandbox")
sbx = load("entitlements.sandboxed.plist")
for k in ["com.apple.security.app-sandbox", "com.apple.security.device.bluetooth",
          "com.apple.security.device.audio-input"]:
    if sbx.get(k) is not True:
        fail.append(f"sandboxed entitlements: {k} missing")
for name, ent in (("developer-id", dev), ("sandboxed", sbx)):
    for k in ent:
        if k in ("com.apple.security.get-task-allow", "com.apple.security.cs.disable-library-validation"):
            fail.append(f"{name} entitlements: {k} would weaken notarization/hardening")
if fail:
    print("\n".join(fail)); sys.exit(1)
print("packaging templates OK (structure only; not validated by macOS)")
PY
