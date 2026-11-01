# First-time pairing

How a phone and a computer find each other and pair, from the user's side and from the security side. Code:
`crates/bridge-kde/src/qrpair.rs` (tokens, QR payload), `crates/bridge-kde/src/pairing.rs` (state machine),
`crates/bridge-service/src/pairing_flow.rs` (service side), `crates/bridge-core/src/pairing_help.rs` (plain-language
help), `crates/bridge-apps/src/pair_ui.rs` (desktop UI), `android/.../QrPairActivity.kt`, `PairingHelp.kt`,
`QrPairing.kt`, `PairPanel.kt` (phone UI).

## What the user sees

1. **Nearby.** Both apps announce themselves on the local network. The computer's Details panel lists nearby phones
   by name; the phone's setup step lists nearby computers by name. One tap on **Pair** dials, asks to pair and shows a
   big code on both screens. Confirm the same code on both (the computer asks for the phone's code to be typed, the
   phone has a **Pair** button).
2. **QR code (no code comparison).** On the computer choose **Pair with QR code**. On the phone choose
   **Scan QR code**, then either point the phone's **camera app** at the code and tap the GeekBridge link it shows, use
   a scanner app, or paste the link (the computer's QR window has **Copy link**). The phone shows "Pair with X?" and
   waits for **Pair**. No camera permission is requested: GeekBridge never opens the camera itself.
3. **Something wrong?** Both sides say what happened in plain words (different network, guest Wi-Fi, VPN, firewall,
   expired QR code, wrong computer) and have a **Test connection** button that checks it step by step.
4. **Done.** A "what to do next" card offers the clipboard and sending a file.

## Discovery

* **UDP announce (done).** KDE Connect's identity datagram (`kdeconnect.identity` with `tcpPort`) to
  `255.255.255.255:1716` and each local `/24` broadcast. The phone already announced and listened. The computer now
  announces every 15 s (`Daemon::announce_loop`) and the service **listens for phones and accepts phones by default**
  (`--listen-inbound` now defaults to `0.0.0.0:1716`, walking up to 1764 if the port is taken; `--no-listen-inbound` and
  `--no-announce` switch these off). Hearing an announcement only lists a device; nothing is trusted.
* **mDNS (not implemented).** There is no mDNS responder or browser. Networks that filter UDP broadcast (some routers,
  guest Wi-Fi) therefore do not auto-list devices; the QR code and "Connect by address" still work there. Adding
  mDNS would need a multicast socket on both sides (and a `MulticastLock` on Android, which the app already holds
  while discovering).
* The emulator's NAT drops broadcasts, so discovery itself is not exercised by the emulator tests
  (see [e2e-findings.md](e2e-findings.md)); the unit/integration tests cover the datagram format and parsing.

## QR payload and token protocol

The QR code encodes one text, also usable as a link:

```
geekbridge://pair?v=1&id=<device id>&fp=<sha256 of TLS cert, hex>&t=<token, 32 hex>&a=<ip:port,...>&n=<name>
```

* `fp` pins the computer's certificate. `a` lists up to 4 LAN addresses (percent-encoded `ip:port`, IPv6 in brackets).
  `v` is the payload version; unknown keys are ignored, a different version is refused with a clear message.
* `t` is a 128-bit random, **single-use, 120-second** token. The computer keeps only its SHA-256
  (`TokenBook`), at most 4 outstanding (a new QR evicts the oldest). Five wrong tokens from one source address lock out
  that address only (a stranger cannot revoke the code on the screen); everything is revoked when the QR window closes.
  One address may hold at most 3 unpaired connections, and a paired phone displaces the oldest stranger when the link
  table is full ([security-review-2.md](security-review-2.md) S2-1, S2-2). Reuse, expiry and a wrong token are distinct, tested outcomes (`RedeemError`).

Flow (phone scans):

1. Phone parses the text in the native core (`pairing_parse_qr`), dials the addresses in order (6 s each) and finishes
   the TLS handshake. The core compares the SHA-256 of the certificate **on that connection** with `fp`
   (`transport_pair_request` with `qr_token` + `qr_cert_sha256`). A mismatch closes the connection and **the token is
   never sent**, so a machine in the middle only learns nothing.
2. Phone sends the normal `kdeconnect.pair` request with an extra `gbToken` field (stock KDE Connect ignores unknown
   body fields, so interoperability is unchanged; `gbToken` must match `[0-9a-f]{32}` or the pairing aborts).
3. The computer redeems the token. Valid: the computer treats this as its own approval (the user put the code on its
   screen on purpose), emits `qr_accepted` and answers the pair request. Invalid: it emits `qr_refused` with a
   plain-language reason and sends `pair:false`; nothing is stored.
4. The phone shows "Pair with X?" (no code, because the certificate was pinned from the QR code) and still requires an
   explicit tap. Trust is written only after that tap, for the certificate on that connection, like every other pairing.

A QR code from a hostile source can at worst make the phone dial the attacker's own device and show "Pair with
<attacker's name>?" with the attacker's certificate fingerprint; a link arriving from outside the app is therefore
shown on a confirmation screen first ("Only continue if you just chose Pair with QR code on your own computer"). A
photographed or screenshotted QR code is useless after 120 seconds or after one use.

## Error and diagnostic help

`bridge_core::pairing_help` (computer) and `PairingHelp.kt` / `NetCheck` (phone) turn raw reasons into plain language and
share the same wording rules. **Test connection** measures: is this device on a network, is the other device on the same
/24, does a TCP connect to it succeed (connected / refused / no answer / no route), is a VPN interface active, and
(computer only) is the service listening at all. It then names the most likely cause: refused means GeekBridge is not
running there; no answer on the same subnet means a firewall or Wi-Fi client isolation; another subnet means a different
network. Nothing is sent but one TCP connect.

## Tests

* `bridge-kde`: `qrpair` unit tests (single use, expiry boundary, wrong token, lock-out, bounded outstanding, payload
  round trip, hostile payloads); loopback tests for the token in the pair packet and the `qr_pinned` prompt.
* `bridge-service/tests/qr_pairing.rs`: real TLS phone endpoint against the service for redeem-once, reuse, wrong token,
  cancel, "no token still needs approval", Test connection, announcement format.
* `bridge-android`: `pair_request` pins the certificate (mismatch drops the connection and nothing reaches the peer),
  field-pairing rules, `pairing_parse_qr`.
* `bridge-apps`: the drawn QR code is decoded back with `rqrr` and parsed.
* Android: `QrPairingTest` (address rotation, one-shot pair request, no retry after a refusal), `PairingHelpTest`.
