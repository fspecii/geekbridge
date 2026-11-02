# Android battery and idle cost audit 2

Scope: foreground service, heartbeat, keepalive, locks, network callbacks, discovery, polling, notifications, widgets, in
Kotlin and the Rust core. Behaviours in `real-device-link-stability.md` (20 s heartbeat, 70 s silence rule, Wi-Fi lock
with 2 minute linger, phone redial, restart on network change) are unchanged.

## Findings

| Area | State | Verdict |
|---|---|---|
| Foreground service | `connectedDevice`, notification built once, never updated | fine |
| Network callback | one default-network callback, debounced restart, unregistered in `onDestroy` | fine |
| WakeLocks | none (no CPU wake lock anywhere); one `WifiLock` held only while linked plus 2 min | fine |
| Multicast lock | only in `PairPanel`, `LinkSection`, `ShareActivity`; released on detach, stop and leaving the foreground; none in `LinkService` | fine |
| Kernel keepalive | 30 s idle / 10 s x 3; app pings (20 s) keep resetting it, so probes rarely fire | fine |
| Widgets | `updatePeriodMillis=0`; event driven; one inexact non-waking `AlarmManager.set(RTC)` for the agenda, cancelled with the last widget; calendar observer debounced 2 s | fine |
| Kotlin polling | `HomeActivity` 2 s, `PairPanel` 0.5/1 s, `LinkSection` 0.5/2 s, `ShareCoordinator` 0.7 s: all only while an Activity is started and stopped in `onStop` | fine |
| Long-poll threads | link, clipboard, calls, data waiters block in the core (25 to 55 s timeouts); no busy loop | fine |
| Rust threads when idle | tokio runtime with 2 workers, max 2 blocking; no spinning timers other than the ones below | fine |
| **Phone heartbeat screen on or off** | pinged every 20 s always | **fixed**: 60 s while the screen is off |
| **UDP announcement while fully linked** | broadcast every 30 s even when every paired computer was already linked | **fixed**: skipped while linked |

Not changed on purpose: the 25 s cap of `transport_data_wait` (its 60 s event TTL assumes it); raising it to 55 s would save
a wake-up every 25 s but changes stale-request handling, so it needs its own review. The PC still pings every 20 s (it
does not know the phone screen state), so the phone's radio still receives every 20 s; the saving is the phone's transmit.

## Changes

* `bridge-core/src/heartbeat.rs`: `IDLE_EVERY` (60 s) and `Heartbeat::ping_due_idle`. The phone's silence limit for the
  peer (70 s) is untouched because the peer keeps its own cadence; the PC still never cuts a sleeping phone off.
* `bridge-android`: new action `transport_screen {on}` (process-wide flag, works without a running transport). The
  heartbeat tick uses the idle cadence while the screen is off. When the screen turns on the next tick (at most 20 s)
  pings again.
* `LinkService` registers a screen on/off receiver while it runs (unregistered in `onDestroy`) and tells the core.
  Lock-screen reconnect is unaffected: the Wi-Fi lock, kernel keepalive, redial and PC redial do not depend on it.
* `discovery::announce_loop` skips the broadcast while `Core::all_paired_linked()` (at least one paired computer and a
  trusted link to each). Any lost link, an unlinked second computer or no pairing at all brings announcements back at the
  next 30 s round; the redials run immediately anyway.

## Tests

* Rust: `idle_pings_are_three_times_rarer_and_wake_up_on_the_next_tick` (heartbeat),
  `announcements_pause_while_every_paired_computer_is_linked` (UDP loopback, both states),
  `the_screen_state_needs_no_running_transport_and_rejects_bad_input` (dispatch).
* Kotlin: `TransportTest` covers `screenRequest`.
* `cargo fmt --all`, `cargo clippy -p bridge-core -p bridge-android --all-targets -- -D warnings` and
  `cargo test -p bridge-core -p bridge-android` exit 0; Gradle `testDebugUnitTest` exit 0.

## Not measured

No AVD run: the emulator has no Wi-Fi radio or battery model, so `dumpsys batterystats` would not show the effect
(same conclusion as the link stability report). To confirm on the Fold5: screen-off battery %/h with the link up, before and
after, plus `dumpsys wifi | grep -i lock` and `dumpsys alarm | grep geekbridge`. If a NAT or AP drops the idle link at 60 s,
lower `IDLE_EVERY`.
