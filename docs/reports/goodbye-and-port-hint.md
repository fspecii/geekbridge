# Goodbye packet and listening-port hint

Follow-up to the "Not changed, worth deciding" list of `redial-collision.md`. Both additions are GeekBridge-only and
invisible to stock KDE Connect peers.

## 1. `geekbridge.goodbye`

Before: `geekbridge-ctl disconnect` (or the Disconnect button) closed the socket; the phone saw a plain loss and redialed
2 s later, which on a real PC (port 1716) brought the link straight back.

Now:

* Packet `geekbridge.goodbye`, body `{"reason": "user_disconnect" | "shutdown" | "unpair"}` (`bridge_core::goodbye`).
* Declared by the phone as an incoming capability next to `geekbridge.heartbeat`; the computer lists it in
  `gate::LOCAL_OUTGOING`, so `negotiated_outgoing` only offers it to a peer that declared it. A stock peer cannot, so it
  is never sent one (`stock_peer.rs` is unchanged and still green, including the "nothing `geekbridge.` is negotiated with
  a stock phone" check).
* Computer: `Daemon::disconnect` sends it through the live trusted loop (`LinkCmd::Goodbye`, 2 s bound, 100 ms grace so the
  packet leaves before the socket is torn down) and then takes the link down as before. If the phone was told, the
  reconnect book pauses it for exactly 5 minutes (`Book::pause_for`) instead of "until silent", so both ends resume
  together. A stock or older phone keeps the old open-ended pause. `Daemon::goodbye_all` sends `shutdown` to every
  connected GeekBridge phone (bounded to 3 s) from `run()` after SIGTERM/Ctrl-C, before the tasks are aborted.
  Unpair keeps using the stock `kdeconnect.pair` unpair packet; the `unpair` reason is parsed and honoured but not sent.
* Phone (`bridge-android`): acts only if the peer declared a `geekbridge.*` capability and the reason is known (unknown
  or malformed: ignored, normal redial). `user_disconnect`/`unpair` record "stay off this computer until T+5 min" in a
  process-wide table (survives a network-change transport restart). The close event is `link_closed` with code
  `peer_goodbye`. `shutdown` suppresses nothing; code `peer_shutdown`, normal back-off.
* The suppression ends early on any connection to that computer (the user's Connect, the computer's own dial, pairing) and
  on unpair. The redial loop re-checks it after each back-off sleep, so a loop already waiting when the goodbye arrives
  (found by the first version of the test) also holds off, and it resumes by itself at expiry.

## 2. Listening port in the secure identity

`Identity.geekbridge_port` / body key `geekbridgePort`, set by `Daemon::set_inbound_listener` through
`Endpoint::set_listen_port`. It is written only in the secure (in-TLS) identity: `addressed_to` clears it for the
plaintext line. On the phone `redial::Book::connected` uses it for an inbound connection (priority: hint, previously known
port, 1716); a port the phone dialed itself is never overridden. Accepted values: JSON integers 1024..=65535; anything else
is dropped and the identity stays valid.

## Tests

* `bridge_core::goodbye` unit tests; `bridge_kde::identity` tests (hint parsing, bad values, secure-only); `reconnect`
  (`pause_for` expiry); `redial` (hint priority, suppression bounds/clearing).
* `crates/bridge-service/tests/goodbye.rs`: goodbye on user disconnect, shutdown to all phones, never sent without the
  declaration, hint seen by a phone dialing in and by one dialed.
* `crates/bridge-service/tests/cross_dial.rs`: `a_goodbye_does_not_disturb_the_tie_break_when_both_ends_dial_again` (both
  id orders after a polite disconnect). Existing cross-dial tests unchanged.
* `crates/bridge-android/tests/chaos.rs` (real phone engine): goodbye stops the redial for well over the first back-off and
  the user's Connect ends it; a shutdown goodbye still redials; a goodbye from a peer without a `geekbridge.*` capability is
  ignored; the phone redials the hinted port (the test never uses 1716).
* Fuzz: new target `goodbye` (reason parse + identity hint oracles, seeds), extra oracle in `identity_line`, regression
  inputs in `crates/bridge-fuzz/tests/regressions.rs`.
* Updated two phone tests that pin the phone's own declared capabilities (`geekbridge.goodbye` now incoming).
  No stock capture fixture or stock-identity test was touched.

Exit codes: `cargo fmt --all --check` 0, `cargo clippy --workspace --all-targets -- -D warnings` 0, `cargo test --workspace`
0, `./gradlew :app:testDebugUnitTest` 0 (no Kotlin source needed changing: the new close codes are shown by the existing
`link_closed` + code event text). One `chaos_link` failure was seen once under heavy machine load from another job and did
not reproduce (alone, and in the full later run).

## Not done

* No phone-to-computer goodbye (phone app stop/exit); the computer's own redial already handles that.
* No UI text for `peer_goodbye`/`peer_shutdown`; they appear as the raw code in the event line.
* Not run against a real phone or a real stock peer; the stock side is covered by the pinned-capture tests.
