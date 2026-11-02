# Why disconnected (parity gap 4)

A paired phone that has no link now gets one reason code, one plain sentence and one suggested fix. It is computed
in the service from data it already has and shown by the desktop Home tile and Details card, `geekbridge-ctl status`
and `doctor`, the tray tooltip and the waybar line.

## Where the answer comes from

`bridge_core::why_disconnected` is a pure function: `Facts` in, `ReasonCode` out (`classify`), then `explain` adds
the English sentence and fix. The service fills `Facts` in `Daemon::why_disconnected` (`bridge-service/src/daemon.rs`),
helpers in `bridge-service/src/why.rs`.

| Fact | Source |
| --- | --- |
| redial attempts, paused by hand | `reconnect::Book::progress` |
| announcing now, last seen age and address | new `Sightings` map, fed from discovery on every `reconnect_tick` (discovery itself forgets a device after 90 s) |
| how the last link ended | newest `LinkLost`, `ConnectFailed`, `PeerUnpaired`/`Unpaired`, `Disconnected` event for the device |
| computer has a network, same network as the phone | `lan_select::system_interfaces` (up, non-loopback, non-link-local; /24 match for IPv4) |
| listener | `Daemon.inbound` |
| firewall | `doctor::firewall_blocks` (ufw enabled and readable rules lack TCP or UDP 1716-1764); only `geekbridge-ctl` can read it, so it refines the answer there (`with_firewall_blocking`) |

Order (first match wins): `computer_offline`, `not_listening`, `paused_by_user`, `pairing_lost`, then if the phone is
announcing: `gave_up` (12 tries) or `reconnecting`; else `different_network`, `link_timed_out` (loss reason mentions
timeout, silence, heartbeat, reset, broken pipe), `phone_silent`, `not_seen_yet`. `firewall_blocking` replaces
`reconnecting`, `gave_up`, `phone_silent` and `link_timed_out` when the ctl knows the firewall is closed.
`unknown` is what an older client shows for a code from a newer service (`#[serde(other)]`).

## Compatibility

`Status` gains `disconnected: Vec<WhyDisconnected>` with `#[serde(default)]`; `IPC_VERSION` is unchanged, old
clients ignore the field, new clients treat a missing field as no reason (the old generic lines still show). Each
entry carries the code, the English sentence and fix, `last_seen_ago_s` and `tries`.

## Surfaces

- Desktop Home phone tile: the sentence replaces "Paired, not reachable right now" and "Reconnecting...".
  Details card: "Try: ..." and "Last heard N min ago". Strings are `why.*` in `bridge-apps/i18n/en.properties`
  (`why_ui.rs` maps code to key); no new literal outside the catalogue, so the allowlist is unchanged.
- `geekbridge-ctl status`: JSON on stdout as before (now with `disconnected`); one "is not connected: ... Try: ..."
  block per phone on stderr. `doctor` uses the same sentence and fix for the Phones check.
- Tray: the status line and tooltip body use the sentence; the tooltip adds "Try: ...".
- waybar: tooltip gains the sentence and fix; class gains `why-<code>` while disconnected.

## Not done

- "Battery restricted by the phone's own report": the phone does not send that state yet, so there is no such code.
  Add a `battery_restricted` code (and a fact) when the phone reports it; old clients will show `unknown` for it.
- No golden changed: the golden screens show a connected phone or the service-off card, never a paired phone
  without a link. `make golden` was not run here because the tree also held the unrelated battery work.

## Tests

Each code has a mapping test in `why_disconnected.rs` (priority, firewall refinement, serde round trip, unknown
code), event/network/sighting helpers in `why.rs`, redial progress in `reconnect.rs`, doctor lines and firewall
rule check in `doctor.rs`, tray and waybar rendering, catalogue/English parity in `why_ui.rs`, and the service
status in `tests/lifecycle.rs`. `fmt --check`, `clippy -D warnings` and the tests of bridge-core, bridge-apps
and bridge-service all exit 0.
