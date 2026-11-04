# Whole call chain: state-machine review of the integration seams

Date: 2026-11-04. Scope: the first real call through every part at once. The chain is the Android
listener (multi-source, `c106a234`), then the phone core, TLS, the service `CallHub`, the incoming-call
card (`call_overlay`), Answer/Decline, `TelecomManager`, Bluetooth call audio (`bridge-calls`,
`call_audio_host`) and the hang-up/unwinding. Different agents built each part against fakes. This
review looks for the seams between the parts. Every seam found has a scripted end-to-end timeline in
`crates/bridge-service/tests/call_chain.rs`, and each fix was checked by turning it off again
(mutation check below).

## The harness (`crates/bridge-service/tests/call_chain.rs`)

Deterministic, on loopback. It needs no Bluetooth and no display. 15 timelines run in about 4 s.

* **Stand-in phone**: a real `bridge_kde` endpoint (TLS) that behaves like the app plus its Rust core.
  * It has three state sources (`default`, `sub2/slot1`, `broadcast`), merged the way
    `CallSourceMerger` merges them: ringing > off-hook > idle. Like the merger, it reports only on change.
  * It greets each new connection with the last report and answers a `report` ask with the same.
  * `answer`/`reject`/`hangup`/`dial` act the way `TelecomManager` does. A request for a call that is
    not there gets `no_such_call`. The result is sent before the state change.
  * It can go **silent**: a frozen app on a link that still looks up.
  * It can act "by hand" with a delayed report.
  * It sends nothing while its link is down.
* **Real service**: `Daemon`, the control API and `CallHub`. The new `DaemonOptions::call_timings` option
  shortens the watchdog clocks.
* **Fake hands-free host** (`HostCalls`): mirrors the same phone, optionally after a lag (HFP indicators
  arrive after the link's report). It records the commands it gets and the audio routes the real
  `LinuxHfp` would make. Answer, dial or "use computer" route the audio here. A call ending while routed
  restores it.
* **Fake card launcher** (new `call_overlay::Launcher`): counts card starts, running cards and kills.
* **Recording banner notifier**: the test can press a banner's Answer/Decline late, like a
  `notify-send --wait` that is still open.

| Timeline | What it asserts |
| --- | --- |
| incoming, 3 sources, answered on PC (link) | one row, one banner, one card; repeated RINGING and the watchdog's `report` asks change nothing; the phone sent exactly `[idle, ringing]`; a double click sends one `answer`; no notice; the card process is stopped after the call starts; hang up from the PC |
| Bluetooth sees the ring 300 ms after the link | still one banner and one card while the row's key changes from `link:` to `hfp:`; Answer goes over HFP; the host routes the audio here; the link gets no request; hang up over HFP; the audio is restored |
| answered on the phone (HFP present) | merged live row, audio route Phone, no host command, nothing to restore |
| missed call | the card (which hangs in this test) is stopped after the linger; no notice |
| declined from PC, caller rings again | a second banner and a card for the new ring; pressing Answer on the **first** banner sends nothing; the second banner answers |
| outgoing dial from PC | `dial` sent once; OFF-HOOK without RINGING gives a live row with no banner and no card |
| link drop mid-call | no ghost call while the link is down; the reconnect greeting brings the call back; a call that ended during the outage does not come back |
| link flap during a ring | after the reconnect, the same ring: no second banner or card; if the card closed itself during the gap, it comes back once |
| lost IDLE (frozen phone) | the watchdog asks twice, then clears the call with a notice, and the card goes; when the phone recovers, the next ring is a new call and the notice is cleared |
| Answer or Decline racing the phone | the phone answers `no_such_call` before its state report; the notice is cleared once the report explains it |
| two phones | A (Bluetooth + link) and B (link only) ring: two rows, B never merged into A's Bluetooth row; one card follows both; answering B sends only to B; A gets nothing and the host gets nothing |
| phone B rings while phone A rings over Bluetooth only | B is never merged into A's Bluetooth row; Answer goes to B, nothing goes to A |
| Bluetooth connects after the call started | merges into the live row; no new ring, no routing |
| Telephony consent off | no row, banner or card |
| card cannot start | the banner still shows; one launch attempt per ring, not four a second |

## Seams found and fixed

| # | Seam | What the user would have seen tomorrow | Fix |
| --- | --- | --- | --- |
| 1 | **Call identity across sources.** The hub de-duplicated announcements by row key. The key is the *primary* source's (`hfp:` wins), so when Bluetooth reported the ring after the link (normal: HFP indicators lag), the key changed | a **second banner** for the same ring; the card reset "Answering…" back to fresh buttons | `CallRow.parts` (core, serde-default) lists every `source\|call` that sees the call. The hub numbers rings by any part it already knows, so a ring keeps its number while sources join or leave. The card (`bridge-apps`) treats a row as the same call if any part matches |
| 2 | **Stale banner answers the next ring.** The link always calls its call `link-current`. A `notify-send --wait` banner lives 30 s; Answer on the banner of a declined or missed ring hit the *next* ring on the same phone | answering a call the user never clicked for | the banner reply carries its ring number and acts only while that ring is still ringing |
| 3 | **Card closing as a new ring arrives** (known limit in `call-overlay.md`). `ring()` saw the old card process alive and started none | no card for a quick second ring | `call_overlay::Supervisor`: if a ring finds no running card, it gets one, once per ring. A card that dies during its ring is not restarted in a loop |
| 4 | **Zombie card processes.** The `Child` was reaped only at the next ring | a `geekbridge-calls --overlay <defunct>` per call until the next ring | the supervisor reaps on every hub change and every 250 ms tick |
| 5 | **Card that never closes.** Nothing stopped a hung card (compositor, stuck window) | a card stuck in the corner | a card still running 6 s (`LINGER`) after the last ring is killed; dropping the hub kills it too |
| 6 | **Lost IDLE.** Android sends changes only. A frozen app on a link that still looks up (TCP keepalive needs up to 60 s; app-level silence never ends the link) left a ringing ghost | the card and "Incoming call" forever | watchdog: after 20 s of silence while ringing (120 s while in a call), the hub sends `report`. Two unanswered asks 5 s apart clear the call, with a notice. A live phone just resends its state (no change, no second banner) |
| 7 | **Link flap during a ring.** `link_down` dropped the call; the reconnect greeting "ringing" looked like a new call | a second banner and a second card for one ring | the rings of a dropped link are remembered for 60 s. A ring from the same phone after the reconnect keeps its number and its banner. A non-ringing report from that phone forgets them |
| 8 | **Double click / card plus banner.** Each click sent another `answer`; the second came back `no_such_call` | "The call had already changed on the phone." during a call that was answered fine | a control already on its way to the phone (same action, pending) is not sent again |
| 9 | **Answer/Decline racing the phone.** The user picks up by hand as the PC click goes out; the phone's result comes before its state | a stale refusal notice that stayed through the call (and through the next ring) | `no_such_call` is no notice when the goal already holds. A race notice is cleared by the next report that moved the call on. A new ring starts without the previous ring's notice |
| 10 | **Two phones, one with Bluetooth.** A link observation joined *any* HFP row of the same class | phone B's ring merged into phone A's Bluetooth row; **Answer answered phone A** | `calls_view::same_phone`: merge only when names match (case-insensitive), or when it cannot be wrong (exactly one HFP and one other source with a call in that class, i.e. one phone) |
| 11 | **Announcements racing each other.** The host thread, link packets, the watchdog and the grace re-check all ran `announce_new_calls`. A view built before another caller's update could replace the ring table with "nothing rings" for a moment | an occasional second banner for the same ring (found by the harness under parallel load) | the whole announcement is serialised: one view, one update at a time |
| 12 | **A phone's late answer after the watchdog cleared its ring** | a second banner when the phone wakes up while the caller still rings | a cleared ring is remembered like a link flap; the notice goes as soon as the phone reports again |

The card now starts at the first sight of a ring. Before, it started after the banner's 1.5 s
"wait for the caller's name". The Android app reports no number, so every ring paid that delay. The
card fills in the name when one arrives; the banner still waits.

The watchdog thread parks while nothing needs watching: no ring, no call on a link, no card, no
dropped link's rings. Something to watch wakes it. An idle service has the same wake-ups as before:
2/s locally with `scripts/perf-check.sh`. The first push ticked 4/s and CI measured the idle budget's
limit, 6/s.

## Seams checked and found sound (no change)

* **Dual-SIM duplicates.** The phone merges `default`, `sub2/slot1` and `broadcast` before reporting.
  The core sends a report only through `transport_calls_report`, after the Kotlin engine's `lastSent`
  dedup, so the PC gets one `ringing`. The harness proves the wire carries exactly `[idle, ringing]`
  for three ringing sources plus repeats.
* **Call id stability ringing → off-hook → idle on the link.** It is always `link-current`. Off-hook
  keeps the number learnt while ringing (`LinkCallReport::apply`, a direction of `Unknown` keeps the
  known direction). Idle records `NotAnswered` or `Completed`.
* **OFF-HOOK without RINGING** (dialled on the PC or the handset) makes a live row. It is never a ring,
  so there is no banner or card.
* **Reconnect reconciliation.** The phone core greets every new trusted connection with its last report.
  `link_up_generation` replaces the entry, and a displaced session's late `link_down` cannot remove its
  successor (generation check).
* **Audio routing triggers.** Routing happens only on the HFP path's `Answer`/`Dial`/`ConnectAudio`
  (`LinuxHfp::execute`). A call answered on the handset or over the link is not routed, and Bluetooth
  connecting mid-call does not route it. A call ending while routed is restored by `CallAudio` on the
  next tick. Without a host, link control works and the status says why the voice cannot come here
  (`readiness`). An absent adapter gives a status line, not an error.
* **Consent and privacy.** Without Telephony consent the hub drops reports, so there is no row, banner
  or card. `GEEKBRIDGE_CALL_OVERLAY=off|name` is read at each launch (`ProcessLauncher` → `preflight`):
  `off` records `cannot: disabled` and starts nothing, once per ring.
* **Display environment discovery** is unchanged (`display_env`, unit-tested). A failed launch is logged
  once in `call-overlay-status` for `doctor`, and the banner and Calls window remain.

## Known limits (not fixed, worth watching tomorrow)

* **Call waiting.** Android reports RINGING for a waiting call during an active one. The link has one
  call id, so the live row turns into a ringing row until the waiting call is decided. With Bluetooth,
  HFP keeps the two calls apart and the link joins the waiting one.
* **Answer over the link while Bluetooth is connected but cannot answer** (HFP capabilities not probed
  yet) leaves the audio wherever Android puts it. Android usually puts it on the connected hands-free
  device, and `CallAudio` then follows its own rules for an unrequested audio link. "Use computer" in
  the Calls window is the way to bring the voice here.
* **The banner and the card are both shown** for a ring, by design. The banner is the fallback, and mako
  draws no buttons on it. With seams 1 and 2 fixed they act on the same ring, and a second Answer is a
  no-op (seam 8).
* **Two phones that both have Bluetooth set up and whose names differ from their GeekBridge names** can
  only be told apart by uniqueness. When both ring at once, the link rows stay separate from the
  Bluetooth rows: one call can then show twice, but no Answer goes to the wrong phone.
* **An old phone app that ignores `report`** would have a call longer than about 130 s cleared by the
  watchdog. Every APK since the consent `report` ask (2026-11-03) answers it, and tomorrow's APK
  (versionCode 20215) does.

## Mutation check (each fix turned off, harness re-run)

Each fix was turned off on its own and the harness run again. Every fix makes at least one timeline fail.

| Fix turned off | Timelines that fail |
| --- | --- |
| `CallRow.parts` (call identity across sources) | Bluetooth sees the ring after the link; answered on the phone; Bluetooth connects after the call started; two phones |
| banner ring check | declined from PC, caller rings again |
| link-flap memory | link flap during a ring |
| card kill after the linger | missed call; lost IDLE; incoming from 3 sources |
| watchdog clear | lost IDLE |
| pending de-duplication | incoming from 3 sources (double click), plus three timelines where the watchdog's asks overlap a click |
| race-notice clearing | Answer or Decline racing the phone |
| `same_phone` | two phones; a ring on phone B never joins phone A's Bluetooth ring |
| respawn of a card for a ring that finds none | no harness timeline fails, because the harness cannot time the old card's exit exactly; `call_overlay::tests::a_card_closing_just_as_a_new_ring_comes_is_replaced_but_a_crashing_one_is_not_looped` covers it |

The harness itself was checked for flakiness with 5 full runs in a row (15/15 each). One race
it found was in the hub itself and is fixed (seam 11 below).

## Files

* `crates/bridge-core/src/calls_view.rs`: `CallRow.parts`, `CallRow::is`, `same_phone`.
* `crates/bridge-service/src/call_control.rs`:
  * `Timings`, the watchdog `tick` (and its thread);
  * ring numbers, the link-flap memory, the banner ring check;
  * pending de-duplication, `no_such_call` handling;
  * `set_overlay`.
* `crates/bridge-service/src/call_overlay.rs`: `Launcher`/`Card`/`Supervisor`/`ProcessLauncher`
  (replaces the global `ring()`).
* `crates/bridge-service/src/daemon.rs`, `lib.rs`, `os_notify.rs`: the card is started by the hub on
  every OS, not by the notifiers.
* `crates/bridge-apps/src/call_overlay.rs`: the card follows `parts`; the next ring after an outcome is
  a fresh card.
* `crates/bridge-service/tests/call_chain.rs`: the harness.
