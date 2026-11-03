# Android settings and setup screens: polish review

Scope: the switches added on 2026-11-03 (battery sharing, notification mirroring with app list and Notification-access guidance, dialling from the computer with "ask me before placing a call", Messages guidance), on the "Share with computer" and "Answer calls" setup steps. Reviewed on a private AVD clone (`gbsettings`, own console port and adb server port, stopped afterwards; the live service and ports 1716, 17381, 8000 were not touched), dark and light, font scale 1.0, 1.5 and 2.0, the `en-XA` (expanded) and `ar-XB` (right-to-left) pseudo-locales, plus the uiautomator accessibility audit (`scripts/e2e/a11y-audit.py`).

## Defects fixed

1. Every switch on the setup page jumped the page back to the top (the whole page is re-rendered). The reading position is now kept when the same page is re-rendered.
2. The step intro and its status line named only contacts, calendar and texts; battery and notifications were not mentioned. Both now list all five.
3. Messages off-state said "Turn on ... below" while the switch is above the message, and was drawn in the red warning colour although "off" is the normal state. Reworded ("Switched off. No text message leaves this phone. Use the switch above to turn it on.") and drawn muted.
4. "Off by default." appeared inside the row text while the switch was on and the state line said "On" (battery, notifications, calendar editing, call status). Removed from those rows; the section intro already says "Optional, off by default" and the status line shows the real state.
5. The notification row was a wall of text (what is sent, what is never sent, what is kept). The row now says what it does and that Android asks for Notification access; the exclusions (ongoing, call, secret, GeekBridge's own notifications, icons) moved into the explanatory note shown when the switch is on. The battery row says what is sent, to which computers (the "Allowed:" line and "Allowed computers..." button appear below it), and that nothing needs a permission.
6. The restricted-settings message repeated the whole help text right above it. The state line is now one sentence pointing at the steps below.
7. Answer calls step: the dial permission was an unexplained plain text button squeezed between two rows, and the page intro did not mention dialling. Added a "Place calls from my computer" heading with a plain explanation (off by default, which computers, when Android asks), a tonal button, and the intro now mentions starting a call. The "done" text now says what was allowed and points at the confirm switch.
8. Hairlines touched the button or note above them. Added 10 dp before them on the two steps.

## Checked and found fine

- Targets: the a11y audit reports no clickable node under 48 dp except partly scrolled-off rows clipped by the footer (not real).
- TalkBack: each switch row is one focus stop exposed as a switch with the checked state, title first and then the description; no unlabeled clickable nodes; state lines are separate read-only stops that follow their row.
- Contrast: warning red `#C4141C` / `#FF5A60` and muted text reuse the tokens already covered by the theme contrast test.
- Font scale 1.5 and 2.0 and both pseudo-locales: no clipped or overlapping text, rows and buttons wrap, the right-to-left mirror places the switch on the start side.

## Not changed

- The state sentence of the notification row is left-aligned to the card, not to the row's text column (the Off/On state of the other rows is inside the row text). Cosmetic, consistent with the Messages line.

## Goldens

New debug gallery pages in `DesignGalleryActivity` (`shareoff`, `shareon`, `dial`) render the real string resources in the new states; `scripts/golden-android.sh` shoots them in light/dark at fs 1.0 and 2.0 (12 new files in `tests/golden/android/`). The 16 existing goldens still pass unchanged.

## Verification

`scripts/build-android.sh` (cargo, `testDebugUnitTest`, `lintDebug`): exit 0. `golden-android.sh --check`: 16 existing ok, the 12 new reviewed and written with `--update` (limited by `ONLY`).
