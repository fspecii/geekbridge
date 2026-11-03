# Emulator end-to-end coverage: recent photos

Script: `scripts/e2e/photos.sh [--stop] [env,pair,photos]` (same pattern as `features.sh`). Everything private: own AVD clone
`gbphotos` (from `gb36`, API 36, console 5674, adb server 16170), own desktop service with its own `HOME` so the save folder is
private too (control 17891, api 17890, discovery 17846, listener 1757, redirect 17961), private D-Bus session, Xvfb `:95`, scratch
under `~/.cache/gb-tmp/photos-e2e`. Never touches the live service, real config or ports 1716, 17381, 8000. Results:
`dist/android/logs/e2e-photos-results.tsv`.

Setup: five generated JPEGs (one 2400x1600 random-noise picture to stress the thumbnail size cap) are `adb push`ed to
`/sdcard/Pictures` and indexed with the `MEDIA_SCANNER_SCAN_FILE` broadcast (two seconds apart so DATE_ADDED orders them);
`pm grant READ_MEDIA_IMAGES`; the phone switch and the per-computer grant are written into the app's preference files as the
other e2e scripts do; desktop consent with `geekbridge-ctl consent ID photos on`.

## Result: 19 checks, 19 PASS (final full run, from a fresh AVD clone and fresh pairing)

| check | what it proves |
|---|---|
| off_default | phone switch off: phone does not offer photos, `refresh` refused, nothing arrives even with consent on |
| needs_grant | switch on, no per-computer grant: refresh never answered |
| needs_consent | switch and grant on, computer consent off: refresh/save refused, nothing held |
| list_newest | list is newest first; names, byte sizes and pixel sizes equal the files |
| thumbnails | all five arrive; valid JPEG, at most 24 KiB and 256 px (the noise picture's is about 6.7 KB, so the shrink step works) |
| save | `photos save` writes the file into the save folder, sha256 identical (3.7 MB noise picture) |
| copy, copy_clipboard | `photos copy` lands in the private scratch folder (mode 700), identical sha256, not in the save folder; `xclip -t image/jpeg` on the private Xvfb round-trips identical bytes |
| unknown_id | an id never listed is refused, nothing written |
| withdraw_desktop, reallow | consent off empties list and thumbnails and refuses refresh/save/thumb; on again restores only on request |
| withdraw_phone | grant removed on the phone: list erased by the reconnect, refresh unanswered |
| switch_off | switch turned off while paired: nothing offered or answered |
| no_access | both photo permissions revoked: requests unanswered |
| partial_none, partial_picked | only `READ_MEDIA_VISUAL_USER_SELECTED`: list marked partial (CLI says "the phone shows only the photos its user picked") and empty; then the real system photo picker is driven (app button "Choose which photos", "Allow limited access", two tiles, Done) and exactly those two pictures are listed |

## Changes

* New `scripts/e2e/photos.sh`; `.github/workflows/emulator-e2e.yml` gets a `photos` suite (adb port 16170), marked
  `experimental: true` (non-blocking) until it has a few green nightlies; `python3-pil` added to the apt list (picture generator);
  its logs are uploaded on failure.
* `geekbridge-ctl photos thumb DEVICE_ID PHOTO_ID FILE` (small addition): writes the held thumbnail so the cap and JPEG-ness
  can be checked from outside. No other product change.

## Findings

No product bug was found: every assertion held on the first correct run. Script-side lessons, all fixed:
* `pm grant/revoke` of a runtime permission kills the app and drops the link; permissions are set before `phone_config`
  (which restarts the app and reconnects). Silence checks now require a live link, otherwise a dropped link would pass them.
* The first pairing tap on "Continue" is occasionally lost (also seen in `features.sh`); the script retries it once.
* Partial-access is exercised on API 36 only; on an image below API 34 that check is recorded as SKIP.
* Clipboard: `geekbridge-ctl` has no copy-to-clipboard command (the window does it); the check runs the same tool and MIME type
  the window uses on X11 against the copied file. Wayland (`wl-copy`), PowerShell and `osascript` are not covered.
* Not asserted from outside: the thumbnail packet's transport, beyond what the CLI exposes.

Everything was stopped afterwards (emulator, service, Xvfb, adb server 16170, AVD clone removed).
