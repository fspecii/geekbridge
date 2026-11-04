# Visual QA 3: desktop and Android UI from the checked-in goldens

Scope: every golden touched in the last hours under `tests/golden/desktop` (pair, home first-run / firewall /
no-Bluetooth / service-off, settings, settings-call-audio-* (11 states), calls-keypad(-no-bluetooth), calls-audio-setup /
-confirm, call-incoming / -active, call-overlay, share, share-photos*, update-banner), light and dark, standard and
minimum size (320 px wide Calls window, 560x520 Home, 380x460 Share). Goldens are 50 % scale; each was viewed at 1.5x
(std) and 2x (min).

Android: `tests/golden/android` holds only the design gallery, Diagnostics and Share pages; none of the new screens
(scan QR, invite page, setup steps, updates card) has a golden, and none changed in these commits. They were reviewed
as strings and layouts only (see "Left"). No emulator was started.

## Defect table

| # | Screen (golden) | Defect | Status |
|---|-----------------|--------|--------|
| 1 | calls-keypad-no-bluetooth (std, min) | The no-Bluetooth warning sat below the keys: cut mid-line by the status bar at std, completely off screen at min (the golden looked identical to the plain keypad) | Fixed: warning is drawn above the keys, the keys shrink to make room |
| 2 | settings-call-audio-* (min, 11 states) | Min goldens did not show the call audio section at all (scroll position lost on resize), so the new states were unreviewable at min | Fixed: the screenshot scroll is re-applied when the window size changes |
| 3 | settings-call-audio-* (min) | Content wider than the window: card and "Time window" / shortcut row clipped at the right edge | Fixed: slider and shortcut field shrink to the column (test `the_sync_body_fits_a_narrow_column`) |
| 4 | settings-call-audio, -done, -failed, calls-audio-setup | Headline "Almost there. Do the next step below.. 5 of 6 steps done" and "...computer.. 6 of 6 steps done": double full stop | Fixed (joiner no longer doubles the period) |
| 5 | calls-audio-setup, calls-audio-confirm (Recents) | Raw repository path "(docs/call-audio-options.md)" shown to the user; the same warning printed two or three times | Fixed: path stripped in the window (terminal and logs keep it), source lines that repeat the note above are skipped |
| 6 | share (std, min) | "Receiving" card overflowed the window: card edge and "Open folder" button clipped | Fixed: path wraps, button below it |
| 7 | pair | Page title "PAIR" while sidebar, button and docs say "Link a phone" | Fixed: title "LINK" (dot-matrix title cannot hold the full phrase at min width) |
| 8 | call-overlay vs call-incoming | Overlay buttons said "Reject", the Calls window and tray say "Decline" | Fixed: "Decline" everywhere (overlay strings and keys renamed) |
| 9 | home-no-bluetooth | Calls tile caption truncated with "…" ("No Bluetooth: talking needs an…") | Fixed: "No Bluetooth: add an adapter" |
| 10 | home-service-off | Sidebar footer said "Background service running" under a card saying "Service not running" | Fixed: footer agrees with the control check |
| 11 | settings-call-audio-* (min) | Tab/focus coverage of the new setup steps was not tested | Fixed: new test tabs through 11 states in both themes; every stop has a name (no `callaudio.*` key) and a focus ring |
| 12 | home-first-run, -firewall (min) | Primary "Link a phone" button is below the QR block, needs one scroll | Left: step 1 (install) comes first by design; sidebar entry "Link a phone" and the sidebar card ("Click to link a phone") are visible at min |
| 13 | all Home-window goldens | Sidebar card detail "Connected over your ne…" truncated | Left: cosmetic, one copy change would also change the Home tile; no information lost (full text is in the tile) |
| 14 | pair | Two rows for the same phone name ("Pixel simulated") | Left: scripted phone fixture, one paired and one nearby entry |
| 15 | settings | Section label duplicated by the card's own overline ("UPDATES" / "UPDATES", "THIS COMPUTER" / "THIS COMPUTER'S NAME"); "Dev (newest main build)" channel choice has weaker button affordance than "Stable" | Left: consistent across Settings, no usability impact |
| 16 | call-active (min) | Card text touches the card's bottom edge ("hands-free device.") | Left: fixed-height card at the Calls window minimum; fits in std |
| 17 | share | Save folder shows `/tmp/gb-golden-home/...` | Left: golden harness uses a fixed HOME so the picture is deterministic (documented in the script); not a product defect |
| 18 | settings-call-audio-done / -failed | Checklist counts "6 of 6 steps" while the flow says "Step 6 of 7" | Left: two different measures (checklist vs. guided flow) |
| 19 | settings-call-audio-* (min) | Cards now reach the window's right edge with no margin while scrolled (no text is clipped) | Left: no clipping; scroll bar overlay |
| 20 | Android | Wording mix: "Link to your computer" / "Link a phone" vs the confirmation dialog "Pair with %1$s?" and "Paired ✓", Diagnostics "Paired computers" | Left: decision of pairing-ux-2 (Link for the action, Pair for the security confirmation); Bluetooth "pair" in call audio is the real Bluetooth term |
| 21 | Android scan-QR, invite page, setup steps, updates card | No goldens, so no pixel review (strings checked: no placeholders, no raw keys, no untranslated fallbacks; English is the only catalogue on desktop, `en.properties`) | Left: add gallery pages for these screens in a follow-up (needs the AVD run) |

## Checks

- Contrast, truncated buttons, raw `callaudio.*` / `call.audio.*` keys, placeholder text: none found in the viewed goldens.
- Dot-matrix titles (LINK, SETTINGS, HOME, PAIR removed) are aligned; date yields to the title at min as designed.
- Tab order and focus rings against `docs/reports/desktop-a11y-audit.md`: the shell-app Tab test already covers the Calls
  window; the new call audio controls were not in it. Added
  `call_audio_ui::tests::every_flow_step_is_reachable_by_tab_named_and_ringed` (Tab through every setup step in light and
  dark: named, ringed). The call overlay and photos tab keep their existing named-control tests
  (`the_card_draws_inside_its_window_with_named_buttons`, `every_control_of_the_tab_has_a_name_...`).
- New tests: `settings_fit_the_minimum_window_width`, `the_sync_body_fits_a_narrow_column`, `plain_line_drops_the_repository_path_only`.

## Goldens

Regenerated in a separate worktree with `scripts/golden-desktop.sh --update` (RUN under `~/.cache`, long timeout) only for
the affected screens (64 images: pair, home-no-bluetooth, home-service-off, settings, settings-call-audio*, calls-keypad-no-bluetooth,
calls-audio-*, call-overlay, share). Each result was viewed again after the update. A first `--check` pass was disturbed by
Xvfb start failures under load; the final updates ran with two workers.
