# Fresh-user walk-through review

Date: 2026-11-04, against origin/main 0a480bc09. Read as a new user: README, user guide, real-device quickstart, desktop first-run
card (`crates/bridge-i18n/i18n/en.properties`, `onboarding.*`), Android setup strings (`android/app/src/main/res/values/*.xml`),
licence files. Commands were checked against `geekbridge-ctl --help`, `hotkey --help`, `scripts/realdevice.sh`, `scripts/run-desktop.sh`
and `packaging/linux/install.sh`; no live service, real config or fixed port was used (sandbox state dir under `~/.cache/gb-fresh`).

## Fixed in this change

| # | Where | Problem | Fix |
| --- | --- | --- | --- |
| 1 | `geekbridge-ctl --help` | `call dial`, `call setup`, `call audio-devices` worked and were in the docs, but `--help` did not list them | added to the help text |
| 2 | README, desktop screenshots | Seven desktop images still showed the old "Pair a phone" sidebar and "Pair" button; the app now says "Link a phone" | replaced from the current goldens (`tests/golden/desktop/*-std.png`, lower resolution: 480 px wide) |
| 3 | README Android row | "not yet on a physical phone" contradicted the status box (one real phone paired) | reworded |
| 4 | README quick start step 2 | said "confirm Connect and Pair"; the phone says **Continue**, then **Pair** | fixed; added that "Link a phone" (computer) and "Pair a computer" (phone) are one job |
| 5 | README, user guide | no list of Linux programs (libnotify, bluez, pipewire, wireplumber, clipboard tool) | added to Requirements and the guide |
| 6 | README step 3 | Ctrl+Shift+V vs the Hyprland hotkey (Super+Shift+V) was contradictory | explained why they differ |
| 7 | README step 5 | did not say that voice on the computer needs Bluetooth, or that Wi-Fi call audio does not exist | said so |
| 8 | README | "Allow restricted settings" for sideloaded Android 13+ was only in the guide | added to step 6 |
| 9 | README | no mention of the dev/stable update channels, debug vs release APK pairing loss | added |
| 10 | README | two "Documentation" sections; the KDE-affiliation sentence | merged into one; added FAQ link |
| 11 | README, guide | no pointer to `geekbridge-ctl report` / SUPPORT.md for a user who is stuck; no uninstall for phone/Windows/macOS | added |
| 12 | user guide | said `calendar_edit`/`calendar_alerts` are not in `--help` (they are now); CLI group list missed `call setup`/`audio-devices` | fixed |
| 13 | user guide | "Diagnostics & about > Unpair" (the phone says **Unlink**) | fixed here and in the quickstart |
| 14 | quickstart | "no rename screen yet" (there is, and `geekbridge-ctl rename`); written for the author's machine but linked as the user path | tester banner at top; docs/README and README describe it as a tester page |
| 15 | release notes v0.1.0 | "no camera permission needed on Android" (the in-app scanner asks for it) | fixed |
| 16 | new | no FAQ | `docs/FAQ.md`, 15 questions |

## Still open (owner or outside docs)

1. **No published stable release.** `gh release list` shows only `v0.1.0-rc.1` (pre-release). `releases/latest`, which the README, the
   Android "Send me the download link" text and the guide all point to, returns 404 until v0.1.0 is published. The desktop first-run
   card links `/releases` (works). Publish v0.1.0 before announcing.
2. **Security contact placeholder.** `SECURITY.md` still has `<!-- OWNER: ... -->` and `docs/release-notes-v0.1.0.md` still shows
   "[OWNER: add the monitored security contact here before publishing]" (visible text). GitHub private vulnerability reporting is
   enabled (checked), so the primary route works. Fill the contact or delete the line before the release.
3. **Desktop has no licence/About screen.** Android has "About and licence" (GPL-3.0-or-later, source link, notices pointer without a
   link). The desktop apps show no licence text; `.deb` ships `LICENSE` and `THIRD_PARTY_NOTICES.md` under `/usr/share/doc/geekbridge/`.
   `LICENSE` (GPLv3 text, 674 lines) and `THIRD_PARTY_NOTICES.md` (478 crates, fonts) are present and consistent with the README licence
   section; `privacy.md` matches the manifest (CAMERA only on the scan screen, BLUETOOTH_CONNECT only on request).
4. **The `.deb` does not list `libnotify-bin`, `bluez`, `pipewire`, `wireplumber` as Recommends** (only `wl-clipboard | xclip`,
   `xdg-utils`, `zenity | kdialog`). Documented in the README now; adding Recommends (`scripts/package-linux.sh`, PKGBUILD optdepends) is a
   packaging change I did not make.
5. **Android screenshots** in the README come from the 2026-11-03 goldens and were not re-taken; strings checked, none showed the renamed
   wording. The desktop images are small (480 px) because the goldens are; a hi-res Xvfb re-shoot (`scripts/screenshots-desktop-nothing.sh`)
   would sharpen them.
6. **Jargon left on purpose:** HFP, WirePlumber, SHA-256 and "pinned certificate" appear in the guide; each is explained next to a
   plain fix. The first-run card and Android wizard are plain.
7. **Real-device claims unchanged:** every feature except pairing/link is still "not yet confirmed on a real phone"; the docs say so.
   Windows and macOS: never run on real machines, no prebuilt binaries.
8. Android `minSdk` is 26 (Android 8); declining/ending a call needs Android 9 (said in the wizard). Docs say "Android 8 or newer"; the
   9+ call limit is not in the README.

## Verified

* Every `geekbridge-ctl` command shown in README, user guide, quickstart, call-audio and omarchy docs exists (after fix 1). `install.sh`
  flags (`--uninstall`, `--purge-data`, `--system`), `run-desktop.sh` flags and `realdevice.sh` commands match.
* `scripts/check-doc-links.sh`: 0 broken links (anchors checked by hand for the FAQ).
* Vocabulary: computer "Link a phone", phone "Pair a computer"/"Pair"/"Unlink" is consistent in code; the docs now state it once.
