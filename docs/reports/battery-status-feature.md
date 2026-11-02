# Phone battery and charging state (parity gap P1)

Closes the "Battery level and charging state of the phone" gap in `parity-gaps-2.md`. Protocol details are in
`docs/kdeconnect-interop.md` ("Battery status").

## What it does

* The phone sends `kdeconnect.battery` (`currentCharge`, `isCharging`, `thresholdEvent`) and answers
  `kdeconnect.battery.request`. Stock names only, so stock KDE Connect interoperates both ways.
* The computer keeps the latest reading per live link, in memory (`links[].battery` in the status), and shows it in
  the tray tooltip and menu status line, the Home phone card, `geekbridge-ctl status` (JSON) and
  `geekbridge-ctl status --waybar` (tooltip line, `charging` and `battery-low` classes, numeric `battery` field).

## Decisions

* **Default off on both ends**, like the other private data: a battery level tells when a phone is idle or away.
  Phone: "Share battery level" switch (Settings and the data section of setup) plus a per-computer allow
  (`GrantFeature.Battery`, the existing grants list; the native core refuses to send to any computer not on it, and a
  computer paired later gets nothing). Computer: `Feature::Battery` consent per phone, set with
  `geekbridge-ctl consent ID battery on` or the one-click "Show phone battery" button on Home (offered only when the
  phone declared the packet; nothing is switched on without a click).
* **Capability declared only when enabled.** The phone adds `kdeconnect.battery` / `kdeconnect.battery.request` to its
  identity only while the switch is on (the existing capability re-declare path reconnects on a change). It needs no
  Android permission, so the gate is the switch alone. The computer always declares incoming `kdeconnect.battery` and
  outgoing `.request`; both need the peer's matching declaration to negotiate.
* **Unasked stock packets are dropped, not punished.** Without the computer's consent a battery packet is dropped
  unread and does not count toward the rejected-packet budget (stock phones send it unasked). Malformed ones do count.
* **Event driven, no polling, no wake lock.** The app registers for the sticky `ACTION_BATTERY_CHANGED` broadcast only
  while the link runs and the switch is on (the sticky delivery is the send at link start). Charging or low-flag
  changes go out at once; percentage changes at most once a minute with one trailing send; repeats are suppressed.
  A computer that is allowed, connects, or sends `.request` gets an immediate reply on its own connection.
* **Withdrawing erases.** Turning the computer-side consent off clears the stored reading at once; the link end clears it too.

## Files

Rust: `crates/bridge-core/src/battery_link.rs` (new: parse and body), `ipc.rs` (`Feature::Battery`, `BatteryStatus`,
`LinkStatus.battery`), `crates/bridge-service/src/{gate,daemon,phone_data,waybar}.rs`, `tray/{model,mod}.rs`,
`bin/geekbridge-ctl.rs`, `crates/bridge-apps/src/home_app.rs`, `crates/bridge-android/src/transport/{mod.rs,engine/data.rs,engine/permits.rs}`,
`crates/bridge-phone-sim/src/data.rs`.
Kotlin: `BatteryReport.kt` (new), `PhoneDataProtocol.kt`, `PhoneDataEngine.kt`, `PhoneDataAndroid.kt`,
`PhoneDataUi.kt`, `ComputerGrants.kt`, `res/values/gb_phonedata.xml`.

## Tests

* Negotiation: `crates/bridge-service/tests/android_negotiation.rs` (updated deliberately: the real stock Android
  1.37.0 identity now negotiates `kdeconnect.battery` as the second incoming type, routed to the battery handler;
  no request is offered because that build accepts only `kdeconnect.battery`), `gate.rs` unit test for a phone that
  declares both sides.
* Desktop: `battery_link` unit tests (strict parsing, stock extras ignored, -1 unknown), tray model and Home card
  tests, end to end over pinned TLS in `crates/bridge-phone-sim/tests/data_link.rs`
  (`battery_is_read_only_with_consent_asked_for_and_erased_when_withdrawn`: dropped without consent, request sent on
  consent, reading kept, malformed counted, unknown ignored, erased on withdraw).
* Phone core: `crates/bridge-android/tests/data_transport.rs` (declared only when enabled, permit list, request event,
  reply to its own connection, off refuses sends); Kotlin `BatteryReportTest` (debounce, urgent changes, disabled,
  rate limit, system extras, gate and grants).
* Stock-peer interop: fixture-level only (the real stock Android identity capture). No live stock daemon or phone
  capture of battery packets exists; that stays a hardware step.

## Results

* `cargo fmt --all --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
* `cargo test --workspace --no-fail-fast`: see the final line of the commit message run (log
  `/home/ambsd/.cache/gb-tmp/bat-cargo-test.log`).
* `./gradlew --offline :app:testDebugUnitTest :app:lintDebug`: exit 0.

## Not done

* Notification-style alerts on the computer for a low battery (the `low` flag is carried and shown as a class).
* Multiple phones in the tray line (the first connected phone that reports a level is shown).
* A live capture against a stock KDE Connect daemon or phone.
