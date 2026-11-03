# README screenshots refresh

Date: 2026-11-03.

## What changed

* Desktop: eight captures from the Xvfb screenshot run (scripted phone, `scripts/screenshots-desktop-nothing.sh`, std size, 960x680 and
  smaller) were quantised with `magick -strip -colors 128 PNG8:` and written to `docs/img/desktop-nothing-*.png`: home dark and light,
  first run, messages, contacts, calendar month (dark), calendar agenda (light), incoming call. Six files replace older images in place; two
  are new (`first-run-dark`, `contacts-dark`). The desktop crates are unchanged between the capture worktree (00cc270a0) and origin/main,
  so the captures were not re-taken.
* Android: no emulator was driven. The four images `docs/img/android-{status-dark,setup-light,share-dark,calendar-dark}.png` come from the
  checked-in goldens in `tests/golden/android/` (`status-dark`, `controls-light`, `shareon-dark`, `calendar-dark`, font scale 1.0), which include
  the settings screens from ac2aa91ef, quantised the same way (540x1080).
* README gallery rewritten with new alt text; `docs/android-app.md` points at the new Android names.
* Old `docs/img/nothing-*.png` (Android, 1080x2400) are no longer in the README; they stay because older reports link them.

## Size

New and changed images total about 0.8 MB on disk, of which the replaced desktop files were already in the repo, so growth is well under
the 2 MB budget.

## Left out on purpose

* Share and Clipboard desktop windows: the std captures are cropped and the Share one shows a scratch path (`/tmp/gb-golden-home/...`), so the
  older `desktop-share.png` / `desktop-clipboard-sync.png` stay as they were (not in the README gallery anymore: share is covered by the Android
  share image).
* Calls keypad: shows "Can't call yet" capability notices in the fixture run; the incoming-call window is used instead.

## Fixture check

Every image was viewed. Contents are only the fixture phone ("Pixel simulated", "Pixel 9"), made-up people (Ada Lovelace, Alan Turing,
Grace Hopper, Katherine Johnson, Margaret Hamilton), a 555 number, `demo@gmail.com`, and June 2026 calendar entries. No real names, numbers,
addresses or home paths.

## Cleanup

No emulator or adb server was running (adb not on PATH, no emulator/qemu process). Scratch is under `~/.cache/gb-tmp/final`.
