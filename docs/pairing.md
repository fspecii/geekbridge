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
2. **QR code (no code comparison), like WhatsApp Web.** On the computer, **Link a phone** shows a big code that
   renews itself before it expires (a 2 minute, single-use token; the first time, with nothing paired, it is already on
   screen) with a live line: *Waiting for phone…*, *Phone scanned — confirm on phone* (the service saw `qr_accepted`),
   *Paired ✓ with X*. The code is revoked when the view is left or **Stop showing the code** is pressed. On the phone,
   **Scan QR code from my computer** opens the in-app camera screen (`QrScanActivity`). Alternatively point the
   phone's **camera app** at the code and tap the GeekBridge link, use a scanner app, or paste the link. After a scan
   the phone asks **Connect to X?** (the name is the one in the code, marked unverified, next to the fingerprint),
   then **Pair with X?**, then lands on Home with **Paired ✓ with X**.
3. **Something wrong?** Both sides say what happened in plain words (different network, guest Wi-Fi, VPN, firewall,
   expired QR code, wrong computer) and have a **Test connection** button that checks it step by step.
4. **Done.** Both sides say **Paired ✓**; the computer shows a "what to do next" card (clipboard, sending a file).
5. **No computer app yet.** The phone offers **Don't have GeekBridge on your computer yet?**: a share sheet / mail draft
   with the public releases page and three install lines, a copyable short address. Nothing about the phone or any
   pairing secret is in that text (`ComputerInvite.kt`, unit-tested).
6. **Unlink.** **Unlink…** on the computer (confirmation names the phone) and **Unlink** on the phone's pair step or
   Diagnostics. A new QR code always pairs again.

## Discovery

* **UDP announce (done).** KDE Connect's identity datagram (`kdeconnect.identity` with `tcpPort`) to
  `255.255.255.255:1716` and each local `/24` broadcast. The phone already announced and listened. The computer now
  announces every 15 s (`Daemon::announce_loop`) and the service **listens for phones and accepts phones by default**
  (`--listen-inbound` now defaults to `0.0.0.0:1716` plus an IPv6-only twin on `[::]:1716`, walking up to 1764 if the port is taken; `--no-listen-inbound` and
  `--no-announce` switch these off). Hearing an announcement only lists a device; nothing is trusted.
* **mDNS (not implemented).** There is no mDNS responder or browser. Networks that filter UDP broadcast (some routers,
  guest Wi-Fi) therefore do not auto-list devices; the QR code and "Connect by address" still work there. Adding
  mDNS would need a multicast socket on both sides (and a `MulticastLock` on Android, which the app already holds
  while discovering).
* The emulator's NAT drops broadcasts, so discovery itself is not exercised by the emulator tests
  (see [e2e-findings.md](reports/e2e-findings.md)); the unit/integration tests cover the datagram format and parsing.

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
  table is full ([security-review-2.md](reports/security-review-2.md) S2-1, S2-2). Reuse, expiry and a wrong token are distinct, tested outcomes (`RedeemError`).

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
5. Scanning is "pair again": if the phone still remembers a computer with the same device id (for example the computer
   forgot the phone), the phone forgets that record first (`QrStep.Forget`) and pairs for real. It never reports
   "Paired" without sending the request, because the computer would not know. A computer that still trusts the
   phone treats the request as a normal approval on the same connection.

A QR code from a hostile source can at worst make the phone dial the attacker's own device and show "Pair with
<attacker's name>?" with the attacker's certificate fingerprint; a link arriving from outside the app is therefore
shown on a confirmation screen first ("Only continue if you just clicked Link a phone on your own computer"); a code scanned with this app's own camera screen gets the shorter "Connect to X?" page. A
photographed or screenshotted QR code is useless after 120 seconds or after one use.

## The in-app scanner (decision)

Options weighed: the Google code scanner (`com.google.android.gms.codescanner`) and ML Kit need Play services or a
bundled model (+2 to 10 MB, closed components, not on F-Droid); CameraX adds ~1.5 MB and a Google dependency chain; a
pure-Kotlin decoder would duplicate what the Rust core can already do. Chosen: **Camera2 (platform API, no library) for
the picture and the native core for the decoding**. The frame's brightness plane (at most 960 x 720) goes to the new
core action `pairing_decode_qr` (base64 `luma`, `width`, `height`), which runs `rqrr`, a small pure-Rust decoder the
desktop tests already use; the answer is `{text}` or the error `qr_not_found`. APK cost is the decoder (tens of KB), no
new Kotlin dependency, works offline. The only new permission is `CAMERA`, asked on the scan screen on first use; the
screen explains it first, offers the system camera app / scanner app / paste when it is denied, and the manifest marks
the camera `required="false"`. The picture is decoded in memory and never stored or sent; the decoded link (it holds
the one-time token) is never logged and is returned only to `QrPairActivity` (not exported scanner).

## Error and diagnostic help

`bridge_core::pairing_help` (computer) and `PairingHelp.kt` / `NetCheck` (phone) turn raw reasons into plain language and
share the same wording rules. **Test connection** measures: is this device on a network, is the other device on the same
/24, does a TCP connect to it succeed (connected / refused / no answer / no route), is a VPN interface active, and
(computer only) is the service listening at all. It then names the most likely cause: refused means GeekBridge is not
running there; no answer on the same subnet means a firewall or Wi-Fi client isolation; another subnet means a different
network. Nothing is sent but one TCP connect.

## Repeated pair requests (replacing a pending prompt)

Stock KDE Connect Android derives the verification key from the timestamp of *every* pair request, so tapping
**Request pairing** twice moves the phone to a new key. The state machine (`pairing.rs`, `Machine`) therefore
**replaces** the pending prompt when the peer sends a second `{pair:true, timestamp}` while its prompt is open:

1. The old prompt is cancelled (the daemon's challenge disappears, the Android prompt entry is removed), the key is
   re-derived from the new timestamp, a new prompt with a new `challenge_id`/`request_id` is shown, and the response
   window restarts.
2. Every prompt has a generation number and every local decision is tagged with the generation it answers. A decision
   for any other generation is dropped: the pairing stays pending and nothing is sent. Over IPC the `challenge_id` is
   that tag (the old one answers `NotFound`/stale); on Android `Core::decide` needs the `request_id` and, to approve,
   the certificate fingerprint **and** the code, so it fails on a mismatch.
3. Nothing is auto-accepted and an acceptance is never carried over to a new key. The accept packet is sent only
   from a decision on the *current* generation, after which the machine is finished, so it goes out exactly once per
   pairing. When the desktop approves, the trust record is
   written to disk before the accept leaves, so a pairing the phone saw as complete is always durable and visible in
   `status` on the computer.
4. Budget: at most 5 requests per 60 seconds per connection may open or replace a prompt (`MAX_REPLACEMENTS`,
   `REPLACEMENT_WINDOW`). More are ignored and count toward the unexpected-packet budget, which drops the link. A
   request with the same timestamp, or outside the clock-skew window, changes nothing. The connection is bound to one
   certificate, so a replacement can only come from the certificate the user is already looking at; a new connection
   starts over with a fresh prompt and still hits the daemon's challenge cap.

Threat: an attacker (or a flaky peer) swaps the request between the moment the user compares the codes and the moment
they click **Approve**, hoping the click lands on a key the user never compared. Mitigation: the click is bound to
the prompt it was made on; when the prompt is replaced, the click is refused and the user must compare the new key.
The attacker also cannot flood the user with prompts beyond the budget above. QR pairing is unchanged: the token is
read from the first request only.

## Tests

* `bridge-kde`: `qrpair` unit tests (single use, expiry boundary, wrong token, lock-out, bounded outstanding, payload
  round trip, hostile payloads); loopback tests for the token in the pair packet and the `qr_pinned` prompt.
* `bridge-service/tests/qr_pairing.rs`: real TLS phone endpoint against the service for redeem-once, reuse, wrong token,
  cancel, "no token still needs approval", Test connection, announcement format.
* `bridge-kde` `pairing.rs`: replacement shows the new key, stale approval and stale rejection are refused, the rate
  limit, same timestamp / bad clock ignored, exactly-once accept, and a replay of the captured stock Android request
  twice; loopback `repeated_peer_request_replaces_the_prompt_with_the_new_key`.
* `bridge-android`: `pair_request` pins the certificate (mismatch drops the connection and nothing reaches the peer),
  field-pairing rules, `pairing_parse_qr`.
* `bridge-apps`: the drawn QR code is decoded back with `rqrr` and parsed.
* Android: `QrPairingTest` (address rotation, one-shot pair request, no retry after a refusal), `PairingHelpTest`.
