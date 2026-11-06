# First-run walkthrough 2 (new Linux user, packaged tarball, private sandbox)

Date 2026-11-06, source head ee17c59c9 plus the fixes below. Everything ran under `/home/ambsd/.cache/gb-firstrun-2`:
`HOME`, all four `XDG_*` directories, `TMPDIR` and `XDG_RUNTIME_DIR` (a path that does not exist, so
`systemctl --user` cannot reach the real manager; confirmed it fails with "Failed to connect to user scope bus"). The
service ran with `--api-port 34570 --discovery-port 34572 --listen-inbound 127.0.0.1:34573 --no-announce --no-calling-probe --dev-loopback`
and `GEEKBRIDGE_CONTROL_PORT=34571` for every `geekbridge-ctl` call. Ports 1716, 17380, 17381 and 8000 were never used. No GUI was
started (no display in the sandbox, so the windows, tray and clipboard bridge were not exercised).

## What was walked

1. Release build, then `scripts/package-linux.sh --out <scratch>`; extracted the installer tarball.
2. `./install.sh` (per-user), with `~/.local/bin` both on and off `PATH`.
3. First start of `geekbridge-service` with the default state directory (`$XDG_DATA_HOME/geekbridge/service`).
4. `geekbridge-ctl status`, `doctor`, `test-connection`, `about`, `firewall`, `token-path`, `update ...`, `privacy ...`, `rename`,
   `consent`, `tray`, `autostart`, `calls`, `diagnose`, a bad command, a bad feature, a bad device id.
5. Pairing with the stand-in phone (`target/release/examples/upgrade-phone`, needs the service started with `--dev-loopback`),
   consents, three text messages, unpair.
6. `./install.sh --uninstall`, then `--uninstall --purge-data`.
7. `scripts/e2e/install-sandbox.sh` after the changes: 74 passed, 0 failed.

## Result against the checklist

| Check | Result |
| --- | --- |
| Install messages clear | Yes. The "not on your PATH" note appears only when it should and says what still works. |
| Files and directories owner-only | State directory 0700, every file in it 0600 except `tray-disabled` (0644; fixed). Installed binaries 0755, desktop entries, unit and licence 0644 (public by design). |
| Nothing written outside the sandbox | Checked the real `~/.config`, `~/.local/{bin,share,state}`, `~/Downloads`, `/tmp` and `~/.cache/geekbridge` for anything named geekbridge and newer than the sandbox: nothing. No `~/.local/state/geekbridge` and no `~/Downloads/GeekBridge` appeared (nothing was received). One exception, see finding 1: Bluetooth. |
| Leftovers after `--uninstall` | Pairing data and settings in `~/.local/share/geekbridge` (stated in the message; matches docs/privacy.md). |
| Leftovers after `--uninstall --purge-data` | Before: empty directories the installer created (`~/.local/bin`, `.config/autostart`, `.config/systemd/user`, icon size directories, `doc`), `applications/mimeinfo.cache` and `icons/hicolor/icon-theme.cache` (written by `update-desktop-database` and `gtk-update-icon-cache`). After the fix the two cache files are gone; empty directories remain (harmless, and shared with other programs on a normal home). |
| Messages free of jargon | Mostly; five fixed below. |

## Findings and fixes

1. **`geekbridge-ctl call setup` could leave the computer's Bluetooth settings changed.** (`crates/bridge-calls/src/setup.rs`)
   It sets Pairable, then DiscoverableTimeout, then Discoverable. When the last one was refused (it was, in this sandbox:
   `org.bluez.Error.Failed`), the early `?` returned before the restore code, so the adapter kept the new timeout. It also
   printed "This computer is visible as a Bluetooth device for 120 s" before it had succeeded. **This reached the real
   adapter** (BlueZ is on the system bus, which the sandbox cannot hide): `DiscoverableTimeout` was left at 120 s. I set it back
   to 180 s (BlueZ's default; the value before is unknown) and `Discoverable` is `no`; `Pairable` reads `no` and may have been
   `no` before, I cannot tell. Fix: the three changes are one step; on failure the earlier ones are put back and the error says
   "could not make this computer visible over Bluetooth (...); check that Bluetooth is on and not blocked (rfkill list)".
   The "visible for N s" line is printed only after success.
2. **Unpair left the unpaired phone in `consent.json.bak`** (device id and its consent list; the privacy page says unpair
   forgets the phone, and a fall-back to the backup would have switched its features back on). The trust store already refreshed
   its backup on removal; the consent store did not. Fix: `ConsentStore::forget` uses `save_and_refresh_backup`; test added to
   `consent_defaults_off_persists_and_forgets`. (`trusted_peers.json.bak` was already clean.)
3. **First command before any service existed** printed `no control token yet: the background service is not running` followed
   by "The ctl and the service use different state directories", which sends a new user hunting for a mismatch that does not
   exist. Now the hint is "Start the service (systemctl --user start geekbridge-service, or log out and in again), then try
   again. If you started it yourself with another --state-dir, pass the same one here." The old text stays for the real
   "rejected the control token" case. (`doctor.rs`, test updated)
4. **Vocabulary.** `doctor` said "No phone is paired yet" and sent the user to "Home > Pair a computer"; the phone says
   "Link a computer". Now "No phone is linked yet ... (Home > Link a computer)". The Share window's "No phone is paired yet" is
   "No phone is linked yet" too.
5. **`geekbridge-ctl consent ID bogus on`** said only `error: unknown feature`; it now lists the twelve valid names. A bad device
   id said `refused the request (BadRequest): invalid device id`; it now adds "(copy it from `geekbridge-ctl status`)".
6. **`tray-disabled` marker was 0644**; written through the atomic owner-only writer now (0600).
7. **Uninstaller.** Refreshes the icon and desktop caches, removes the two cache files when nothing else is in them, says
   "No pairing data found" instead of "were left in place" when there were none, and, when `~/Downloads/GeekBridge` exists,
   says the received files stay there (the uninstaller never deletes them).

## Seen, not changed

* `geekbridge-ctl pair-qr` prints a `geekbridge://pair?...` link, not a QR. The help says the desktop apps draw the QR; a terminal user
  who only has the ctl has to paste the link on the phone ("Paste from clipboard").
* The tray tooltip and `tray-headless.sh` still say "No phone paired yet" (a script asserts the string).
* `geekbridge-service-launch` was not run: its fallback starts `geekbridge-service` on the default ports, which are the live
  service's. Its `exec` line rewrite by the installer was checked by reading the installed file only.
* No systemd user manager in the sandbox, so the "Service enabled and started (systemd user unit)" branch was not run here;
  `install-sandbox.sh` covers it with a stub `systemctl`.
* Windows/macOS paths, the `.deb` (no `dpkg-deb` here; contents were checked by `install-sandbox.sh`), the GUI windows and a real
  phone were out of scope.
* Mixed `cargo test` run: `chaos_clipboard::a_phone_update_flood_...` failed once at machine load 60+ ("flood 111" vs "flood 199") and
  passed twice on rerun, with and without these changes.
