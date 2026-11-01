#!/usr/bin/env python3
"""End-to-end check of `geekbridge-pair` against an independent OpenSSL-backed peer.

The peer is Python's `ssl` module (OpenSSL), not rustls, and computes the pairing
verification code on its own with hashlib + `openssl` CLI output. It uses
extension-less, serial-1, ecdsa-with-SHA512 certificates like stock Android's.

This is loopback evidence about the TLS roles, certificate shapes and the verification
code. It is NOT evidence about a real phone.

Usage: python3 -I openssl_peer_e2e.py /path/to/geekbridge-pair
"""
import hashlib, json, os, pty, select, socket, ssl, subprocess, sys, tempfile, threading, time

CLI = sys.argv[1]
PEER_ID = "ab" * 16
TLS_MAX = {"1.2": ssl.TLSVersion.TLSv1_2, "1.3": ssl.TLSVersion.TLSv1_3}


def run(*a, **k):
    return subprocess.run(a, check=True, capture_output=True, **k)


def make_cert(d, name, cn):
    key, crt = f"{d}/{name}.key", f"{d}/{name}.crt"
    cnf, ext, csr = f"{d}/req.cnf", f"{d}/ext.cnf", f"{d}/{name}.csr"
    open(cnf, "w").write("[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=x\n")
    open(ext, "w").write("[e]\nsubjectKeyIdentifier=none\nauthorityKeyIdentifier=none\n")
    run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", key)
    run("openssl", "req", "-new", "-key", key, "-subj", f"/CN={cn}/OU=Test/O=Test", "-out", csr, "-config", cnf)
    run("openssl", "x509", "-req", "-in", csr, "-signkey", key, "-sha512", "-set_serial", "1", "-days", "3650",
        "-extfile", ext, "-extensions", "e", "-out", crt)
    return crt, key


def spki_of_pem(pem_bytes):
    pub = subprocess.run(["openssl", "x509", "-pubkey", "-noout"], input=pem_bytes, check=True, capture_output=True).stdout
    return subprocess.run(["openssl", "pkey", "-pubin", "-outform", "DER"], input=pub, check=True, capture_output=True).stdout


def der_to_pem(der):
    return ssl.DER_cert_to_PEM_cert(der).encode()


def vkey(a, b, ts):
    hi, lo = (a, b) if a > b else (b, a)  # bytes compare unsigned-lexicographically
    return hashlib.sha256(hi + lo + str(ts).encode()).hexdigest()[:8].upper()


def readline(sock):
    out = b""
    while True:
        c = sock.recv(1)
        if not c:
            raise EOFError("peer closed")
        if c == b"\n":
            return json.loads(out)
        out += c


def send(sock, kind, body):
    sock.sendall((json.dumps({"id": int(time.time() * 1000), "type": kind, "body": body}) + "\n").encode())


def identity(did, **extra):
    b = {"deviceId": did, "deviceName": "OpenSSL Peer", "protocolVersion": 8, "deviceType": "phone",
         "incomingCapabilities": [], "outgoingCapabilities": []}
    b.update(extra)
    return b


class Peer(threading.Thread):
    """Peer acting as TCP acceptor + TLS client (what a phone does when the desktop dials it)."""

    def __init__(self, crt, key, tls, pair):
        super().__init__(daemon=True)
        self.crt, self.key, self.tls, self.pair = crt, key, tls, pair
        self.srv = socket.socket()
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(1)
        self.port = self.srv.getsockname()[1]
        self.result, self.key_ready = {}, threading.Event()

    def run(self):
        try:
            conn, _ = self.srv.accept()
            conn.settimeout(20)
            self.result["plain"] = readline(conn)
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
            ctx.maximum_version = TLS_MAX[self.tls]
            ctx.load_cert_chain(self.crt, self.key)
            s = ctx.wrap_socket(conn)
            self.result["tls"] = s.version()
            send(s, "kdeconnect.identity", identity(PEER_ID))
            self.result["secure"] = readline(s)
            peer_der = s.getpeercert(binary_form=True)  # the certificate the CLI presented as TLS server
            self.result["cli_der"] = peer_der
            if self.pair:
                req = readline(s)
                ts = req["body"]["timestamp"]
                mine = spki_of_pem(open(self.crt, "rb").read())
                theirs = spki_of_pem(der_to_pem(peer_der))
                self.result["key"] = vkey(mine, theirs, ts)
                self.key_ready.set()
                send(s, "kdeconnect.pair", {"pair": True})
                time.sleep(1.5)
            else:
                time.sleep(0.5)
        except Exception as e:  # reported by the driver
            self.result["error"] = repr(e)
            self.key_ready.set()


def cli(args, typed=None, wait_event=None, peer=None):
    """Run the CLI; with `typed` set, run it under a pty and answer the pairing prompt."""
    if typed is None:
        p = subprocess.run([CLI] + args, capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
        return p.stdout + p.stderr, p.returncode
    pid, fd = pty.fork()
    if pid == 0:
        os.execv(CLI, [CLI] + args)
    buf, sent, end = b"", False, time.time() + 60
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.5)
        if r:
            try:
                d = os.read(fd, 4096)
            except OSError:
                break
            if not d:
                break
            buf += d
            if not sent and b"press Enter to reject" in buf:
                wait_event.wait(10)
                os.write(fd, (typed(peer) + "\n").encode())
                sent = True
    _, status = os.waitpid(pid, 0)
    return buf.decode(errors="replace"), os.waitstatus_to_exitcode(status)


def check(cond, msg):
    if not cond:
        print("FAIL:", msg)
        sys.exit(1)
    print("ok:", msg)


def main():
    with tempfile.TemporaryDirectory() as d:
        state = f"{d}/state"
        out, rc = cli(["--state-dir", state, "identity"])
        check(rc == 0, "identity created")
        my_id = [l for l in out.splitlines() if l.startswith("device id:")][0].split()[-1]

        crt, key = make_cert(d, "peer", PEER_ID)

        for tls in ("1.2", "1.3"):
            # Fresh state per TLS version.
            st = f"{d}/state-{tls}"
            out, rc = cli(["--state-dir", st, "identity"])
            my = [l for l in out.splitlines() if l.startswith("device id:")][0].split()[-1]

            peer = Peer(crt, key, tls, pair=False)
            peer.start()
            out, rc = cli(["--state-dir", st, "inspect", "--connect", f"127.0.0.1:{peer.port}", "--device-id", PEER_ID])
            peer.join(10)
            check(rc == 0, f"[TLS<={tls}] inspect succeeds against an OpenSSL TLS client")
            check(peer.result.get("plain", {}).get("body", {}).get("targetDeviceId") == PEER_ID, "plaintext identity targets the peer")
            check(peer.result["plain"]["body"].get("targetProtocolVersion") in (8, "8"), "plaintext identity carries targetProtocolVersion")
            check(peer.result["secure"]["body"]["deviceId"] == my, "secure identity carries our device id")
            check("NOT paired" in out, "unpaired peer is reported as unverified")
            check(peer.result["tls"] == ("TLSv1.2" if tls == "1.2" else "TLSv1.3"), f"negotiated {peer.result['tls']}")
            check(subprocess.run([CLI, "--state-dir", st, "list"], capture_output=True, text=True).stdout.strip() == "no paired devices",
                  "inspect stored nothing")

            # Pair, answering the prompt with the code the OpenSSL peer computed itself.
            peer = Peer(crt, key, tls, pair=True)
            peer.start()
            out, rc = cli(["--state-dir", st, "pair", "--connect", f"127.0.0.1:{peer.port}", "--device-id", PEER_ID],
                          typed=lambda p: p.result.get("key", "00000000"), wait_event=peer.key_ready, peer=peer)
            peer.join(10)
            check("error" not in peer.result, f"peer saw no error ({peer.result.get('error')})")
            check(rc == 0 and "trust record written" in out, "pairing completed after typing the independently computed code")
            fp = subprocess.run(["openssl", "x509", "-in", crt, "-outform", "DER"], capture_output=True).stdout
            lst = subprocess.run([CLI, "--state-dir", st, "list"], capture_output=True, text=True).stdout
            check(PEER_ID in lst and hashlib.sha256(fp).hexdigest() in lst, "trust store pins the peer's exact certificate")

            # Wrong code must not pair.
            st2 = f"{d}/state-wrong-{tls}"
            cli(["--state-dir", st2, "identity"])
            peer = Peer(crt, key, tls, pair=True)
            peer.start()
            out, rc = cli(["--state-dir", st2, "pair", "--connect", f"127.0.0.1:{peer.port}", "--device-id", PEER_ID],
                          typed=lambda p: "DEADBEEF", wait_event=peer.key_ready, peer=peer)
            peer.join(10)
            check(rc != 0 and "does NOT match" in out, "wrong code is rejected")
            check(subprocess.run([CLI, "--state-dir", st2, "list"], capture_output=True, text=True).stdout.strip() == "no paired devices",
                  "nothing stored after a wrong code")

            # Reconnect: now trusted.
            peer = Peer(crt, key, tls, pair=False)
            peer.start()
            out, rc = cli(["--state-dir", st, "inspect", "--connect", f"127.0.0.1:{peer.port}", "--device-id", PEER_ID])
            peer.join(10)
            check(rc == 0 and "paired (certificate matches the pinned one)" in out, "reconnect is recognised as the pinned device")

            # Substitution: same id, different certificate.
            crt2, key2 = make_cert(d, f"evil-{tls}", PEER_ID)
            peer = Peer(crt2, key2, tls, pair=False)
            peer.start()
            out, rc = cli(["--state-dir", st, "inspect", "--connect", f"127.0.0.1:{peer.port}", "--device-id", PEER_ID])
            peer.join(10)
            check(rc != 0 and "pinned" in out, "substituted certificate is refused")
            check(PEER_ID in subprocess.run([CLI, "--state-dir", st, "list"], capture_output=True, text=True).stdout,
                  "pin untouched by the substitution attempt")

            # Role 2: the OpenSSL peer dials us (TCP initiator, TLS server).
            s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
            cli_crt = open(f"{st}/identity.pem").read()
            res = {}

            def dial():
                time.sleep(1.0)
                try:
                    c = socket.create_connection(("127.0.0.1", port), timeout=20)
                    send(c, "kdeconnect.identity", identity(PEER_ID, targetDeviceId=my, targetProtocolVersion="8"))
                    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                    ctx.maximum_version = TLS_MAX[tls]
                    ctx.load_cert_chain(crt, key)
                    ctx.verify_mode = ssl.CERT_REQUIRED
                    ctx.load_verify_locations(cadata=cli_crt[: cli_crt.index("-----BEGIN PRIVATE KEY-----")])
                    t = ctx.wrap_socket(c, server_side=True)
                    res["tls"] = t.version()
                    send(t, "kdeconnect.identity", identity(PEER_ID))
                    res["secure"] = readline(t)
                    time.sleep(0.5)
                except Exception as e:
                    res["error"] = repr(e)

            th = threading.Thread(target=dial, daemon=True); th.start()
            out, rc = cli(["--state-dir", st, "inspect", "--listen", f"127.0.0.1:{port}", "--peer-ip", "127.0.0.1", "--wait-secs", "20"])
            th.join(10)
            check("error" not in res, f"OpenSSL TLS server handshake with the CLI ({res.get('error')})")
            check(rc == 0 and "paired (certificate matches the pinned one)" in out, f"[TLS<={tls}] role 2: peer dials us, pinned cert recognised")
            check(res["secure"]["body"]["deviceId"] == my, "role 2: secure identity received by the OpenSSL server")
    print("ALL OK")


main()
