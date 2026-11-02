# Black-box security test: a live, unauthenticated/malicious-peer attacker

> Status: snapshot from 2026-11-02. Live tests ran against a release build of `geekbridge-service`
> in a private network namespace (`unshare -Urn`, a dummy interface with three addresses) with a
> private `--state-dir` under `/home/ambsd/.cache/gb-tmp/` — never the real user instance, never
> `~/.config`. The attacker side was a small Python client speaking the KDE Connect wire protocol
> (plaintext identity line, then the connecting side taking the TLS *server* role as the protocol
> requires, matching `bridge-kde/src/link.rs`). Everything below is either a result of running that
> client against the live service, or, where noted, a fact read from the code because the live
> environment could not exercise it (no second real UID available; no `nmap`/`testssl.sh`
> installed; a real file-transfer write would have landed in the real `~/Downloads/GeekBridge`, so
> it was not performed). All test processes and scratch state were torn down afterward; nothing was
> written outside `/home/ambsd/.cache/gb-tmp/`.

## 1. Recon

* `ss -ltnup` inside the test namespace, with the service started with its defaults:
  * UDP `0.0.0.0:1716` (discovery, unauthenticated by protocol, read-only – see §3).
  * TCP `0.0.0.0:1716` (inbound link port, phones/attackers reach this).
  * TCP `127.0.0.1:17380` (public read-only discovery API).
  * TCP `127.0.0.1:17381` (authenticated control API).
  * No other ports in 1716-1764 are opened until a payload transfer is in progress (`bridge-kde/src/payload.rs`, confirmed by code: the receiver only dials a port in that range on the already-pinned peer's IP, and the listener is consumed by `send`).
  * Confirmed: the control API binds **loopback only** (`127.0.0.1:17381`), never `0.0.0.0`.
* State directory and token file permissions: `state/` is `0700`; `identity.pem`, `identity.pem.bak`, `ipc-token`, `trusted_peers.json`, `share.json` (and their `.lock` files) are all `0600`. **One exception found and fixed**, see Finding BB-1 below.
* Another local user / another uid: `sudo` is unavailable in this environment and there is no second real UID to switch to (`setpriv` can drop capabilities but not become a different real user without root). The closest available substitute, a separate user namespace (`unshare -U`, uid 0 mapped, no access to a second real uid's files), does not let a different *real* local account attempt to read the token, so this specific sub-test could not be run live. Falling back to code reading: the control API's `exchange()` in `bridge-service/src/control.rs` never checks peer credentials (loopback TCP has none to check), so the design explicitly relies on filesystem permissions, not `SO_PEERCRED`, to keep another local user out — confirmed sound given the `0700`/`0600` permissions above, but this is the one layer that was not independently exercised live.
* `/proc/<pid>/cmdline` and `/proc/<pid>/environ`: `cmdline` contains only `--state-dir`, `--name`, `--no-tray`, `--no-calling-probe` — no token, no secret. `environ` is `-r--------` (owner-only) and, separately, contains no GeekBridge-related variable at all (checked by grepping for `token|secret|key|pass`, case-insensitive — the only matches were unrelated desktop-session variables). No secret is ever passed as an argument or inherited into the environment.

## 2. TLS

Environment limits: no `nmap`/`testssl.sh` available to install or reach from this sandbox; the
system's own OpenSSL (3.6.5) and Python's `ssl` module both refuse to even negotiate TLS 1.0/1.1 at
the library level (`openssl s_server -tls1` itself fails with "no protocols available"), so a live
downgrade probe against *this* machine's stack is not meaningful. What is verified:

* **Role.** The protocol has the connecting peer act as the TLS *server* and the accepting side as
  the TLS *client* (`bridge-kde/src/link.rs: accept_io` uses `TlsConnector`, not `TlsAcceptor`,
  for inbound connections). Confirmed live: a plaintext identity, then a TLS handshake with us as
  server, completes normally.
* **Protocol versions, live.** TLS 1.2 and TLS 1.3 both complete successfully
  (`ECDHE-ECDSA-AES256-GCM-SHA384` / `TLS_AES_256_GCM_SHA384`). TLS 1.0/1.1 could not be offered
  from this sandbox's OpenSSL build to test rejection live, but by construction this is moot: the
  service uses `rustls`, and `rustls::version::TLS12` / `TLS13` are the *only* protocol versions
  `bridge-kde/src/tls.rs::TlsVersions::list` can ever select (`Tls12Only` or `Tls12And13`) — rustls
  has no SSLv3/TLS1.0/TLS1.1 implementation to fall back to, so there is no code path that could
  negotiate them regardless of flags.
* **Cipher suites.** Same reasoning: rustls never implements NULL, EXPORT, RC4 or 3DES suites: there
  is no suite table entry for them to be offered. Live handshakes only ever showed AEAD suites.
* **Certificate.** Read from the live identity: ECDSA P-256 (`prime256v1`), `CN=<32-hex deviceId>`,
  no SAN, self-signed, ~10 year validity window generated at pairing time. This matches the design
  (`bridge-kde/src/cert.rs`, `bridge-kde/src/tls.rs`): public-CA fields are irrelevant because
  verification is always pinning (TOFU + explicit approval), never CA/hostname validation.
* **Renegotiation.** Not implemented by rustls for TLS 1.2 at all (no code path exists); moot for
  TLS 1.3.
* **Session resumption.** Explicitly disabled in both directions by code, confirmed by reading
  `bridge-kde/src/tls.rs::server_config` (`cfg.session_storage = NoServerSessionStorage{}`,
  `send_tls13_tickets = 0`) and `client_config` (`cfg.resumption = Resumption::disabled()`).
* **Unpinned client cannot complete pairing without explicit approval.** Confirmed live in §4
  (device-id impersonation and the pairing-approval test): an unpaired connection reaching the
  secure-identity stage gets `{"pair": false}` back and carries no feature traffic until a human
  calls `geekbridge-ctl approve CHALLENGE_ID CODE` with the verification key. No path to completed
  pairing without that step was found.

## 3. Unauthenticated abuse (live)

| Test | Result |
|---|---|
| Oversized line before any newline (200 KB, over the 8192-byte `identity_line` limit) | Connection reset immediately, no response, no crash. |
| Malformed JSON on the plaintext identity line | Connection closed, no response. |
| Invalid UTF-8 / binary garbage on the identity line | Connection closed, no response. |
| `targetDeviceId` set to a device id that is not ours | Connection closed, no response (`TargetMismatch`). |
| `protocolVersion: 999` | Connection closed, no response (`UnsupportedProtocol`). |
| Connect-flood: 40 plaintext-identity connections from one source, no TLS follow-through | All 40 TCP connects succeeded (no connect-level rate limit), but each is gated by a 42-slot semaphore (`Endpoint::unpaired_slots`, `max_unpaired = 42`) before TLS even starts, and every slot's TLS handshake has a hard 10s timeout; a legitimate handshake sent right after the flood still completed normally. Verified live that a **paired** device bypasses this semaphore entirely (`stored.is_some()` skips `take_unpaired_slot`), so an unpaired-connection flood cannot block a paired phone's reconnect — confirmed sound by code and by the post-flood handshake succeeding. |
| Slow-loris on the plaintext identity line (1 byte every 200ms, no final newline) | Connection closed at exactly the 5s `plaintext_identity` deadline. |
| Slow-loris during the TLS handshake (TCP connects, plaintext identity sent, then nothing) | Connection closed at exactly the 10s `tls_handshake` deadline. |
| UDP discovery: a well-formed and an oversized (>16 KiB) identity datagram from a spoofed-style source address | **No UDP reply of any kind** to either packet — the discovery listener is receive-only; it never echoes or amplifies anything back to a sender, confirmed live. (Announcements, separately, go only to the LAN broadcast/multicast address with the service's own identity — read from the code, not re-tested live since it would touch the real LAN.) |
| Packet-storm of every declared-but-unconsented capability (clipboard item, clipboard.connect, ping) on an **unpaired** link | All silently ignored; the link only ever answered `{"pair": false}`. No crash, no state change. |

No live test above produced a crash, a hang, or any growth in the service's RSS beyond normal
connection bookkeeping (checked after each run with `ps -o rss`).

## 4. Authenticated-but-malicious peer (live)

A test device was paired for real (full flow: inbound connect → `kdeconnect.pair` request with a
correct Unix-seconds timestamp → `geekbridge-ctl status` showed the challenge and verification key
→ `geekbridge-ctl approve` with that key → `paired: [...]`), then attacked:

* **Device-id impersonation.** A second connection claimed the *same* paired device id but
  presented a different certificate (different keypair) as the TLS peer certificate. Result: TLS
  handshake fails outright (`SSL: TLSV1_ALERT_ACCESS_DENIED`) — the service's client-certificate
  verifier for a `Pinned` peer rejects any certificate that is not byte-identical to the one stored
  at pairing, before any application data is exchanged. Confirmed live, matches
  `bridge-kde/src/link.rs::finish` (`pin.der() != peer_cert.der()` → `PinMismatch`) and
  `bridge-kde/src/tls.rs` `PeerVerifier`.
* **Cert replacement after pairing.** Same test as above *is* the cert-replacement attack (a
  "paired" device id showing up with a new certificate): refused identically.
* **Reconnect with the correct, pinned certificate** immediately afterward still worked normally —
  confirms the rejection is specific to the mismatched certificate, not a side effect that broke the
  link generally.
* **Unconsented feature packets on a paired link.** With the device's consents all left at their
  default (off) and a `CANARY-*` string embedded in each, three packets were sent on the live,
  paired, TLS-secured link: an unsolicited `kdeconnect.contacts.response_uids`, an unsolicited
  `kdeconnect.sms.messages`, and an unsolicited `kdeconnect.telephony.request` with a canary caller
  name. The service stayed up, the canary strings never reached stderr, and `geekbridge-ctl
  diagnose` afterward showed the device's every consent still `off` and no crash in "Last errors".
  (The link had already closed from the test script's own exit by the time `status` was queried
  again, so `rejected_packets` on that specific link entry could not be read back after the fact,
  but the behavioural result — nothing acted on, nothing logged, service healthy — is the property
  that matters and was observed directly.)
* **File-transfer / path-traversal into the real save folder.** `geekbridge-ctl share status`
  reported the save folder as the real `~/Downloads/GeekBridge` (there is no `--state-dir`-relative
  or env-var override for it in `bridge-service/src/share.rs::default_save_dir`). Actually sending a
  traversal filename would have written into the user's real home directory, which this task
  explicitly rules out, so **this one sub-test was not run live**; it is covered instead by
  `bridge-kde/src/share.rs::sanitize_filename`'s existing unit tests, which already assert
  `"../../etc/passwd" -> "passwd"`, `"..\\..\\boot.ini" -> "boot.ini"`, and `".." -> "file"`, and by
  `reserve_unique`'s "never overwrites" test. Zip/tar bombs: confirmed by reading
  `bridge-kde/src/share.rs::receive_file` — received files are written byte-for-byte to disk and
  never passed to an archive extractor anywhere in the receive path, so nothing is ever unpacked.
* **Request floods, calendar write abuse, SMS send flood, symlink races.** Not exercised live
  (would need a simulated phone-side `bridge-android` engine, out of scope for a desktop-only
  black-box pass); covered by the existing code/tests already catalogued in
  [security-review-2.md](security-review-2.md) (`RequestLedger`, hourly SMS cap, numeric-id-only
  calendar ops, the per-device consent gate in `gate.rs`) and not re-verified here.

## 5. Data exposure

* With the service's stderr captured for the whole session and a distinct `CANARY-*` string used
  in every scenario above (unpaired clipboard/ping flood, unconsented contacts/SMS/telephony on a
  paired link), `grep -a CANARY` over the captured stderr, the `geekbridge-ctl diagnose` output, and
  every file in the private `state/` directory found **no match** anywhere.
* `geekbridge-ctl diagnose` on the paired, exercised instance showed only: version strings, a
  redacted device identifier (`device-1`), consent states (all `off`, correctly), platform
  capability summaries, and event *kinds* with timestamps (`link_lost`, `packet_rejected`,
  `pair_failed` with the generic reason text) — no device names, addresses, fingerprints, or
  packet content, matching the tool's own "redacted: no names, addresses, keys or content" banner.
* `/proc/<pid>/cmdline` and `/proc/<pid>/environ`: see §1, no secret present.
* No crash occurred during the whole session, so crash-dump exposure could not be (and did not need
  to be) checked.

## Findings

### BB-1 — Fixed: non-secret state files in the private state directory were not owner-only

* **Where:** `crates/bridge-core/src/statefile.rs`, `write_atomic_plain` (used by `PlainIo`, the
  backend for the update-checker's `update-state.json` and the "what's new" state file).
* **Found:** `update-state.json` inside the test instance's `0700` state directory was `0644`
  (world/group-readable), unlike every secret-bearing file next to it (`identity.pem`, `ipc-token`,
  `trusted_peers.json`, `share.json`, all `0600` via `bridge-kde::fsutil`). The same was true of
  `whats-new.json` in an unrelated, already-running real-device test instance observed during recon
  (not altered). `std::fs::File::create` leaves the final mode to the process umask; nothing in
  `write_atomic_plain` restricted it the way `fsutil`'s owner-only helpers do for secrets.
* **Impact:** low in practice — the state directory itself is `0700`, so another local user cannot
  traverse into it to read these files regardless, and their content (update-check metadata, "what's
  new" text) is not sensitive. But it breaks the "everything under the private state dir is
  owner-only" invariant the rest of the code enforces, and would matter if the directory were ever
  copied, backed up, or the parent directory's permissions were loosened by mistake.
* **Fix:** `write_atomic_plain` now opens its temp file with explicit mode `0600` on Unix
  (`OpenOptions::mode(0o600)`) before writing and renaming, so every `PlainIo`-backed file ends up
  owner-only on disk, consistent with the `fsutil`-backed ones. Windows is unaffected (that platform
  never relied on Unix mode bits here). Test-first: added
  `statefile::tests::plain_io_writes_owner_only_files`, which failed (`0644`) before the fix and
  passes (`0600`) after it. `cargo test -p bridge-core` and `cargo clippy -p bridge-core --all-targets -- -D warnings`
  both clean on the touched crate; `cargo fmt` reports no diff for the touched file (two unrelated
  pre-existing formatting diffs elsewhere in the crate, from other concurrent work in this tree, are
  untouched).

No other defect was found in this pass; every other row in §§3-5 behaved as the code predicts.

## What this pass did not cover

* A real second local UID reading the token file (environment limitation, see §1).
* `nmap --script ssl-enum-ciphers` / `testssl.sh` (not installed, no path to install them here);
  substituted with direct `openssl`/`rustls`-source reasoning in §2.
* Phone-side request floods, calendar-write abuse, SMS-send flood, and save-folder symlink races
  (would need a simulated phone engine or writing into the real `~/Downloads`, both out of scope for
  this pass); relied on the existing coverage in `security-review-2.md` instead.
