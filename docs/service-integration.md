# Service integration: daemon, control API, client wiring

Status as of 2026-11-02 (the request table below is the original core; later ops are listed after it). `geekbridge-service` now owns the TLS identity, the trust store and the one live
link per phone, and exposes them through an **authenticated loopback control API**. The old public
discovery API is unchanged. Verified with generated `bridge_kde` peers, scripted phones and the Android emulator;
**no real phone has been paired** (see "Gaps").

## Two listeners, two trust levels

| | Public discovery API | Control API |
|---|---|---|
| Port | `--api-port`, default `17380` | `--control-port`, default `17381` (`$GEEKBRIDGE_CONTROL_PORT`) |
| Bind | `127.0.0.1` | `127.0.0.1` (never a remote or wildcard address, no CORS) |
| Auth | none (read-only, exact `GET /v1/discovery HTTP/1.0\r\n\r\n`) | per-user random token, HMAC challenge-response |
| Content | untrusted UDP announcement metadata (`bridge_core::Snapshot`) | pairing, links, challenges, consent, events |
| Client | `bridge_core::fetch_snapshot(port)` | `bridge_core::ipc::*` |

The public endpoint never carries phone content or commands, never names a paired/linked state, and its
JSON shape (`api_version`, `capabilities_truncated`, `discovery_port`, `discovery_error`, `devices`) is
byte-for-byte what the existing apps parse. Pairing does not create or alter discovery records
(`tests/control_api.rs::public_api_stays_read_only_discovery_metadata`, plus all 20 `discovery_api` tests).

A separate port (rather than extra routes on the public one) keeps the strict public parser as small as
it was and makes "nothing but discovery on 17380" a property of the code, not of a route table.

## Starting the service

```
geekbridge-service [--state-dir DIR] [--api-port P] [--control-port P] [--discovery-port P]
                   [--name NAME] [--listen-inbound ADDR:PORT] [--no-calling-probe]
                   [--dev-loopback] [--print-token-path]
```

* State directory: `--state-dir`, else `$GEEKBRIDGE_STATE_DIR`, else `$XDG_DATA_HOME/geekbridge/service`
  (`~/.local/share/...`), `~/Library/Application Support/geekbridge/service`, or
  `%LOCALAPPDATA%\geekbridge\service`. This is deliberately **not** `geekbridge-pair`'s directory
  (`.../geekbridge/kde`): the daemon keeps trust in memory, so do not run `geekbridge-pair` against a live
  daemon's directory.
* Files (all through `bridge_kde::fsutil`, i.e. owner-only, symlinks and loose modes refused):
  `identity.pem` (TLS key+cert, created once, never regenerated), `trusted_peers.json` (pins),
  `consent.json` (per-device opt-ins, no phone content), `ipc-token` (64 hex chars).
* `--print-token-path` prints the token file's **path**. Nothing prints or logs the token.
* `--listen-inbound` is off by default. The service does not broadcast its identity, so phones do not dial
  it unless you enable this and arrange discovery; the usual flow is outbound `connect`.
* `--dev-loopback` (tests/developers only) allows loopback peers and any TCP port. Without it, dialled
  endpoints must be private/link-local/CGNAT/ULA addresses on ports 1716-1764.

### Protected state unavailable

If the state directory, identity, trust store, consent file or token cannot be opened privately (wrong
owner or mode, symlink, corrupt file, unsupported platform such as Windows until `fsutil` lands there),
the public API keeps working and the control port answers every connection with

```
GB-IPC 1 unavailable <code> <text>
```

and nothing else. Codes: `state_insecure`, `platform_unsupported`, `trust_store_corrupt`,
`identity_unusable`, `token_invalid`, `random_unavailable`, `state_error`. Texts are generic (no paths or
secrets). Detailed errors go to the service's stderr. Corrupt files are left untouched; a malformed token
file is reported, not silently replaced. `bridge_core::ipc::call` returns `IpcError::Unavailable` without
sending the request.

## Wire contract (`bridge_core::ipc`, IPC version 1)

One request per TCP connection, newline-terminated lines:

```
S -> C  GB-IPC 1 <64 hex nonce>
C -> S  <64 hex mac> <request json>          mac = HMAC-SHA256(token, "geekbridge-ipc-v1 request\0" || nonce || json)
S -> C  <64 hex mac> <response json>         mac = HMAC-SHA256(token, "geekbridge-ipc-v1 response\0" || nonce || request mac (hex) || json)
S -> C  ERR unauthorized                     (wrong/missing mac; request never parsed or executed)
```

* The token never crosses the socket. A process that wins the port race cannot harvest it, and the
  client rejects any response whose MAC does not verify.
* Limits: request line <= 16 KiB, response <= 256 KiB, one exchange per connection with a 3 s server
  deadline (client: 4 s total including connect), at most 8 concurrent exchanges (extra connections are
  closed immediately, idle ones are cut at the deadline).
* Requests use `{"op": ..., ...}` with unknown ops/fields rejected (`deny_unknown_fields` on every
  variant that carries fields). The authentication check happens before JSON parsing.
* Long operations (connect, pair) are asynchronous: the call returns `Ack` and the state is read from
  `Status` / `Events`.

### Requests (`bridge_core::ipc::Request`)

| op | effect | refusal codes |
|---|---|---|
| `status` | identity, paired devices (+ consent matrix), live links, pending challenges, `event_seq` | |
| `events {after}` | bounded event page; `gap:true` if events were discarded or the cursor is from an earlier service run | |
| `connect {device_id, endpoint?}` | dial. Endpoint is the user's choice; if omitted the current discovery record is used. A known device is authenticated against its pinned cert; an unknown one ends **unpaired** (TLS up, nothing else) | `bad_request`, `not_found`, `invalid_endpoint`, `busy` (link exists), `too_many` |
| `pair {device_id, mode: request\|await}` | start pairing on an unpaired link | `not_found`, `not_unpaired` |
| `approve {challenge_id}` / `deny {challenge_id}` | the **only** way a pairing completes or is refused | `not_found`, `challenge_expired`, `challenge_stale` |
| `disconnect {device_id}` | close the link, keep pairing | `not_found` |
| `unpair {device_id}` | delete trust + consent, tell the phone if linked | `not_found`, `failed` |
| `set_consent {device_id, feature, enabled}` | per-device opt-in (paired devices only) | `not_found` |
| `ping {device_id}` | one `kdeconnect.ping` on a connected paired link (no body, no consent needed) | `not_connected`, `unsupported`, `busy` |
| `calling_status` | read-only calling backend summary (counts + verdict, no numbers/addresses) | `busy`, `failed` |

Later ops (see `bridge_core::ipc::Request` for the exact shapes): `pair_qr` / `pair_qr_cancel` and `test_connection`
([pairing.md](pairing.md)); `calls`, `call_action`, `call_mute`, `call_audio` ([call-control.md](call-control.md));
`update_status`, `update_check`, `update_download`, `update_skip`, `update_set_enabled` ([updates.md](updates.md));
`clipboard_status|pull|push|settings` ([clipboard-sync.md](clipboard-sync.md)); `share_status|send_file|send_text|decide|
cancel|dismiss|settings` ([sharing.md](sharing.md)); `agenda_view` and `calendar_edit` ([phone-data-link.md](phone-data-link.md)).

### Link and challenge model

* **One live link per device id.** `connect` while any link (connecting, unpaired, pairing, connected)
  exists returns `busy`. An *inbound* connection that matched the pinned certificate replaces an older
  link of the same device (event `link_lost: replaced by a newer authenticated connection`); an
  unauthenticated/unpaired inbound connection never displaces anything. Cap: 16 links.
* **Phases** (`LinkStatus.phase`): `connecting` -> `unpaired` (cert fingerprint visible, nothing
  negotiated) -> `pairing` -> `connected`; paired devices go straight `connecting` -> `connected`.
  Outbound unpaired links wait 120 s for a `pair` call, then close. Inbound unpaired links wait for the
  phone's request (send nothing) and still need approval.
* **Challenge** (`PairChallenge`): `challenge_id` (128 random bits), device, direction, `connection_id`,
  `peer_cert_sha256`, `own_cert_sha256`, `verification_key` (8 hex chars; compare with the phone's
  screen) and `expires_in_ms`. It exists only while the pairing attempt is running, is bound to that one
  link attempt, connection id and certificate fingerprint (re-checked on every decision), can be decided
  once, is removed when the connection or attempt ends, and is refused after expiry. `bridge_kde` writes
  the trust record only after the local decision and only for the certificate on that connection.
  Cap: 8 pending challenges.
* **Never auto-approved.** Neither `connect`, `pair`, an incoming phone request nor a phone-side accept
  completes pairing without `approve`.
* **Peer events**: phone unpair (`peer_unpaired`, trust already deleted by `bridge_kde`, link closed),
  re-pair request (`peer_requested_re_pair`, informational), link loss/EOF/keepalive timeout
  (`link_lost`, pairing kept), failed dial (`connect_failed`, e.g. pin mismatch), `pair_failed`.
  Event log: 256 retained, <= 128 per page, `ping_received` coalesced to one per second.

### Feature negotiation and consent

The service declares the packet types in `gate::LOCAL_INCOMING` / `LOCAL_OUTGOING` (14 each at the time of writing:
ping, clipboard, `share.request`, the `geekbridge.calls`, contacts, calendar and SMS packets). A packet is
accepted from a paired phone only if its type is in (phone's outgoing ∩ our incoming); everything else
(notifications, other telephony, unknown types) is counted in
`rejected_packets`, dropped unread, never routed, and a link that sends 64 of them is dropped. Packet
lines are capped at 64 KiB (raise it together with an adapter). Discovery capability lists never take
part in negotiation and never touch the trust store; the pin only ever comes from a locally approved
pairing.

`set_consent` stores per-device booleans for `sms`, `contacts`, `clipboard`, `notifications`,
`telephony`, **all off by default**. Consent is necessary but not sufficient: `gate::check_outgoing`
also needs the type negotiated and an integrated adapter (`ConsentEntry.adapter_integrated`, currently
`false` for every feature), so enabling consent today sends nothing. Ping is the single exception and
carries no content. Consent is removed on unpair. Status/events/consent files contain connection facts
only; the types have no field for message bodies, contacts, clipboard text or ping text.

## Client integration (for the GUI wiring)

1. Add `bridge-kde = { path = "../bridge-kde" }` to the client crate (only for `fsutil::read_private_file`;
   `bridge-core` cannot depend on it without a cycle). Keep using `fetch_snapshot(api_port)` for the
   device list exactly as today.
2. Resolve configuration with `bridge_core::ipc`: `default_state_dir()` (honours `GEEKBRIDGE_STATE_DIR`),
   `token_path(&dir)`, `control_port_from_env()`; allow `--state-dir` / `--control-port` overrides next to
   the existing `--api-port`.
3. Read the token safely and build a handle:

```rust
use bridge_core::ipc::{self, Request, Response, Token};

let dir = state_dir_flag.or_else(ipc::default_state_dir).ok_or("no state dir")?;
let bytes = bridge_kde::fsutil::read_private_file(&ipc::token_path(&dir))?   // checks owner/mode/symlink
    .ok_or("service has not created its token yet")?;
let token = Token::parse(&bytes)?;
let port = ipc::control_port_from_env();

let status = ipc::status(port, &token)?;                       // blocking, <= 4 s
ipc::call(port, &token, &Request::Connect { device_id, endpoint: None })?;
```

   Every call is blocking with a hard deadline: run them on the poller/worker thread, never on the UI
   thread.
4. Poll `ipc::status` (about 1 Hz, or on `Events` cursor change) from the same worker that polls the
   snapshot. Render `status.links` / `challenges` / `paired`. A pairing sheet should show the challenge's
   `verification_key` and the peer name, and call `Approve`/`Deny` only from an explicit button press;
   do not pre-select Approve.
5. Error mapping (`IpcError`): `Io` -> "service not running" (the discovery snapshot error already says
   this); `Unavailable{code,..}` -> "service is running but cannot protect pairing: <message>" and keep
   the UI read-only; `Unauthorized` -> token/state-dir mismatch (usually a different `--state-dir`);
   `Service(body)` -> show `body.message`, branch on `body.code`.
6. Treat every string from the phone (names) as untrusted text; they are control-character-filtered and
   length-capped but not trustworthy as an identity. Prefer showing the cert fingerprint prefix with the
   name.
7. Do not show features as "available" because the phone advertises them: only `negotiated`
   (ping today) and `ConsentEntry.adapter_integrated` mean anything.

`geekbridge-ctl` is a working reference client (`status`, `events`, `connect`, `pair`, `approve ID CODE`
(refuses unless CODE equals the challenge's verification key shown on the phone), `deny`, `disconnect`,
`unpair`, `consent`, `ping`, `calling`, `token-path`). It prints JSON and never the token.

## Calling

`calling_status` runs `bridge_calls::probe::run` (read-only: owner checks, introspection, property
reads; never a control method) on a blocking thread with a 2 s deadline and a single in-flight probe, and
returns `{platform, backend, verdict, telephony_service_present, bluetooth_controllers, phones, calls,
read_only}`. Only counts: no numbers, names or Bluetooth addresses. `--no-calling-probe` replaces it with
a static "disabled" answer. There is no dial/answer/hang-up request in the protocol, and no bridge from
the KDE link to the Bluetooth stack. On this development machine the live probe reports
`no_bluetooth_controller` (0 controllers), so nothing beyond the unavailable-hardware path was observed.

## Tests

`cargo test -p bridge-core -p bridge-service` (all loopback, ephemeral ports, generated certificates,
no phone/BT/LAN):

* `bridge-core::ipc` (8): HMAC RFC 4231 vectors, token parsing/redaction, MAC binding, strict request
  decoding, hello parsing, client behaviour against an unavailable service, forged responses and
  trickling servers.
* `bridge-service` unit (12): private-state creation/permissions/malformed token, consent persistence,
  feature gate matrix, CLI parsing.
* `tests/control_api.rs` (24): explicit approval with certificate binding and persistence; deny; expiry;
  inbound request needs approval and a trusted re-dial replaces the old link; wrong/missing/garbled/
  replayed-nonce MAC -> `ERR unauthorized` with zero dials, zero events, zero store changes; valid MAC
  with bad body is only `bad_request`; unavailable state; insecure directory; connection cap, deadline
  and size limits; one link per device and `busy`; failed dials; endpoint policy; link loss and
  reconnect against the pin; peer unpair; local unpair notifying the phone; discovery record pointing a
  paired id at an impostor (pin mismatch, pin unchanged); unnegotiated packet rejection without content
  leakage into status/events/disk; flood drop; consent defaults and ping; public API unchanged;
  calling probe only on request; event cursors.
* `tests/discovery_api.rs` (20): the existing public API suite, unchanged.

## Gaps and honest limits

* **No hardware interoperability.** Pairing/linking is exercised against `bridge_kde` peers (and
  `bridge_kde`'s own raw-rustls tests), not a stock Android KDE Connect or the GeekBridge Android app.
  Real-phone discovery -> connect -> pair has not been run.
* Clipboard, sharing, call control, contacts, calendar and SMS are integrated (each opt-in per phone); notifications
  mirroring is not. Bluetooth call *audio* is not carried by this service.
* The service announces itself on the LAN and listens for phones by default (`--no-listen-inbound` disables this; see
  [pairing.md](pairing.md)). No mDNS.
* Link liveness depends on TCP keepalive/user-timeout from `bridge_kde` (~1 minute to notice a dead
  Wi-Fi phone); there is no application-level heartbeat.
* The control token is a bearer secret for the whole user session: any process running as the same user
  can read it (as with the identity key). Other users are stopped by the owner-only directory, and by the
  MAC scheme if they race for the port. Loopback ports are not user-scoped, so another local user can
  occupy the control port first (the service then fails to start; a client still never sends the token).
* Windows: if the private-file support is not verified on a system, the control API reports `platform_unsupported`
  (Windows has not been run on real hardware). The core ipc code and CLI
  compile everywhere; only the Unix path was run here.
* Event reasons embed `KdeError` text (truncated to 160 printable ASCII chars) which, for pre-trust
  failures, may echo a few characters of peer-controlled data; treat as untrusted text.
* Concurrent writers: the trust store is cached in the daemon; another process writing the same state
  directory is unsupported (no lock file).


## Parent desktop integration

The four native desktop apps now read protected status on their own bounded background workers. Details includes nearby/paired links, explicit Connect/Pair/Disconnect/Forget actions, a manual address option, and a pairing dialog that requires typing the matching eight-digit phone code. The connection strip and Messages status distinguish authenticated connection from discovery. Phone feature buttons remain unavailable until real adapters are connected; pairing does not enable them.

Configure a custom service with `GEEKBRIDGE_STATE_DIR` (absolute path) and `GEEKBRIDGE_CONTROL_PORT` in the desktop apps' environment. The GUI never logs or displays the token. Commands use a bounded queue and token-file reads use the shared secure platform helpers. IPC token creation now also holds a cross-process private-file lock to avoid concurrent initialization races.


## Clipboard bridge

`clipboard` is the first feature adapter. Control requests: `clipboard_status` (backend, watching, per-device
opt-in/connected/negotiated and counters, freshness window, hotkey and its binding state, last pull outcome; never
clipboard text), `clipboard_pull {device_id?}`, `clipboard_push {device_id?}`, `clipboard_settings {window_ms?, hotkey?,
max_image_bytes?}` (hotkey validated, the others clamped). The per-device switch is the existing `set_consent`. Packet
negotiation adds the clipboard types to `gate.rs`; a device with the consent off has its clipboard packets ignored without
counting as misbehaviour. See [clipboard-sync.md](clipboard-sync.md).
