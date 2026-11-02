#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Mock GitHub release server for the Android update-check e2e.

Serves GET /repos/fspecii/geekbridge/releases/latest (a newer release), /SHA256SUMS and the APK asset
from a directory. Binds 127.0.0.1 only. Usage: mock_release_server.py PORT APK_FILE [VERSION_TAG]

Optional environment (used by scripts/e2e/update-install.sh):
  MOCK_EXTRA=name=path[,name=path...]  more assets (e.g. geekbridge-linux-x86_64.tar.gz), listed in SHA256SUMS
  MOCK_MODE_FILE=path                  re-read on every request; one word per line, switchable while running:
      corrupt       serve every asset with one flipped byte (same size) but publish the genuine digest
      tamper_sums   publish a wrong digest for every asset
      nosums        no SHA256SUMS asset in the release
      tag=vX.Y.Z    override the release tag
"""
import hashlib, json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(sys.argv[1]); apk = sys.argv[2]; tag = sys.argv[3] if len(sys.argv) > 3 else "v99.0.0"
data = open(apk, "rb").read()
base = f"http://127.0.0.1:{port}"
files = {"geekbridge-android.apk": data}
for item in filter(None, os.environ.get("MOCK_EXTRA", "").split(",")):
    n, _, path = item.partition("=")
    files[n] = open(path, "rb").read()
mode_file = os.environ.get("MOCK_MODE_FILE")

def modes():
    try:
        return set(open(mode_file).read().split()) if mode_file else set()
    except OSError:
        return set()

def build():
    m = modes()
    t = next((x[4:] for x in m if x.startswith("tag=")), tag)
    served = dict(files)
    if "corrupt" in m:
        for n, v in files.items():
            b = bytearray(v); b[len(b) // 2] ^= 0xFF
            served[n] = bytes(b)
    # the digests always describe the genuine files, unless tampered
    sums = "".join(f"{'0' * 64 if 'tamper_sums' in m else hashlib.sha256(v).hexdigest()}  {n}\n" for n, v in files.items()).encode()
    assets = [{"name": n, "browser_download_url": f"{base}/dl/{n}", "size": len(v)} for n, v in files.items()]
    if "nosums" not in m:
        assets.append({"name": "SHA256SUMS", "browser_download_url": f"{base}/dl/SHA256SUMS", "size": len(sums)})
    rel = {
        "tag_name": t, "name": f"GeekBridge {t}", "draft": False, "prerelease": False,
        "html_url": f"{base}/release-page", "body": "Mock release for the e2e test.\n- fake fix", "assets": assets,
    }
    return rel, sums, served

class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *a):
        sys.stderr.write("%s\n" % (fmt % a,))
    def send(self, body, ctype):
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        release, sums, served = build()
        if self.path.endswith("/releases/latest"): self.send(json.dumps(release).encode(), "application/json")
        elif self.path == "/dl/SHA256SUMS": self.send(sums, "text/plain")
        elif self.path.startswith("/dl/") and self.path[4:] in served:
            self.send(served[self.path[4:]], "application/vnd.android.package-archive" if self.path.endswith(".apk") else "application/octet-stream")
        elif self.path == "/release-page": self.send(b"<html>release</html>", "text/html")
        else: self.send_error(404)

HTTPServer(("127.0.0.1", port), H).serve_forever()
