# Network hostility: the desktop service and the phone core on a realistic home network

Method: `unshare -rn` network namespaces (works unprivileged here), dummy and veth interfaces, a second
namespace as the "phone", a built `geekbridge-service` with a private `--state-dir`. Nothing touched the live service
or ports 1716, 17381, 8000 of the host (the sandbox has its own network stack). The rig is
`scripts/netns-hostility.sh nics|isolate|flood|ipv6` (scratch under `~/.cache/gb-netns`). Findings that were wrong
have a regression test; the rest is documented behaviour with how it was verified.

## Found wrong, fixed (test first where a test could be written before the change)

| # | Finding | Evidence before | Fix and test |
|---|---------|-----------------|--------------|
| 1 | Directed announcement used a hard-coded `/24` (`a.b.c.255`). On a /16 or /23 Wi-Fi that address is an ordinary host, so nobody heard the announcement unless the limited broadcast (default route only) happened to reach them. | rig `nics`: `wlan0 10.20.0.10/16` was sent to `10.20.0.255` | `Iface.prefix` read from `getifaddrs` netmask; `lan_select::announce_targets` uses `Iface::directed_broadcast`; tests `directed_broadcast_follows_the_real_netmask`, `announcements_go_to_each_lan_like_nic_and_not_to_vpn_or_docker`. Rig now sends `10.20.255.255`. |
| 2 | "Different network" was a `/24` compare in two places (`pairing_help::same_subnet24`, `why::on_our_network`). A phone at 10.20.7.3 next to a PC at 10.20.0.10/16 was told it is on a different network (the `different_network` reason, with the wrong advice). IPv6 was never judged and a v6 address on the PC made every v4 phone count as "same network". | unit reproduction | Prefixes carried into `Measured.own_prefixes` and `Iface`; families compared only with their own family. Tests `a_slash_16_or_23_home_network_is_not_a_different_network`, `ipv6_prefixes_are_judged_and_mixed_families_are_not`, `same_network_follows_the_interface_prefix_on_any_usable_up_interface`. |
| 3 | Client isolation: `geekbridge-ctl test-connection` failed with "cannot reach GeekBridge service control port: service closed the connection" exactly when the router silently dropped TCP. The probe waited 4 s, the control API's exchange deadline is 3 s. | rig `isolate` (phone announces, its namespace blackholes replies) | `PROBE_TIMEOUT` 1.5 s, test `the_connection_probe_gives_up_before_the_control_exchange_does`. Output now: "The phone can be seen but does not accept connections (client isolation or a firewall)", the step says the announcement was heard, and the first advice is the guest-Wi-Fi / "client isolation" line, then app, firewall. Test `announcement_heard_but_tcp_blocked_says_client_isolation_first`. |
| 4 | An IPv6-only PC put `[fd..]:1716` in the QR but listened on `0.0.0.0` only: the phone's dial was refused. | rig `ipv6`: `connect ... Connection refused` for the ULA and for the link-local address | A second, IPv6-only listener on `[::]:<same port>` (not dual-stack, so IPv4 phones keep arriving as IPv4 addresses and the sighting/trust records are unchanged), skipped with a log line when the machine has no IPv6. Test `the_ipv6_twin_listens_on_the_same_port_and_leaves_ipv4_alone`. Rig: ULA and `fe80::..%ph0` both connect. |
| 5 | A unique-local IPv6 address was never in the QR when IPv4 existed, so a phone on an IPv4-less Wi-Fi had nothing to dial although the PC was reachable. | code | ULA now trails the IPv4 addresses (the phone tries them in order; a working IPv4 never waits). Test `a_unique_local_address_trails_ipv4_but_a_global_one_does_not`. |
| 6 | Wrong clock. Certificate dates were enforced in three places (own identity at load, the TLS verifier in both roles, a second check after the secure identity). A device whose clock says 2020 or 1970 refused to start ("own certificate unusable"), or refused every paired peer ("not yet valid"); a phone that generated its identity while its clock said 1970 got a certificate that expires in 1980 and the code then refused to rotate it: permanently stuck. | code reading, test failing on the old behaviour | Trust is the byte-exact pin (paired) or the user-confirmed fingerprint (new), so the dates carry nothing: not enforced anywhere. New identities are valid 1970-01-02 to 2100-01-01 so strict third-party tools also cope. Existing narrow certificates keep working. Tests `a_wrong_clock_does_not_make_our_own_certificate_unusable`, `an_old_intact_identity_is_loaded_and_never_rotated`, `a_wrong_clock_does_not_stop_a_pinned_or_new_peer_certificate`, `the_pin_and_the_claimed_name_still_decide_whatever_the_clock_says`. |

## Verified, no change

1. **IPv6 and the announcement family.** UDP discovery is IPv4 broadcast only, on both sides (KDE Connect protocol); the
   identity datagram carries no address, the receiver uses the packet source. There is no IPv6 multicast. So an IPv6-only
   guest Wi-Fi gives no discovery in either direction: pairing works by QR (now with a working IPv6 listener, finding 4),
   reconnecting after a PC restart needs the QR again. Link-local `fe80::` is never put in a QR and the phone's host field
   takes no scope id (docs/android-transport.md); the PC accepts a link-local peer with a scope (rig). **Open limit:**
   `AddressPolicy` accepts private, link-local, CGNAT and ULA only, so a network that has *only* global IPv6 addresses
   is refused ("address is not on a permitted local network"). Fix would need an on-link test (same prefix as an own
   interface); not done here, documented in real-device-quickstart T6.
2. **Several NICs.** Rig with `eth0 192.168.1.10/24` (default route), `wlan0 10.20.0.10/16`, `docker0`, `tailscale0`,
   `wg0`: announcements leave as limited broadcast (default-route NIC only, as the kernel routes it) plus the directed
   broadcast of `eth0` and `wlan0`; none to docker0, tailscale0 or wg0 (name-prefix filters in `lan_select`, tested in
   `busy_machine`). The QR lists default-route private address first, then the other private NIC, never docker, tunnels,
   CGNAT or link-local.
3. **AP client isolation.** See finding 3. `geekbridge-ctl doctor` does not probe a phone; for a paired phone that is
   heard but will not connect it says "would not connect" (`gave_up`), whose fix text mentions the firewall only. Adding
   "client isolation" there needs the string in every translation (`bridge-i18n`), so it is left as follow-up; the
   test-connection answer carries it.
4. **mDNS absent.** No mDNS code exists; nothing waits for it. QR pairing and the manual address need no discovery; a
   network that filters broadcast only loses "the phone appears in the list" and reconnect after a restart.
5. **UDP loss, duplication, reordering, oversized datagrams.** Announcements are idempotent upserts keyed by device id
   (duplicates and reorder change only the timestamp), loss is covered by the 15 s (PC) and 30 s (phone) repeat and a
   90 s / 120 s expiry. Over-size (16 KiB + 1, 60 000 bytes), empty, truncated JSON all dropped in the flood rig with no log
   line; the Windows `WSAEMSGSIZE` case is handled in `listen_discovery`. Already fuzzed.
6. **100 phones announcing.** Rig `flood`: 100 phones from 100 source addresses, every packet duplicated, 7 rounds, plus one
   host inventing 60 ids, plus garbage: RSS 10.2 MB to 14.4 MB (flat after the first round), 103 devices listed (100
   real + 3 extra ids of the spoofing source: 4 ids per source, 30 packets per 10 s per source, 256 sources, 128
   devices overall), service log still 1 line. Beyond 128 devices new ones are silently not listed until entries expire
   (a hostile LAN with 128 spoofing sources could keep real phones out of the *list* for 90 s; pairing by QR is
   unaffected). The phone core keeps 32 entries, 64 packets/s, 120 s TTL and evicts the stalest.
7. **Clock skew.** TLS: pinned certificate only (finding 6). Pairing itself still has the protocol's +-1800 s timestamp
   window on the pair request, so a phone whose clock is far off (year 2020) will **not pair** and says "The clocks on the
   two devices disagree. Turn on automatic date and time" (`ClockSkew`, advice text tested in `failures_get_plain_words`).
   That is the KDE Connect replay guard and was kept deliberately; once paired, a wrong clock no longer matters.

## Not covered

Real Wi-Fi hardware (the AP isolation and broadcast filtering above are simulated by routing), NAT64/DNS64, IPv6 router
advertisements, Windows and macOS interface lists (`system_interfaces` is Unix-only; elsewhere the route probes decide and
the prefix falls back to /24), the Android core's own interface choice (it binds to the Wi-Fi IPv4 address, unchanged).

## Checks

`cargo fmt --check` 0, `cargo clippy --workspace --all-targets -- -D warnings` 0, `cargo test --workspace --no-fail-fast`:
all green except two that are not from this change: `bridge-apps` `calendar::render_tests::stepping_and_jumping_move_the_shown_day_in_every_mode`
(fails the same on the untouched base, depends on today's date) and `bridge-calls` `dbus_mock service_loss_and_restart_void_old_handles`
(failed once under the full-workspace load, passes alone 4 of 4 times, also on the base).
