#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Helpers for scripts/e2e/coexist.sh (run as `python3 -I coexist_net.py MODE ...`).

  sniff OUTFILE            bind UDP 1716 with SO_REUSEADDR and log "<src> <deviceId> <tcpPort>" per identity
  udp-squat                bind UDP 1716 WITHOUT reuse (a rude program) and sleep
  tcp-squat FIRST LAST     listen on every TCP port FIRST..LAST and sleep
"""
import json, socket, sys, time


def sniff(out):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("0.0.0.0", 1716))
    with open(out, "a", buffering=1) as f:
        while True:
            data, src = s.recvfrom(65535)
            try:
                body = json.loads(data.split(b"\n")[0])["body"]
                f.write(f"{src[0]} {body.get('deviceId')} {body.get('tcpPort')}\n")
            except Exception:
                f.write(f"{src[0]} ? ?\n")


def udp_squat():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 1716))
    print("squatting", flush=True)
    time.sleep(3600)


def tcp_squat(first, last):
    held = []
    for port in range(first, last + 1):
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("0.0.0.0", port))
        s.listen(1)
        held.append(s)
    print("squatting", flush=True)
    time.sleep(3600)


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "sniff":
        sniff(sys.argv[2])
    elif mode == "udp-squat":
        udp_squat()
    elif mode == "tcp-squat":
        tcp_squat(int(sys.argv[2]), int(sys.argv[3]))
