#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Turn the captures of scripts/interop-kdeconnect.sh into sanitised test fixtures.

    python3 -I scripts/interop-kdeconnect-export.py OUTDIR FIXTURE_DIR

Only packet lines, the two PUBLIC certificates and the verification keys the daemon displayed are
written. Private keys are never read. The one substitution is the machine's hostname (the daemon
announces it as `deviceName`); device ids are random per run and carry no information.
"""
import json, os, re, sys

out, dest = sys.argv[1], sys.argv[2]
os.makedirs(dest, exist_ok=True)


def lines(tag):
    with open(f"{out}/captures/{tag}.jsonl") as f:
        return [json.loads(l) for l in f]


def sanitise(line):
    return line.replace('"deviceName":"%s"' % os.uname().nodename, '"deviceName":"interop-daemon"')


manifest = []


def put(name, rec, origin, note):
    line = sanitise(rec["line"])
    assert "PRIVATE" not in line
    with open(f"{dest}/{name}.json", "w", newline="") as f:
        f.write(line + "\n")
    manifest.append({"file": f"{name}.json", "origin": origin, "type": json.loads(line)["type"], "note": note})


def pick(recs, direction, kind, nth=0, pred=lambda j: True):
    hits = [r for r in recs if r["dir"] == direction and json.loads(r["line"])["type"] == kind and pred(json.loads(r["line"]))]
    return hits[nth]


s1, s2, s3 = lines("s1"), lines("s2"), lines("s3")
D, G = "daemon", "geekbridge"
# S1: GeekBridge dialled (daemon = TLS client)
put("daemon-identity-secure", pick(s1, "in", "kdeconnect.identity"), D, "identity sent inside TLS by the stock desktop (protocol 8)")
put("daemon-pair-accept", pick(s1, "in", "kdeconnect.pair", pred=lambda j: j["body"].get("pair") is True), D, "answer to our pair request: no timestamp")
put("daemon-pair-unpair", pick(s1, "in", "kdeconnect.pair", pred=lambda j: j["body"].get("pair") is False), D, "unpair notification")
put("daemon-clipboard-connect-empty", pick(s1, "in", "kdeconnect.clipboard.connect"), D, "empty clipboard: content is JSON null, timestamp 0")
put("daemon-ping-message", pick(s1, "in", "kdeconnect.ping", pred=lambda j: "message" in j["body"]), D, "kdeconnect-cli --ping-msg")
put("daemon-ping-empty", pick(s1, "in", "kdeconnect.ping", pred=lambda j: not j["body"]), D, "kdeconnect-cli --ping")
put("daemon-clipboard", pick(s1, "in", "kdeconnect.clipboard"), D, "clipboard update")
put("daemon-share-file", pick(s1, "in", "kdeconnect.share.request", pred=lambda j: "filename" in j["body"]), D, "file offer with payloadSize and payloadTransferInfo")
put("daemon-share-text", pick(s1, "in", "kdeconnect.share.request", pred=lambda j: "text" in j["body"]), D, "shared text")
put("daemon-share-url", pick(s1, "in", "kdeconnect.share.request", pred=lambda j: "url" in j["body"]), D, "shared URL")
# S2: daemon dialled (daemon = TLS server)
ident_in = [r for r in s2 if r["dir"] == "in" and json.loads(r["line"])["type"] == "kdeconnect.identity"]
put("daemon-identity-plaintext", ident_in[0], D, "first plaintext line of a daemon-initiated connection (has targetDeviceId/targetProtocolVersion)")
put("daemon-pair-request", pick(s2, "in", "kdeconnect.pair", pred=lambda j: "timestamp" in j["body"]), D, "pair request from the stock desktop")
put("daemon-clipboard-connect", pick(s2, "in", "kdeconnect.clipboard.connect"), D, "clipboard state announced when the link came up")
# Outgoing (GeekBridge) packets the real daemon accepted
put("geekbridge-identity-plaintext", pick(s1, "out", "kdeconnect.identity", 0), G, "plaintext first line when GeekBridge dials")
put("geekbridge-identity-secure", pick(s1, "out", "kdeconnect.identity", 1), G, "identity inside TLS")
put("geekbridge-pair-request", pick(s1, "out", "kdeconnect.pair"), G, "pair request")
put("geekbridge-ping", pick(s1, "out", "kdeconnect.ping"), G, "ping")
put("geekbridge-clipboard", pick(s1, "out", "kdeconnect.clipboard"), G, "clipboard update (daemon adopted it)")
put("geekbridge-clipboard-connect", pick(s1, "out", "kdeconnect.clipboard.connect"), G, "clipboard.connect with timestamp")
put("geekbridge-share-text", pick(s1, "out", "kdeconnect.share.request", pred=lambda j: "text" in j["body"]), G, "text share")
put("geekbridge-share-url", pick(s1, "out", "kdeconnect.share.request", pred=lambda j: "url" in j["body"]), G, "URL share")
put("geekbridge-share-file", pick(s1, "out", "kdeconnect.share.request", pred=lambda j: "filename" in j["body"]), G, "file offer (daemon downloaded it byte-identically)")
for kind, short in [("geekbridge.calendar.request", "calendar-request"), ("geekbridge.calendar.write", "calendar-write"),
                    ("geekbridge.calls.request", "calls-request"), ("geekbridge.clipboard.pull", "clipboard-pull")]:
    put(f"geekbridge-ext-{short}", pick(s1, "out", kind), G, "GeekBridge extension; daemon logged 'discarding unsupported packet' and kept the link")


def cert_only(path):
    pem = open(path).read()
    m = re.search(r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----\n", pem, re.S)
    return m.group(0)


kc = f"{out}/home/.config/kdeconnect"
open(f"{dest}/daemon.crt.pem", "w").write(cert_only(f"{kc}/certificate.pem"))
open(f"{dest}/geekbridge.crt.pem", "w").write(cert_only(f"{out}/work/gb/identity.pem"))

keys = re.findall(r"daemon verificationKey=([0-9A-F]{8})", open(f"{out}/results.txt").read())
vectors = {
    "s1_geekbridge_requested": {"key_shown_by_daemon": keys[0], "request": "geekbridge-pair-request.json"},
    "s2_daemon_requested": {"key_shown_by_daemon": keys[1], "request": "daemon-pair-request.json"},
}
json.dump({"vectors": vectors, "files": manifest}, open(f"{dest}/manifest.json", "w"), indent=2)
open(f"{dest}/manifest.json", "a").write("\n")
print(len(manifest), "packet fixtures;", "keys", keys)
