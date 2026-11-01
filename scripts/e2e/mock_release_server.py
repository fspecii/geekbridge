#!/usr/bin/env python3
"""Mock GitHub release server for the Android update-check e2e.

Serves GET /repos/fspecii/geekbridge/releases/latest (a newer release), /SHA256SUMS and the APK asset
from a directory. Binds 127.0.0.1 only. Usage: mock_release_server.py PORT APK_FILE [VERSION_TAG]
"""
import hashlib, json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(sys.argv[1]); apk = sys.argv[2]; tag = sys.argv[3] if len(sys.argv) > 3 else "v99.0.0"
data = open(apk, "rb").read()
digest = hashlib.sha256(data).hexdigest()
base = f"http://127.0.0.1:{port}"
sums = f"{digest}  geekbridge-android.apk\n".encode()
release = {
    "tag_name": tag, "name": f"GeekBridge {tag}", "draft": False, "prerelease": False,
    "html_url": f"{base}/release-page", "body": "Mock release for the e2e test.\n- fake fix",
    "assets": [
        {"name": "geekbridge-android.apk", "browser_download_url": f"{base}/dl/geekbridge-android.apk", "size": len(data)},
        {"name": "SHA256SUMS", "browser_download_url": f"{base}/dl/SHA256SUMS", "size": len(sums)},
    ],
}

class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *a):
        sys.stderr.write("%s\n" % (fmt % a,))
    def send(self, body, ctype):
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        if self.path.endswith("/releases/latest"): self.send(json.dumps(release).encode(), "application/json")
        elif self.path == "/dl/SHA256SUMS": self.send(sums, "text/plain")
        elif self.path == "/dl/geekbridge-android.apk": self.send(data, "application/vnd.android.package-archive")
        elif self.path == "/release-page": self.send(b"<html>release</html>", "text/html")
        else: self.send_error(404)

HTTPServer(("127.0.0.1", port), H).serve_forever()
