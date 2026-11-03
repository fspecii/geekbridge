# Pairing: the first tap on "Continue" that gets lost

A dated snapshot, not living documentation.

Reports from `scripts/e2e/photos.sh`, `features.sh` and earlier `call-ui.sh` runs: after the phone shows "Connect to a
computer?" the first `ui tap Continue` is occasionally lost, and the page is still there 40 s later. A second tap works.
Question: an e2e timing problem, or something a real user can hit?

## Short answer

I could not make the app swallow a tap, on the confirmation page or anywhere else on the way to it. In about 30 timed
trials the first tap always started pairing. The app has no code that disables Continue and re-enables it later, no
overlay or obscured-touch filter, and no double-submit guard that could eat a first tap. The lost tap is most likely
the e2e driver, which sent the tap once and ignored the result. I did not catch one of the original losses in the act,
so the cause is not proven.

There was one real, smaller app flaw next to it, which is fixed here (below).

## What I read

- `QrPairActivity`: Continue on the confirmation is a plain `TextView` with `setOnClickListener { if (isEnabled) ... }`, always
  enabled. The click handler is set when the view is built, not after async work. `render()` rebuilds the whole page, but only
  on a mode change; nothing re-renders a shown confirmation (the 700 ms ticker acts only in `Connecting`).
- `Screen`/`SystemBars`: insets are applied before the first draw and again on `requestInsets()`, which changes padding only.
  There is no scroll restore on this page and no `filterTouchesWhenObscured`. The dot-matrix title (`DotText`) is built once
  per render and sits above the button.
- The Setup wizard's own Continue/Next buttons use the same `UiKit.pill` and the same listener shape.
- No permission dialog can overlay it: `POST_NOTIFICATIONS` and the others are granted with `pm grant` before the link is opened.

## Reproduction (AVD clone, own adb server and ports, nothing shared)

Clone `gbtap` of `gb36` under `~/.cache/gb-firsttap-emu` (emulator console 5694, adb server 16190, desktop service on control
port 17893, Xvfb :97). Free disk before starting: 57 GB. Ports 1716, 17381 and 8000 and the live service were not touched.

| Experiment | Result |
|---|---|
| Fake link, `input tap` at fixed coordinates 0, 100, 300, 500 and 1500 ms after `am start -W` returns (cold start after `pm clear`) | 5 of 5 started pairing. `mCurrentFocus` was `QrPairActivity` every time. |
| The e2e sequence (`pm clear`, grants, `am start -W`, `adbui wait`, `adbui tap Continue`), host idle | 10 of 10 |
| Same, with 30 busy loops on 24 cores | 8 of 8 |
| Real `photos.sh pair` against a real desktop service, with a diagnostics patch (focus and `uiautomator` dump before and after the tap) | 3 of 3 clean runs paired first time |
| Same, with 24 busy loops | 6 of 6 |

The one failing run I got is not evidence of an app bug. Two copies of the pair stage were running against the same
emulator by my own mistake, and every `uiautomator dump` in that window came back empty. That does show how the driver
fails, though: with no dump it cannot find the button, it does not tap, and nothing says so.

## Why the driver can lose a tap (reading `scripts/e2e/adbui.py`)

1. `tap` makes a single attempt. If the dump fails or does not parse, `nodes()` returns `[]`, `tap` returns false and
   the scripts discard that result (`>/dev/null`). `wait` polls, so it hides the same failures.
2. A failed `uiautomator dump` leaves the previous dump in `/sdcard/gb-ui.xml`, and `cat` then reads that stale file.
   The tap can land on coordinates from an older screen.
3. `adb` timeouts raised `TimeoutExpired`, which ended the whole call without a message.
4. Nothing checked that the window had input focus or that the screen changed afterwards, so one lost tap failed the stage.

`photos.sh` already carried a workaround (wait 15 s, tap again); `features.sh` and `call-ui.sh` did not.

## Changes

E2E (shared helper, used everywhere the pairing page is tapped):

- `adbui.py tapto TEXT [--expect TEXT2] [--tries N] [--verify S]`: waits for a node that is enabled and clickable (itself
  or an ancestor), whose window holds input focus, and whose bounds are the same in two dumps; taps; then requires the
  screen to move on (the node gone, or TEXT2 visible); otherwise logs the focus state and node flags to stderr and
  retries. Shell helper `tap_to` in `scripts/e2e-android.sh`.
- Dumps no longer fall back to a stale file, and adb timeouts count as a failed dump.
- `features.sh`, `photos.sh`, `call-ui.sh` and `pair-lifecycle.sh` use `tap_to` for Continue (and `Pair` where it was tapped).
  The ad-hoc retry in `photos.sh` is gone.

App (small, real):

- When a link arrives from outside, `QrPairActivity` used to draw the paste form, with its own "Continue", "Open the camera
  app" and "Scan with a scanner app" buttons, until the core had parsed the link and the page was replaced with the
  confirmation. A tap in that moment hit a different button. It now shows "Reading the pairing link…" with no form (`Mode.Reading`).
- A parse answer that arrives after the user pressed Cancel, or after a newer link replaced the first one, is ignored instead
  of bringing the old confirmation back (`QrLinkGate`, tickets).
- Continue starts pairing once (`QrContinue.mayBegin`); a second tap while connecting does nothing.

Tests: four new cases in `QrPairingTest` (gate order, late answer after Cancel, newer link wins, Continue guard). The
state logic is plain Kotlin, no Robolectric.

## Verification

- `gradle testDebugUnitTest lintDebug`: exit 0 (`QrPairingTest`: 11 tests, 0 failures).
- `photos.sh pair` with `tap_to`, real desktop service: PASS. `tapto` run against the emulator 3 times (fake link): exit 0 each time, 7 to 8 s each (the dumps are slow on this loaded
  host: stability needs two). A nonexistent label returns 1 with a diagnostic line after 30 s.
- The app change was not run on a device: it needs the native core build, which I did not do here. The e2e runs above used the
  existing debug APK. Please include the next `photos.sh` or `features.sh` run in the check.
- The retry path of `tapto` (tap did not change the screen) was written and read through but never exercised, because no
  tap was lost.

If a pairing stage still fails, the `tapto[...]` lines in the log now say whether the node was not tappable, which window had
focus, and whether the tap changed anything.
