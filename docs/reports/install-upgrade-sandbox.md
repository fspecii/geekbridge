# Install, upgrade, uninstall and update in a sandbox (2026-11-02)

Script: `scripts/e2e/install-sandbox.sh` (61 checks, about 55 s, exit 1 on any failure). Everything runs under
`~/.cache/gb-tmp/gb-install-sandbox` (removed afterwards, `--keep` to inspect): `HOME` and all `XDG_*` point
into it, `XDG_RUNTIME_DIR` is a nonexistent path, and `systemctl` is a stub on `PATH` that only logs its arguments, so
the real `~/.config`, `~/.local` and the real systemd user manager are never reached (the script also hashes the real
`~/.config/systemd` and `~/.config/autostart` before and after). Binaries are the existing `target/release` build;
packages are built fresh with `scripts/package-linux.sh`. Disk: about 250 MB in the sandbox, freed on exit.

## Verified

* **.deb**: expected binaries, systemd user unit (mode 0644, `systemd-analyze verify` clean), autostart entry, 8 desktop
  entries (`desktop-file-validate`), 32 icons, every `Icon=` and `Exec=` resolves, `md5sums` match, nothing outside
  `/usr` and `/etc`, no setuid, maintainer scripts parse and never reference a home directory.
* **Installer tarball**: no absolute or `..` paths; per-user install writes binaries, 8 desktop entries, 32 icons, the
  unit (ExecStart absolute into the sandbox prefix), the autostart entry and launcher with absolute paths; the unit
  verifies; `geekbridge-ctl status --waybar` on a stopped service prints valid one-line JSON with exit 0. The waybar
  module is configuration (docs/omarchy.md), not a shipped file.
* **Arch package**: checked statically only (PKGBUILD app list equals the shipped `.desktop` files, `.install` parses,
  enables nothing, names the right state directory). A full `makepkg` build (about 10 minutes) was not repeated; its
  contents were inspected earlier ([packaging-linux.md](../packaging-linux.md)).
* **Upgrade**: installing over an existing install leaves the v1 state directory (`crates/bridge-service/tests/fixtures/state-v1`)
  byte-identical, creates no duplicate entries, and the freshly installed service starts on that state and still lists
  the paired device.
* **Updater** (`GEEKBRIDGE_UPDATE_BASE_URL` against `scripts/e2e/mock_release_server.py`, real archive of the nine
  binaries): newer release offered with the published SHA-256; a corrupted download is refused with "does not match" and
  leaves no file; the installed binaries are untouched by a failed or successful download (the updater never replaces
  anything, by design); the verified download is byte-correct.
* **Apply and rollback**: applying is manual (docs/updates.md). The script runs the documented procedure (extract, copy
  over `~/.local/bin`) with a backup and a smoke test: a good archive installs and the service restarts on the
  pre-update state with its pairing; a broken `geekbridge-ctl` is detected and the previous binaries are restored.
  Rollback is therefore a property of this procedure, not a feature of the app.
* **Uninstall**: removes every installed file, disables the unit, is idempotent, leaves the state directory untouched.

## Bugs found and fixed

1. `install.sh` upgrade did not restart a running service (`enable --now` does nothing for an active unit), so the old
   binary kept running. Now `systemctl --user try-restart` follows.
2. `install.sh --uninstall` and the Arch `post_remove` message told users their pairing data was in
   `~/.local/share/geekbridge`; it lives in `$XDG_STATE_HOME/geekbridge` (`~/.local/state/geekbridge`). Both corrected
   and the script now prints the real path.
3. No way to remove user data on request: added `install.sh --uninstall --purge-data` (deletes only
   `$XDG_STATE_HOME/geekbridge`; refuses without `--uninstall`; default stays "keep").
4. The `.deb` `Depends` lacked `libxkbcommon-x11-0`, `libxcursor1`, `libxi6`, `libxrandr2`, which the binaries dlopen.
   Added in `scripts/package-linux.sh`.

Test-script defects found while writing it (not product bugs): the fixture copy made the state directory mode 0755, which
the service correctly refused (`state_insecure`); `update-desktop-database` and `gtk-update-icon-cache` leave cache files
in the install prefix, which uninstall does not remove (harmless, they are regenerated).

## Not covered

No real `dpkg -i` or `pacman -U` (no root, would touch the host); no real systemd user manager (stubbed, unit syntax is
verified with `systemd-analyze`); `--system` mode (needs root); the Windows and macOS installers.
