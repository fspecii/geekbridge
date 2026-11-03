#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Redaction filter for scripts/realdevice-report.sh (stdin -> stdout).

Defence in depth: the report script never asks for message, notification, contact or clipboard
content in the first place; this filter removes whatever still slips through (a debug phone log, a
service log line, a JSON value) before anything is written to disk.

    report_redact.py [--needle-file FILE]...   FILE: secrets to remove literally (e.g. the control token;
                                               its content is read, never printed)

A line that starts with "HASH " is a file hash the report itself printed and is passed through.
Masks: private-key blocks, literal needles, pairing links, values of content/secret JSON keys,
secret-looking key=value text, certificate fingerprints (shortened), long hex/base64 runs, e-mail
addresses, phone numbers, MAC addresses, the last two IPv4 octets, the home-directory user name.
"""
import getpass
import os
import re
import sys

CONTENT_KEYS = (
    "body text title content message clipboard preview subject sender number phone phone_number "
    "contact display_name name device_name ticker big_text sub_text summary snippet address_line "
    "token secret password passphrase authorization private_key pairing_code otp api_key"
).split()
KEY_RE = re.compile(
    r'("(?:%s)"\s*:\s*)("(?:[^"\\]|\\.)*"|\[[^\]]*\])' % "|".join(CONTENT_KEYS), re.I
)
SECRET_KV = re.compile(
    r"\b(token|secret|password|passphrase|authorization|api[_-]?key|private[_-]?key|bearer)\b(\s*[=:]\s*|\s+)(\S+)",
    re.I,
)
# Free text: `body=...`, `sms body "..."`, `clipboard: ...`, `contact Name`, `answers as "Name"`, the rest of a
# `notification:` / `last:` line. Over-redacting a log line is acceptable; leaking message content is not.
FREE_KV = re.compile(
    r"\b(body|text|title|content|message|clipboard|notification|preview|subject|sender|caller|contact|"
    r"last|name|device_name|ticker)s?\b(\s*[=:]\s*|\s+)(\"[^\"]*\"|'[^']*'|\S.*)$",
    re.I,
)
QUOTED_AFTER = re.compile(r"\b(as|named|called|from|to)\s+\"[^\"]*\"", re.I)
PEM = re.compile(r"-----BEGIN [A-Z ]*(?:PRIVATE KEY|CERTIFICATE)-----.*?-----END [A-Z ]*-----", re.S)
PAIR_LINK = re.compile(r"geekbridge://pair\?[^\s\"']*")
CERT_FP = re.compile(r'(cert[a-z_]*sha256"?\s*[:=]\s*"?)([0-9A-Fa-f]{64})')
HEX_LONG = re.compile(r"(?<![0-9A-Za-z])[0-9A-Fa-f]{32,}(?![0-9A-Za-z])")
B64_LONG = re.compile(r"(?<![A-Za-z0-9+/=_-])[A-Za-z0-9+/=_-]{40,}(?![A-Za-z0-9+/=_-])")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
MAC = re.compile(r"(?<![0-9A-Fa-f:])(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}(?![0-9A-Fa-f:])")
IPV4 = re.compile(r"(?<![\d.])(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}(?![\d.])")
PLUS_NUM = re.compile(r"\+\d[\d ()./-]{5,}\d")
SPACED_NUM = re.compile(r"(?<![\d.:/-])\(?\d{2,4}\)?[ .-]\d{3,4}[ .-]\d{3,4}(?:[ .-]\d{2,4})?(?![\d.:/-])")
LONG_DIGITS = re.compile(r"(?<![\w.])\d{7,15}(?![\w.])")
JSON_NUM_LINE = re.compile(r'^\s*"[^"]+"\s*:\s*-?\d+,?\s*$')


def FREE_GUARD(line):
    """Free-text redaction is for log/plain-text lines; JSON lines are handled by KEY_RE and keep
    structure (reason codes, timestamps) intact."""
    t = line.lstrip()
    return not (t.startswith(("{", "}", '"', "[", "]")) or t.startswith("$ "))


def hexish(s):
    return bool(re.search(r"[0-9]", s)) and bool(re.search(r"[a-z]", s)) and bool(re.search(r"[A-Z]", s))


def short_fp(m):
    return m.group(1) + m.group(2)[:12] + "..."


def redact_line(line, user):
    if user:
        line = line.replace("/home/" + user, "/home/USER").replace("/Users/" + user, "/Users/USER")
    if line.startswith("HASH "):
        return line
    line = KEY_RE.sub(lambda m: m.group(1) + '"<redacted>"', line)
    if not line.lstrip().startswith(("HASH ", "[ ok ]", "[FAIL]", "[warn]")) or "answers as" in line:
        line = QUOTED_AFTER.sub(lambda m: m.group(1) + ' "<redacted>"', line)
    line = FREE_KV.sub(lambda m: m.group(1) + m.group(2) + "<redacted>", line) if FREE_GUARD(line) else line
    line = PAIR_LINK.sub("geekbridge://pair?<redacted>", line)
    line = CERT_FP.sub(short_fp, line)
    line = SECRET_KV.sub(lambda m: m.group(1) + m.group(2) + "<redacted>", line)
    line = HEX_LONG.sub("<hex-redacted>", line)
    line = B64_LONG.sub(lambda m: "<secret-redacted>" if hexish(m.group(0)) else m.group(0), line)
    line = EMAIL.sub("<email>", line)
    line = MAC.sub("xx:xx:xx:xx:xx:xx", line)

    def ip(m):
        a, b = m.group(1), m.group(2)
        if a == "127" or (a == "0" and b == "0"):
            return m.group(0)
        return "%s.%s.x.x" % (a, b)

    line = IPV4.sub(ip, line)
    line = PLUS_NUM.sub("<number>", line)
    line = SPACED_NUM.sub("<number>", line)
    if not JSON_NUM_LINE.match(line):
        line = LONG_DIGITS.sub(
            lambda m: m.group(0)
            if re.search(r'(_ms|_s|_at|unix|seq|time|since|bytes|size|pid|id)"?\s*[:=]\s*$', line[: m.start()], re.I)
            else "<number>",
            line,
        )
    return line


def main(argv):
    needles = []
    i = 0
    while i < len(argv):
        if argv[i] == "--needle-file" and i + 1 < len(argv):
            try:
                with open(argv[i + 1], "r", errors="replace") as f:
                    v = f.read().strip()
                if len(v) >= 6:
                    needles.append(v)
            except OSError:
                pass
            i += 2
        else:
            sys.stderr.write("report_redact.py: unknown argument %s\n" % argv[i])
            return 2
    try:
        user = getpass.getuser()
    except Exception:
        user = os.environ.get("USER", "")
    if len(user) < 3:
        user = ""
    text = sys.stdin.read()
    text = PEM.sub("<key-or-certificate-block-removed>", text)
    for n in needles:
        text = text.replace(n, "<secret-redacted>")
    out = [redact_line(l, user) for l in text.split("\n")]
    sys.stdout.write("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
