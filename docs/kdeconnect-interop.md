# KDE Connect interoperability: what is verified, what is only spec-checked

Run date: 2026-11-02 (re-run after the real-device fixes: see the last section). This file records the results against stock peers; the stock **desktop** daemon was exercised
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
| `kdeconnect.notification*`, `kdeconnect.telephony*` | Historical (the 0.1.0 capture): not claimed then. Notification mirroring is now declared when the user switches it on (see "Notification mirroring" below); telephony is still NOT CLAIMED | notifications: SPEC-CHECKED only; telephony: nothing to verify |
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

`geekbridge.clipboard.{pull,item,none}`, `geekbridge.calls.{request,state,result}` (the request's `action` includes
`dial`), `geekbridge.calendar.{request,sync,write,write_result}`, `geekbridge.sms.status`,
`geekbridge.debug.{log,state,state_request}` (debug-build phones only), `geekbridge.goodbye` (below) and the empty
marker `geekbridge.heartbeat`, which a GeekBridge phone declares in every identity so the computer knows to send it
the periodic `kdeconnect.ping`. The heartbeat is a plain ping every 20 s from the computer and from the phone (the phone slows to 60 s while its
screen is off and nobody is using the link); a
peer that has heartbeated and then says nothing for 70 s is dropped (`bridge-core/src/heartbeat.rs`). A stock peer
never gets the periodic ping and is never dropped for silence.

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

### Goodbye (`geekbridge.goodbye`) and the listening-port hint

Two GeekBridge-only additions, invisible to a stock peer (`docs/reports/goodbye-and-port-hint.md`).

**Goodbye.** `geekbridge.goodbye`, body `{"reason": "user_disconnect" | "shutdown" | "unpair"}`, computer to phone.
The phone declares it in its `incomingCapabilities` (with `geekbridge.heartbeat`); the computer lists it in its
outgoing capabilities and sends it only when the negotiated list contains it, so a stock peer, which cannot
declare it, never receives one. The computer sends it just before it closes the socket on a user disconnect
(`geekbridge-ctl disconnect`, the Disconnect button, the tray) and on service shutdown (SIGTERM/Ctrl-C). The phone
acts on it only from a peer that declared a `geekbridge.*` capability and only for a known reason; anything else is
counted as an ignored packet.

* `user_disconnect`, `unpair`: the phone does not redial that computer for 5 minutes. A connection from either
  side (the user's Connect on the phone, the computer's own dial) ends the request at once, as does unpairing.
  The computer pauses its own automatic redial of that phone for the same 5 minutes, then both resume together.
  The request survives a transport restart (network change) and outlives a redial wait already in progress.
* `shutdown`: nothing is suppressed. The phone redials with its normal back-off (2 s, 4 s, ... 60 s, 12 tries) and is
  back as soon as the service is. The close shows as `link_closed` with code `peer_shutdown`; the other reasons as
  `peer_goodbye`.
* An unpair is still announced by the stock `kdeconnect.pair` unpair packet; the `unpair` reason is accepted for
  completeness and not sent today.
* A stock computer never sends the type; a stock phone is never sent it. Tests: `crates/bridge-service/tests/goodbye.rs`,
  `cross_dial.rs`, `crates/bridge-android/tests/chaos.rs`, `bridge_core::goodbye` unit tests, fuzz target `goodbye`.

**Listening port.** The computer's secure identity (inside TLS only; never the plaintext identity line) carries an extra
body key `geekbridgePort`, the TCP port it accepts connections on. The UDP identity already has `tcpPort`; this lets a
phone that was only ever dialed by the computer (and so knows just its IP) redial the right port instead of guessing
1716, which may belong to another program when the service had to move to 1717..1764. Stock peers read named keys
only and ignore it. The receiver accepts a JSON integer from 1024 to 65535 and silently drops anything else (the
identity stays valid); a port the phone dialed itself is never replaced by the hint. The phone pairs it with the
address the connection came from, so it can only point the phone at the peer's own IP. Pinned stock-peer captures
(`kdeconnect-real/*`, `kdeconnect-android-real/*`) and their tests are unchanged.

### Recent photos (`geekbridge.photos.*`)

GeekBridge-only packets (`geekbridge.photos.request`, `.list`, `.thumb`): a stock KDE Connect peer neither declares nor is
sent them, so nothing changes for it (`tests/stock_peer.rs` and `tests/android_negotiation.rs` pin that the stock lists are
unchanged and that no photo packet is negotiated with them). Shapes, limits and privacy rules:
[design/recent-photos.md](design/recent-photos.md). A full picture is an ordinary `kdeconnect.share.request` file offer.

## Battery status (`kdeconnect.battery`)

Phone battery level and charging state use the stock packets, so a stock KDE Connect peer interoperates in both
directions. Stock names and fields only; nothing GeekBridge-specific is on the wire.

| packet | direction | body |
| --- | --- | --- |
| `kdeconnect.battery` | phone to computer | `currentCharge` (integer 0 to 100, -1 = unknown), `isCharging` (bool), `thresholdEvent` (0 none, 1 low; optional) |
| `kdeconnect.battery.request` | computer to phone | `request: true`; the phone answers with a `kdeconnect.battery` packet |

* **Consent, default off on both ends.** Battery is a private signal (it says when a phone is idle or away), so it
  follows the other opt-ins. Phone: a "Share battery level" switch in Settings (off by default) plus a per-computer
  allow (`GrantFeature.Battery`, the same list as SMS and contacts; a computer paired later never inherits it). Computer:
  `Feature::Battery` consent per phone (`geekbridge-ctl consent DEVICE_ID battery on`, or the "Show phone battery"
  button on Home, offered only when the phone declared the packet).
* **Capabilities.** The phone declares `kdeconnect.battery` (outgoing) and `kdeconnect.battery.request` (incoming)
  only while its switch is on, so a stock computer is never told otherwise. The native core sends only to computers
  in the per-computer list. The computer always declares incoming `kdeconnect.battery` and outgoing
  `kdeconnect.battery.request`; negotiation still needs the peer to declare the matching side.
* **Unasked packets are dropped, not punished.** Stock phones send their level unasked; without the computer's
  `battery` consent the packet is dropped unread and does not count toward the link's rejected-packet budget. A
  malformed one (missing fields, charge outside 0 to 100, wrong types) does count. `-1` shows nothing.
* **Storage.** In memory only, on the live link's status (`links[].battery` in `geekbridge-ctl status`:
  `percent`, `charging`, `low`). It is gone when the link ends, and withdrawing the consent erases it at once.
* **Phone sends on change, with no polling and no wake lock.** The app listens to the system's sticky
  `ACTION_BATTERY_CHANGED` broadcast only while the link runs and the switch is on. A change of charging state or of
  the low flag (15 percent or less while not charging) goes out at once; a plain percentage change at most once per
  minute with one trailing send; an unchanged reading is never repeated. A computer that is allowed, connects, or
  sends `kdeconnect.battery.request` gets the current state immediately.
* **Shown on the computer** in the tray tooltip and menu status line, the Home phone card, and
  `geekbridge-ctl status --waybar` (tooltip line, `charging` / `battery-low` classes, numeric `battery` field);
  `geekbridge-ctl status` carries it in the JSON.
* **Interop evidence.** The negotiated list against the real stock Android identity capture (1.37.0,
  `android-identity-secure.json`, which lists `kdeconnect.battery` both ways) is pinned in
  `crates/bridge-service/tests/android_negotiation.rs`. That build accepts only `kdeconnect.battery` (not `.request`),
  so no request is offered to it; it sends its own state on connect. The end-to-end flow over a pinned-TLS link with a
  scripted phone is `battery_is_read_only_with_consent_asked_for_and_erased_when_withdrawn`
  (`crates/bridge-phone-sim/tests/data_link.rs`); the phone core is `crates/bridge-android/tests/data_transport.rs`,
  and the Kotlin policy is `BatteryReportTest`. **SPEC-CHECKED only** against a live stock desktop or phone: no
  stock daemon capture of battery packets exists yet (next hardware step).

## Notification mirroring (`kdeconnect.notification`, `.request`, `.action`)

Phone notifications on the computer use the stock packets, so a stock KDE Connect peer interoperates in both
directions. Typed replies (`kdeconnect.notification.reply`) are deliberately not part of this build.

| Packet | Direction | Body (stock field names) |
| --- | --- | --- |
| `kdeconnect.notification` | phone to computer | `id`, `appName`, `title`, `text`, `ticker`, `time` (decimal string of ms), `isClearable`, `silent`, `onlyOnce`, `actions` (button titles); a removal is `id` + `isCancel: true` |
| `kdeconnect.notification.request` | computer to phone | `request: true` (send what is showing) or `cancel: <id>` (dismiss it) |
| `kdeconnect.notification.action` | computer to phone | `key: <id>`, `action: <button title>` |

* **Consent, default off on both ends.** Phone: a "Show phone notifications on the computer" switch (off), Android's
  Notification access (a special access, granted in system settings), an app allow-list that starts **empty** (nothing
  leaves until the user picks apps) and a per-computer allow (`GrantFeature.Notifications`; a computer paired later never
  inherits it). Computer: `Feature::Notifications` consent per phone (`geekbridge-ctl consent DEVICE_ID notifications on`,
  the "Show phone notifications" button on Home or the tray item; nothing is switched on without a click).
* **Capabilities.** The phone declares `kdeconnect.notification` (outgoing) and `.request` + `.action` (incoming) only while
  the switch is on **and** Notification access is granted (a change reconnects to re-declare). The computer always
  declares incoming `kdeconnect.notification` and outgoing `.request` + `.action`; negotiation needs the peer's matching
  declaration. The phone core refuses to send to a computer that is not on the per-computer list.
* **Filters on the phone.** Only apps on the list; never ongoing, foreground-service, group-summary, call, progress,
  transport, alarm, "secret" (lock-screen hidden) or GeekBridge's own notifications; empty ones are skipped; an identical
  re-post is not re-sent. The Android key never leaves the phone: the computer gets an opaque id (`gb<N>`) that the phone
  maps back, bounded to 128, and a dismiss or button press is honoured only for a notification that was mirrored, is still
  showing, still from an allowed app, and (dismiss) clearable. Buttons that take typed input are never offered.
* **Bounds and storage.** Id 128, app 64, title 160, text 600 characters, 3 buttons of 48 characters, control characters
  stripped (the `notification_wire` fuzz target pins these caps). The computer keeps at most 50 per phone (8 phones) in
  memory for 3 hours; consent off, link loss or unpairing erases them. Up to 15 are sent per "send what is showing" and a
  burst of banners is rate limited (6, then one per 2 s); the rest are only listed. Replies to `request` are marked
  `silent`, so connecting never raises a flood of banners.
* **Icons.** Stock phones attach an icon as a payload; the computer never fetches it (it ignores the payload and judges the
  packet by its body), and this phone sends none. So there is no icon byte cap to exceed: size is zero both ways.
* **Privacy.** Titles and texts are never logged on either end. The desktop banner names the app and the phone; the title
  and text appear in it only when message content in notifications is enabled (the same switch as texts,
  `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` or the `sms-notify-content` file). `geekbridge-ctl notifications` is an explicit
  local query and does print them.
* **Desktop actions.** On Linux the banner is a `notify-send` with a Dismiss button (sends `.request {cancel}`) and up to 3
  of the phone's own buttons (`.action`); Windows and macOS show a plain banner and use `geekbridge-ctl notifications
  dismiss|action`. A cancel from the phone removes the entry here.
* **Unasked stock packets are dropped, not punished.** Without the consent a `kdeconnect.notification` is dropped unread and
  does not count toward the rejected-packet budget; a malformed one does.
* **Interop evidence.** Negotiation against the real stock Android identity capture (it declares `kdeconnect.notification`,
  `.request`, `.reply`, `.action`) is pinned in `crates/bridge-service/tests/android_negotiation.rs` (we negotiate
  `kdeconnect.notification` in and `.request` + `.action` out, never `.reply`); `gate.rs` has the unit test for the
  stock names; the end-to-end flow over pinned TLS is
  `notifications_need_consent_are_bounded_dismissed_and_erased_when_withdrawn`
  (`crates/bridge-phone-sim/tests/data_link.rs`); the phone core is `crates/bridge-android/tests/data_transport.rs`;
  the Kotlin policy is `NotificationMirrorTest`. **SPEC-CHECKED only** against a live stock daemon or a real phone: no
  stock capture of notification packets exists yet (next hardware step, including the Samsung restricted-settings flow).

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
| Battery, notifications, telephony | Historical capture, before battery support (see "Battery status" below; `kdeconnect.battery` is now declared and negotiated, and consent-gated). Notifications and telephony are NOT CLAIMED by GeekBridge: the app sends none of them (zero `kdeconnect.battery` / `notification` / `telephony` lines in the capture) and `negotiated` is exactly `ping, clipboard, clipboard.connect, share.request, contacts.response_*, sms.messages`. The tests also pin that these types would be rejected | no handler exists, so there is nothing to verify |
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

## After the real-device session (2026-11-02)

The real-device fixes added an application-level heartbeat (`bridge-core/src/heartbeat.rs`), a PC auto-redial
(`bridge-service/src/reconnect.rs`), `geekbridge.debug.*` packets, keepalive and capability changes. The matrix
below was re-run on the current tree against the REAL stock peers (same isolation as above; the stock Android
APK only ever inside an emulator, in a private `unshare -Urn` namespace together with the emulator, adb server and
every GeekBridge process, so the owner's live service on 1716 was never involved).

### Regression found and fixed

**The heartbeat pinged stock peers.** The PC (`daemon.rs::trusted_loop`) and the phone (`engine.rs::run_trusted`)
sent a plain `kdeconnect.ping` every 20 s to *every* peer that listed `kdeconnect.ping`, which is every stock peer.
Observed against the real desktop daemon: **16 "Ping!" notifications in 330 s** of idle. The link itself was not
cut. Fix: the periodic ping now only goes to peers that declared a `geekbridge.*` capability
(`heartbeat::is_geekbridge_peer`, unit test `only_peers_that_declare_a_geekbridge_capability_get_the_heartbeat`);
the silence limit already never applied to a peer that does not heartbeat. Re-run after the fix: **0** pings and
0 `geekbridge.*` packets towards the stock daemon and towards the stock app over 330 s.

### Results

Desktop run: `GB_IDLE_SECS=330 scripts/interop-kdeconnect.sh KC_ROOT OUT` (new scenario S8 in
`scripts/interop-kdeconnect-scenarios.sh`, wire tap via `GEEKBRIDGE_WIRE_TAP_FILE`). Emulator runs were driven by
hand with `adb`, `scripts/e2e/adbui.py` and `scripts/interop-announce.py` (injects the UDP announcement a LAN
broadcast would deliver; an emulator cannot carry broadcasts).

| # | Flow | Stock desktop daemon <-> geekbridge-service | Stock Android app (emulator) <-> geekbridge-service | GeekBridge Android debug <-> stock desktop daemon |
| --- | --- | --- | --- | --- |
| 1 | Identity (plaintext + secure), TLS | PASS (S1-S7) | PASS (app dials us, we dial the app) | PASS |
| 2 | Pairing, we/PC request, peer accepts | PASS (S1, S5) | PASS (keys equal `E708F6CF`) | PASS (daemon accepted, keys equal `DC6CB12D`; phone requested) |
| 3 | Pairing, peer requests, we approve | PASS (S2) | PASS (keys equal `F501CA46`) | not run (daemon-initiated request is never shown by the phone's address-connect; see note) |
| 4 | Unpair from the peer | PASS (S1) | PASS (`peer_unpaired`) | not run |
| 5 | Ping both ways | PASS | PASS | PASS daemon -> phone; phone -> daemon not exercised |
| 6 | Clipboard both ways | PASS | PASS (PC text adopted by the app and echoed back) | NOT RUN (offscreen Qt clipboard, no way to set the phone clipboard) |
| 7 | Share text/URL | PASS | PASS both ways | PASS both ways (URL daemon -> phone, text phone -> daemon) |
| 8 | Share file 300 kB, both ways | PASS byte-identical | PASS byte-identical (md5 equal) | NOT RUN |
| 9 | SMS (`sms.messages`, requests) | n/a (desktop cannot answer) | PASS (injected SMS listed, `rejected_packets` 0) | n/a |
| 10 | Contacts | n/a | NOT RUN (stock app needs its in-app consent dialog, as documented above) | n/a |
| 11 | Idle 330 s, peer sends no heartbeat: link NOT cut | PASS | PASS (zero wire traffic, still connected, `rejected_packets` 0) | PASS (still reachable + paired; a URL shared after idle arrived; phone sent 0 pings) |
| 12 | We send NO `kdeconnect.ping` / `geekbridge.*` to the stock peer in idle | PASS after the fix (FAIL before: 16 pings) | PASS (0 lines on the wire) | PASS (0 `pingReceived` on the daemon) |
| 13 | Stock peer's own traffic still works after idle | PASS (daemon ping mid-idle and after) | PASS | PASS (daemon ping and URL after idle) |
| 14 | Unknown `geekbridge.*` packets ignored, link stays | PASS (S1 `ext`, daemon discards) | PASS (not negotiated, none sent) | n/a |
| 15 | Forced drop, then automatic redial | PASS (S8c, `ss -K`, redial < 1 s) | PASS (link cut with `ss -K`, service redialled the app) | not run |
| 16 | Cross-dial: peer dials us while we dial it | PASS (3 rounds, exactly one link, stable 20 s) | PASS (the app dialled on every injected announcement for 40 s, one link remained and pinged) | not run |
| 17 | Wi-Fi flap (`svc wifi/data disable/enable`) | n/a | NOT MEANINGFUL: the emulator's slirp/adb-forward links survive radio toggles, so neither side noticed; the drop was therefore forced at the TCP level (row 15) | not run |

Real-packet fixtures and unit tests: `kdeconnect_real`, `kdeconnect_android_real` (bridge-kde) and
`android_negotiation` (bridge-service) all PASS; the full `bridge-core`, `bridge-kde`, `bridge-service`,
`bridge-android` test suites pass (a `share_api` payload-port test failed once under load and passed on rerun),
fmt and `clippy -D warnings` are clean. No Kotlin was touched.

Note on row 3 / GeekBridge phone with a stock desktop: the phone's *Developer options > Connect* button only
tests a connection ("this does not pair"); a request from the daemon timed out there. The supported route is the
setup wizard's **Look for computers**, which lists the stock daemon and pairs with the **Pair** button (works).
The phone-side prompt must be answered within the daemon's 30 s pairing window.
