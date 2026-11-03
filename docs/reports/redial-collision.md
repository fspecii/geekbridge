# Redial collision: why links dropped after a toggle and why redials failed with `tls_failed`

Follow-up to `regression-2026-11-03.md` ("about 4 to 60 s after such a toggle the link closed and the phone's redials failed with
`tls_failed`") and to `real-device-link-stability.md`. Evidence: the three phone logcats the regression run kept
(`e2e-home3`, `drive`, `e2e-sub36x`; the desktop service logs of those runs were deleted with their run directories).

## What the logs say

The `GB` tag lines of all three runs have one shape:

```
link: connected computer 1 (...) connection=N
link_closed code=peer_closed            (or conn_aborted)       <- the desktop closed
redial (2 s later)  -> connect_failed code=tls_failed, within 3 ms to 500 ms
[redial again at +5 s, +8 s, +17 s, +33 s: tls_failed each time]
link: connected computer 1 connection=N+1                        <- the desktop dialed back
```

### 1. The closes were the e2e harness, not the link

`scripts/e2e-android.sh` `ensure_session` runs at the start of nearly every stage and begins with
`ctl disconnect "$PHONE_ID"; sleep 1; ...; desk_dial`. That is a deliberate desktop-side close (`take_down`), seen by the phone as
`peer_closed`, followed by the desktop's own dial 5 to 7 s later. The timestamps match the stage starts (clip_d2p at 04:02:53,
close at 04:02:57; the second close at 04:03:08 is clip_p2d's `ensure_session`). The `conn_aborted` variants are the same close
arriving while the phone was writing. "Once per desktop disconnect in passing runs" in the regression report is exactly this.
Nothing in these logs shows the link closing by itself.

### 2. The `tls_failed` redials dial the wrong program

The e2e desktop service logs `Using port 1730 because 1716 is in use by another program`. When the desktop only ever dialed the
phone, the phone does not know the desktop's listening port and redials `<desktop ip>:1716` (`redial.rs`, `DEFAULT_PORT`). On the
emulator that is the host alias, i.e. whatever owns port 1716 on the developer machine (the live service, another agent's
service). Its certificate is not the pinned one, so the phone's handshake fails at once with `tls_failed`; a refused port would
have shown another code. That is the 3 ms to 500 ms failures, and why they stop only when the desktop's own dial lands.
It is harmless to the link, but each wrong guess burns one of the phone's 12 tries and it also dials a program that is not the
paired computer. On a real PC the same happens whenever 1716 was taken at start-up and the service moved to another port.

### 3. A real race the logs could not show: the desktop had no cross-dial tie-break

The phone already decides a simultaneous cross-dial deterministically (`prefer_new`: the connection opened by the smaller
device id wins). The desktop did not: `Daemon::install_ready` always let the newest authenticated connection replace the link it
had (and abort a dial in flight). With the desktop's id smaller than the phone's, the two rules disagree:

* desktop dials A, phone dials B at the same time;
* phone: keeps A (opened by the smaller id), turns B away as `duplicate_connection`;
* desktop: B arrives last, replaces A, aborts the dial;
* result: the phone closed B, the desktop closed A, no link until the next redial, which can cross again.

This is the toggle case: a capability change makes the phone restart its link and redial remembered computers right away
(`redial_remembered`), while the desktop's `reconnect_tick` dials the phone within 2 s of the loss. It is also the original
"drops every few minutes, recovers" family on the Fold5 for whichever pairing has the unlucky id order (half of all pairs).
The emulator runs did not hit it because the phone's redials went to the wrong port (2), so no cross-dial happened.

Reproduced deterministically: `crates/bridge-service/tests/cross_dial.rs` pairs a stand-in phone, lets the desktop dial it, has the
phone dial the desktop and finish before it answers the desktop's dial. Before the fix, with the phone id above the desktop's:
`the computer's dial must complete: Tls("tls handshake eof")` (the desktop had aborted its own dial).

## Fix

* `bridge_kde::tiebreak::prefer_new` is the one rule, moved out of the phone engine and used by both ends.
* `bridge_service::collision::keep_held` + a hook in `install_ready`: when an authenticated connection arrives while the held
  link is still being dialed or was made less than 5 s ago, the desktop applies the same rule (turns the arrival away if the held
  connection was opened by the smaller id; otherwise replaces as before).
* Stock peers are untouched: the rule only applies to peers that declare a `geekbridge.*` capability (the phone's heartbeat
  marker). Stock KDE Connect keeps the newest connection and so does the desktop for it (`a_stock_phone_keeps_the_newest...`).
* A held link older than 5 s is still replaced by the newest: after a silent loss the old socket can be a zombie for up to 60 s
  and the phone must be able to get back in.

Tests: `cross_dial.rs` (both id orders, stock phone), `collision::tests`, `tiebreak::tests`, and the phone engine's existing
`cross_dial_resolution_is_symmetric`.

## Not changed, worth deciding

* **Phone redial overrides a manual desktop disconnect.** `geekbridge-ctl disconnect` closes the socket; the phone sees
  `link_closed` and redials after 2 s (the desktop's `reconnect` book is paused, but `install_ready` does not consult it). On a
  real PC (port 1716) the link comes straight back. A "goodbye" the phone can tell apart from a loss would fix it; it needs a
  GeekBridge-only packet or capability, so it was left for a decision.
* **Redial port guess.** When the desktop only dialed in, the phone cannot know its listening port. Carrying the port in a
  GeekBridge-only field of the secure identity would remove the wrong-service dials in 2.
* e2e: `ensure_session`'s disconnect makes the phone log a redial storm of `tls_failed` on every stage; harmless, but noisy.
