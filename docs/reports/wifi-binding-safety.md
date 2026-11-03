# Wi-Fi binding: safety net and kill switch

`wifi-binding.md` makes the phone core dial from the Wi-Fi IPv4 address first. Its author flagged the risk that a bound
socket can connect and then die (E2E-14), which the connect-time fallback does not catch. The owner's phone is already
fragile, so binding now has a way to back out by itself and a way to be switched off.

## 1. Self-diagnosis (core, `transport/bindhealth.rs`, `engine.rs`)

A bound dial is now `Endpoint::connect_bound` (new in `bridge-kde`: bound connect only, no plain fallback inside)
under **one 6 s budget over connect and the TLS/identity handshake**. Outcomes:

| Bound dial | Action | Reason code |
| --- | --- | --- |
| handshake done | link runs; watched for early death (below) | |
| refused | final, as before (the plain route would say the same) | none |
| connect timeout, or connected and the handshake did not finish in 6 s (the blackhole) | mark unhealthy now, retry unbound at once | `bound_stalled` |
| other I/O error or EOF (reset, unreachable, address gone) | retry unbound at once; mark only if that retry succeeds (if the plain route fails too, the bind is not the culprit) | `bound_io` |
| trust or protocol error (pin mismatch, bad identity) | reported as before, not the bind's fault | none |

Early death: a link that came from a bound dial and closes within 10 s having never heard the computer (silence
at least its age minus 1 s), with a failure code (an orderly `peer_closed`, which the cross-dial rule produces, does not
count) marks the address and the normal redial then leaves unbound. Code `bound_early_death`. The handshake already
includes the identity exchange, so "never exchanged a valid identity" is covered by the 6 s rule; this covers a socket
that completes it and then dies.

The mark is **per Wi-Fi address, in memory, 10 minutes** (`Health`, pruned on insert). While marked, `effective_bind()`
is none, so dials *and the UDP announcement socket* (which swaps before the next send) are unbound; a different
network (different address) starts clean; after 10 minutes the next dial tries the bound path again. A restart of the
process forgets it.

Each fallback emits a `link` event `wifi_bind_fallback` with only the reason code (and the device id, as for other
events); the app logs `event wifi_bind_fallback code=...` in every build (`TransportController`). No addresses.

## 2. Kill switch

* Preference `bind_wifi` (`AppState.bindWifi`, default on). `LinkService` pushes it with the first Wi-Fi address push of
  each service instance (`transport_wifi_bind_enabled {enabled}`, new core action, process wide, no running transport
  needed), so it survives restarts. Off: nothing is bound, announcements included.
* Debug builds only can change it: `adb shell am broadcast -n dev.geekbridge.android/.DebugControlReceiver -a
  dev.geekbridge.debug.SET --es what bind_wifi --ez on false`, or the "Bind to the Wi-Fi address (bind_wifi)" check box
  in the debug card of Diagnostics. Release builds have no way to flip it (no UI); the code path reads it.
* `GB_BIND_WIFI=0` (also `false`, `off`, `no`) in the core's environment: the same switch for host tests.

## 3. Debug "Link health"

Section lines: `Wi-Fi binding (bind_wifi): on|OFF (kill switch)`, `Wi-Fi bind fallbacks since the app started: N; last
X ago, reason <code>` and `dials and announcements stay unbound for about M more` / `bound dials are tried again`. The
remaining time is computed from the 10 minute constant (`LinkHealthText.UNHEALTHY_MS`, mirrors the core).

## Tests

* `bindhealth` unit tests: mark lasts 10 minutes for that address only (clock passed in), expired marks pruned,
  environment switch parsing, the early-death rule (young, silent, failed, not an orderly close).
* `transport/mod.rs`: `dial_source_with` (loopback only for a core started with `allow_loopback`), effective address
  follows switch and mark.
* `transport/bind_tests.rs` (Linux; 127.0.0.2 stands in for the Wi-Fi address, the listener tells sources apart):
  a listener that accepts the bound dial and never answers leads to a successful unbound connection, taking at least
  the 6 s budget and at most budget + 4 s, the event `wifi_bind_fallback` / `bound_stalled`, the address marked; then
  three disconnect/connect rounds (a flapping link) all leave unbound within 3 s and the listener saw exactly **one**
  bound attempt in total. Switched off, no bound attempt is ever made.
* `tests/transport.rs`: the new action validates its input. Kotlin: request shape, Link health lines.

Deviation: the spec asked for the chaos proxy. `bridge_chaos::FaultProxy`'s blackhole applies to every connection and
cannot tell the bound source from the plain one, so the test uses its own accept-then-silence listener with the
same semantics (sockets kept open, nothing read or written back) and serves the other source with a real `Endpoint`.

## Not verified here

* The early-death path is covered by its rule test, not by an end-to-end test (it needs a trusted link whose bound
  socket dies in the first 10 s).
* Nothing ran on the Fold5. The 6 s budget is generous for a LAN handshake (milliseconds); a very slow AP would be
  marked unhealthy and cost only the bound path for 10 minutes, which is what the code did before binding existed.

## Results

`cargo fmt --check` 0, `cargo clippy --workspace --all-targets -D warnings` 0, `cargo test -p bridge-core -p bridge-kde
-p bridge-android` 0, Gradle `:app:testDebugUnitTest :app:lintDebug` 0.
