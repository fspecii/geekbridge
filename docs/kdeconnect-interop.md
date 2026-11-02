# KDE Connect interoperability: what is verified, what is only spec-checked

Run date: 2026-11-02. This file records the results against stock peers; the stock **desktop** daemon was exercised
directly and the stock **Android**
app was rehearsed on an emulator, see [Stock Android app](#stock-android-app-emulator-rehearsal).

## Method

A **real stock KDE Connect desktop daemon** was obtained without root: the Arch package
`kdeconnect 26.08.2-1` (plus the few KDE libraries not installed system-wide) was downloaded from the
configured mirror and extracted with `bsdtar` into a scratch tree. `kdeconnectd` and `kdeconnect-cli`
run from there. (Package signatures were not checked; this is a throw-away test tree.)

Isolation, all in `scripts/interop-kdeconnect.sh`:

* `unshare -Urn`: private user + network namespace with only `lo` and one dummy private address
  (`10.77.0.2`). The daemon's UDP broadcasts have nowhere to go, so nothing reaches the LAN or a real
  phone. The dummy address exists because the daemon ignores UDP identities from loopback senders
  (`lanlinkprovider.cpp:274`).
* private D-Bus (`dbus-run-session` with a no-activation config), private `HOME`/`XDG_*`, offscreen Qt.
  The real `~/.config` and any real KDE Connect data are never touched.
* The daemon is driven with `kdeconnect-cli` and its D-Bus API (`acceptPairing`, `verificationKey`);
  its reactions are observed on D-Bus (`dbus-monitor`), in its own debug log and on disk.

GeekBridge side: `crates/bridge-kde/examples/real_peer_probe.rs` (a probe over the real
`bridge_kde::Endpoint`, with an opt-in `WireTap` that records every packet line) and, for scenario S5,
the shipped `geekbridge-service` + `geekbridge-ctl` binaries.

Result of the final run: **25 checks passed, 0 failed**, plus one characterisation note (protocol 7). The
checks are the `PASS`/`FAIL` lines of `scripts/interop-kdeconnect-scenarios.sh`, written to `results.txt`.

### Reproducing

```
# 1. Get the daemon without root: copy the sync databases into a private dbpath, list the package
#    URLs there (-Sp needs no root), download and extract them.
mkdir -p /tmp/kc/{db/sync,dl,root}; cp /var/lib/pacman/sync/* /tmp/kc/db/sync/; ln -s /var/lib/pacman/local /tmp/kc/db/local
pacman --dbpath /tmp/kc/db -Sp --needed kdeconnect > /tmp/kc/urls.txt
(cd /tmp/kc/dl && xargs -a ../urls.txt -n1 -P6 curl -fsSLO); for f in /tmp/kc/dl/*.zst; do bsdtar -xf "$f" -C /tmp/kc/root; done
# 2. Build and run (about 3 minutes); results.txt lists every PASS/FAIL/NOTE
cargo build -p bridge-kde --example real_peer_probe && cargo build -p bridge-service --bins
scripts/interop-kdeconnect.sh /tmp/kc/root /tmp/kc/out
# 3. Re-export the fixtures (new device ids and certificates; the tests derive everything from the files)
python3 -I scripts/interop-kdeconnect-export.py /tmp/kc/out crates/bridge-kde/tests/fixtures/kdeconnect-real
```

## Status per flow

**VERIFIED** = observed against kdeconnectd 26.08.2 in the run above and re-checked by the committed
fixtures in `crates/bridge-kde/tests/kdeconnect_real.rs`. **SPEC-CHECKED** = compared with the pinned
reference sources (kdeconnect-kde `25c0cc7`, kdeconnect-android `4de397e`), not run against a stock peer.
**EXTENSION** = GeekBridge-only.

| Area | Status | Evidence |
| --- | --- | --- |
| UDP identity -> stock daemon dials us (daemon = TCP initiator = TLS server; we = TLS client) | VERIFIED | S2; the daemon only reacts to a non-loopback sender and a `tcpPort` in 1716-1764 |
| We dial the daemon (we = TCP initiator = TLS server; daemon = TLS client) | VERIFIED | S1, S3, S5, S6 |
| Plaintext identity with `targetDeviceId` / `targetProtocolVersion` (both directions), then secure identity | VERIFIED | `daemon-identity-plaintext.json`, `daemon-identity-secure.json`, `geekbridge-identity-*.json` |
| Protocol **8** | VERIFIED | all links |
| Protocol **7** | NOT SUPPORTED by design (see below) | S4, unit tests |
| TLS 1.3 (rustls default offer, daemon picks `TLS13_AES_256_GCM_SHA384`) | VERIFIED | `LINK` lines |
| TLS 1.2 only (what Android's stack negotiates) | VERIFIED against the desktop | S6 |
| Certificate shape: our rcgen ECDSA-P256 cert accepted; the daemon's Qt-generated cert parsed (CN = device id, validity) | VERIFIED | `daemon_certificate_matches_its_identity_and_is_valid` |
| Pinning: reconnect with pinned certs on both sides | VERIFIED | S3 |
| Pinning: a stranger with a **new certificate under a paired device id** | VERIFIED refused by the daemon at the TLS layer | S7 |
| Pairing, we request, daemon accepts; pairing, daemon requests, we approve | VERIFIED | S1, S2, S5; the 8-hex verification key we compute equals the one the daemon displayed for every pairing in the run, pinned by `verification_keys_equal_the_ones_the_real_daemon_displayed` |
| Unpair sent by the daemon -> we drop trust | VERIFIED | S1 (`PeerUnpaired`) |
| Unpair sent by GeekBridge -> daemon unpairs | SPEC-CHECKED (loopback tests only) | not exercised live |
| Our "crossed pair request is not an accept" deviation | NOT exercised live | deliberate deviation, unchanged |
| `kdeconnect.ping`, with and without `message` | VERIFIED both ways | the daemon raised its "Ping!" notification (`pingReceived` on D-Bus); we received `{"message":...}` and `{}` |
| `kdeconnect.clipboard` update both ways | VERIFIED | daemon clipboard adopted ours (read back with `--send-clipboard`); we parsed the daemon's |
| `kdeconnect.clipboard.connect` both ways | VERIFIED; **bug found and fixed** (below) | `daemon-clipboard-connect*.json` |
| `kdeconnect.share.request` text and URL | VERIFIED both ways (text lands in the daemon clipboard; a URL is handed to the desktop's URL opener) | S1 |
| `kdeconnect.share.request` file, payload socket (TLS, role inversion), both ways | VERIFIED, 300 kB and 200 kB random files byte-identical | S1 |
| Shipped `geekbridge-service` / `geekbridge-ctl`: connect, pair, approve, ping both ways | VERIFIED | S5; negotiated set is exactly `ping`, `clipboard`, `clipboard.connect`, `share.request` |
| Daemon plugins GeekBridge activates on a stock desktop | `ping`, `clipboard`, `share` only | daemon log `Loaded plugin` |
| `kdeconnect.notification*`, `kdeconnect.telephony*` | NOT CLAIMED by GeekBridge (not in the identity); the desktop loads no such plugin for us | nothing to verify |
| `kdeconnect.contacts.*`, `kdeconnect.sms.*` requests and responses | SPEC-CHECKED against kdeconnect-android (`ContactsPlugin.kt`, `SMSPlugin.kt`) | a stock *desktop* is the requester, so it cannot answer them; needs a phone. Key names are asserted in `sms_and_contacts_requests_use_the_keys_the_android_plugins_read` |
| mDNS discovery, reverse connect on failure, IPv6, `share.request.update` / multi-file batches, image clipboard (`clipboard.file`) | NOT tested | |

### Protocol 7

The released 26.08.2 daemon **still dials** a peer whose UDP identity says `protocolVersion: 7`
(S4), while kdeconnect-kde master at `25c0cc7` drops `protocolVersion < 8` in
`LanLinkProvider::deviceDiscovered`. GeekBridge refuses protocol 7 on both sides (it needs the
protocol 8 pinned handshake), so an old protocol-7 phone cannot link; that is a documented limitation,
not a bug found here.

## Interop bug found and fixed

* **`kdeconnect.clipboard.connect` with `"content":null`.** A stock desktop with an empty clipboard
  sends `{"content":null,"timestamp":0}` right after the link comes up (captured:
  `daemon-clipboard-connect-empty.json`). `bridge_core::clipboard_wire::parse` treated that as
  `MissingContent`, so the service counted a rejected packet (`skipped`, `last_skip = "rejected"`)
  on **every** connect to a stock desktop with nothing copied. Fixed: for `.connect` only, a null
  `content` parses as `Connect { content: "", timestamp_ms: 0 }`, which no caller applies (timestamp 0
  means unknown). A live `kdeconnect.clipboard` without text is still malformed. Regression test:
  `clipboard_packets_from_the_daemon_are_understood`.

Everything else the daemon sent was accepted unchanged, and every captured line (26 of them, both
directions) re-serialises **byte-for-byte** identically through `Packet::from_line` /
`Packet::to_line` (Qt's compact JSON and serde_json both order keys alphabetically).

Also added for the capture: `bridge_kde::WireTap` / `Endpoint::with_wire_tap` (off by default; sees
each packet line after TLS, including the pairing exchange).

## GeekBridge extensions (stock peers ignore them)

Types with the `geekbridge.` prefix are only ever sent to a peer that advertised them in its identity,
but GeekBridge also lists them in its own `incomingCapabilities` / `outgoingCapabilities`:

`geekbridge.clipboard.{pull,item,none}`, `geekbridge.calls.{request,state,result}`,
`geekbridge.calendar.{request,sync,write,write_result}`, `geekbridge.sms.status`.

Checked against the stock desktop:

* The unknown capability strings in our identity did not stop pairing or plugin loading.
* The probe deliberately **sent** `geekbridge.calendar.request`, `geekbridge.calendar.write`,
  `geekbridge.calls.request` and `geekbridge.clipboard.pull` on a paired link (`ext` command). The
  daemon logged `discarding unsupported packet "<type>" for "GeekBridge"` for each (`device.cpp:402`),
  the link stayed up, it still listed the device reachable, and a ping sent afterwards was delivered.
  Fixtures: `geekbridge-ext-*.json`.
* Stock Android drops unknown types the same way (`Device.kt:467`, "Ignoring packet with type ... because
  no plugin can handle it") and keeps the link. **SPEC-CHECKED only**; no phone was used.

Note that `kdeconnect.contacts.*` and `kdeconnect.sms.*` are stock types but are *requests that only a
phone answers*; GeekBridge also adds a `requestId` field to `kdeconnect.sms.request` that stock
phones ignore (SPEC-CHECKED: `SMSPlugin.kt` reads named keys only).

## Not covered / next hardware step

Everything about the stock **Android** app: its Conscrypt TLS 1.2 stack against our certificate and
SHA-256/random-serial/`OU=GeekBridge` shape, the 10 s secure-identity budget on a real network,
trusted-network gating, the phone-side behaviour of the SPEC-CHECKED rows, and
`kdeconnect.pair` timing on a real handset. The first-hardware-session checklist in `docs/real-device-test-plan.md` still applies unchanged, with S1-S7 of the script as the expected
desktop baseline.

## Stock Android app (emulator rehearsal)

Run date: 2026-11-02. The real **KDE Connect 1.37.0 (versionCode 13700)** F-Droid build of
`org.kde.kdeconnect_tp` against `geekbridge-service` on the host. This is an **emulator**, not a handset:
Wi-Fi, trusted-network gating, UDP broadcast/mDNS discovery and real-network timing are not exercised.

### Method

* APK from `https://f-droid.org/repo/org.kde.kdeconnect_tp_13700.apk`, kept outside the repository
  (it is not redistributed here). SHA-256 `a51c6e85...0dd01` and signer certificate SHA-256
  `79b50031...40d92` both equal the values in F-Droid's published `index-v2.json` (fetched over HTTPS; the
  index's own jar signature was not checked), and `apksigner verify` accepts v1/v2/v3 signatures. The APK
  was never run on the host, only `adb install`ed into the emulator.
* Own AVD `gb36x` (Android 36, google_apis x86_64, console port 5674, adb server port 16074).
* Service: `geekbridge-service --dev-loopback --discovery-port 1716` with a private state dir, a private
  Xvfb for the clipboard, and **`GEEKBRIDGE_WIRE_TAP_FILE=<path>`**, a new developer-only switch
  (`daemon.rs::dev_wire_tap`, active only together with `--dev-loopback`, owner-only file) that records
  every packet line after TLS in both directions through `bridge_kde::WireTap`.
* Topology: in the app, *Add devices by IP* `10.0.2.2` sends its UDP identity to the host's `1716`
  (it arrives from `127.0.0.1`); `adb forward tcp:1716 tcp:1716` lets `geekbridge-ctl connect ID 127.0.0.1 1716`
  dial the app. **Payload sockets need the opposite plumbing per direction**: the app sees the forwarded link
  as coming from its own `127.0.0.1`, so for *desktop -> phone* files the range `1739-1764` must be
  `adb reverse`d to the host, and for *phone -> desktop* files it must be `adb forward`ed (the two cannot
  coexist; the app binds its payload port in the same range). A real LAN needs none of this.
* Capture: `crates/bridge-kde/tests/fixtures/kdeconnect-android-real/` (21 packet lines, README there) and
  `tests/kdeconnect_android_real.rs`, plus `crates/bridge-service/tests/android_negotiation.rs`.

### Result per flow

**VERIFIED** = observed live with the stock app. The GeekBridge side was always the TCP initiator / TLS
server (the desktop dialled the app); the app dialling the desktop was **not** exercised.

| Flow | Status | Evidence |
| --- | --- | --- |
| Android's UDP identity datagram parsed (protocol 8, `deviceType: phone`, 27 + 26 capabilities, `tcpPort`) | VERIFIED | `android-udp-identity.json` |
| We dial the app: plaintext identity, TLS (the app is the TLS *client*, it negotiated `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`), secure identity | VERIFIED | `android-identity-secure.json`; our rcgen certificate was accepted by Conscrypt |
| Pairing requested by GeekBridge, accepted on the phone, verification key identical on both screens (`48BAADE6`) | VERIFIED | `geekbridge-pair-request.json`, `android-pair-accept.json` |
| Pairing requested by the app (needs `geekbridge-ctl pair ID await` first), keys identical (`0162C13C`), desktop approves | VERIFIED | `android-pair-request.json` |
| Trusted reconnect with pinned certificates | VERIFIED | reconnected paired, `connected` phase |
| Unpair sent by GeekBridge -> the app shows "Device not paired"; unpair sent by the app -> `peer_unpaired` | VERIFIED both | `android-pair-unpair.json` |
| `kdeconnect.ping` both ways | VERIFIED | app raised its "Ping!" notification; `pings_received` = 1 |
| Clipboard desktop -> phone (read back by pasting on the phone) and phone -> desktop (menu *Send clipboard*, also sent on copy) | VERIFIED | `android-clipboard.json`, xclip on the private display |
| Share text and URL both ways | VERIFIED | `android-share-text.json`, `android-share-url.json` (the app sends a URL as `url`) |
| Share file desktop -> phone and phone -> desktop, 300 000 random bytes | VERIFIED byte-identical (SHA-256 compared on both sides) | `android-share-file.json`, payload port 1752 |
| `kdeconnect.sms.messages` pushed on an incoming SMS, `request_conversations`, `request_conversation`, `sms.request` send | VERIFIED | the sent SMS is in the phone's SMS store (type 2) and the app echoed it as `sms.messages`; messages parsed and shown by `geekbridge-ctl messages` |
| `kdeconnect.contacts.*` (uids/timestamps, then vCards) | VERIFIED | one contact parsed (name, phone, email); the app's `X-KDECONNECT-ID-DEV-<our id>` vCard line is accepted |
| Battery, notifications, telephony | NOT CLAIMED by GeekBridge: the app sends none of them (zero `kdeconnect.battery` / `notification` / `telephony` lines in the capture) and `negotiated` is exactly `ping, clipboard, clipboard.connect, share.request, contacts.response_*, sms.messages`. The tests also pin that these types would be rejected | no handler exists, so there is nothing to verify |
| Our `geekbridge.*` extensions towards the app | not sent (not negotiated) | `negotiated_outgoing` has none |
| mDNS / UDP broadcast discovery, the app dialling us, protocol-7 phones, Wi-Fi/trusted networks, multi-file batches, `share.request.update`, image clipboard | NOT tested | emulator limits |

### Interop bugs found

**None in the packet formats or the handshake**: every captured line parses, every packet we sent was
acted on. Behaviour worth knowing, none of it changed:

* **A repeated pairing request replaces the pending prompt** (`pairing.rs`; see `docs/pairing.md`). The app derives
  a new key from every request's timestamp, so tapping *Request pairing* twice moves the phone to a new key. The
  desktop now cancels the old prompt and shows the new key; an approval of the old key is refused, and at most 5
  requests per minute are honoured.
* **Phone-initiated pairing only works while the link is "awaiting"**: without `pair ID await` the app's
  request sits unread in the TCP buffer and is processed late (twice, if the user taps again). A desktop UI
  must call await automatically on every unpaired link.
* **The stock app's Contacts plugin is off by default and needs an in-app consent dialog**
  (*Plugin settings* -> *Contacts Synchronizer* -> OK); until then the app logs "Ignoring packet ... because
  no plugin can handle it" and never answers. SMS works as soon as the permissions are granted.
* A text share is echoed back by the app as a `kdeconnect.clipboard` (its share plugin copies the text and its
  clipboard plugin forwards it); `ctl clipboard push-to-phone` sent two identical clipboard packets (watcher
  plus explicit push). Both harmless.
* The app prints `TelephonyHelper` stack traces when sending an SMS on an emulator without MMS settings; the
  SMS is stored regardless.

### Still needs a real stock phone

Everything under "NOT tested", plus: the app dialling GeekBridge after receiving its UDP broadcast, trusted
network gating, battery optimisation killing the app's service, real Wi-Fi timing of the 5 s secure-identity
budget, MMS, and large SMS histories.
