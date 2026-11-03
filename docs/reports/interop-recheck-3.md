# Stock-peer interop re-check 3 (2026-11-03)

## What could and could not be run

`which kdeconnectd kdeconnect-cli` finds nothing and `pacman -Q kdeconnect` reports the package as not
installed, and there is no stock KDE Connect tree here, so `scripts/e2e/coexist.sh` (real `kdeconnectd` in a
sandbox) and the real stock Android app were NOT run. This re-check is against the pinned captures only:
`crates/bridge-kde/tests/fixtures/kdeconnect-android-real/` (stock Android 1.37.0, F-Droid) and
`kdeconnect-real/` (stock desktop daemon), used as real `bridge_kde` endpoints over TLS on loopback. The live
run is still owed whenever a stock tree is available (docs/kdeconnect-interop.md, docs/coexistence.md).

## Findings and fixes

| Behaviour | Result |
| --- | --- |
| `kdeconnect.battery` accepted only with consent | OK. Unconsented stock battery packets are dropped unread and never counted as misbehaviour (150 in a row leave the link at 0 rejected). With consent the level is shown; stock `-1` (unknown) is not an offence. Pinned. |
| Heartbeat ping only to geekbridge peers | OK. The stock Android identity is not a geekbridge peer, so neither side schedules the periodic ping; the only ping a stock phone gets is the user's manual one. Pinned (decision and wire). The marker capability another agent is adding for a phone with every feature off is `geekbridge.*`, which stock ignores. |
| Identity capability lists | OK. Nothing in our lists is in the telephony, dial, mpris, runcommand, sftp, etc. families; no duplicates; a stock phone's negotiation never contains a `geekbridge.*` type in either direction. Pinned (`stock_peer.rs`). |
| Dial / telephony never sent to stock | Service side OK: with every consent on, a dial cannot be aimed at a stock phone and nothing but stock-declared requests is sent. **Phone side was a bug**, see below. |
| Unknown / undeclared packets from stock | Counted and dropped unread, one `packet_rejected` event per link, no content leak, link survives (pinned). Mirrored `kdeconnect.notification` without the notifications consent is likewise dropped without spending the budget (150 pinned). Note: the budget of 64 would cut a link whose peer sends 64 genuinely undeclared packets; stock only sends what we declare, so this is not reachable today. |
| UDP announce pause vs stock desktops | **Bug**, see below. |

### Bug 1: phone sent GeekBridge-only packets to a stock desktop (fixed)

`geekbridge.calls.state` (caller number, name, can-dial) and the data packets (`geekbridge.calendar.sync`,
`geekbridge.sms.status`, ...) were queued for every trusted, permitted computer without checking that the
computer declared them. With the per-computer permit on (or before permits are set) a stock desktop would
have been sent a caller number it cannot use. Clipboard already checked; calls and data did not. Fix: each
connection keeps the capabilities the computer declared (`ConnEntry::peer_accepts`) and `permitted_ctls` now
takes the packet kind and only returns connections that accept it. Pinned by
`a_stock_desktop_never_hears_call_state_even_when_allowed` (fails without the fix) and
`a_stock_desktop_gets_stock_packets_only` (stock `kdeconnect.battery` still flows to a daemon that declares it).

### Bug 2: paused announcements hid the phone from a new stock desktop (fixed)

While every paired computer is linked the phone stops broadcasting. A not-yet-paired stock desktop finds the
phone only through the phone's broadcast (the phone never dials an unpaired computer by itself), so it would
not list the phone until the next link loss. Fix: `record_discovery` rings a `Notify` when the announcing
device is not paired, and `announce_loop` answers it at once, at most once per 5 s. Pinned by
`a_newcomer_is_answered_even_while_every_paired_computer_is_linked`; the existing pause test still passes.

## Tests added

* `crates/bridge-service/tests/stock_peer.rs` (4 tests, stock Android fixture capabilities)
* `crates/bridge-android/tests/calls_transport.rs`, `data_transport.rs`: stock daemon fixture (1 test each)
* `crates/bridge-android/src/transport/discovery.rs`: newcomer wake unit test

## Status of the tree at commit time

Other agents' work in progress was in the tree: `android_negotiation.rs` and two `bridge-android` `transport`
tests (`start_reports_identity_listener_and_only_ping`, `announce_follows_a_ready_listener_and_is_kde_shaped`)
fail on their changes (notification capabilities; heartbeat marker), and clippy/fmt failures exist in their
unfinished files (`mirror.rs`, `daemon.rs`, `phone-sim data_link.rs`). All tests in my files and the
bridge-android lib/calls/data/clipboard/chaos suites pass; clippy -D warnings on bridge-android and bridge-core
is clean. The notification author should re-pin `android_negotiation.rs` (stock Android now negotiates
`kdeconnect.notification` incoming and `.request` / `.action` outgoing, consent-gated).
