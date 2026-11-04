import socket, sys, json, time
# usage: phone_announce.py SRC_IP DST_IP COUNT [DEVICE_ID] [TCP_PORT]
src, dst, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
did = sys.argv[4] if len(sys.argv) > 4 else "p"*32
body = {"deviceId": did, "deviceName": "Test phone", "deviceType": "phone",
        "tcpPort": 1716, "protocolVersion": 8,
        "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": []}
pkt = (json.dumps({"id": int(time.time()*1000), "type": "kdeconnect.identity", "body": body}) + "\n").encode()
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
s.bind((src, 0))
for _ in range(n):
    s.sendto(pkt, (dst, 1716)); time.sleep(0.5)
