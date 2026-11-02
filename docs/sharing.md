# Sharing files, links and text (AirDrop-style)

Send files, links and text between a paired phone and this computer over the local network, both ways.
It is the KDE Connect `share` plugin (`kdeconnect.share.request`), so it interoperates in principle with
the stock KDE Connect apps; **nothing here has been run against a real phone or a stock KDE Connect device**
(see [Verification](#verification)).

## Using it

| You want to | Do this |
| --- | --- |
| Send from the phone | In any app (Gallery, Browser, Files, ...) choose **Share → Send to computer**. If several computers are paired, pick one. Progress shows in the screen and in a notification. |
| Send from the computer | Run `geekbridge-share`, drop files on the window or press **Choose files…**. Or `geekbridge-ctl send-file DEVICE_ID PATH...`. |
| Send a link or note | Window: paste into *Link or text*. CLI: `geekbridge-ctl send-url DEVICE_ID URL`, `send-text DEVICE_ID TEXT`. |
| Receive on the computer | Turn on *Allow this phone to send me files, links and text* for that phone (off by default), then accept each file in the window or notification, or turn on *Accept its files without asking* for that phone only. Files land in `~/Downloads/GeekBridge` (the XDG download folder on Linux if configured). |
| Receive on the phone | Incoming files show an Accept / Decline prompt; the per-computer switch *Auto-accept files from this computer* skips it. Files are saved to *Downloads/GeekBridge* through MediaStore. |

`geekbridge-ctl share` lists transfers and permissions; `share accept|decline|cancel|dismiss ID`,
`share allow DEVICE on|off` and `share auto-accept DEVICE on|off` do what they say.

## Protocol

* A share is a packet `kdeconnect.share.request` with one of: `filename` + `payloadSize` +
  `payloadTransferInfo.port` (file; also `lastModified` in ms, `numberOfFiles`, `totalPayloadSize`), `text`,
  or `url`. `kdeconnect.share.request.update` (multi-file bookkeeping) is accepted and ignored. Both types are
  advertised in the identity packet of the desktop and the phone, in both directions.
* **Payload sockets** (`crates/bridge-kde/src/payload.rs`): the sender opens a listener on 1739-1764 and is the
  **TLS server**; the receiver connects to the sender's address and is the **TLS client** (the same inversion as the
  main link, see [kdeconnect-interop.md](kdeconnect-interop.md)). Both sides use the pinned
  certificate of the paired device: the sender requires a client certificate and accepts only the one stored at
  pairing, the receiver verifies the sender against it. A stranger who connects first fails the handshake and
  cannot use up the one-shot payload slot. No identity is exchanged on a payload socket; the stream is raw bytes
  until the announced size has been transferred or the sender closes.
* **Which route the receiver dials.** The OS-chosen route first. Only if that fails to reach a finished handshake does
  it try a socket bound to the control link's local address (a multi-homed host whose default route shows the sender
  an address it does not accept: the sender closes at once), then the plain route once more with the full connect budget.
  Binding first looks safer but is wrong on Android: a bound socket does not leave through that address's interface, and
  it can connect and then die (`docs/reports/e2e-findings.md` E2E-14). A pinned-certificate failure is final on any route.
* Payload context is only obtainable from a `TrustedLink` (`TrustedLink::payload_context`), so an unpaired peer can
  never be a payload peer.

## Safety rules

* **File names** from the peer are reduced to one path component (`bridge_kde::share::sanitize_filename`): no
  `/`, `\`, `..`, control characters, Windows-reserved characters or device names, no leading dot, at most 200
  bytes. The destination is created with `create_new` (`name (1).ext`, `name (2).ext`, ...), so an existing file is
  **never overwritten**. Bytes go to a hidden `.gb-*.part` file that is renamed into place only after the whole
  payload arrived; cancelled, truncated or failed transfers leave nothing behind.
* **Consent.** Receiving needs the per-device *receive* permission (default off; the packet is refused and counted
  otherwise). Files then wait for Accept unless that device's *auto-accept* is on (default off; it implies receive,
  and turning receive off clears it). Unpairing deletes the device's share settings. Text and links are only
  held for the user: never opened, never put on the clipboard, and only `http`/`https` links get an *Open* button.
* **Limits.** 8 GiB per file; 3 transfers at once (2 per device); 8 unanswered offers; offers expire after 120 s
  (desktop) / 5 min (phone); 64 / 32 transfers listed (finished ones are forgotten after 10 minutes); payload ports
  must be in 1739-1764 when dialling; 10 s connect (3 s per route while another is left), 4 s TLS handshake, 30 s idle between chunks, 120 s for the
  receiver to show up on a send; received text 32 KiB (desktop) / 16 KiB (phone), sent text 8 KiB, links 4 KiB.
* **Cancel** works on every live transfer from both ends; the payload task is cancelled through a
  `CancelToken` and the partial file removed.
* Transfers use the existing local network policy: only private-network peers, only paired devices.

## Where the code is

| Piece | Location |
| --- | --- |
| Payload sockets, cancel, limits | `crates/bridge-kde/src/payload.rs` (+ `payload/tests.rs`) |
| Share packets, filename hygiene, safe receive, send helpers | `crates/bridge-kde/src/share.rs` |
| Desktop transfer manager, per-device settings, notifications | `crates/bridge-service/src/share.rs`, hooks in `daemon.rs` / `gate.rs` |
| IPC types (`Share*` requests, `Response::Share`, events) | `crates/bridge-core/src/share.rs`, `ipc.rs` |
| CLI | `geekbridge-ctl send-file / send-text / send-url / share ...` |
| Desktop window | `crates/bridge-apps/src/share_app.rs`, binary `geekbridge-share`, `packaging/linux/dev.geekbridge.Share.desktop` |
| Phone engine and JNI actions (`share_*`) | `crates/bridge-android/src/transport/engine/share.rs`; contract in [android-contract.md](android-contract.md) |
| Phone UI (Share sheet, MediaStore, notifications) | `android/.../ShareActivity.kt`, `ShareCoordinator.kt`, `ReceivedFiles.kt`, ... see [android-app.md](android-app.md) |

Desktop notifications for offers use `notify-send --action` (libnotify 0.8+) on Linux when present; without it the
Share window is the only prompt. Windows and macOS compile the same code paths (file picker via PowerShell /
`osascript`, `explorer` / `open` for Open and Show in folder) but have **not been run**; there is no native
notification on those platforms yet.

## Verification

Automated (all over loopback with generated certificates and real pinned TLS; no real phone):

* `bridge-kde`: payload round trip with progress, stranger rejected while the real peer still succeeds, wrong
  server certificate refused, size limit and port range, truncated payload, accept timeout, cancel, filename
  sanitising, never-overwrite, atomic receive.
* `bridge-service/tests/share_api.rs`: receive off by default (no trace, socket untouched), accept saves safely and
  keeps existing files, decline/expiry write nothing, per-device auto-accept, text and links held for the user,
  send file / text to a phone, unsupported phone, unpair forgets settings.
* `bridge-android/tests/transport.rs` (`sharing::*`): offer written only after `share_accept`, decline, sending from a
  duplicated descriptor, cancel, text both ways, request validation, offer flood and oversize caps, stranger.
* Kotlin JVM tests for request shapes, snapshot parsing, name sanitising, intent classification, policy.

Not verified: any real device (Android phone, stock KDE Connect on either side), the Share sheet entry, MediaStore
saving, notifications and permission prompts, `notify-send` actions, Windows and macOS runs, large (multi-GB)
transfers, behaviour on flaky Wi-Fi.
