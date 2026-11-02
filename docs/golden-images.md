# Golden images (visual regression)

The Nothing x Continuity look is protected by golden-image tests: every screen is rendered under fixed conditions and
compared with a committed PNG. A change that moves, recolours or restyles anything fails the check and leaves a
side-by-side diff.

```
make golden               # desktop + Android --check
make golden-desktop       # scripts/golden-desktop.sh --check
make golden-android       # scripts/golden-android.sh --check
make golden-update        # re-render the goldens (only after a visual review, see below)
```

Failures write `dist/golden-diff/<platform>-<name>.png`: expected | actual | differing pixels in red.

## Updating goldens consciously

Goldens change **only** with a visual review. When a check fails and the change is intended:

1. Open the diff images in `dist/golden-diff/` and confirm every highlighted change is what you meant.
2. Run `scripts/golden-desktop.sh --update` (and/or `scripts/golden-android.sh --update`; `ONLY=regex` limits it).
3. Review the changed PNGs in `git diff` / your Git UI before committing; commit them in the same change as the UI edit.

Never run `--update` just to make a red check green. An unexplained diff is a regression.

## How it stays deterministic

Desktop (`scripts/golden-desktop.sh`, Linux with Xvfb, xdotool, ImageMagick `import`, a C compiler, python3 + Pillow):
private Xvfb and service state; a scripted phone (`bridge-phone-sim`); the wall clock frozen at 2026-06-15 12:00 UTC by an
`LD_PRELOAD` shim (`scripts/golden/fakeclock.c`); `TZ=UTC`, 24 h clock, `en_US`; fonts from `third_party/fonts` only
(`GEEKBRIDGE_BUNDLED_FONTS_ONLY=1` skips system fonts); animations are off in the theme; update banner from a loopback
mock release server (no network); each screen is captured until two consecutive frames are identical. Every screen is shot
in light and dark at the standard window size (`-std`) and resized to the app's minimum (`-min`).

Android (`scripts/golden-android.sh`): a headless emulator on its own console port and adb server (`16058`), started
read-only so the AVD is not modified. System state: demo mode (clock 12:00, full battery), animations off, rotation locked,
en-US, UTC, date pinned to 2026-06-15, font scale 1.0 and 2.0, light and dark; screencaps cropped to the app area.
The AVD that gives stable pixels: system image `system-images;android-36;google_apis;x86_64` (the google_apis flavour,
which allows `adb root`), 1080x2400, density 420, locale en-US, software GPU (`-gpu swiftshader_indirect`, the launcher
default). The default is the project AVD `gbnothing` (`scripts/bootstrap-android.sh`); to make your own:
`avdmanager create avd -n gbgolden -k "system-images;android-36;google_apis;x86_64" -d pixel_6` then set
`hw.lcd.density=420`, `hw.lcd.width=1080`, `hw.lcd.height=2400` in its `config.ini`, and run with `GOLDEN_AVD=gbgolden`.
A different image, density or GPU renders text a few pixels differently, so the goldens are only valid on that setup.

## Tolerance and size

Both goldens are 50 % scale, 256-colour PNGs (about 1.5 MB for the desktop set). A pixel counts as different when any
RGB channel differs by more than 24/255 (`GOLDEN_PIXEL_THRESHOLD`); the check fails when more than 0.05 % of the pixels
differ (`GOLDEN_MAX_RATIO`) or the size changes (`scripts/golden/golden_img.py`).

## Known limits

* The pairing QR dialog is not covered: its code embeds fresh keys on every run.
* The home screen shows "Updated to <version>" with the changelog bullets, so a release or changelog edit changes the
  `home-*` goldens; update them with the release.
* Desktop goldens depend on the Noto/Hack fonts and the egui version; an egui upgrade legitimately re-renders everything.
