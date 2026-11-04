# Fresh install audit (Linux desktop, stranger reading only README.md and docs/)

Date 2026-11-04, tree at 3550eeca1 plus the changes listed below. Packages built with `scripts/package-linux.sh` from
`target/release` (0.1.0), installer tarball extracted and `./install.sh` run literally as the README says, in a sandbox:
private `HOME` and all `XDG_*` directories under `~/.cache/gb-fresh/walk`, no systemd user manager, no session bus, service on
private ports (18480/18481/18416, never 1716, 17380/17381 or 8000), private Xvfb for the windows. "No Bluetooth" was simulated
with bubblewrap (no system bus, empty `/sys/class/bluetooth`). No real config was touched.

## What a stranger sees, step by step

| Step | Result |
| --- | --- |
| Get the package | README says "latest GitHub release"; only the pre-release exists (known, owner item in fresh-user-review.md). |
| `./install.sh` (no systemd) | Works. Prints "Service will start at your next login (XDG autostart)", then the optional-programs list (clear, one line per missing feature). **Defects 1 and 2 below.** |
| Run `geekbridge-ctl status` / `doctor` before the service is up | `status`: "no control token yet; start geekbridge-service with the same --state-dir first" (jargon for a first-time user). `doctor`: Service FAIL, with `systemctl --user start ...` as the fix even where there is no systemd user session. **Defect 3.** |
| `geekbridge-ctl doctor`, service up, fresh machine | Service ok, Listener ok, Firewall ok or the ufw/firewalld message, Desktop session warn (only because the sandbox has no display), Phones warn "No phone is paired yet" with the QR fix, Clipboard warn with the exact package, Calls, Hear calls here, Call overlay. Exit 0 when nothing failed. Reads well except **defects 4 and 5**. |
| `geekbridge-ctl firewall` | Prints ufw / firewalld commands. **Defect 6.** |
| Home window, first run | "Get started": install the phone app (QR to the releases page), Link a phone, optional features, computer name, update status. Matches README step 2. With the service down it says "The background service is not running" and shows `systemctl --user enable --now geekbridge-service.service`, which is right only on systemd (the tarball installer handles no-systemd by autostart; see open item B). |
| `geekbridge-ctl pair-qr`, `test-connection`, `report --preview`, `hotkey install` (no Hyprland), `autostart`, `tray`, `update status` | All exist, all answer in plain words. `hotkey install` without Hyprland points at `hotkey print` for GNOME, KDE, sway. |
| Every `geekbridge-ctl <subcommand>` named in README, FAQ, user guide, Omarchy, pairing, sharing, clipboard, call, privacy, SUPPORT docs | All exist in `--help` (checked by script). `scripts/check-doc-links.sh`: 0 broken links. |

## No Bluetooth controller

Checked in the Calls window (screenshot, Xvfb), `geekbridge-ctl doctor`, `calling`, `calls`, `call dial`.

* Calls window banner: "This computer has no Bluetooth. You can see and answer calls here, but talking needs Bluetooth (add a USB
  adapter). Messages, contacts and the other features work normally." The same sentence is in `doctor`, `calling`, `calls`, and a
  one-line headline ("Call audio: NO BLUETOOTH. Calls can't carry voice on this computer"); the tray tooltip and Home tile say
  "No Bluetooth". It is a warning, never a failed check, exit code stays 0.
* The README, FAQ, user guide and `call-audio-options.md` say the same (answer, decline, hang up and dial go over the GeekBridge
  link; only the voice needs Bluetooth). **Decision needed:** the wording is "calls need Bluetooth for voice, Messages work", not
  "only Messages work". If the product decision means the call UI should be disabled without Bluetooth (not just labelled), that is
  a code change (Calls window and tray would hide Answer/Dial), not a wording one. The text as it stands is accurate to the code.
* `geekbridge-ctl call dial` without a call backend says "dialing is not available right now; nothing was dialed" and names what
  is needed; correct.

## Defects found

Fixed in this change:

| # | Where | Problem | Fix |
| --- | --- | --- | --- |
| 1 | `packaging/linux/install.sh` | A per-user install put `Exec=geekbridge`, `Exec=geekbridge-calls` ... in the application-menu entries. `~/.local/bin` is only on PATH after the next login (Debian/Ubuntu add it from `~/.profile` only if it exists), so a menu click right after install could do nothing. | The menu entries now name the programs by absolute path (as the autostart entry already did). New check `install-menu-entries-absolute` in `scripts/e2e/install-sandbox.sh` (71 checks, all pass). |
| 2 | `install.sh` final lines | "Note: ... is not on your PATH." and then "run: geekbridge" (which fails in that case). No hint about the next step or the firewall. | Prints the full path to run when the directory is not on PATH and what to do about it; ends with "click Link a phone, allow TCP and UDP 1716-1764 (`geekbridge-ctl firewall`)". |
| 3 | `doctor`, service not running | Fix was always `systemctl --user start ...` when a unit file exists, even where the user manager is missing (no systemd session, the tarball's no-systemd case). | Adds "No systemd user session? Run: geekbridge-service-launch". |
| 4 | `doctor`, Calls row | Printed the probe's internal code, `[warn] Calls: no_telephony_service`. | Translated to a sentence for all four codes (`no_telephony_service`, `no_phone`, `phone_registered`, `platform_unavailable`); unit test makes sure no code is printed raw. |
| 5 | `docs/user-guide.md` | Said no Bluetooth "is reported as information, not an error", but `doctor` prints it as a warning (and counts it). Also said Wi-Fi call audio is impossible with no mention of the experimental debug-build option. | Both reworded to match the program. |
| 6 | `geekbridge-ctl firewall` (and the doctor fix it is pointed to) | The ufw lines allow `192.168.0.0/16` only. On a 10.x.x.x or 172.16-31.x.x network (very common) the phone is still blocked, and `doctor` then reports "ufw allows 1716-1764" because it accepts any source, so the stranger gets a green check and a dead pairing. | The printed help now says which range to use for 10.x / 172.16-31.x (`ip -4 addr`) and that a rule for the wrong range keeps the phone out (test added). The doctor check itself is unchanged, see open item A. |

## Open items A-F: fixed afterwards

A. **ufw source range.** `doctor` now reads this computer's LAN IPv4 address and ignores ufw rules whose `-s` range does not contain
   it; a 192.168.0.0/16 rule on a 10.x network is a Fail that names the address and points at `geekbridge-ctl firewall`
   (`ufw_allows_from`, tests in `bridge-core` and `doctor`). Unknown address: every source counts, as before.
B. **No-systemd Home card.** With the service down, Home also shows "No systemd user session here? ..." and `geekbridge-service-launch`
   (Linux). The golden image of that state needs a refresh the next time goldens are regenerated.
C. **Tarball README.** `scripts/package-linux.sh` rewrites the README's relative doc and image links to absolute GitHub URLs for the
   tarball and the .deb; `install-sandbox.sh` checks none are left (`tarball-readme-links-absolute`).
D. **`status` with no service** now says "no control token yet: the background service is not running; run `geekbridge-ctl doctor`".
E/F. **Hotkey and firmware.** `clipboard status` explains the binding state in words ("works in the GeekBridge window only; for
   anywhere, bind ... to a key"); the no-adapter firmware hint lists Arch, Debian/Ubuntu and Fedora commands.

G. Known earlier and unchanged: no stable GitHub release yet (`releases/latest` is a 404), the security contact placeholder, no
   desktop licence screen (fresh-user-review.md).

## Incident during the audit (my mistake, restored)

`geekbridge-ctl call setup` was run once in the sandbox to look at the no-controller path. The sandbox override
`GEEKBRIDGE_BT_SYSFS` only hides the sysfs controller list, not BlueZ on the system bus, so the command used the machine's real
adapter: it made it discoverable and started pairing with a nearby device ("DESKTOP-MHB5JOV"). The pairing was refused and
cancelled (`--confirm no`), the sandbox service was stopped, discovery stopped and the discoverable timeout expired. The adapter
now shows Discoverable no, Discovering no. Killing the service mid-setup had left `Pairable: no`; it was set back to `yes` (BlueZ's
default; the original value was not recorded). The unpaired device cache entry was not present afterwards. After that, the
no-Bluetooth runs used bubblewrap (no system bus, empty `/sys/class/bluetooth`), which cannot reach BlueZ. Lesson for the
audit scripts: never run `call setup` outside bubblewrap or a fake BlueZ.
