# Wi-Fi binding, Wi-Fi lock honesty and link health

Follow-ups from `link-stability-review-2.md` ("Found but not changed"): the phone's redials and announcements used
mobile data when it was the default network, the Wi-Fi lock claim was unverified, and the real-phone report could not
explain a drop on its own.

## 1. Keeping the core's sockets on Wi-Fi

**Problem.** One UI's "Switch to mobile data" (and poor-Wi-Fi handover) makes mobile data the default network while Wi-Fi
stays associated. An unbound socket follows the default network, so the core's redials and UDP announcements could
leave over mobile data, where the computer is not.

**Rejected:** `ConnectivityManager.bindProcessToNetwork` (captures every socket of the app: share, update check,
Android components) and a JNI callback into `Network.bindSocket` (the core creates sockets on its own runtime threads;
a callback per socket adds a JNI crossing and a failure mode to every dial).

**Design (least invasive).** Kotlin tells the core one thing: the Wi-Fi IPv4 address.

* `LinkService` records the IPv4 addresses of each Wi-Fi network from `onLinkPropertiesChanged`, forgets them in
  `onLost`, and calls `WifiBindPolicy.choose`: exactly one usable address (not 0.0.0.0, loopback, link-local,
  multicast, broadcast) overall, else none. Two Wi-Fi networks or several addresses mean stay unbound, because a
  wrong guess would pin the link to a network the computer is not on. It pushes only on change, and always once per
  service instance (the core's setting outlives the service object), and clears it on service destroy.
* New core action `transport_wifi_bind {ip?}` (docs/android-transport.md). Process-wide like `transport_screen`: no
  running transport needed, survives a restart. Rust validates again (unicast IPv4 only).
* Dials: `Endpoint::connect_from(addr, target, source)` in `bridge-kde` (`connect` is unchanged and calls it with
  `None`). With a source it tries the TCP connect from that address for up to 4 s, then falls back to the plain connect.
  The fallback covers: the address is gone (bind fails), no route from it, a timeout. A refusal is final (the plain route
  would say the same). IPv6 targets, loopback targets and other-family sources are never bound. The same
  bind-then-fall-back idea already exists for payload sockets (`dial_plan`, E2E-14).
* Announcements: the UDP socket is created from the Wi-Fi address (`bind_announce`, unbound fallback) and the loop
  swaps in a new socket before the next send when the wanted address changed. No restart needed.
* Existing connections are never touched. A Wi-Fi change still restarts the transport as before (`ServicePolicy`), and
  the new address is pushed before that restart runs.

**Tests.** Rust: `bridge-kde` `dial_tests` (bound dial leaves from the given address, with 127.0.0.1 as the stand-in and
127.0.0.2 on Linux to prove the bind is real; a source that is not ours falls back; no source / other family dial
plainly; a refusal is not retried); `discovery` tests (announcement leaves from the Wi-Fi address and follows a change;
real source on Linux; gone address falls back); `transport/mod.rs` (`dial_source_with` rules, address validation, set and
clear); `tests/transport.rs` (the action end to end). Kotlin: `WifiBindPolicy` (choice, rejection list, push rule) and
the request shape.

**Not verified here (needs the Fold5).** That Android routes a socket bound to the Wi-Fi address over Wi-Fi when the
default network is mobile. Android's policy routing selects by fwmark and by source address rules, and the LAN route
is normally in the local-network table for all sockets, so I expect it to work, and the 4 s fallback bounds the cost if
it does not. But E2E-14 showed that a socket bound to an address the default network does not own can connect and then
die, and that case is not caught by a connect-time fallback. To check: turn on "Switch to mobile data" (or toggle mobile
data as default with Wi-Fi still connected), kill the link from the PC (`geekbridge-ctl` disconnect or stop the
service), and watch the phone's redial in `adb logcat -s GB` and `ss -tn` on the PC for the phone's Wi-Fi source address.
If a bound socket does die, the lever is to drop the bind and use `Network.bindSocket` through a JNI callback.

## 2. The Wi-Fi lock

No Android sources are available offline here, so this is reasoning from the documented behaviour, stated as such.

* `WIFI_MODE_FULL_LOW_LATENCY` (API 29): Android applies it only with the screen on and the holder in the foreground.
  Confirmed by the review (F-7) and the platform documentation. It does nothing with the screen locked.
* `WIFI_MODE_FULL_HIGH_PERF`: exists on every level; deprecated in API 34 and documented as non-functional there.
  Before that it was the mode meant to work in the background.
* Therefore: the service now holds **both** (`WifiLockPolicy.modes`: HIGH_PERF everywhere, LOW_LATENCY added from API 29).
  The OS applies whichever it honours. Cost where HIGH_PERF works is battery, only while a computer is linked (plus the
  2 min redial window).
* Honest consequence: on the Fold5 (Android 14+) nothing keeps the radio out of power save with the screen locked.
  The link survives it through the 20 s / 60 s heartbeat, kernel keepalive and redial. Corrected in
  `docs/android-power.md`, `real-device-link-stability.md` (the Wi-Fi lock row and the battery paragraph) and marked
  in `link-stability-review-2.md` F-7. To verify: `adb shell dumpsys wifi | grep -iA3 lock` with the screen off, and
  `dumpsys batterystats` Wi-Fi-on time.

## 3. Link health in the debug state

`DebugState` already had the link phase, a 50-entry transition history with the core's reason code, and the current
network type. Missing: the network *at the time of a drop* and *how long the computer had been silent*. Added:

* Core: `link_closed` events carry `quiet_ms` (time since anything was last heard, from `Heartbeat::silent_for`).
  Kotlin parses it (`LinkEvent.quietMs`, optional, ignored if negative or not a number).
* Debug build: each `link_closed` records a `DropNote` (time, reason code, network description then, silence), the
  history line now reads `... reason: ping_timeout; silent for 74 s; network: wifi, internet validated, unmetered`, and
  the dump has a **Link health** section: last close (age, reason, network then), silence before it, and the number of
  closes since the app started. Structured facts only, no addresses.

Read it as: `ping_timeout` with a large "silent for" and network `cellular` points to the default-network handover;
`conn_reset` with a small silence points to the computer or the AP; `tcp_timeout` is the kernel's 60 s limit.

## Results

`cargo fmt --check` 0, `cargo clippy --workspace --all-targets -D warnings` 0, `cargo test -p bridge-core -p bridge-kde
-p bridge-android` 0, Gradle `:app:testDebugUnitTest :app:lintDebug` 0 (includes the debug-only tests).
