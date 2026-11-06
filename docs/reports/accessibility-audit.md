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

# Accessibility re-audit, 2026-11-06

Scope: the UI added since the audit above. Desktop: the Messages search box and results, the reinstalled-phone hint in the
pairing prompt and in the phones list, Settings > Updates, the new golden states. Android: the setup wizard Battery "denied"
state, the Quick Settings tile, the Direct Share row, the reconnect notice, Home link states, large font and display size,
dark mode, touch targets, Back. Method: the egui AccessKit harness (`a11y_tests::audit_screen`: Tab walk in both themes,
names, focus rings, crowded targets) for the desktop, and `uiautomator` dumps on a private API 36 emulator (own AVD, console
port 5694, adb server port 16410, wiped afterwards) driven by `scripts/e2e/a11y-audit.py`. Contrast was computed from
`res/values/colors.xml` and `values-night/colors.xml`. No TalkBack, Orca or NVDA session was run: live regions, headings and
names are checked as tree attributes and in source, not as speech.

## Desktop

| Area | Result |
| --- | --- |
| Search keyboard flow | Ctrl/Cmd+F focuses the box (already there, `ctrl_f_puts_the_cursor_in_the_search_field`). Tab reaches every result (header and message rows are named buttons with a focus ring in both themes; `search_results_draw_in_both_themes_...`). Enter on a focused row opens it. Missing: Down arrow from the box (egui keeps arrows inside a text field), Escape to clear, and any announcement of how many results there are. See defects 1 and 2. |
| Names and group headers | Box "Search messages"; group header "Ann, 2 matching messages, open the conversation"; row "Ann, 3 d ago: you: the paper is due Friday". Matches are read in the order they are drawn: header, then its messages. No defect. |
| Reinstalled-phone hint | In the pairing prompt it is plain text (not a colour) placed between the "type the code" explanation and the code field, so a reader meets it before the field. Under the older phones-list entry it is a plain caption after that entry's buttons. The hint is not a shortcut: approving still needs the typed code (existing test). The buttons were the problem, see defect 3. |
| Settings > Updates | Section label, summary, check, automatic switch, channel choice and notes read in layout order; every stop is named and has a ring (new `the_updates_section_and_banner_are_named_...`). Defect 4. |
| New golden states | Only `messages-search` changed (the new count line, below); the other 100+ desktop goldens match. A first full `--check` on this loaded machine hit X-server start-up failures on other screens and re-captured "differs" screens on retry; no diff image was left, so those were load noise, not changes. |
| Contrast | Unchanged theme tokens; covered by `theme::contrast_tests`. The new count line uses the caption colour (secondary text, AA). |

### Defects fixed (desktop)

1. **Escape did not clear the search and Down arrow did not reach the results.** Escape on a non-empty box now clears it and
   keeps the cursor (egui drops focus on Escape before the app sees the key, so the box remembers that it had focus; an empty
   box still lets Escape go). Down arrow moves to the first result, and egui's own arrow navigation then walks the rows.
   Test: `the_search_is_usable_from_the_keyboard_alone` (Ctrl+F, type, Down, Down, Enter opens the hit, Escape clears).
2. **Nothing announced the number of results.** A polite live-region line under the box says "3 matching messages. Down arrow
   moves to the results, Escape clears the search." (or "N conversations match..."), and "No conversation matches." is live
   while a search is active. It is visible text too, so it also teaches the keys. Test: `the_result_count_is_a_polite_live_region`.
   Golden `messages-search-*` regenerated for the extra line (reviewed).
3. **Buttons in the phones list were not tied to a phone.** "Connect", "Unlink...", "Disconnect", "Link" and "Check
   connection" had the same name on every row, and after a reinstall two rows can have the same phone name, which is exactly when
   the hint says to unlink "the old entry". Each is now named for its phone ("Unlink Pixel", "Disconnect Pixel 8") and the
   flagged older entry says so ("Unlink Pixel, the older entry"). Test: `the_phones_list_and_pairing_prompt_read_in_order_...`.
4. **Three controls on the way had no keyboard marker or name.** (a) The "Link phone" and "Unlink phone?" dialogs used egui's
   title bar, a drag handle that Tab lands on with no name and no ring; the title is now a text line and names the window.
   (b) "Connect by address" (phones list) and "Can't scan?" (pairing panel) were collapsing headers with no focus ring; both
   draw the shared ring now. (c) The channel buttons "Stable" / "Dev" are named "Update channel: Stable", and the update
   summary and the install progress lines (Installing, Installed, failed) are polite live regions, so the result of
   "Check for updates" and an install are spoken without moving focus.

### Remaining desktop items

- The calendar "New event" / "Edit event" dialog still uses the title bar (drag handle with no ring); the same fix fits but
  it is outside this scope.
- Search results are not exposed as a list with item counts; each row is a button. Fine for Orca and NVDA browse mode, untested.
- Real Orca / NVDA pass still open.

## Android

| Item | Result |
| --- | --- |
| Setup wizard Battery, Deny state | After "Allow unrestricted battery" and "Deny" the page shows the warning sentence, "Try again", the OEM guide, "Open Pixel settings", "Open app settings", then Back / Skip (dump order). Buttons are 48 dp or taller, focus order follows layout, labels are text (no content descriptions needed), and the state is a sentence, not colour. TalkBack focus is restored to the same slot after the rebuild (existing `focusPath`). The sentence was not announced, defect 5. |
| Quick Settings tile | Label "Computer link", subtitle "Connected · Desk", state Active/Inactive set from the connection. The description spoke "·" as "middle dot"; defect 6. |
| Direct Share row | Short label is the computer name; Android's chooser adds the app name. Added a long label "Send to Desk" for the launcher list and descriptions (defect 7). |
| Reconnect notice | Title "GeekBridge", text "Android stopped the background connection. Tap to reconnect.", low importance, whole notification is the action, channel "Reconnect reminder". Plain words, says what to do. No defect. |
| Home link states | Home, the design gallery `status` / `controls` / `updatesavail` / `pairdlgcode` pages, the setup overview and the Battery page, in light, dark, font scale 1.5 and 2.0, display size large (density 600): no unlabeled control, overlap or off-screen clip. The only SMALL hits were rows cut by the scroll viewport at the edge of a page (a 6 dp slice of "Open app settings" before scrolling). At display size large with dark mode and 1.5 the emulator itself crashed part way through (an emulator segfault, adb dropped), so that combination was only partly covered. |
| Touch targets | Every clickable is 48 dp or taller; the only sub-48 rows remain the non-interactive info rows (`Ui.row`, 40 dp). |
| Back | A page opened with a `step` extra (the Battery page) leaves the wizard on Back; inside the wizard Back goes to the previous page (`SetupModel.back` tests). Home keeps the default Back (not run). No defect. |

Contrast (WCAG), computed from the theme colours (ratios, text needs 4.5:1, non-text 3:1):

| Pair | Light | Dark |
| --- | --- | --- |
| text on surface / card / stripe / fill (worst) | 16.2 | 16.5 |
| muted text on surface / card / stripe / fill (worst) | 4.73 (fill) | 5.93 (fill) |
| warn / bad text on those (worst) | 4.99 | 5.40 |
| on-accent on accent, on-hero on hero | 19.7 | 18.1 to 21.0 |
| hero-muted on hero | 8.28 | 6.92 |
| red dot on surface, card, stripe, fill (worst) | 4.27 | 3.70 |
| checked switch track on card | 19.7 | 18.7 |
| unchecked thumb (muted) on its track (line) | 3.94 | 5.00 |

All pass. The unchecked track itself (line on card, 1.45 / 1.34) is below 3:1, as is the "off" dot colour; state is carried by
the thumb position and thumb colour (which pass) and by the words beside it, and the dot-matrix "off" dots are decoration
(text is also in the content description). Noted, not changed.

### Defects fixed (Android)

5. **The Battery "denied" sentence was not announced.** The answer to Android's dialog arrives as a rebuilt page; the sentence
   is now a polite live region (like the scan hint and the paired banner). Test: `A11yLabelsTest.theBatteryDeniedSentenceIsSpokenWithoutMovingFocus`.
6. **The tile description read the middle dot.** The visible subtitle keeps "·"; `contentDescription` now joins with a comma
   ("Computer link. Connected, Desk"). Test: `theTileDescriptionHasNoMiddleDot`.
7. **Direct Share shortcut had a short label only.** Added `setLongLabel("Send to <name>")`, name bidi-isolated.
8. Contrast test extended: red dot on every surface, checked track, thumb on track, on-accent as a 3:1 non-text check
   (`ThemeContrastTest`).

### Remaining Android items

- Run TalkBack on a device for 5 and 6; live-region attributes cannot be read from a `uiautomator` dump.
- The emulator crashed repeatedly when started while Gradle was building; the large-display dark 1.5 run is incomplete.

## Checks run

`cargo test -p bridge-apps` (all pass), `cargo clippy -p bridge-apps --all-targets -- -D warnings`, `cargo fmt --check`,
`gradle testDebugUnitTest lintDebug assembleDebug` (pass; debug classes.dex +924 B, release effect smaller after R8),
`scripts/golden-desktop.sh --update` limited to `messages-search`. The i18n allowlist for `connection.rs` went from 64 to 62.
