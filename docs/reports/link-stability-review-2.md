# Link stability review 2: whole-system model, failure modes, fixes

Date 2026-11-03. Scope: the phone-to-computer link on the owner's Galaxy Z Fold5 (One UI, Wi-Fi), which dropped
repeatedly, mostly with the screen locked. Everything shipped since `real-device-link-stability.md` was reviewed together:
PC redial (`bridge-service/src/reconnect.rs`), phone redial (`bridge-android/src/transport/engine/redial.rs`), heartbeat
(20 s PC, 20/60 s phone, 70 s silence rule, `bridge-core/src/heartbeat.rs`), goodbye, cross-dial tie-break
(`bridge-kde/src/tiebreak.rs`, `bridge-service/src/collision.rs`), paused announcements while every paired computer is
linked plus the newcomer wake, Doze-window ping, foreground service and Wi-Fi lock (`LinkService.kt`, `WifiLinkLock.kt`),
kernel keepalive (`bridge-kde/src/keepalive.rs`). Earlier reports: `real-device-link-stability.md`, `redial-collision.md`,
`android-battery-audit-2.md`, `battery-optimisation-onboarding.md`, `docs/android-power.md`.

No real-device run was possible for this review. Findings are ranked by how likely they are to explain what the Fold5
did. Each fixed defect has a test that failed before the fix.

## 1. One clock fact that drives most of this

Every phone-side timer (tokio `sleep`/`interval`, `std::time::Instant`) runs on `CLOCK_MONOTONIC`, and so do the kernel's
TCP timers. **`CLOCK_MONOTONIC` does not advance while the phone is suspended.** With the screen locked and no wake lock
held, a Fold5 spends most of its time suspended. The CPU wakes for a packet addressed to it (Wi-Fi wake-on-unicast), an
alarm, or another app's work. A "2 s redial", a "30 s announcement" or the "70 s silence rule" therefore means 2, 30 or
70 seconds of time the CPU is awake. That can be hours of wall-clock time. Neither the foreground service nor the Wi-Fi
lock keeps the CPU awake, and nothing in the app holds a wake lock.

## 2. State machine

### 2.1 Phone, per paired computer (`bridge-android` core plus `LinkService`)

```
                 user Connect / PC dial (inbound) / redial lands
   ┌───────────────────────────────────────────────────────────────────────┐
   v                                                                       │
 LINKED ──(recv error: conn_reset/peer_closed/tcp_timeout/conn_aborted)──> LOST ──spawn_redial──> REDIAL_WAIT(delay_n ±20 %)
  │  ^  │   (ping_timeout: heartbeat peer silent > 70 s, checked on 20 s tick)                │      ^      │ nudge (screen on,
  │  │  │                                                                                     │      │      │ Doze window, Wi-Fi
  │  │  └─(goodbye user_disconnect/unpair)──> SUPPRESSED(5 min) ──timeout──> REDIAL_WAIT      │      │      │ available) cuts the wait
  │  │                                                                                        v      │      v
  │  │  (inbound trusted conn while LINKED: age < 5 s -> tie-break; age >= 5 s -> newest wins) DIALING(≤20 s handshake)
  │  └──────────────────────────────────────────────────── success ────────────────────────────┘      │ fail
  │                                                                                                   v
  │                                                                       tries < 12 -> REDIAL_WAIT; tries = 12 -> SPENT
  │                                                                       SPENT --nudge--> one more try (DIALING)
  └─(user Disconnect on the phone)──> SUPPRESSED(5 min)        (unpair / transport stop -> FORGOTTEN)

Transport level (LinkService):  STOPPED <-> RUNNING
  RUNNING --(another Wi-Fi network, or Wi-Fi IPv4 set changed; debounced 3 s)--> restart: STOPPED -> RUNNING,
            remembered PC addresses carried over, every remembered PC redialed (REDIAL_WAIT, first delay ~2 s)
  first Wi-Fi network since start, screen on, Doze window -> nudge (no restart)
Announcements: every 30 s (awake time) while some paired PC is unlinked; at once on a newcomer or a nudge.
```

### 2.2 Computer, per paired phone (`bridge-service` daemon)

```
CONNECTED ──(LinkLost: reset / timeout / EOF)──> UNLINKED
   ^   │                                            │  announcing (heard < 20 s ago) and Book due -> DIALING (Book back-off 2..60 s, 12 tries;
   │   │                                            │  history kept while heard within 90 s, i.e. through announcement gaps)
   │   │                                            │  not announcing, loss was silent (not EOF), endpoint known, not paused
   │   │                                            │      -> DIAL_BACK after 4 s, gaps 4,8,16,32,60.. s, 12 tries (Fallback)
   │   └─(user Disconnect: goodbye + pause 5 min / pause until silent 90 s)──> PAUSED
   │   (inbound trusted conn while CONNECTED: held < 5 s -> tie-break; else newest wins)
   └──────────── any trusted connection (dial or inbound) ─────────────────────┘
Dead-link detection while CONNECTED: PC pings every 20 s -> unacked data -> TCP_USER_TIMEOUT 60 s (Linux), so ≤ ~80 s.
The PC never cuts a phone off for silence.
```

### 2.3 Both ends at once: the composite states that matter

| Composite state | How it arises | Who gets out, and how fast |
|---|---|---|
| Both unlinked, phone asleep | the link died while the screen was locked (PC suspend/resume, PC service restart, a reset on the path) | **Before:** nobody. The phone's redial and announcement timers stand still. The PC only dialed phones that had announced in the last 20 s. Both waited for the phone's CPU, typically until the user unlocked it. **Now:** the PC dials back the last endpoint 4 s after a silent loss (the SYN wakes the phone), and screen on also nudges the phone's redial (F-1). |
| Phone LINKED (zombie), PC UNLINKED | the PC gave up on a flow whose close never reached the phone (blackholed path, Wi-Fi power save, PC's kernel timed it out) | **Before:** the phone applied the cross-dial tie-break to a minutes-old connection. If the phone's id was the smaller one, it turned away every reconnect from the PC (`duplicate_connection`) until its own silence rule fired, after 70 s of awake time. **Now:** newest wins outside the 5 s window, as on the PC (F-4). |
| PC CONNECTED (zombie), phone UNLINKED | phone IP change, Wi-Fi drop, phone process killed without RST | The phone's redial lands and the PC replaces a held link that is older than 5 s (already true, now under test). Otherwise the PC's TCP_USER_TIMEOUT ends it in ≤ 80 s. |
| Both DIALING (cross-dial) | both noticed the same RST at the same instant | The tie-break resolves it inside the 5 s window, identically on both ends. Jitter on the phone and the PC's 4 s dial-back grace keep it rare (F-8). |
| PC PAUSED (manual disconnect), phone announcing | the user pressed Disconnect on the PC | **Before:** the pause was dropped in the first gap between two announcements, and the PC redialed within about 30 s. **Now:** it lasts until the goodbye ends or the phone is silent for 90 s (F-3). |

## 3. Findings, by likelihood on the Fold5

| # | Finding | Likelihood | Status |
|---|---|---|---|
| F-1 | **Both sides wait for the sleeping phone.** After a loss with the screen locked, the phone's redial back-off (2, 4, 8 s…) and its 30 s announcement run on a clock that stops in suspend. The PC dialed only phones it had *heard announce in the last 20 s*. Result: no recovery until the phone woke for some other reason, typically when unlocked. This matches "drops, especially with the screen locked". The Wi-Fi lock "2 minute linger so the redial is quick" does not help, because it does not keep the CPU awake. | very high | **fixed**: PC dial-back after a silent loss (`reconnect::Fallback`, `Daemon::dial_back_tick`). The phone declares its listening port in its secure identity (`set_listen_port`). Nudges (screen on, Doze window, Wi-Fi back) cut a sleeping redial short and make the announcement go out at once (`redial_on_nudge`, `newcomer_announced`). |
| F-2 | **Default-network flips kill a healthy Wi-Fi link.** `LinkService` restarted the whole transport whenever the *default* network changed. One UI moves the default between Wi-Fi and mobile data on its own ("Switch to mobile data", Wi-Fi re-validation). The Wi-Fi link survives such a flip, since an existing socket stays on its network, but the restart closed it on purpose. The redial and announcements then went out over mobile data, where the LAN is not routed, until Wi-Fi became the default again. Cause 1 in `real-device-link-stability.md`, left open there. | high | **fixed**: the service watches the Wi-Fi network (`TRANSPORT_WIFI`, internet not required). It restarts only for another Wi-Fi network or a changed Wi-Fi IPv4 address; default-network flips are ignored. **Open:** new sockets (redial, announcement) still follow the default network while Wi-Fi is not the default. Binding them to the Wi-Fi network (`android_setsocknetwork` from Rust, or `Network.bindSocket`) needs a device test. |
| F-3 | **PC: every gap between announcements reset the redial history.** `reconnect_tick` called `keep_only` with phones heard in the last 20 s, but the phone announces every 30 s. Every 30 s cycle dropped the entry, which had two effects. A manual Disconnect lasted about one announcement, after which the PC redialed. And the back-off never grew: a phone that announced but could not be reached over TCP was dialed once per announcement, forever (a TLS attempt and a radio wake-up each time). | high (certain whenever the phone is unlinked and announcing) | **fixed**: history kept while the phone was heard within `DEVICE_TTL_MS` (90 s); dialing still needs a 20 s-fresh announcement. |
| F-4 | **Zombie connection on the phone turns the PC away.** See 2.3. Half of all pairings have the unlucky id order. The window is 70 s of awake time, which with the screen off is unbounded. | medium-high | **fixed**: `CROSS_DIAL_WINDOW` (5 s) is shared by both ends (`bridge_kde::tiebreak`), and `Core::admit` applies the tie-break only inside it. |
| F-5 | **Redial budget spent while away, then nothing.** After 12 failed tries (about 9 min of awake time) the phone stopped. Only a Wi-Fi change, a user Connect or the PC's dial brought it back, and the PC's dial needed announcements (F-1). | medium | **fixed**: each nudge buys one more try (`Book::grant_one`). A phone-side user Disconnect now suppresses dials for 5 min, so a nudge cannot undo it. |
| F-6 | **Stale "Connected" bounds.** PC side: ≤ ~80 s when the phone vanishes (20 s ping, then TCP_USER_TIMEOUT 60 s). If the phone's process is frozen while its kernel still ACKs, the PC shows Connected indefinitely. That is by design (it never cuts a phone off for silence), and data waits in the socket. Phone side: the silence rule closes ≤ 90 s of awake time after the last packet from a heartbeating PC (soak test below: 70 to 95 s). Before the PC is recognised as heartbeating (the first two pings, 20 to 40 s), only keepalive / TCP_USER_TIMEOUT apply. On unlock the phone used to wait for its next 20 s tick to ping. | medium | **improved**: screen on pings at once, so a dead link shows within TCP_USER_TIMEOUT (60 s), or at once when the PC's kernel answers with an RST (PC rebooted or resumed). |
| F-7 | **The Wi-Fi lock does nothing with the screen locked.** `WIFI_MODE_FULL_LOW_LATENCY` (used on API 29+) is applied by Android only while the screen is on and the holder is in the foreground. The battery and stability reasoning in earlier reports assumed it kept the radio out of power save with the screen off. | medium (affects expectations; the link must cope with power save anyway) | documented in `WifiLinkLock.kt`, not changed. To verify on the Fold5: `adb shell dumpsys wifi \| grep -iA3 lock` with the screen off. Holding a `FULL_HIGH_PERF` lock instead costs battery and behaves differently across Android versions; decide after measuring. |
| F-8 | **Lockstep redials.** Both ends used deterministic back-offs, and the phone had no jitter. With the PC now dialing back by itself, both would hit 2, 4, 8 s… together and spend a TLS handshake per step on a cross-dial. | low-medium | **fixed**: phone delays spread ±20 %, and the PC dial-back starts after a 4 s grace (the phone's first try is 1.6 to 2.4 s). |
| F-9 | **The fault proxy's "reset" was a clean close.** `bridge-chaos` split sockets with `into_split`, whose write half sends a FIN when dropped, so `reset_all` and `reset_after_bytes` usually delivered EOF instead of ECONNRESET. Tests of the reset path were mostly testing the EOF path. That is why an existing test accepted `conn_reset \| peer_closed`. | test fidelity | **fixed**: `tokio::io::split` plus linger 0 on registered sockets. Self-tests: RST on both sides, clean EOF still clean, blackhole silent. |
| F-10 | **Dial failures were all `io_error`.** A redial log could not tell "PC service not listening" from "no route" (wrong network, F-2). | diagnostics | **fixed**: `conn_refused`, `unreachable`. The UI maps them to the same plain words as before. |
| F-11 | **Phone hotspot.** The PC is on the phone's hotspot. Announcements go to 255.255.255.255 over the default network (mobile) and never reach the PC, so before this change only the phone's redial recovered a lost link. | low (setup-specific) | improved by F-1's dial-back. Tethered subnets are routed for apps, so the PC's SYN reaches the phone. |
| F-12 | **AP client isolation / filtered broadcast** (guest networks, mesh "multicast enhancement"). Discovery and the PC's announcement-driven redial never work. Unicast either works (only broadcast filtered) or nothing works (full isolation). | low-medium | the dial-back now covers "unicast works, broadcast does not". Full isolation cannot be fixed by the app; the pairing help already says so. |
| F-13 | **Two PCs, one away.** `all_paired_linked` is false, so the phone announces every 30 s (awake time) indefinitely, and each nudge spends one dial on the absent PC (≤ 20 s SYN retries). | low (battery) | accepted. It is bounded by OS and user events, with no timer of its own. |
| F-14 | **IPv6.** The link is IPv4 end to end: broadcast announcements, the PC dials the announced IPv4 address, and the phone listens on 0.0.0.0. IPv6 privacy-address rotation correctly does not restart anything. A remembered IPv6 link-local PC address (only if typed by hand) has no scope id and cannot be redialed. | low | noted. |
| F-15 | **Keepalive / user timeout coverage.** Linux and Android: SO_KEEPALIVE 30 s idle, 10 s × 3, TCP_USER_TIMEOUT 60 s, on every dialed and accepted socket (test `link_sockets_have_the_keepalive_timer_armed`, `accepted_sockets_have_the_keepalive_timer_armed`). Windows: idle and interval through `SIO_KEEPALIVE_VALS`, plus TCP_KEEPCNT and TCP_MAXRT (best effort). macOS: no user-timeout option, so dead-link detection with data in flight falls back to the retransmission timeout (minutes). The 20 s heartbeat keeps data in flight, so on Linux the user timeout, not keepalive, is what fires. | ok / low (macOS) | noted. Suspend does not cause false timeouts: TCP timers stop too. |

Not a finding: NAT idle timeouts. The phone and PC normally share one L2 segment with no NAT between them. Where a
stateful router sits between VLANs, the PC's 20 s ping is far below any conntrack timeout. A flow such a router drops
silently is a blackhole, handled like F-4 and F-6.

## 4. Fault-injection test plan

Columns: fault, how it is injected, expected behaviour (bound), test. "new" means added in this change.

| Fault | Injection | Expected | Test |
|---|---|---|---|
| RST mid-link (phone side) | `FaultProxy::reset_all` | phone redials, one connection | `chaos::the_phone_redials_after_a_reset_mid_link_and_keeps_a_single_connection` |
| RST on the next packet | `Faults::reset_after_bytes` | both ends read ECONNRESET | new `bridge_chaos::tests::reset_after_bytes_is_a_reset_on_both_sides` |
| Proxy reset fidelity | `reset_all` | RST both ways, never a FIN first | new `bridge_chaos::tests::reset_all_is_a_reset_on_both_sides_not_a_clean_close` |
| Blackhole without RST, PC heartbeating | `blackhole(true)` after two pings | phone closes with `ping_timeout` 70 to 95 s later, then redials | new `chaos::a_silent_half_open_link_is_closed_by_the_heartbeat_rule_within_its_bound` (soak only, about 2 min) |
| Half-open zombie on the phone + PC reconnect | blackhole, PC drops its side, dials again after 6 s | phone keeps the new connection, `rejected_duplicates == 0` | new `chaos::a_reconnect_replaces_a_half_open_connection_the_phone_still_holds` |
| Half-open zombie on the PC + phone back from a new address | blackhole the old path, phone dials the PC's listener directly after 5.5 s | PC replaces the held link, traffic flows | new `link_stability::a_phone_back_from_a_new_address_replaces_the_half_open_link` |
| IP change mid-session, phone side | transport restart (network change) | remembered PC redialed after the restart | existing `redial::a_restarted_transport_inherits_lan_addresses…` plus the case above |
| Silent loss with the phone unable to announce (asleep, broadcast filtered) | RST via proxy, no announcements | PC dials back the identity-declared port after 4 s | new `link_stability::a_phone_that_cannot_announce_is_dialed_back_after_a_silent_loss` |
| Clean close from the phone | phone drops its link | PC does **not** dial back | new `link_stability::a_clean_close_from_the_phone_is_not_dialed_back` |
| Redial asleep in its back-off | PC port refuses; nudge after two tries | third try within 2 s of the nudge; link as soon as the PC is back | new `chaos::a_nudge_wakes_a_redial_that_is_sleeping_in_its_back_off` |
| Spent budget | 12 failures | exactly one more try per nudge | new `redial::a_nudge_buys_exactly_one_more_try_once_the_budget_is_spent` |
| Phone user's disconnect + nudge | `transport_disconnect`, then `transport_heartbeat_now` | no redial | new `chaos::a_nudge_does_not_undo_the_phone_users_disconnect` |
| Announcement gaps (30 s cadence vs 20 s freshness) | announcements aged 25 s, then fresh | manual pause kept; back-off not reset (1 dial, not 4) | new `link_stability::a_manual_disconnect_survives_the_gap_between_two_announcements`, `…does_not_reset_the_back_off` |
| Reconnect storm bounds | pure schedules | phone: 12 tries, 2→60 s ±20 %, two phones not in lockstep; PC: Book 12 tries, ≤ 25 dials in 10 min with flapping addresses; dial-back 4, 8, 16, 32, 60 s, 12 tries | new `jitter_stays_in_its_band_and_actually_spreads`, `dial_back_gaps_double_from_the_grace_up_to_a_minute`, `a_silent_loss_is_dialed_back_after_a_grace_with_back_off_and_a_budget`; existing `review3_a_phone_announcing_from_alternating_addresses_is_not_dialed_in_a_storm` |
| Cross-dial | both dial at once | one connection, same on both ends | existing `cross_dial.rs`, `tiebreak::tests`, `collision::tests` |
| Fd/thread/memory under a storm | 100 (soak 1000) cycles | no growth | existing `reconnect_storm_leaks_no_fds_threads_or_memory` (both crates) |
| Loss/latency/fragmentation/slow handshake | `Faults` | pairing completes; a stalled handshake leaves nothing behind | existing |

**Not injectable in-process (need root, netns or a device):** a true no-ACK blackhole that triggers the kernel's
TCP_USER_TIMEOUT (needs `tc`/nftables in a network namespace), real suspend, Wi-Fi roam, DHCP renumbering, Android
default-network flips, Doze. For the Fold5, debug build, `adb logcat -s GB`:

1. Screen locked for 30 min while linked. Then `geekbridge-ctl events` on the PC and the phone's `link_closed code=` lines.
   Expect at most rare losses, each followed by `redial` or a PC dial-back within seconds.
2. Suspend the PC for 10 min with the phone locked, then resume. Expect the PC's dial-back (`connecting` 4 to 6 s after
   `link_lost`) to land without touching the phone.
3. Turn on "Switch to mobile data" and walk to the edge of Wi-Fi coverage. The log must show `wifi network available`
   lines but no "closing the link to re-announce" unless the Wi-Fi network or its address really changed.
4. Router DHCP lease change or reboot: phone `IPv4 address set changed`, a restart, a redial, and the PC replacing its
   zombie link.
5. `adb shell dumpsys wifi | grep -iA3 lock` with the screen off (F-7).
6. Battery: %/h with the screen off, linked, over two hours, before and after this change. The change adds no timer. It
   adds at most one dial per unlock while unlinked, and a PC-initiated dial per silent loss.

## 5. What changed

Rust (tests first; each new test failed before its fix):

* `bridge-kde/tiebreak.rs`: `CROSS_DIAL_WINDOW` (5 s), shared. `bridge-service/collision.rs` uses it.
* `bridge-android` engine: `admit` applies the tie-break only inside the window, so a zombie is replaced. Redial waits
  are jittered (±20 %) and race the nudge. `redial_on_nudge` grants one try to a spent budget. The announcement goes out on
  a nudge while some PC is unlinked. The phone's listening port is declared in its secure identity. A user disconnect
  suppresses redials for 5 min. Dial failures name `conn_refused` / `unreachable`.
* `bridge-service`: redial history kept while the phone was heard within 90 s; dial-back after a silent loss
  (`reconnect::Fallback`: endpoint learnt from announcements, our own dials and the phone's port hint; 4 s grace, doubling
  to 60 s, 12 tries; never after a clean close, a disconnect or an unpair). "Reconnecting" in the status includes it.
* `bridge-chaos`: resets are real RSTs.

Android: `LinkService` watches the Wi-Fi network, not the default network (`ServicePolicy.onWifiAvailable`, tested). Screen
on nudges the core. `PairingHelp` maps the new codes. The `WifiLinkLock` comment states the screen-off limitation.

Battery: no new timers, wake locks or polling. Added work per event: one ping per unlock; one dial per unlock per unlinked
PC; on the PC, at most 12 dial-backs over about 10 minutes after a silent loss. Removed: the PC's endless one-dial-per-
announcement loop (F-3) and the transport restarts on default-network flips (F-2).

## 6. Open items, in order

1. Bind the phone's outbound and announcement sockets to the Wi-Fi network (F-2 remainder). Needs the network handle
   passed to the core and `android_setsocknetwork`, then a device test with "Switch to mobile data" on.
2. Measure F-7 on the Fold5, then decide whether a high-performance Wi-Fi lock is worth its cost.
3. A bounded partial wake lock (for example 60 s after a loss while the screen is off) would let the phone's first five
   redials run in real time. F-1's PC dial-back covers the case where the PC is up, so measure first.
4. Persist the PC's dial-back endpoints across a service restart. They are in memory, so a PC that reboots while the
   phone is asleep still waits for an announcement.
