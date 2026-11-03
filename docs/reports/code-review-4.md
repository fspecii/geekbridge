# Code review 4: production changes since v0.1.0-rc.1 (bb2ff09cc..origin/main)

Scope: reconnect, doctor, why, phone_data, daemon (service); battery_link, why_disconnected, heartbeat,
debug_link, statefile, calls_link (core); the Android transport (screen pause, announce pause, dial);
Kotlin QrPairActivity, BatteryReport, SmsReadiness, dial, LinkService screen receiver; packaging/linux/install.sh.

## Fixed

1. **Computer never heartbeated a default phone (regression of the stock-peer ping rule).** The computer pings only
   peers that declare a `geekbridge.*` capability. The phone declared one only for clipboard, calls, calendar, SMS or
   debug. With every optional feature off (the default), it declared just `kdeconnect.ping` and
   `kdeconnect.share.request`, so the computer sent no pings, the phone's silence rule (which arms on the computer's
   pings) never armed, and a vanished computer (suspend, Wi-Fi loss) was noticed only by TCP retransmission timeout
   instead of within 70 s. Reproduced by
   `transport::coexist_tests::a_phone_with_everything_off_still_identifies_as_a_geekbridge_peer` (failed first).
   Fix: the phone always declares the packet-less marker `geekbridge.heartbeat` (`heartbeat::MARKER`) in both
   directions. Stock KDE Connect ignores unknown capabilities; two transport tests that pinned the exact list were updated.
2. **Stale battery level.** A stock phone that reports `currentCharge: -1` (level unknown) left the previous
   percentage on screen as current. `shown_battery` now maps `Unknown` to nothing.

## Checked, no bug

* **Heartbeat arithmetic.** Phone idle ping every 60 s (ticks of 20 s, `ping_due_idle` with 1 s slack, `last_sent`
  stamped after the write: pings at 60, 120, ... s). Silence limit 70 s is enforced only by the phone, on the
  computer's pings (every 20 s, margin 50 s, and any packet counts as life). The computer never cuts a phone off for
  silence (`peer_silent` is used only in the Android engine), so a single late or lost idle ping cannot drop a healthy
  link; TCP does not lose pings, it only delays them. Margin if a computer ever did enforce: 10 s, i.e. less than one
  20 s tick, so that would be unsafe; do not add computer-side enforcement without lifting the limit above
  `IDLE_EVERY + EVERY`.
* **Battery consent, both ends.** Phone: feature off by default, declared only when on, sent only to connections
  permitted per computer (`permitted_ctls`), incoming request dropped unless permitted. Computer: consent default
  off, packets dropped unread without consent, withdrawal clears the value, request sent only with consent and a
  phone that declared the request; malformed packets count toward the rejected-packet limit.
* **Dial.** Number validated identically in three places (Rust `valid_dial_number`, Kotlin `dialable`, ctl/app):
  digits with optional leading `+`, at most 20, so no USSD/MMI. The hub re-checks that the view still offers dial
  (Telephony consent, phone reported CALL_PHONE, no call going on); the phone requires CALL_PHONE, an idle known state
  and the per-computer `calls` grant. `geekbridge-ctl call dial` refuses without a terminal or `--yes`. The
  desktop app builds the request only after its confirmation (`dial_request` doc). Any holder of the control token can
  still send `CallDial` directly; the confirmation is a client duty by design.
* **IPC backward compatibility.** All new `Status`/`LinkStatus`/`CallsStatus`/`CallsView` fields carry
  `#[serde(default)]`; `ReasonCode` has `#[serde(other)]`. `Request::CallDial` is a new variant (an old service
  answers with a bad-request error, not a crash).
* **Unbounded growth.** Send book pruned to 200; notification map capped at 64; sightings capped at 256; debug log
  flood test is bounded; `last_end` scans a bounded event queue.
* **Locks.** `why_disconnected` takes inbound, reconnect, sightings in sequence while holding core; no path takes
  them in the reverse order.
* **Hostile input.** Battery, call-state, doctor ufw parsing and debug sanitising use checked parsing; no slicing or
  unwraps on remote data. Device names are sanitised at identity parse before they reach `doctor`.
* **install.sh.** `--purge-data` only deletes `$XDG_STATE_HOME/geekbridge` and only when the path matches
  `/*/geekbridge`; all expansions are quoted; no eval. The service data is under `XDG_DATA_HOME` for the daemon
  (see `ipc.rs`) while the script purges the state directory: the message names the directory it removes, but check
  that pairing data (`~/.local/share/geekbridge`) is what users expect to be purged (open question below).
* **Android.** QrPairActivity replays the launch link only while a confirmation was pending and keeps the one-time
  token out of saved state; LinkService unregisters the screen receiver and restores "screen on"; `BatteryReporter` is
  event driven, with a single trailing send guarded by `pending`; `driftAtLinkUp` cannot loop because `declared` is
  updated before the restart.

## Open

* `install.sh --purge-data` deletes the state directory only; pairing and identity live under
  `$XDG_DATA_HOME/geekbridge` (`~/.local/share/geekbridge`), so the purge may not remove pairings as its message says.
  Left alone: needs a decision on which directories the installer owns.
* The phone stops announcing while every paired computer is linked; a new, unpaired computer is found because it
  announces itself and the phone dials it, so pairing still works, but this was not exercised on a device.
