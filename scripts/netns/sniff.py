import socket, struct, sys, time, collections
s = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
s.settimeout(0.5)
end = time.time() + float(sys.argv[1])
seen = collections.OrderedDict()
while time.time() < end:
    try:
        data, (ifn, proto, ptype, *_ ) = s.recvfrom(65535)
    except socket.timeout:
        continue
    if ptype != 4 or proto != 0x0800 or len(data) < 42: continue  # outgoing IPv4 only
    ip = data[14:]
    ihl = (ip[0] & 15) * 4
    if ip[9] != 17: continue
    sport, dport = struct.unpack('!HH', ip[ihl:ihl+4])
    if dport != 1716: continue
    k = (ifn, socket.inet_ntoa(ip[12:16]), socket.inet_ntoa(ip[16:20]))
    seen[k] = seen.get(k, 0) + 1
for (ifn, src, dst), n in seen.items():
    print(f"UDP/1716 out {ifn:10s} src={src:15s} dst={dst:15s} x{n}")
