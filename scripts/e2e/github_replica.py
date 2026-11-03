#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""A faithful local replica of the parts of GitHub the update checkers talk to, serving REAL release files.

    github_replica.py --port 18893 --repo fspecii/geekbridge --releases DIR [--mode-file F] [--log F] [--token T]

DIR holds one directory per release tag (`DIR/v0.1.1/…`): the real assets (installers, `SHA256SUMS`, …), an
optional `NOTES.md` (the markdown release body) and an optional `release.json`
(`{"prerelease": bool, "draft": bool, "published_at": "2026-11-02T10:00:00Z"}`). Binds 127.0.0.1 only.

Modelled on GitHub's documented behaviour (the response schema is checked by validate_release_payload.py):

  GET  /repos/{o}/{n}/releases/latest        newest published, non-draft, non-prerelease release (404 otherwise);
                                             the full schema (urls, node ids, author, assets[].digest/content_type/
                                             size/state/…), `ETag: W/"…"` + 304 on If-None-Match (a 304 is not counted
                                             against the quota), `X-RateLimit-*` headers (60/h unauthenticated),
                                             `X-GitHub-Api-Version-Selected`, `Cache-Control: public, max-age=60`
  GET  /repos/{o}/{n}/releases/assets/{id}   Accept: application/octet-stream -> 302 to the pre-signed asset host
                                             (what a token holder uses for a private repository)
  GET  /{o}/{n}/releases/download/{tag}/{f}  browser_download_url: 302 to /release-assets/…?se=…&sig=… (the
                                             release-assets.githubusercontent.com hop; the signature expires)
  GET  /release-assets/…                     200, Content-Disposition, Accept-Ranges/206, HEAD; 403 XML when the
                                             signature is bad or expired
  GET  /{o}/{n}/releases/tag/{tag}           the HTML release page

The real hosts differ (api.github.com, github.com, release-assets.githubusercontent.com); the updaters' allow-list
admits only https GitHub hosts or the single configured loopback origin, so here all three live behind one port
and are told apart by path. `--token T`: the repository is private; /releases/latest answers 404 unless
`Authorization: Bearer T` (or `token T`) is sent. A token that reaches the asset host is logged as a violation.

Switchable while running (MODE_FILE is re-read on every request; one word per line):
  only=vX.Y.Z[,vA.B.C]  only these tags exist (e.g. before a release is published)
  tag=vX.Y.Z            force /latest to answer with this tag even if it is a prerelease/draft (unfaithful; a robustness test)
  corrupt               serve every asset but SHA256SUMS with one flipped byte (same size); digests stay genuine
  truncate              advertise the full length, send half, then drop the connection
  tamper_sums           publish a wrong digest for every asset inside SHA256SUMS (and in `digest`)
  nosums                no SHA256SUMS asset in the release
  ratelimit             403 "API rate limit exceeded", X-RateLimit-Remaining: 0
  secondary             429 secondary rate limit with Retry-After: 120
  notfound              /latest answers 404 (private or no releases)
  error502              /latest answers 502
  expired               asset redirects carry an expired signature (the CDN refuses them with 403)
  evil_redirect         asset redirects point at another host:port (must be refused by the client)
  noetag                no ETag header on /latest
"""
import argparse, hashlib, hmac, json, mimetypes, os, re, secrets, sys, threading, time, urllib.parse
from datetime import datetime, timedelta, timezone
from email.utils import formatdate
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--repo", default="fspecii/geekbridge")
ap.add_argument("--releases", required=True)
ap.add_argument("--mode-file")
ap.add_argument("--log")
ap.add_argument("--token")
ARGS = ap.parse_args()
OWNER, NAME = ARGS.repo.split("/", 1)
BASE = f"http://127.0.0.1:{ARGS.port}"
SECRET = secrets.token_bytes(32)
REPO_ID = 1127420002
LOG_LOCK = threading.Lock()
RATE = {"used": 0, "reset": int(time.time()) + 3600}
RATE_LOCK = threading.Lock()


def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def modes():
    try:
        return set(open(ARGS.mode_file).read().split()) if ARGS.mode_file else set()
    except OSError:
        return set()


def mode_value(m, key):
    return next((x[len(key) + 1:] for x in m if x.startswith(key + "=")), None)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


_digests = {}


def digest(path):
    st = os.stat(path)
    key = (path, st.st_size, st.st_mtime_ns)
    if key not in _digests:
        _digests[key] = sha256_file(path)
    return _digests[key]


def author(login="github-actions[bot]", uid=41898282):
    u = f"https://api.github.com/users/{urllib.parse.quote(login)}"
    return {
        "login": login, "id": uid, "node_id": "MDM6Qm90NDE4OTgyODI=", "avatar_url": f"https://avatars.githubusercontent.com/in/15368?v=4",
        "gravatar_id": "", "url": u, "html_url": "https://github.com/apps/github-actions", "followers_url": u + "/followers",
        "following_url": u + "/following{/other_user}", "gists_url": u + "/gists{/gist_id}", "starred_url": u + "/starred{/owner}{/repo}",
        "subscriptions_url": u + "/subscriptions", "organizations_url": u + "/orgs", "repos_url": u + "/repos",
        "events_url": u + "/events{/privacy}", "received_events_url": u + "/received_events", "type": "Bot", "user_view_type": "public",
        "site_admin": False,
    }


def load_releases(m):
    """[(tag, meta, [(name, path)])] for every tag directory, newest first."""
    only = mode_value(m, "only")
    only = set(only.split(",")) if only else None
    out = []
    for tag in sorted(os.listdir(ARGS.releases)):
        d = os.path.join(ARGS.releases, tag)
        if not os.path.isdir(d) or (only and tag not in only):
            continue
        meta = {"prerelease": False, "draft": False}
        mj = os.path.join(d, "release.json")
        if os.path.exists(mj):
            meta.update(json.load(open(mj)))
        meta.setdefault("published_at", iso(os.stat(d).st_mtime))
        meta["notes"] = open(os.path.join(d, "NOTES.md")).read() if os.path.exists(os.path.join(d, "NOTES.md")) else ""
        files = sorted(f for f in os.listdir(d) if f not in ("release.json", "NOTES.md") and os.path.isfile(os.path.join(d, f)))
        if "nosums" in m:
            files = [f for f in files if f != "SHA256SUMS"]
        out.append((tag, meta, [(f, os.path.join(d, f)) for f in files]))
    out.sort(key=lambda r: (r[1]["published_at"], r[0]), reverse=True)
    return out


def release_id(tag):
    return 200000000 + int(hashlib.md5(tag.encode()).hexdigest()[:4], 16)


def release_json(tag, meta, files, m, rid):
    assets = []
    for i, (fname, path) in enumerate(files):
        aid = 400000000 + rid * 100 + i
        d = digest(path)
        if "tamper_sums" in m:
            d = "0" * 64
        ctype = {"SHA256SUMS": "text/plain"}.get(fname) or {".gz": "application/gzip", ".deb": "application/vnd.debian.binary-package",
                ".apk": "application/vnd.android.package-archive", ".zst": "application/zstd"}.get(os.path.splitext(fname)[1]) or \
                mimetypes.guess_type(fname)[0] or "application/octet-stream"
        assets.append({
            "url": f"{BASE}/repos/{OWNER}/{NAME}/releases/assets/{aid}", "id": aid, "node_id": "RA_kwDOQ" + hashlib.md5(f"{tag}/{fname}".encode()).hexdigest()[:8],
            "name": fname, "label": None, "uploader": author(), "content_type": ctype, "state": "uploaded",
            "size": os.path.getsize(path), "digest": "sha256:" + d, "download_count": 0,
            "created_at": meta["published_at"], "updated_at": meta["published_at"],
            "browser_download_url": f"{BASE}/{OWNER}/{NAME}/releases/download/{tag}/{fname}",
        })
    api = f"{BASE}/repos/{OWNER}/{NAME}"
    return {
        "url": f"{api}/releases/{rid}", "assets_url": f"{api}/releases/{rid}/assets",
        "upload_url": f"https://uploads.github.com/repos/{OWNER}/{NAME}/releases/{rid}/assets{{?name,label}}",
        "html_url": f"{BASE}/{OWNER}/{NAME}/releases/tag/{tag}", "id": rid, "author": author(), "node_id": "RE_kwDOQ" + hashlib.md5(tag.encode()).hexdigest()[:8],
        "tag_name": tag, "target_commitish": "main", "name": f"GeekBridge {tag.lstrip('v')}", "draft": bool(meta["draft"]),
        "immutable": False, "prerelease": bool(meta["prerelease"]), "created_at": meta["published_at"], "updated_at": meta["published_at"],
        "published_at": None if meta["draft"] else meta["published_at"], "assets": assets,
        "tarball_url": f"{api}/tarball/{tag}", "zipball_url": f"{api}/zipball/{tag}", "body": meta["notes"],
    }


def sums_override(files, m):  # noqa
    """With tamper_sums the SHA256SUMS content itself carries wrong digests."""
    return "".join(f"{'0' * 64}  {n}\n" for n, _ in files if n != "SHA256SUMS").encode()


def sign(path, se):
    return hmac.new(SECRET, f"{path}\n{se}".encode(), hashlib.sha256).hexdigest()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "github.com"
    sys_version = ""

    def log_message(self, fmt, *a):
        pass

    def record(self, status, **extra):
        rec = {"t": round(time.time(), 3), "method": self.command, "path": self.path.split("?")[0], "status": status,
               "ua": self.headers.get("User-Agent"), "accept": self.headers.get("Accept"), "api_version": self.headers.get("X-GitHub-Api-Version"),
               "if_none_match": self.headers.get("If-None-Match"), "auth": bool(self.headers.get("Authorization")), "range": self.headers.get("Range")}
        rec.update(extra)
        if ARGS.log:
            with LOG_LOCK, open(ARGS.log, "a") as f:
                f.write(json.dumps(rec) + "\n")

    def reply(self, status, body=b"", ctype="application/json; charset=utf-8", headers=(), **extra):
        self.send_response(status)
        if status != 304:
            self.send_header("Content-Type", ctype)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD" and status != 304:
            self.wfile.write(body)
        self.record(status, **extra)

    def api_headers(self, consume):
        with RATE_LOCK:
            if time.time() > RATE["reset"]:
                RATE.update(used=0, reset=int(time.time()) + 3600)
            if consume:
                RATE["used"] += 1
            used = RATE["used"]; reset = RATE["reset"]
        return [("X-GitHub-Media-Type", "github.v3; format=json"), ("X-GitHub-Api-Version-Selected", "2022-11-28"),
                ("X-RateLimit-Limit", "60"), ("X-RateLimit-Remaining", str(max(0, 60 - used))), ("X-RateLimit-Reset", str(reset)),
                ("X-RateLimit-Used", str(used)), ("X-RateLimit-Resource", "core"), ("X-GitHub-Request-Id", secrets.token_hex(8).upper()),
                ("Access-Control-Allow-Origin", "*"), ("X-Content-Type-Options", "nosniff")]

    def json_error(self, status, message, extra=(), doc="https://docs.github.com/rest/releases/releases#get-the-latest-release"):
        body = json.dumps({"message": message, "documentation_url": doc, "status": str(status)}).encode()
        self.reply(status, body, headers=self.api_headers(False) + list(extra))

    def authorized(self):
        if not ARGS.token:
            return True
        a = self.headers.get("Authorization", "")
        return a.split(" ", 1)[-1] == ARGS.token if " " in a else False

    do_HEAD = lambda self: self.do_GET()

    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        p = u.path
        m = modes()
        rx = re.fullmatch(r"/repos/([^/]+)/([^/]+)/releases/latest", p)
        if rx:
            return self.latest(rx, m)
        rx = re.fullmatch(r"/repos/([^/]+)/([^/]+)/releases", p)
        if rx:
            return self.release_list(rx, m)
        rx = re.fullmatch(r"/repos/([^/]+)/([^/]+)/releases/assets/(\d+)", p)
        if rx:
            return self.api_asset(rx, m)
        rx = re.fullmatch(rf"/([^/]+)/([^/]+)/releases/download/([^/]+)/([^/]+)", p)
        if rx:
            return self.download_redirect(rx, m)
        if p.startswith("/release-assets/"):
            return self.asset_host(u, m)
        if re.fullmatch(rf"/{OWNER}/{NAME}/releases/tag/[^/]+", p, re.I):
            return self.reply(200, b"<html><body>GeekBridge release page</body></html>", "text/html; charset=utf-8")
        self.reply(404, b"Not Found", "text/plain; charset=utf-8")

    # ---- api.github.com
    def latest(self, rx, m):
        if (rx.group(1).lower(), rx.group(2).lower()) != (OWNER.lower(), NAME.lower()):
            return self.json_error(404, "Not Found")
        if "ratelimit" in m:
            ip = "203.0.113.7"
            body = json.dumps({"message": f"API rate limit exceeded for {ip}. (But here's the good news: Authenticated requests get a higher rate limit. Check out the documentation for more details.)",
                               "documentation_url": "https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting", "status": "403"}).encode()
            h = self.api_headers(False)
            h = [(k, ("0" if k == "X-RateLimit-Remaining" else "60" if k == "X-RateLimit-Used" else v)) for k, v in h]
            return self.reply(403, body, headers=h)
        if "secondary" in m:
            return self.json_error(429, "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
                                   [("Retry-After", "120")], "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api#about-secondary-rate-limits")
        if "error502" in m:
            return self.reply(502, b"<html>Bad Gateway</html>", "text/html")
        if "notfound" in m or not self.authorized():
            return self.json_error(404, "Not Found")
        rels = load_releases(m)
        forced = mode_value(m, "tag")
        pick = next((r for r in rels if r[0] == forced), None) if forced else next((r for r in rels if not r[1]["draft"] and not r[1]["prerelease"]), None)
        if pick is None:
            return self.json_error(404, "Not Found")
        tag, meta, files = pick
        body = json.dumps(release_json(tag, meta, files, m, release_id(tag)), indent=1).encode()
        etag = 'W/"' + hashlib.md5(body).hexdigest() + '"'
        inm = self.headers.get("If-None-Match")
        base_h = [("Cache-Control", "public, max-age=60, s-maxage=60"), ("Vary", "Accept, Accept-Encoding, Accept, X-Requested-With"),
                  ("Last-Modified", formatdate(os.stat(os.path.join(ARGS.releases, tag)).st_mtime, usegmt=True))]
        if "noetag" not in m:
            base_h.append(("ETag", etag))
        if inm and "noetag" not in m and inm.removeprefix("W/") == etag.removeprefix("W/"):
            return self.reply(304, b"", headers=self.api_headers(False) + base_h, tag=tag)
        self.reply(200, body, "application/json; charset=utf-8", headers=self.api_headers(True) + base_h, tag=tag)

    def release_list(self, rx, m):
        """GET /repos/o/n/releases: every published release (pre-releases included, drafts only for the owner,
        so never here), newest first. The updater asks for it when /latest is 404 to tell 'only pre-releases'
        from 'private repository'."""
        if (rx.group(1).lower(), rx.group(2).lower()) != (OWNER.lower(), NAME.lower()):
            return self.json_error(404, "Not Found")
        for mode, status, extra in (("ratelimit", 403, []), ("secondary", 429, [("Retry-After", "120")])):
            if mode in m:
                return self.json_error(status, "API rate limit exceeded", extra)
        if "error502" in m:
            return self.reply(502, b"<html>Bad Gateway</html>", "text/html")
        if "notfound" in m or not self.authorized():
            return self.json_error(404, "Not Found")
        rels = [r for r in load_releases(m) if not r[1]["draft"]]
        body = json.dumps([release_json(t, mt, f, m, release_id(t)) for t, mt, f in rels], indent=1).encode()
        self.reply(200, body, "application/json; charset=utf-8", headers=self.api_headers(True))

    def api_asset(self, rx, m):
        if not self.authorized():
            return self.json_error(404, "Not Found")
        aid = int(rx.group(3))
        for tag, meta, files in load_releases(m):
            rid = release_id(tag)
            for i, (fname, _) in enumerate(files):
                if 400000000 + rid * 100 + i == aid:
                    if "octet-stream" in (self.headers.get("Accept") or ""):
                        return self.redirect_to_host(tag, fname, m, api=True)
                    return self.json_error(404, "Not Found")
        self.json_error(404, "Not Found")

    # ---- github.com
    def download_redirect(self, rx, m):
        if (rx.group(1).lower(), rx.group(2).lower()) != (OWNER.lower(), NAME.lower()) or ARGS.token:
            return self.reply(404, b"Not Found", "text/plain; charset=utf-8")
        tag, fname = urllib.parse.unquote(rx.group(3)), urllib.parse.unquote(rx.group(4))
        self.redirect_to_host(tag, fname, m)

    def redirect_to_host(self, tag, fname, m, api=False):
        rel = next((r for r in load_releases(m) if r[0] == tag), None)
        if not rel or fname not in dict(rel[2]):
            return self.reply(404, b"Not Found", "text/plain; charset=utf-8")
        if "evil_redirect" in m:
            return self.reply(302, b"", "text/html", [("Location", f"http://127.0.0.1:{ARGS.port + 1}/evil/{fname}")], tag=tag, asset=fname)
        se = int(time.time()) + (-60 if "expired" in m else 300)
        path = f"/release-assets/github-production-release-asset/{REPO_ID}/{tag}/{secrets.token_hex(8)}-{fname}"
        q = urllib.parse.urlencode({"sp": "r", "sv": "2018-11-09", "sr": "b", "spr": "https", "se": iso(se), "se_unix": se, "rscd": f"attachment; filename={fname}",
                                    "rsct": "application/octet-stream", "skoid": "96c2d410-5711-43a1-aedd-ab1947aa7ab0", "sig": sign(path, se), "tag": tag, "f": fname})
        h = [("Location", f"{BASE}{path}?{q}"), ("Cache-Control", "no-cache")]
        self.reply(302, b"", "text/html; charset=utf-8", h, tag=tag, asset=fname, via="api" if api else "browser")

    # ---- release-assets.githubusercontent.com
    def asset_host(self, u, m):
        q = dict(urllib.parse.parse_qsl(u.query))
        if self.headers.get("Authorization"):
            self.record(0, violation="Authorization reached the asset host")
        try:
            se = int(q["se_unix"])
        except (KeyError, ValueError):
            se = 0
        if not hmac.compare_digest(q.get("sig", ""), sign(u.path, se)) or time.time() > se:
            body = (b'<?xml version="1.0" encoding="utf-8"?><Error><Code>AuthenticationFailed</Code><Message>Server failed to authenticate the request. '
                    b'Make sure the value of Authorization header is formed correctly including the signature.</Message></Error>')
            return self.reply(403, body, "application/xml", [("x-ms-error-code", "AuthenticationFailed")])
        rel = next((r for r in load_releases(m) if r[0] == q.get("tag")), None)
        fname = q.get("f", "")
        path = dict(rel[2]).get(fname) if rel else None
        if not path:
            return self.reply(404, b"", "application/xml")
        size = os.path.getsize(path)
        lo, hi, status = 0, size - 1, 200
        rng = self.headers.get("Range")
        if rng and (mm := re.fullmatch(r"bytes=(\d*)-(\d*)", rng.strip())):
            a, b = mm.groups()
            lo = int(a) if a else max(0, size - int(b or 0))
            hi = min(size - 1, int(b)) if a and b else size - 1
            status = 206
        send = hi - lo + 1
        if fname == "SHA256SUMS" and "tamper_sums" in m:
            data = sums_override(rel[2], m)
            return self.reply(200, data, "application/octet-stream", [("Content-Disposition", f"attachment; filename={fname}")])
        st = os.stat(path)
        h = [("Content-Disposition", f"attachment; filename={fname}; filename*=UTF-8''{fname}"), ("Accept-Ranges", "bytes"),
             ("ETag", '"0x' + hashlib.md5(f"{path}{st.st_mtime_ns}".encode()).hexdigest()[:15].upper() + '"'),
             ("Last-Modified", formatdate(st.st_mtime, usegmt=True)), ("x-ms-version", "2018-11-09"), ("x-ms-blob-type", "BlockBlob"),
             ("Cache-Control", "max-age=2592000"), ("X-GitHub-Request-Id", secrets.token_hex(8).upper())]
        if status == 206:
            h.append(("Content-Range", f"bytes {lo}-{hi}/{size}"))
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        for k, v in h:
            self.send_header(k, v)
        self.send_header("Content-Length", str(send))
        self.end_headers()
        sent = 0
        if self.command != "HEAD":
            flip = size // 2 if "corrupt" in m and fname != "SHA256SUMS" else None  # the digests stay genuine
            limit = send // 2 if "truncate" in m else send
            try:
                with open(path, "rb") as f:
                    f.seek(lo)
                    while sent < limit:
                        chunk = f.read(min(1 << 20, limit - sent))
                        if not chunk:
                            break
                        if flip is not None and lo + sent <= flip < lo + sent + len(chunk):
                            b = bytearray(chunk); b[flip - lo - sent] ^= 0xFF; chunk = bytes(b)
                        self.wfile.write(chunk)
                        sent += len(chunk)
            except (BrokenPipeError, ConnectionResetError):
                pass
            if sent < send:
                self.close_connection = True
        self.record(status, asset=fname, bytes=sent, of=send)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 32


if __name__ == "__main__":
    Server(("127.0.0.1", ARGS.port), H).serve_forever()
