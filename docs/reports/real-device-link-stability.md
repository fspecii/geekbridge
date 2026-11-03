# Real-device link stability (Galaxy Z Fold5, One UI)

Symptom: on the owner's Fold5 (Wi-Fi 192.168.1.156, PC 192.168.1.150) the link drops every few minutes. The PC logs
`link_lost` with `peer closed the connection` or `I/O error: Connection reset by peer (os error 104)`; the PC's own
redial (`crates/bridge-service/src/reconnect.rs`) recovers within seconds.

## 1. What the PC log shows

Not much, and that is a finding. The service's event ring (`geekbridge-ctl events 0`) is in memory and holds only
what happened since the last service start (18:56 on 2026-11-02, three events: `connecting`, `connected`,
`consent_changed`); `dist/realdevice/service.log` holds the banner only. The earlier drops are gone, so **no interval
analysis of the real drops was possible** and none is claimed here. What the two reasons tell us:

* `Connection reset by peer` (ECONNRESET): the phone's kernel (or a middlebox) sent an RST. Typical causes: the app
  process was killed and the kernel closed its sockets, Wi-Fi roam/power-save with a stale NAT/AP state, or the app
  closed the socket itself while data was in flight.
* `peer closed the connection` (EOF): an orderly close from the phone's side: the app (or its process teardown) closed
  the socket.

Both mean the **phone side ended the connection**; neither is the PC timing out. The most likely candidates in this code,
none confirmable from the old log:

1. `LinkService` restarts the whole transport (`controller.stop` then `start`) on every default-network change or
   IPv4 address change (`onAvailable` with another network handle, `onLinkPropertiesChanged`). That closes a healthy link
   on purpose. One UI flips the default network between Wi-Fi and mobile data ("Switch to mobile data", Wi-Fi
   re-validation), which produces exactly this on a schedule that looks irregular but recurs every few minutes.
2. Wi-Fi power save on the radio (One UI is aggressive): beacon-only reception drops or delays packets on an idle link.
3. The app process being stopped/restarted by One UI (a new process has new sockets).
4. Not a candidate: the foreground-service timeout (`connectedDevice` has none; `dataSync`'s 6 h cap does not apply)
   and kernel keepalive (below), which is active.

The phone now records the reason (see 3.), so the next occurrences are attributable.

## 2. Code review of the socket handling

* **Keepalive on Android: already applied, not excluded.** `bridge-kde/src/keepalive.rs` is `cfg(any(linux, android,
  macos))` for the probe interval/count and `cfg(any(linux, android))` for `TCP_USER_TIMEOUT`; `configure_tcp` is
  called on both `Endpoint::connect` and `Endpoint::accept`, so every phone socket gets 30 s idle / 10 s interval x 3 /
  60 s user timeout. The phone transport also had a second `enable_keepalive` (120 s / 30 s x 4) that ran before and was
  overridden by that call; it was misleading and is removed. Type-checked for `aarch64-linux-android`
  (`source scripts/android-env.sh && cargo check -p bridge-kde -p bridge-android -p bridge-core --target
  aarch64-linux-android`: clean).
* **No application traffic on an idle link.** Nothing was sent unless something happened, so NAT/AP idle timers and the
  phone's network-idle handling saw only kernel keepalive. Fixed by a heartbeat.
* **No loss reason on the phone.** `link_closed` carried `io_error` for every I/O failure. Fixed (below).
* **Redial only on the PC.** The phone never dialed back. Fixed (below).

## 3. Changes

| Area | Change |
|---|---|
| Heartbeat | New `bridge-core/src/heartbeat.rs` (pure, tested). Both sides send `kdeconnect.ping` every 20 s. The phone closes a link whose peer has been heartbeating but is then silent for 70 s (`ping_timeout`); a peer that never pinged (stock KDE Connect) is never cut off. The PC does **not** cut the phone off for silence: a sleeping phone must not be dropped by a timer, the kernel keepalive decides. The phone's timeout check is last in the biased `select!`, so packets already queued after a long sleep win. The clock is `CLOCK_MONOTONIC`, which does not run during deep sleep. |
| Loss reason | `kde_code` now names `conn_reset`, `broken_pipe`, `conn_aborted`, `tcp_timeout` (still `io_error` otherwise) and the loop reports `ping_timeout`. On the phone (debug builds, tag `GB`) `TransportController` logs every transport event with its code (`link_closed code=conn_reset`), `LinkService` logs default-network available/lost, IPv4 change, "closing the link to re-announce on the new network", link-lost and service-destroyed, and `TransportController.stop` already logs "close: requested by this phone (reason)". `adb logcat -s GB` answers *who closed and why*. |
| Phone redial | `transport/engine/redial.rs`: after an unexpected loss (`link_closed`; not the user's disconnect, unpair, stop or peer unpair) the phone dials the computer again at the address it last dialed (or the computer's IP and the port last used, default 1716, if the computer only ever dialed in). Backoff 2 s doubling to 60 s, 12 tries, one task per device, paired devices only, address policy enforced, emits a `redial` event. It goes through `begin_attempt`/`admit`, so a redial that crosses the PC's own redial ends with one connection (`prefer_new`, the existing symmetric cross-dial rule, plus `duplicate_connection` on a second one). LAN addresses survive a transport restart (network change) in a process-level map and are dialed right after the restart; loopback is never carried over; unpair forgets. |
| Wi-Fi lock | `WifiLinkLock.kt` / `WifiLockPolicy.modes`: `WifiLock`s held only while a computer is paired **and** a trusted link is up, plus 2 minutes after a loss; released otherwise and in `onDestroy`. Modes: `FULL_HIGH_PERF` on every API level and, from API 29, `FULL_LOW_LATENCY` as well. **What this does and does not do** (corrected, see docs/reports/wifi-binding.md): low-latency is applied by Android only with the screen on and the holder in the foreground; high-perf is deprecated in API 34 and documented as having no effect there. So on the Fold5 (Android 14+) nothing keeps the radio out of power save with the screen off, and the lock is a screen-on improvement; the screen-off link relies on the 20 s / 60 s heartbeat, kernel keepalive and the redial. Earlier text here said the lock kept the radio awake while linked; that claim was not verified and is withdrawn. Needs the normal permission `WAKE_LOCK`; it does not keep the CPU awake. |
| Multicast lock | Already scoped to discovery screens only (`PairPanel`, `LinkSection`, `ShareActivity`), none in `LinkService`. Unchanged. |

### Battery impact

Cost is one 20 s ping each way (and, with the screen on, the radio kept out of power save by the lock), which keeps
the radio busy about as often as the 30 s kernel keepalive already did. With no link nothing is held. The emulator has
no real Wi-Fi radio or battery model, so `dumpsys batterystats` Wi-Fi/wake time on it is **meaningless**; the number
that matters has to be measured on the Fold5 over a few hours: battery %/h with the link up vs. down, and
`dumpsys wifi | grep -i lock` / `dumpsys batterystats` Wi-Fi-on and "Wifi lock" time. If it is too high, raise
`bridge_core::heartbeat::EVERY` (the PC side needs the same constant; they share it) before dropping the lock.

## 4. Verification

* Rust: `bridge-core` heartbeat tests (due/defer, never cut off a non-heartbeating peer, timeout and reset);
  `redial` tests (backoff 2..60 s, 12-try budget, single task, port/IP handling, carry-over/forget); `kde_code` cause
  names; existing `cross_dial_resolution_is_symmetric`; chaos test
  `the_phone_redials_after_a_reset_mid_link_and_keeps_a_single_connection` (bridge-chaos proxy resets a live link; the
  phone redials through the proxy, the loss reason is named, exactly one trusted connection remains); the existing
  reconnect-storm, keepalive-armed and PC chaos/lifecycle suites still pass. `cargo fmt`, `cargo clippy --all-targets
  -D warnings` clean on bridge-core, bridge-android, bridge-service.
* Android: `scripts/build-android.sh` (host tests, Gradle unit tests including `WifiLockPolicy` tests, lint) passes.
* Emulator (own adb server 16091, AVD gb36x, own PC service on 17590/17591): pairing and PC-initiated reconnect after
  `adb shell am force-stop` pass (`e2e-android.sh --only env,pair,reconnect`; the first `reconnect` assertion fails only
  because the freshly installed app was not running yet, the restart assertion passes). Phone-initiated redial was
  **not** driven on the emulator: its only known PC address there is the host alias 10.0.2.2, port 1716, which is the
  owner's live service, and a test dial would have hit it. That path is covered by the chaos test instead.
  `svc wifi disable/enable` on the emulator only moved the default network (virtual Wi-Fi to mobile data) and
  produced the network-change restart log above; it does not reset TCP and cannot reproduce Wi-Fi power-save drops.

## 5. Known trade-offs / open items

* A **PC-side manual disconnect** now also gets a phone redial (an EOF cannot be told from a drop), so "disconnect"
  on the PC reconnects within a few seconds. If that is not wanted, the PC needs to tell the phone (a small protocol
  message) or the phone should skip the redial after `peer_closed`.
* The phone's guess of the PC's port for an inbound-only link is 1716; a PC listening elsewhere makes the first redial
  tries fail (target-id check protects against a wrong host) until the phone has dialed it once.
* The network-change restart (cause 1 above) still closes a healthy link on purpose; the phone now redials right after
  it, but if the logs show it as the main cause, the next step is to skip the restart when the Wi-Fi IPv4 did not change.
* Please collect `adb logcat -d -s GB` (debug build) after the next drops; the `link_closed code=` lines and the network
  lines before them name the cause.

## 6. Artifacts

PC release binaries rebuilt in `target/release` (`geekbridge-service`, ctl, apps; the owner's running service was not
restarted). Debug APK: `dist/android/geekbridge-debug.apk` and `dist/android/serve/geekbridge-debug.apk`
(sha256 `53d0449f…eb4`, replaced atomically).
