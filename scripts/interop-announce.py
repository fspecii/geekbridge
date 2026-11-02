# SPDX-License-Identifier: GPL-3.0-or-later
import json, socket, sys, time
# announce.py DEVICEID NAME TCPPORT HOST UDPPORT [SRCADDR]  -- a UDP identity as a KDE Connect peer broadcasts it
did, name, tcp, host, port = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4], int(sys.argv[5])
caps = ["kdeconnect.ping", "kdeconnect.clipboard", "kdeconnect.clipboard.connect", "kdeconnect.share.request"]
ident = {"id": int(time.time()*1000), "type": "kdeconnect.identity", "body": {
    "deviceId": did, "deviceName": name, "protocolVersion": 8, "deviceType": "desktop",
    "incomingCapabilities": caps, "outgoingCapabilities": caps, "tcpPort": tcp}}
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
if len(sys.argv) > 6: s.bind((sys.argv[6], 0))
s.sendto((json.dumps(ident) + "\n").encode(), (host, port))
