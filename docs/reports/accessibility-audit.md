# Accessibility audit, 2026-11-05

Scope: the desktop egui apps (Home, Calls with the in-call card, keypad, Recents and decline picker, Messages,
Contacts, Agenda, Share, Settings, Photos) and the Android app. Builds on `docs/accessibility.md`,
`docs/reports/desktop-a11y-audit.md` and `docs/reports/android-lint-a11y.md`; this report only lists what is new.
Everything was checked from code and headless tests (AccessKit trees, painted shapes, theme tokens). No TalkBack,
Orca or NVDA session was run, and the emulator was not started.

## Desktop

| Area | Result |
| --- | --- |
| Contrast (WCAG AA 4.5:1 text, 3:1 UI) | Already computed from the theme tokens in `theme::contrast_tests` for light, dark and high contrast: text, secondary text, accent, status colours, on-accent, selection, derived egui colours, field outlines and the focus ring on every surface. The card buttons use fixed fills with white text (green 5.1:1, grey 7.2:1, brand red), noted in `call_overlay.rs`. No defect. |
| Keyboard reach and focus indicator | `every_tab_stop_draws_a_focus_indicator_in_both_themes` only walked the shell apps with nothing in progress. New `a11y_tests::audit_screen` tabs through any screen in both themes and reports a stop without a ring, an unnamed stop or control, and a crowded small target. It now covers the Calls window in seven states (dial pad with the service down and up, Recents with Call back / Message, ringing sheet, ringing with replies, in call, in-call keypad open; `calls_window_states_are_named_focus_marked_and_reachable_by_keyboard`) and the overlay card in five states (ringing, decline picker, in call, in call with every option, outcome; `every_card_state_has_named_buttons_a_focus_ring_and_a_tab_order`). |
| Names for icon-only and card buttons | All named: Close "Hide this card...", Answer / Decline "... the call from Ada", Hang up, Mute, Keypad, Listen over Wi-Fi, Recents round buttons "Call back Ada Lovelace" and "Message Ada Lovelace". Checked in both tests above. |
| Minimum target size | The card is 360 x 96 pt and its close button (20 pt), saved-text buttons (22 pt) and small in-call buttons are under 24 pt. WCAG 2.5.8 allows that when no other target comes within a 24 pt circle; the audit applies that rule and finds none crowded. Not enlarged, because the card cannot grow without changing its fixed size. The shell windows keep the strict 24 pt test at 100% and 150%. |
| Text scaling / minimum text size | The 12 pt floor test only scanned nine hand-listed files. It now scans every source file under `src`. It found 11.5 pt text on the card's "Decline with message" link. |
| High contrast, reduced motion | Covered by existing tests and notes (`high_contrast_makes_secondary_text_and_edges_strong`, state fades off, no animation loops). No change. |
| Screen-reader announcements of the call card | See defect 1. |

### Defects fixed (desktop)

1. **The in-call card was re-announced every second.** Every card state was an assertive `Alert` live region, and the
   in-call text was "IN CALL 00:12, Ada" with the running clock, so a screen reader would interrupt itself each second for
   the whole call. Now only the ring ("Incoming call from Ada", and the decline picker prompt) is an assertive `Alert`,
   and its text does not change while it rings, so it is announced once. The in-call state ("IN CALL, Ada", clock
   removed by `without_clock`) and each outcome are a polite `Status` node. Test: `the_in_call_announcement_does_not_change_with_the_clock`
   and the live-region assertions in the card test. The brief asked for a polite ring; the ring stays assertive because
   an incoming call is time-critical and `docs/reports/desktop-a11y-audit.md` records that as a confirmed decision. Say
   if you want it polite.
2. **"Decline with message" had no focus indicator.** It is a frameless button, so a keyboard user could Tab onto it and
   see nothing. It now draws the shared focus ring. Caught by the new card audit.
3. **"Decline with message" was 11.5 pt**, below the project's 12 pt floor. Now 12 pt; the floor test covers all sources.
4. **Dial pad `*` and `#` were named "Key *" and "Key #"**, which readers say as "asterisk" or "number sign" in different
   ways. They use the same names as the in-call keypad ("Key star", "Key pound"), and the hard-coded English string is
   gone (i18n allowlist lowered from 13 to 12).

### Remaining desktop items

- The ring card and the Calls window ringing sheet are both assertive; if both are open a reader announces the ring twice.
  They are separate windows, so this was left alone.
- The dial pad keys do not speak their letters ("2 ABC"). Low value, not changed.
- A real Orca / NVDA pass over the card is still open (the card is a no-activate window, so focus behaviour needs a
  real compositor).

## Android

Reviewed in code: `Ui.kt`, `UiKit.kt`, `HomeActivity`, `SetupActivity`, `PairPanel`. Already in place: 48 dp minimum
heights on buttons, switches, expanders and the chip; headings on titles; dot-matrix text spoken as words; switch rows
that read as one switch with its state; expanders with `stateDescription`; polite live regions on the scan hint and the
paired banner; pane titles on each setup page; contrast and font-scale tests (`ThemeContrastTest`, `ScaleTest`, golden
gallery `fs15`).

| Item | Result |
| --- | --- |
| "Update available" chip | Labelled "Update available, version X. Opens the update card.", announced as a button, 48 dp high. No defect. |
| "More call options", "Other ways to link" | Buttons whose label flips, but they gave no state, and the screen was rebuilt on every tap without putting TalkBack focus back (see below). |
| Setup wizard focus order | Header, step icon, title, status chip, explanation, panel, footer: reading order follows layout. The stepper is hidden from TalkBack because the "Step N of M" caption says it. No defect. |
| Touch targets | The only sub-48 dp rows are the non-interactive info rows (`Ui.row`, 40 dp), as recorded before. |

### Defects fixed (Android)

5. **Show/hide buttons announced no state.** New `UiKit.disclosure` adds `stateDescription` Expanded / Collapsed (API 30+,
   minSdk is 26 so it is guarded) using the existing strings. "Other ways to link" and "More call options" use it.
6. **TalkBack focus was dropped when the pairing panel or a setup page was rebuilt in place** (toggling a switch, "More
   call options", "Other ways to link"): the views were removed and recreated and focus went back to the top. Both now
   save the focus path before and restore it after, as `HomeActivity` already did.
   Test: `A11yLabelsTest.showHideButtonsGoThroughTheDisclosureHelper` (a source check: there is no Robolectric in the
   project, so the behaviour itself is untested on the JVM).

### Remaining Android items

- Run TalkBack on a device or the emulator for items 5 and 6. No layout changed, so the golden gallery (including
  font scale 1.5 / 2.0) is not stale.
- No unit test can check the focus restore without Robolectric or an instrumented test.

## Checks run

`cargo test -p bridge-apps` (all pass), `cargo clippy -p bridge-apps --all-targets -- -D warnings`, `cargo fmt --check`,
`gradle --offline testDebugUnitTest lintDebug` (both pass).
