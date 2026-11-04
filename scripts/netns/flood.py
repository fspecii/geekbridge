import socket, json, time, sys
# 100 phones, each with its own source address 192.168.1.(100+i), announcing 3 rounds, plus
# one host announcing 50 different ids, and duplicates/garbage.
def pkt(did, name="Phone"):
    body = {"deviceId": did, "deviceName": name, "deviceType": "phone", "tcpPort": 1716,
            "protocolVersion": 8, "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": []}
    return (json.dumps({"id": 1, "type": "kdeconnect.identity", "body": body}) + "\n").encode()
socks = []
for i in range(100):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind((f"192.168.1.{100+i}", 0)); socks.append(s)
for rnd in range(3):
    for i, s in enumerate(socks):
        did = f"{i:032x}"
        s.sendto(pkt(did, f"Phone {i}"), ("192.168.1.10", 1716))
        s.sendto(pkt(did, f"Phone {i}"), ("192.168.1.10", 1716))  # duplicate
    time.sleep(0.3)
# one source, many ids
for j in range(60):
    socks[0].sendto(pkt(f"{1000+j:032x}", f"Spoof {j}"), ("192.168.1.10", 1716))
# garbage and oversized
for n in (0, 1, 100, 16*1024+1, 60000):
    socks[1].sendto(b"x" * n, ("192.168.1.10", 1716))
socks[1].sendto(b'{"type":"kdeconnect.identity"', ("192.168.1.10", 1716))
