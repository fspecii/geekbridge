# Real-device rehearsal (emulator, 2026-11-02)

Goal: run the owner's real-phone session on the emulator exactly as [real-device-quickstart.md](../real-device-quickstart.md)
describes it, and fix every unclear step, wrong label, missing command and dead end before the real phone is plugged in.

Setup: fresh `dist/android/geekbridge-debug.apk`, app data cleared (`pm clear`, first-run wizard), clean private state dir
(`desktop --fresh`), an emulator on its own adb server (`--adb-port 15999 --serial emulator-5584`), AVD `gb36z`
(Android 16). Scripts used: `realdevice.sh` `check`, `install`, `desktop`, `screens`, `logs`, `battery-baseline`,
`battery-report`, and `run-desktop.sh`. A full pairing (QR link -> **Connect to a computer?** -> **Pair**) completed;
`geekbridge-ctl status` showed the phone paired and `connected`. The firewall was not changed (commands are printed only).

## This machine

| Fact | Evidence |
| --- | --- |
| Active firewall: **ufw**, default incoming DROP | `systemctl is-active ufw` = active; `nftables`/`firewalld`/`iptables` inactive; `/etc/default/ufw` `DEFAULT_INPUT_POLICY="DROP"` |
| No rule for 1716-1764 | `/etc/ufw/user.rules` (world-readable): SSH, 53317, a few dev ports from `192.168.1.0/24` |
| Interfaces (`ip -br a`) | `lo`, `enp5s0 192.168.1.150/24` (default route via `192.168.1.1`), `docker0 172.17.0.1/16` (down). No Wi-Fi interface, no tunnels at the time |
| Service listens | `tcp 0.0.0.0:1716` and `udp 0.0.0.0:1716` (`ss -lntup`) |

The ufw allow/undo commands are in the quickstart ("Firewall on this machine"); they were checked for syntax only
(`ufw --dry-run` needs root, which the rehearsal does not use).

## Findings and what changed

| # | Friction found while following the docs literally | Change |
| --- | --- | --- |
| 1 | Release binaries built after `source scripts/android-env.sh` land in `target/android` (it sets `CARGO_TARGET_DIR`), so `target/release` kept an old service; nothing warned | `check` now explains this when a binary is missing and warns when `crates/*/src` is newer than `target/release/geekbridge-service`; quickstart row 1 and T9 say "not in a shell that sourced android-env.sh" |
| 2 | QR address choice: `lan_addresses()` only probed four routes. No interface list, so it could not be tested, and a full-tunnel VPN or a second NIC gave a wrong or missing first choice | New `crates/bridge-service/src/lan_select.rs` (pure `rank()` + `getifaddrs` on Unix). Rules: default-route private address first, other private NICs next, never loopback / link-local / CGNAT 100.64/10 / down interfaces / container bridges / VPN tunnels, Docker's 172.17/16 only as last resort, IPv6 only when no IPv4, de-duplicated, capped. `pairing_flow.rs` uses it |
| 3 | `GEEKBRIDGE_QR_ADDRESSES` only worked with the developer flag `--dev-loopback`, so the owner had no override on a machine where auto-detection is wrong | The override now works in production; bare IPs use the listener port, `ip:port` and `[v6]:port` also accepted. `run-desktop.sh` warns when the variable is set but the service was already running without it (it is read at start) |
| 4 | `run-desktop.sh` printed `LAN address(es): 192.168.1.150 172.17.0.1` (docker0 included), contradicting what the QR carried | Same filter as the service (no bridges, tunnels, CGNAT, link-local), default-route interface first, interface names shown; after the QR it prints "the QR makes the phone try (in this order): ..." |
| 5 | `realdevice.sh desktop` told the owner to open "Add computer > Scan QR code"; there is no such label | Replaced by the real path: wizard step 1 **Pair with your computer** > **Scan QR code** > **Open the camera app** (later: Home > **Pair a computer**), paste fallback, **Connect to a computer?** > **Continue**, **Pair**. Same text in `run-desktop.sh` |
| 6 | `check` could not tell a firewall/isolation drop from a missing service; the TCP path was never tested | New section "Phone to this PC": `ping` from the phone and `nc` from the phone to the service port. Timeout = packets dropped (T1/T2/T3), refused = wrong address/no listener (T4). On an emulator it says why the result does not prove the firewall |
| 7 | Phone-side blockers were invisible | `check` now reports VPN on the phone, always-on VPN, Wi-Fi "connected, no internet" (not validated), Battery Saver, Private DNS, and (real phone) whether the phone and PC share a gateway |
| 8 | PC-side blockers invisible; `PC addresses` list included docker | `check` lists only plausible LAN addresses, warns about VPN-style interfaces up and about several LAN addresses, fails when none exists, and prints the addresses the QR will carry (from the running service, no token shown) |
| 9 | `adb devices` problems were not explained (`no permissions`, charge-only cable, USB mode) | `no permissions` handled; the no-device message lists the causes and the wireless-debugging route; quickstart T0 |
| 10 | Quickstart order: nothing said that the TCP test needs the service up, so `check` before `desktop` looked like it tested the firewall | New step 0.5 ("run `check` again"); the top checklist runs `check` after `desktop` |
| 11 | Missing real-phone guidance after `check` | `check` ends with "What this means for the real phone": for an emulator the three lines that only count on the phone (subnet, TCP test, UDP broadcast); for a phone the order of next steps |
| 12 | `qrencode` missing was silent | `run-desktop.sh` warns with the install command |
| 13 | No troubleshooting for the likely real-world failures | Quickstart T0-T9: adb/USB, firewall, subnets/guest Wi-Fi/isolation, VPN either side, interface selection and the override, discovery blocked (QR-only), IPv6 and link-local, hostnames and Private DNS, Wi-Fi power saving / Doze / OEM managers, build problems. Plus the top "Bring-up checklist in 10 minutes" table (step, command, expected output, if it fails go to section) |

## QR address selection test

`crates/bridge-service/src/lan_select.rs` unit tests use a fake machine: `lo` (127.0.0.1, ::1), `docker0` (172.17.0.1,
fe80::), `virbr0` 192.168.122.1, `br-*` 172.18.0.1, `veth*` 169.254.7.7, `tailscale0` 100.101.102.103 + `fd7a:...`,
`wg0` 10.66.66.2, `enp5s0` 192.168.1.150 + fe80::, `wlan0` 192.168.1.77. Cases: default-route address first and junk
dropped; never loopback/link-local/CGNAT/containers/tunnels (and no IPv6 next to IPv4); second NIC order flips with the
default route; down interfaces skipped; a full-tunnel VPN as the default route does not win; Docker's range only as last
resort on an unknown name; IPv6 only without IPv4; empty result; de-duplication and limit; CGNAT boundaries; the real
`getifaddrs` returns loopback. On this machine the live service now produces exactly `192.168.1.150:1716`; with
`GEEKBRIDGE_QR_ADDRESSES=192.168.1.150,192.168.1.77:1720` it produces `192.168.1.150:1716  then  192.168.1.77:1720`.

## Verification

- `cargo fmt -p bridge-service --check`, `cargo clippy -p bridge-service --all-targets -- -D warnings`: clean.
- `cargo test -p bridge-service --lib` (182 passed, including 11 in `lan_select`), `--test qr_pairing` (9 passed).
- `scripts/check-doc-links.sh`: 0 broken links. `bash -n` on both scripts. No Kotlin changed, so no Gradle run.
- The Windows/macOS side compiles `lan_select` to an empty interface list (route probes only) behind `cfg(unix)`; `scripts/xcheck.sh`
  was not re-run here.

## Not verifiable on an emulator (check on the real phone)

- Firewall: the emulator's connections originate from this PC, so ufw never sees a foreign source; the real test is
  `check` section "Phone to this PC" after the ufw rules are in.
- Subnet/gateway comparison, client isolation, guest Wi-Fi, VPN apps, Wi-Fi validation: the emulator is NAT-ed with
  no VPN. The new `check` lines exist for the phone.
- UDP broadcast discovery and reconnect after a PC restart (quickstart step 11).
- IPv6 in the QR (only emitted when there is no IPv4; least tested path).
- Battery drain: the emulator is "on AC"; `battery-baseline` warns and prints the wireless-debugging recipe.

Left on disk: nothing from the rehearsal (the emulator, adb servers 15998/15999 and the temporary `dist/realdevice` bundles were removed).
