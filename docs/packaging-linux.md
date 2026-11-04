# Linux packaging

Three outputs exist. All were built and inspected on 2026-11-02 (Arch Linux, x86_64, makepkg, rsvg-convert,
bsdtar, desktop-file-validate and systemd-analyze present; `namcap` is **not** installed, so no namcap lint was run).
Nothing was installed with pacman/dpkg: every check below runs on an extracted copy in a scratch directory.

| Output | How | Result |
| --- | --- | --- |
| Arch package `geekbridge-0.1.0-1-x86_64.pkg.tar.zst` | `packaging/arch/build-local.sh` (PKGBUILD + `geekbridge.install`) | built from the working tree, 28.8 MB, 54 files |
| `geekbridge_0.1.0_amd64.deb` | `scripts/package-linux.sh` | 14.5 MB, 54 files |
| `geekbridge-0.1.0-linux-x86_64-installer.tar.gz` | `scripts/package-linux.sh` | 29.3 MB, 26 files, per-user `install.sh` |

## Arch package

`packaging/arch/build-local.sh [--out DIR] [--offline]` packs the tracked + untracked-but-not-ignored files into
`geekbridge-<ver>.tar.gz` (directory `geekbridge-<ver>/`, like a GitHub tag archive), pins its SHA-256 into a
copy of the PKGBUILD, and runs `makepkg --cleanbuild --nodeps` in a scratch directory (`$GB_TMP`/`$TMPDIR`).
Cargo's `CARGO_HOME` is also in scratch (seeded from `~/.cargo` so no network is needed); nothing is written
outside scratch and `--out`. It refuses to run if `pkgver` differs from `[workspace.package] version`.
It runs 3 cargo jobs by default (`GB_JOBS=n` to change): with unlimited jobs the nine release links exhausted
31 GB of RAM and swap on this machine and the first attempt had to be killed.

Build: `cargo build --frozen --release -p bridge-service -p bridge-apps`, 9 m 58 s with 3 jobs, `Finished release`.
makepkg also emitted `geekbridge-debug-0.1.0-1-x86_64.pkg.tar.zst` (18 KB, debug symbols split off); ignore it.
`options=('!lto')`. Package SHA-256 of the checked build:
`1739772ba7ede8e98d18cb1d779bc7362b8aabf59606b1bb9d446526b4f81a59` (not reproducible bit for bit: it embeds the build time).

### Contents (all under `/usr`, none elsewhere, no setuid/setgid)

* `/usr/bin`: `geekbridge`, `-service`, `-ctl`, `-calls`, `-messages`, `-clipboard`, `-agenda`, `-contacts`, `-share`.
* `/usr/lib/systemd/user/geekbridge-service.service` (`ExecStart=/usr/bin/geekbridge-service`; shipped, **not enabled**).
* `/usr/share/applications/dev.geekbridge.{GeekBridge,Calls,Messages,Agenda,Contacts,Clipboard,Share,PasteFromPhone}.desktop`.
* Icons for the same 8 names in `hicolor/{scalable (svg),48x48,128x128,256x256}/apps`: 32 files.
* `/usr/share/doc/geekbridge/{README.md,THIRD_PARTY_NOTICES.md,CHANGELOG.md}` and
  `/usr/share/licenses/geekbridge/{LICENSE,THIRD_PARTY_NOTICES.md}`.
* `.INSTALL`: refreshes icon/desktop caches and prints first-run hints. It enables nothing, touches no home directory.

Differences from the `.deb`: no `/etc/xdg/autostart` entry and no `geekbridge-service-launch` (autostart is the
user's choice: `geekbridge-ctl autostart enable` or `systemctl --user enable --now geekbridge-service.service`);
adds CHANGELOG and THIRD_PARTY_NOTICES (the `.deb` carries `copyright` in DEP-5 format and the full GPL text).

**Licence:** GPL-3.0-or-later ([licence-decision.md](licence-decision.md)); the PKGBUILD says
`license=('GPL-3.0-or-later')` and installs `LICENSE`.

### Dependencies

`depends`: glibc, gcc-libs, wayland, libxkbcommon, libxkbcommon-x11, libglvnd, libx11, libxcb, libxcursor, libxi, libxrandr.
`ldd` on all nine binaries lists only `libgcc_s`, `libm`, `libc` and the loader (0 "not found"), because the GUI
loads windowing/GL libraries with `dlopen`. The dlopen names were read from the binary (`libwayland-client.so.0`,
`libwayland-egl.so.1`, `libxkbcommon.so.0`, `libxkbcommon-x11.so.0`, `libEGL.so.1`, `libGL.so.1`, `libX11.so.6`,
`libxcb.so.1`, `libXcursor.so.1`, `libXi.so.6`, `libXrandr.so.2`) and each exists on this machine and belongs to
one of the packages above. That check found `libxkbcommon-x11` missing from the first draft of `depends`; it was
added and the package rebuilt, and the numbers in this document are from the rebuilt package.
`makedepends`: cargo, pkgconf, librsvg.

Optional programs (the apps start without them; each loses one feature). Arch `optdepends` carries a one-line reason for
each, the `.deb` lists them as `Recommends` (installed by default by apt) and the installer tarball names the missing ones
when it finishes:

| Feature | Arch `optdepends` | Debian `Recommends` | Program the code runs |
| --- | --- | --- | --- |
| Clipboard sync | wl-clipboard (Wayland), xclip (X11) | `wl-clipboard \| xclip` | `wl-copy`, `wl-paste`, `xclip` |
| Desktop notifications | libnotify, mako | `libnotify-bin` (`Suggests: mako-notifier \| dunst \| notification-daemon`) | `notify-send` |
| Call audio | bluez, pipewire, wireplumber, pipewire-pulse | `bluez`, `pipewire`, `wireplumber`, `pipewire-pulse` | `bluetoothctl`, `wpctl`, `pactl` |
| Open links, files, the licence text | xdg-utils | `xdg-utils` | `xdg-open` |
| File picker in Share | zenity | `zenity \| kdialog` | `zenity`, `kdialog` |

### About and licence

The Settings window has an "About GeekBridge" section (version, commit, update channel, licence, independence statement,
project address, buttons for the licence text and the third-party notices) and `geekbridge-ctl about` / `--version` print
the same lines. The documents are looked for next to the binary, then in `<prefix>/share/doc/geekbridge` (the installer
puts `LICENSE` and `THIRD_PARTY_NOTICES.md` there; uninstall removes them), then in `/usr/share/doc/geekbridge` (`.deb`) and
`/usr/share/licenses/geekbridge` (Arch), and finally the GitHub pages are opened instead.

### Scratch-root install test (extracted with `bsdtar -xf … -C <scratch>`)

| Check | Result |
| --- | --- |
| file list: `bsdtar -t` vs extracted tree | identical, 54 files; 0 outside `/usr`; removal by pacman removes exactly these (nothing is created at install time except the icon/desktop caches, which `post_remove` refreshes) |
| `ldd` on 9 binaries | 0 "not found" (see above) |
| `geekbridge-ctl --version` | `geekbridge-ctl 0.1.0` |
| `geekbridge-service --help` | prints usage, exit 0 |
| `systemd-analyze verify` of the unit | passes (run on a copy whose `ExecStart` points at the extracted binary; `--root=` is rejected by this systemd with "Invalid argument", so the absolute-path copy was used) |
| `desktop-file-validate` | all 8 `.desktop` files clean |
| icons | 32 files (8 names × svg/48/128/256); PNG headers confirmed 48×48 and 256×256 RGBA; every `Icon=` in the desktop files has a matching file |
| `Exec=` lines | all resolve to a binary in `/usr/bin` (`geekbridge`, `geekbridge-ctl`, `geekbridge-share` take `%F` etc.) |

Not done: an actual `pacman -U` / install / uninstall round trip, running the GUI, enabling the unit, `namcap`.

## `scripts/package-linux.sh`

Run against the existing `target/release` binaries into a scratch `--out`: produced the `.deb` and the installer
tarball. `.deb`: control file well formed (`Depends: libc6, libgcc-s1, libxkbcommon0, libwayland-client0, libgl1, libx11-6, libxcb1`, plus `Recommends` and `Suggests` as in the table above; `scripts/e2e/install-sandbox.sh` parses the fields, and runs `dpkg-deb -I` where dpkg is installed),
54 files, unit mode 0644, includes the autostart entry and `geekbridge-service-launch`. Tarball: 26 files
(`bin/`, `share/`, `install.sh`, README). The Debian dependency list now includes `libxkbcommon-x11-0`, `libxcursor1`, `libxi6` and `libxrandr2`, which the
binaries also dlopen (fixed 2026-11-02, see [reports/install-upgrade-sandbox.md](reports/install-upgrade-sandbox.md)).
Note that these artifacts were packaged from the binaries already in `target/release`, not a fresh build.

## Install, upgrade and uninstall

`install.sh` (per user, `~/.local`): upgrading over an existing install restarts a running service
(`systemctl --user try-restart`). `install.sh --uninstall` removes exactly what it installed and keeps the state
directory (`$XDG_DATA_HOME/geekbridge`, default `~/.local/share/geekbridge`: pairings, identity, settings, under `service/`);
`install.sh --uninstall --purge-data` also deletes that directory and the updater's notes in `$XDG_STATE_HOME/geekbridge`. The whole flow runs in a sandbox with
`scripts/e2e/install-sandbox.sh` (fake HOME/XDG, stub `systemctl`).
