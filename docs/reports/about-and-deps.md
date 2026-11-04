# About GeekBridge on the desktop, and real runtime dependencies in the packages

Date: 2026-11-04. Closes items 3 and 4 of [fresh-user-review.md](fresh-user-review.md).

## 1. About section (item 3)

* `crates/bridge-core/src/about.rs`: the facts (version, licence id `GPL-3.0-or-later`, project URL) and the lookup of
  `LICENSE` and `THIRD_PARTY_NOTICES.md` (next to the binary, `<prefix>/share/doc/geekbridge`, `<prefix>/share/licenses/geekbridge`,
  `/usr/share/doc/geekbridge`, `/usr/share/licenses/geekbridge`, `/usr/local/share/doc/geekbridge`; otherwise the GitHub page).
* Desktop Settings has an "About GeekBridge" section (`crates/bridge-apps/src/about_ui.rs`): version and commit, update channel
  (from the service), licence, "Independent implementation compatible with the KDE Connect protocol; not affiliated with KDE.",
  project URL, and three buttons (Licence text, Third-party notices, Project page) with spoken names ("Open the third-party
  notices" ...). Opening uses `xdg-open` (`open` / `explorer` elsewhere) on fixed targets only.
* `geekbridge-ctl about` prints the same lines (the channel appears when the service answers). `geekbridge-ctl --version`
  prints `geekbridge-ctl <version>` first, then the same lines. The first line is kept exact because `release-local.sh` and
  `updater-real.sh` compared the whole output; both now read the first line only (`docs/release-local.md` updated).
* Strings: `about.*` in `crates/bridge-i18n/i18n/en.properties`; `i18n_catalogue.rs` also scans `about.rs` for key use.
* The installer tarball now installs `LICENSE` and `THIRD_PARTY_NOTICES.md` into `<prefix>/share/doc/geekbridge` (removed by `--uninstall`).
* Tests: `bridge-core` about unit tests (lookup order, URL fallback, lines), `about_ui` text test, `settings_about_section_is_named`
  (AccessKit: three named buttons, no unnamed controls), `crates/bridge-service/tests/about_ctl.rs`.
* Goldens: Settings at the standard size shows the top only, so only the eight `settings-call-audio-{mac,noadapter,testing,visible}-{light,dark}-std`
  screens moved (the page is longer, so the scrolled view shifts); each diff was viewed, the change is the scroll offset only, the
  `-min` screens and the plain `settings` screens are unchanged. No About golden was added: it would show the build commit, which
  changes with every commit.

## 2. Packaging dependencies (item 4)

| Where | Now |
| --- | --- |
| `.deb` control | `Recommends: wl-clipboard \| xclip, libnotify-bin, xdg-utils, zenity \| kdialog, bluez, pipewire, wireplumber, pipewire-pulse`; `Suggests: mako-notifier \| dunst \| notification-daemon` |
| Arch `PKGBUILD` | `optdepends` for wl-clipboard, xclip, libnotify, mako, bluez, pipewire, wireplumber, pipewire-pulse, xdg-utils, zenity, each with the feature it enables |
| `packaging/linux/install.sh` | after installing, lists any of notify-send, wl-copy/xclip, xdg-open, bluetoothctl, pipewire+wpctl that are missing and what each is for; never fatal |
| `docs/packaging-linux.md` | feature to package table and an About section |
| `scripts/e2e/install-sandbox.sh` | control fields present and well formed, Recommends complete, Suggests, `dpkg-deb -I` (SKIP where dpkg is absent, as on this Arch machine), `.deb` ships LICENSE and notices, PKGBUILD optdepends explained, installed docs present, `ctl about` finds them, optional-programs note |

## Checks

* `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace --no-fail-fast`: clean.
* `scripts/e2e/install-sandbox.sh` (release ctl rebuilt, other release binaries older): exit 0, 70 PASS, 0 FAIL, 1 SKIP (`dpkg-deb`).
* Not done: a real `dpkg -i` / apt round trip, a fresh `makepkg`, an About screenshot (below the fold in the golden window).
* `scripts/golden-desktop.sh` still creates `/tmp/gb-golden-home` (a fixed HOME the Share window prints); not changed here.
