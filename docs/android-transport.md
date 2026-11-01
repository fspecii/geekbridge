# Android phone transport (`bridge-android`, `transport_*` actions)

The Rust side of the Android companion can now open the KDE Connect protocol 8 transport (TCP + TLS,
certificate-pinned pairing) through the same `nativeRequest` JSON boundary as the validation actions in
`android-contract.md` / `android-native.md`. It reuses `bridge-kde` for the handshake, TLS pinning,
pairing state machine, trust store and identity files; nothing of the protocol or its security logic is
re-implemented in `bridge-android`.

## What this is, and is not

**Implemented and tested on a host with generated peers:** explicit start/stop, inbound listener,
explicit outbound dial, pairing in both directions with an explicit local decision, trusted reconnect,
unpair (both directions), one connection per device, bounded state, optional UDP discovery, a `ping`
packet in both directions.

**Not done / not proven:**

- It has not run on ART, an emulator or a phone, and has not interoperated with a real KDE Connect
  device or with the desktop apps. The cdylib was cross-built and linked for both ABIs, nothing more.
- It advertises `kdeconnect.ping` and `kdeconnect.share.request` (incoming and outgoing) and nothing else.
  Sharing is described in [sharing.md](sharing.md) and the `share_*` actions in [android-contract.md](android-contract.md).
  No SMS, contacts, calendar, telephony or notification packets exist here; those need their own
  reviewed work and must not be advertised until they do. Clipboard packets are declared only when `transport_start` is given a
  `clipboard` object (see [Clipboard actions](#clipboard-actions)). Cellular calls and SMS are not part of this transport.
- It is not a background service. There is no foreground service, no persistent link and no reconnect
  loop; see [Lifecycle](#lifecycle).
- Discovery is optional, off by default, and never connects or trusts anything on its own.

`version` and `validate_*` stay side-effect free. Creating the dispatcher (loading the library) starts no
thread, socket or file; the first `transport_start` does.

## Model

```
Kotlin ──nativeRequest(JSON)──► handle_request ──► Host ──► tokio runtime (2 workers, "gb-transport")
                                   quick, bounded            accept loop · dials · pairing · discovery
```

* One `Host` per process (per `Dispatcher` in tests). States: `stopped → starting → running → stopping`.
* `transport_start` creates the runtime and binds sockets, then returns. It never waits for a peer.
* Every other command inspects/changes small in-memory state and returns at once. Handshakes, pairing
  and I/O run on the runtime and are observed through `transport_status`.
* There are no JNI callbacks. Kotlin polls `transport_status` (see [Polling](#polling)).
* `transport_stop` rejects every pending prompt, closes every socket, ends the runtime (waits at most
  2 s) and returns. A later `transport_start` sees no connection, prompt, event or attempt from before;
  request ids never repeat inside a process.
* Dropping the dispatcher also stops the transport.
* The crate changes no global panic hook. A panic inside a request becomes `internal_error`; a panic in
  a runtime task is contained by tokio and ends only that task.

### Connection phases

```
inbound TCP   ──► pairing_await ──(peer asks)──► prompt ──(local approve + peer accepts)──► trusted
outbound dial ──► parked ─ transport_pair_request{request|await} ─► pairing_request | pairing_await ─► …
already-pinned peer (either direction) ───────────────────────────────────────────────► trusted
```

* **Inbound unpaired** connections start listening for the peer's pair request immediately
  (`pairing_await`). They cannot ask the peer from this side; the 30 s pairing window closes them if the
  peer says nothing.
* **Outbound unpaired** connections are `parked`: TLS is up, nothing is read or sent until the UI calls
  `transport_pair_request`. A parked connection that is not used for 60 s is closed (`link_closed`,
  code `park_timeout`).
* `trusted` means the peer's exact certificate is pinned. Only then are feature packets read or sent.
  Parsing a response never implies `trusted`; check `connections[].trusted` or `paired[]`.

### Trust rules (enforced by `bridge-kde`, plus these in `bridge-android`)

* A pairing completes only after an explicit `transport_pair_decision` with `approve: true` naming the
  exact `request_id`, `connection_id`, the displayed `verification_code` and the displayed
  `peer_cert_sha256` — and, for requests we send, the peer's accept. No auto-approve exists. A crossed
  request (both sides ask at once) is not treated as an accept; it times out.
* Wrong code/fingerprint is refused and counted; the third wrong try rejects the request. A decision
  after `expires_in_ms`, for another connection, a stale `request_id` or after stop/restart fails with
  `unknown_request` / `request_expired` and changes nothing.
* A peer certificate must be byte-equal to the pinned one for a known device id; otherwise the handshake
  fails (`pin_mismatch` on a dial; counted in `counters.inbound_failed` inbound). Discovery, a handshake
  or a connection never writes the trust store; only a completed local approval does.
* One connection per device id. A second *unpaired* connection for a device is dropped
  (`counters.rejected_duplicates`). A second *trusted* connection replaces the old one if both were
  opened by the same side (a reconnect), and otherwise the connection opened by the device with the
  lexicographically smaller id survives on both ends, so a simultaneous cross-dial leaves one link.
* Packet lines are capped at 64 KiB (the protocol maximum is 32 MiB). A longer line closes the
  connection (`link_closed`/`packet_too_large`) and does not touch trust.
* The identity (`kde/identity.pem`, contains the private key) and trust store (`kde/trusted_peers.json`)
  live in a `0700` directory created by `bridge-kde`, which refuses a directory that is loose, a symlink
  or owned by another user. Neither the key, the state path nor any peer-supplied text appears in an
  error; errors are fixed strings. The crate writes no logs.

## Requests

Same envelope as the other actions: `{"version":1,"action":"…", …}`; unknown fields, duplicate keys
(at any depth, including inside `listen`/`discovery`), wrong types, version != 1 and oversized input
fail exactly as before. Transport requests are additionally limited to 16 KiB (`request_too_large`).
Omit optional keys; an explicit `null` is `invalid_field`. Results below are the `result` member.

### `transport_start`

| field | type | notes |
| --- | --- | --- |
| `state_dir` | string, required | absolute path of an existing directory, ≤ 1024 bytes, no `..`. Pass `context.filesDir.absolutePath`. Native code creates and uses the subdirectory `<state_dir>/kde` (mode 0700): `filesDir` itself may be group-accessible on Android, which `bridge-kde` would refuse. Symlinked parents (`/data/user/0`) are fine. |
| `device_name` | string, required | ≤ 128 chars; the protocol strips punctuation (`" ' , ; : . ! ? ( ) [ ] < >`) and control characters and keeps 32 characters. Empty after that → `invalid_device_name`. |
| `listen` | object, optional | `enabled` (bool, default true), `host` (IP literal, default `0.0.0.0`), `port` (0–65535, default 1716; 0 picks a free port). With `enabled: false` neither `host` nor `port` may be given: outbound-only. |
| `discovery` | object, optional | `listen` (bool, default false), `announce` (bool, default false), `port` (default 1716; 0 picks a free UDP port), `announce_to` (`ip:port`, only with `announce`; default `255.255.255.255:<port>`). Announcing needs the TCP listener (`discovery_requires_listener`). |
| `allow_loopback` | bool, default false | **Development only.** Allows 127.0.0.0/8 for inbound peers, dials, listen/announce targets and discovery datagrams. Never enable it in a release build. |

Result: `{state:"running", device_id, device_name, cert_sha256, identity_created, listen:{host,port}|null,
discovery:{listening,port,announcing,error}, capabilities:{incoming:["kdeconnect.ping","kdeconnect.share.request"],outgoing:[same]}}`.

`device_id` and `cert_sha256` are public and are what the peer shows. The first start generates the
identity (one key generation plus a few small file writes, typically a few milliseconds to tens of
milliseconds on a phone): call it from a background dispatcher, never the main thread. A failed start
leaves the host `stopped` and restartable. A bind failure of the optional discovery socket does not fail
the start; it is reported as `discovery.error = "discovery_bind_failed"`.

Errors: `already_running`, `busy`, `invalid_state_dir`, `invalid_device_name`, `invalid_field`,
`loopback_not_allowed` (listening on loopback without `allow_loopback`), `discovery_requires_listener`,
`address_rejected` (announce target), `insecure_state`, `identity_unusable`, `state_corrupt`,
`state_unavailable`, `unsupported_platform`, `listen_failed`, `runtime_failed`.

### `transport_status`

Optional `since` (u64, default 0): only events with `seq > since` are returned. Never blocks on I/O and
never starts anything. When not running it returns only `{state: "stopped" | "starting" | "stopping"}`.
Running:

```jsonc
{
  "state": "running",
  "local": { "device_id": "…", "device_name": "…", "cert_sha256": "…" },
  "listen": { "host": "0.0.0.0", "port": 1716 },            // null when outbound-only
  "discovery": { "listening": false, "port": null, "announcing": false, "error": null },
  "discovered": [ { "device_id", "name", "device_type", "ip", "tcp_port", "age_ms", "paired" } ],
  "connections": [ {
      "connection_id": 3, "role": "acceptor" | "initiator",
      "phase": "parked" | "pairing_request" | "pairing_await" | "trusted", "trusted": false,
      "peer": { "device_id", "name", "device_type", "cert_sha256", "ip" },
      "age_ms": 1200, "pings_received": 0, "ignored_packets": 0 } ],
  "prompts": [ {                                           // pending local decisions only
      "request_id": 7, "connection_id": 3, "direction": "incoming" | "outgoing",
      "peer_device_id", "peer_name", "peer_cert_sha256", "own_cert_sha256",
      "verification_code": "1A2B3C4D", "timestamp": 1737228658, "expires_in_ms": 24000 } ],
  "attempts": [ { "device_id", "age_ms" } ],               // dials still handshaking
  "paired": [ { "device_id", "name", "device_type", "cert_sha256", "paired_at_unix" } ],  // ≤ 64
  "paired_error": false,
  "events": [ { "seq", "kind", "connection_id"?, "device_id"?, "code"? } ],
  "transfers": [ … ],                                       // sharing, see android-contract.md
  "last_event_seq": 12,
  "counters": { "inbound_dropped": 0, "inbound_failed": 0, "rejected_duplicates": 0 }
}
```

`peer.*`, `discovered[].name` and `verification_code` are shown to the user; treat them as untrusted
display text (the name is already stripped of control characters and capped at 32). The state fields
are authoritative; `events` are a small (64) advisory ring for explaining *why* something changed
(an unauthenticated peer cannot fill it: inbound timeouts and disconnects before any exchange raise no
event). `paired` is read after the rest of the snapshot, so it is never older than the events beside it.
`counters` count unauthenticated activity that is otherwise invisible.

Event `kind` / `code`:

| kind | meaning | codes |
| --- | --- | --- |
| `pairing_prompt` | a prompt appeared | — |
| `paired` | trust was recorded and the link is trusted | — |
| `pairing_failed` | pairing ended without trust | `pairing_rejected_locally`, `pairing_rejected_by_peer`, `pairing_timeout`, `clock_skew`, `pairing_malformed`, `pairing_unexpected_packets`, `pairing_binding_changed`, `link_closed`, `store_error`, `tls_failed`, … |
| `connect_failed` | a `transport_connect` dial failed | `timeout`, `io_error`, `tls_failed`, `pin_mismatch`, `identity_mismatch`, `target_mismatch`, `unsupported_protocol`, `duplicate_connection`, `too_many_connections`, … |
| `link_closed` | a connection ended | `packet_too_large`, `peer_closed`, `io_error`, `malformed_packet`, `park_timeout`, … |
| `peer_unpaired` | the peer unpaired us; the local record is already gone | — |
| `peer_requested_repair` | the peer forgot us and asks to pair again; unpair locally, then pair | — |
| `unpaired` | `transport_unpair` ran | — |

Unknown `kind`/`code` values must be treated as generic. `pin_mismatch` for a device you have paired is
a security-relevant signal (a different certificate under a known id); surface it, do not retry silently.

### `transport_stop`

No fields. Result `{state:"stopped", was_running: bool}`; idempotent. Returns `busy` only while another
start/stop is in progress.

### `transport_connect`

`host` (IP **literal**, no DNS: `invalid_endpoint` otherwise; IPv6 scope ids are not supported), `port`
(1–65535), `device_id` (the peer's id, 32–38 chars `[A-Za-z0-9_-]`, `invalid_device_id`). Result
`{accepted:true, device_id}` immediately; the dial runs in the background (20 s ceiling). The address
must satisfy the address policy — private, link-local, CGNAT and unique-local addresses, plus loopback
only with `allow_loopback` (`address_rejected`). Public addresses are always refused. The device id is a
claim you chose to dial: the peer must prove it with a certificate whose common name matches, and a
known device must present its pinned certificate.

On success the connection appears in `connections` as `trusted` (already paired) or `parked`.
Errors: `not_running`, `busy`, `already_connected` (connection or dial to that device exists),
`too_many_connections`, `invalid_endpoint`, `invalid_device_id`, `address_rejected`. A failed dial
appears as a `connect_failed` event, not as an error here.

### `transport_pair_request`

`connection_id` and optional `mode`: `"request"` (default; send a pair request and show our own prompt
with `direction: "outgoing"`) or `"await"` (send nothing; wait for the peer's request). Only valid for a
`parked` connection; otherwise `wrong_state`; unknown id → `unknown_connection`. Result
`{connection_id, phase}`. Then wait for a prompt in `transport_status`.

### `transport_pair_decision`

`request_id`, `connection_id`, `approve` (bool). When `approve` is true both `verification_code` and
`peer_cert_sha256` are required (`missing_field` otherwise): they must be the values the user was
shown for that prompt (case-insensitive; the code is trimmed). `approve: false` needs only the ids.
Result `{decision: "approved" | "rejected"}`.

**`approved` is only the local half.** The pairing still needs the peer's accept (for a request we sent)
and can still fail; wait for `connections[].trusted` / `paired[]` / the `paired` or `pairing_failed`
event. Errors: `unknown_request` (also for a replayed, stale, mismatched or post-restart id),
`request_expired`, `verification_mismatch`, `certificate_mismatch`, `too_many_attempts` (3 wrong tries
reject the request), `not_running`.

### `transport_unpair`

`device_id`. Deletes the trust record first, then closes any connection to that device; on a trusted
link the peer is told on a best-effort basis. Result `{was_paired, disconnected}`. Works for a device
that is not connected. `store_error` if the record could not be removed.

A trusted peer that sends `kdeconnect.pair {pair:false}` is treated the same way from the other side: its record is
deleted, its transfers are cancelled, the connection ends and the event is `peer_unpaired`. The peer can only do this
while connected; a computer that unpairs while the phone is out of reach cannot tell it, and the phone keeps its
record until it is removed there.

### `transport_wait`

`{since?, timeout_ms?}` (max 600 s, default 30 s) -> `{last_event_seq, stopped}`. Blocks the calling thread (never the
dispatcher; other requests keep working) until the newest event sequence differs from `since`, the transport stops, or
the timeout passes. `not_running` when it is not running. Blocking means no timer runs in between, which is how the app
hears about a change while nothing polls: the app's only client is `LinkRuntime`, which then fetches one
`transport_status`. A peer's `{pair:false}` reaches `AppState.recordPaired` this way, and with no computer left the
foreground service stops.

### `transport_disconnect`

`connection_id`. Closes the connection without touching trust (a parked connection, a pairing in
progress, or a trusted link). Result `{disconnected:true}`; `unknown_connection` otherwise.

### `transport_ping`

`connection_id`. Queues one `kdeconnect.ping` on a trusted connection (`not_trusted` otherwise,
`busy` if its queue is full). Result `{queued:true}`. Received pings only increment
`connections[].pings_received`; their content is not kept. Any other packet type from a trusted peer
is dropped and counted in `ignored_packets`.

### `version`

Unchanged, plus `transport_actions` (the list above) and `limits.max_transport_request_bytes`.
`validation_only: true` remains true of `actions` (the stateless ones); the transport actions are the
opposite and do nothing until `transport_start`.

## Limits

| what | limit |
| --- | --- |
| transport request | 16 384 bytes (all requests: 394 240) |
| `state_dir` / `device_name` / code, fingerprint | 1024 bytes / 128 chars / 64 chars |
| connections / dials in flight / unpaired links | 16 / 4 / 8 |
| concurrent inbound handshakes | 8 (extra TCP connections are closed unread; `inbound_dropped`) |
| pending prompts | 4 |
| prompt wrong tries | 3 |
| events / discovered peers / `paired` listed | 64 / 32 / 64 |
| packet line (trusted) / identity line | 64 KiB / 8 192 bytes |
| handshake | TCP 5 s, plaintext identity 5 s, TLS 10 s, secure identity 5 s, 20 s overall |
| pairing | 30 s while waiting for the peer, 25 s for the local user after the peer's request |
| parked outbound connection | 60 s |
| discovery datagram / rate / table TTL / announce period | 8 192 bytes / 64 per second / 120 s / 30 s |
| runtime | 2 worker threads, 2 blocking threads, shut down within 2 s on stop |

## Discovery (optional)

Only with `discovery.listen` / `discovery.announce`.

* **Announce** sends a KDE-shaped `kdeconnect.identity` datagram (protocol 8, our device id and name,
  `deviceType: phone`, ping and share capabilities, `tcpPort`) once the TCP listener is bound, then every
  30 s while running. Default target is the limited broadcast `255.255.255.255:<port>`; any other
  target must satisfy the address policy.
* **Listen** binds UDP `0.0.0.0:<port>` and lists valid datagrams in `discovered` (device id, name,
  type, source ip, advertised TCP port, age, whether the device is already paired). Datagrams from
  outside the address policy, our own announcements, other protocol versions, ports below 1024,
  oversized or malformed input and anything beyond 64/s are ignored. It never dials: the UI calls
  `transport_connect` with a `discovered` entry, and pairing still needs the full local approval.
* Receiving broadcasts on most phones needs a `WifiManager.MulticastLock` (acquire while
  `discovery.listening` is true, release on stop). Port 1716 may already be taken (stock KDE Connect on
  the same phone): that is `discovery.error = "discovery_bind_failed"`, not a failed start.
* Not covered: mDNS, IPv6 multicast, interoperability with a real KDE Connect app, behaviour on a real
  Wi-Fi network.

## Development: emulator and desktop peer

Never ship `allow_loopback: true`; gate it on `BuildConfig.DEBUG`. Android's emulator reaches the host
at `10.0.2.2`, which is an ordinary private address:

1. **Emulator dials the desktop app** (no loopback opt-in needed on Android). The desktop app listens on
   a TCP port `P` on the host; start Android with `listen: {enabled: false}` or any listener, then
   `transport_connect {host: "10.0.2.2", port: P, device_id: <desktop id>}` and
   `transport_pair_request`. The desktop sees the connection come from the emulator's NAT, which is
   normally reported as `127.0.0.1` on the host, so the *desktop* side needs its own loopback opt-in
   (verify; not exercised here).
2. **Desktop dials the emulator.** `adb forward tcp:H tcp:1716`, start Android with
   `listen: {host: "0.0.0.0", port: 1716}` and `allow_loopback: true` (adbd's hop reaches the app from
   the guest's loopback), and have the desktop connect to `127.0.0.1:H` with the Android `device_id`.
3. `adb reverse tcp:D tcp:P` plus `transport_connect {host: "127.0.0.1", port: D}` also works with
   `allow_loopback: true`.

Discovery by broadcast does not cross the emulator's NAT; use explicit endpoints.

## Kotlin integration checklist

All of this is for the parent once the UI exists; nothing under `android/**` was changed for it.

### Manifest additions (not yet present; do not forget)

| permission | needed for |
| --- | --- |
| `android.permission.INTERNET` | **required.** Without it socket creation fails (`listen_failed`, `io_error`). Declared in the manifest (also used by the update check, which is outbound HTTPS only and never listens). |
| `android.permission.ACCESS_NETWORK_STATE` | recommended: to start only on Wi-Fi/LAN and to restart when the network changes. |
| `android.permission.CHANGE_WIFI_MULTICAST_STATE` | only if `discovery.listen` is used (`MulticastLock`). |

Not needed: cleartext/network-security-config (it governs HTTP stacks, not raw sockets), a foreground
service (this phase stops when the app leaves the foreground), `NEARBY_WIFI_DEVICES`, location.
Android has been moving toward gating local-network access behind a runtime permission for newer
target SDKs; the app targets 36 and nothing here was tested against that policy. Re-check before
raising `targetSdk`. The existing `allowBackup=false` and `data_extraction_rules.xml` (which exclude
the `file` domain for cloud backup and device transfer) keep `files/kde/identity.pem` off backups;
keep it that way.

### Threading

* Call `nativeRequest` for every `transport_*` action from a background dispatcher (`Dispatchers.IO`),
  never the main thread. `transport_start` does a few small file operations and, the first time, key
  generation; `transport_stop` can wait up to 2 s for the runtime. `status`, `connect`, `pair_*`, `ping`,
  `unpair` and `disconnect` return immediately.
* Calls may overlap from several threads; `start`/`stop` racing each other get `busy`.
* Serialise only what you need to: the native side holds no lock across I/O.

### Lifecycle

1. App-level foreground (`ProcessLifecycleOwner` `ON_START`, not an `Activity`, so rotation does not
   restart it): `transport_start` with `state_dir = filesDir.absolutePath`, a device name (e.g.
   `Settings.Global.DEVICE_NAME` or `Build.MODEL`) and the listener you want. Acquire the
   `MulticastLock` only if discovery listening was requested and succeeded.
2. While running, poll `transport_status`.
3. App-level background (`ON_STOP`): `transport_stop`, release the `MulticastLock`. This rejects pending
   prompts and closes all links by design. Do not rely on any link or prompt surviving it.
4. Process death needs no cleanup: only the identity and trust records persist.
5. After the screen/Doze wakes the app, assume links are dead and `transport_connect` again. Known
   limit: `bridge-kde` sets TCP keepalive intervals, retries and `TCP_USER_TIMEOUT` only for
   `target_os = "linux"|"macos"`, and Android is `target_os = "android"`, so a silently dead link may
   take the kernel default (many minutes) to be noticed instead of about a minute. The fix is adding
   `target_os = "android"` to those two `cfg`s in `crates/bridge-kde/src/link.rs`; it was not made here
   (not this crate's file).

### Polling

Foreground UI: `transport_status` every 500 ms while a pairing sheet or connect progress is visible,
every 2 s otherwise, passing the last `last_event_seq` as `since`. Never poll while stopped/backgrounded.
The response is bounded (well under 100 KiB at the limits above).

### Pairing UI

* A new entry in `prompts` is a request for a decision. Show `peer_name`, `peer_device_id`, the
  `verification_code` in large type, `expires_in_ms`, and ask the user to compare the code with the one
  on the other device. For `direction: "outgoing"` the same applies.
* **Send back what was displayed.** On "Pair", call `transport_pair_decision` with the `request_id`,
  `connection_id`, `verification_code` and `peer_cert_sha256` of the prompt *as rendered*, not a fresh
  read, so a prompt that changed under the dialog fails instead of being approved. "Decline" sends
  `approve: false` with the ids only.
* Treat `unknown_request` / `request_expired` as "the request is gone"; dismiss the dialog.
* After approval keep the sheet in a "waiting for the other device" state until the connection is
  `trusted` or a `pairing_failed` event arrives. Never show "paired" from the decision response.
* Outbound: `transport_connect` → wait for a `parked` connection (or a `connect_failed` event) → user
  taps Pair → `transport_pair_request` → prompt appears.
* Show `pin_mismatch` and `peer_requested_repair` as security-relevant states, not generic errors.

### Call sketch

```kotlin
// Wrapper to add next to CoreClient; all calls off the main thread.
private fun call(action: String, build: JSONObject.() -> Unit = {}): JSONObject {
    val req = JSONObject().put("version", 1).put("action", action).apply(build)
    return JSONObject(NativeBridge.nativeRequest(req.toString()))
}
// ok: resp.getBoolean("ok") → resp.getJSONObject("result"); else resp.getJSONObject("error").getString("code")

call("transport_start") {
    put("state_dir", context.filesDir.absolutePath)
    put("device_name", deviceName)
    put("listen", JSONObject().put("port", 1716))                // omit for the default
    if (BuildConfig.DEBUG) put("allow_loopback", true)
}
val st = call("transport_status") { put("since", lastSeq) }
call("transport_connect") { put("host", "10.0.2.2"); put("port", 40000); put("device_id", id) }
call("transport_pair_request") { put("connection_id", cid) }     // optional "mode": "await"
call("transport_pair_decision") {
    put("request_id", p.getLong("request_id")); put("connection_id", p.getLong("connection_id"))
    put("approve", true)
    put("verification_code", p.getString("verification_code"))
    put("peer_cert_sha256", p.getString("peer_cert_sha256"))
}
call("transport_stop")
```

## Error codes

Envelope codes are unchanged (`request_too_large`, `malformed_json`, `invalid_request`,
`unsupported_version`, `missing_field`, `invalid_field`, `unknown_action`, `unknown_field`,
`internal_error`, …). Transport additions, all with fixed messages that never echo input, paths,
addresses, names, codes or fingerprints:

`not_running`, `already_running`, `busy`, `invalid_state_dir`, `invalid_device_name`,
`invalid_endpoint`, `invalid_device_id`, `loopback_not_allowed`, `discovery_requires_listener`,
`address_rejected`, `already_connected`, `too_many_connections`, `unknown_connection`, `wrong_state`,
`not_trusted`, `unknown_request`, `request_expired`, `verification_mismatch`, `certificate_mismatch`,
`too_many_attempts`, `insecure_state`, `identity_unusable`, `state_corrupt`, `state_unavailable`,
`unsupported_platform`, `store_error`, `listen_failed`, `runtime_failed`. Treat unknown codes as a
generic failure.

## Verification

Host tests (`cargo test -p bridge-android`): 11 unit, 30 validation/JNI-contract (unchanged), 29
transport integration, 1 process-wide `handle_request` test. The integration tests run a real
`bridge_kde::Endpoint` with generated certificates in its own runtime as the "desktop" and cover:
nothing starts before `transport_start`; start/status/stop/restart (identity stable, port released,
no prompt/connection/event/request id survives, in-flight dials closed, concurrent status during
start/stop); strict schemas (unknown/duplicate/nested/oversized/typed fields, paths, names, policy);
inbound and outbound pairing with approve and refuse on each side, exact code/certificate/request
binding, replay, wrong-attempt limit, no pairing without a decision, no packet actions before trust;
ping both ways, other packet types ignored; trusted reconnect and replacement; local and peer unpair;
a different certificate under a known id refused; duplicate unpaired connection refused; loopback refused
by default (inbound, dial, listen, discovery); an oversized packet closing the link without losing trust;
discovery listing without dialling, announce shape and ordering, bind failure. Two guarded mutations
(skipping the code check, allowing duplicate connections) fail the matching tests. 200 runs of the
transport tests under 8-way parallel load passed after the fixes noted below.

Android: `cargo check -p bridge-android --target aarch64-linux-android` and `x86_64-linux-android`, and a
release `cdylib` link for both with the project toolchain (`scripts/android-env.sh`): `libgeekbridge_android.so`
exports `Java_dev_geekbridge_android_NativeBridge_nativeRequest`, `PT_LOAD` alignment is `0x4000`,
about 4.7 MB unstripped per ABI, `NEEDED` is only `libdl`, `libm`, `libc`. The one libc import newer than
API 26 is `getrandom`, and it is a weak import (the `getrandom` crate checks for it at run time), so it
does not prevent loading on API 26/27. Nothing was run on ART.

`cargo clippy -p bridge-android --all-targets --no-deps -- -D warnings` is clean. Without `--no-deps`
the run stops earlier in `bridge-core/src/ipc.rs` (`clippy::chunks_exact_to_as_chunks`), unrelated to
this crate.

Notes from building this:

* The test peer and the phone share `bridge-kde`, so the tests show the actor drives it correctly, not
  that a different implementation interoperates with it (that is what the stock-peer checks in
  `docs/kdeconnect-interop.md` are for).
* While this was being written the root workspace briefly failed to resolve (a new member,
  `bridge-calls-macos`, asks `objc2-core-audio` for a feature it does not have), so the final runs used
  an isolated workspace holding only `bridge-core`, `bridge-kde` and `bridge-android` with a copy of
  `Cargo.lock`. The crate's own dependency entry in the root lock (`bridge-kde`, `tokio`, `tempfile`)
  matches `Cargo.toml`.
* Found and fixed by running the tests under load: the status snapshot used to read the trust store
  before the event log, so an event could be shown next to a stale `paired` list; and the test
  harness gives each phone its own 127.x.y.z listen address because Linux hands a just-freed ephemeral
  port to the next `bind(0)`, which made "the port is closed after stop" probes race other tests.


## Clipboard actions

Optional; negotiated only when `transport_start` carries `clipboard: {enabled?: bool = true, max_image_bytes?: u64}`
(unknown keys are refused). Then the identity declares `kdeconnect.clipboard`, `kdeconnect.clipboard.connect`,
`geekbridge.clipboard.pull`, `geekbridge.clipboard.item` (incoming) and `kdeconnect.clipboard`,
`geekbridge.clipboard.item`, `geekbridge.clipboard.none` (outgoing), and the packet line limit rises from 64 KiB to 1 MiB.
Full behaviour and policy: [clipboard-sync.md](clipboard-sync.md).

* `transport_clipboard_config {enabled?, max_image_bytes?}`: switch handling on/off at runtime. Off drops queued events and
  makes the core answer pulls with `none: disabled` by itself.
* `transport_clipboard_wait {since?, timeout_ms?}`: blocks the calling thread (never the dispatcher; other requests keep
  working) until an event newer than `since` exists, the transport stops, or `timeout_ms` (max 55 s) passes. Events with
  `seq <= since` are released first. Result `{events, last_seq, stopped, enabled}`; events are
  `{seq, kind: "pull", connection_id, device_id, request_id, max_age_ms}` or
  `{seq, kind: "item", item: text|url|html|image, device_id, text?, html?, path?, mime?, size?}` (image `path` is in
  `<state_dir>/clipboard-cache/`; delete it after use). At most 8 events are kept.
* `transport_clipboard_send {connection_id?, request_id?, none? | kind, text?, html?, image_fd?, age_ms?}`: the only way
  anything leaves the phone. `request_id` answers a pull and needs its `connection_id`; `none` is one of `locked`, `expired`,
  `empty`, `needs_gesture`, `disabled`, `too_large`, `unsupported`, `sensitive`. Without `connection_id` an explicit send goes
  to every trusted computer (an image only to the first). Text/HTML are validated like received ones (`too_large`,
  `invalid_field`); an image is a duplicated descriptor whose first bytes must be PNG/JPEG (`unsupported_type`) and whose
  size must be within the limit (`too_large`). Peers that do not speak `geekbridge.clipboard.item` get plain text through the
  stock packet, only for explicit sends. Errors also include `clipboard_disabled`, `not_connected`, `unknown_connection`.
  This one action may exceed the 16 KiB transport request limit (the overall request limit applies).

## Call control actions

Optional; negotiated only when `transport_start` carries `calls: {enabled?: bool = false}` (unknown keys refused). Then the
identity additionally declares `geekbridge.calls.request` (incoming) and `geekbridge.calls.state`,
`geekbridge.calls.result` (outgoing). Full behaviour, permissions and honesty rules: [call-control.md](call-control.md).

* `transport_calls_config {enabled}`: switch call control on/off at runtime. Off drops queued events and the last report,
  wakes a parked waiter, and makes the core answer requests with `permission_missing` by itself.
* `transport_calls_report {state, number?, name?, can_answer?, can_end?}`: the phone's coarse state (`idle`, `ringing`,
  `offhook`). Validated with the same parser the computer uses (an idle report drops the party; fields ≤ 128 bytes, no
  control characters). The latest report is sent to every trusted computer now and to any that connects later.
  Errors: `calls_disabled`, `invalid_field`, `unknown_field`.
* `transport_calls_wait {since?, timeout_ms?}`: blocks the calling thread (never the dispatcher) until an event newer than
  `since` exists, the transport stops, or `timeout_ms` (max 55 s) passes. Result `{events, last_seq, stopped, enabled}`;
  events are `{seq, kind: "request", connection_id, request_id, action: answer|reject|hangup}`. At most 8 are kept.
* `transport_calls_result {connection_id, request_id, failure?}`: tells the computer how a request went; `failure` is one of
  `permission_missing`, `no_such_call`, `failed` (absent = done). Errors: `invalid_field`, `unknown_connection`, `busy`.

