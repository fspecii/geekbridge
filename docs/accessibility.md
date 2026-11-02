# Accessibility

Audit of the desktop apps (`crates/bridge-apps`, eframe/egui 0.31) and the Android app (TalkBack, Switch Access,
large text). Scope: keyboard operation, screen-reader exposure, contrast, text size, motion, colour independence,
locale, window management. Date: 2026-11-02. Screenshots of focus states: `dist/screenshots/desktop/a11y-*.png`
(regenerate with `dist/screenshots/desktop/a11y-capture.sh`; Xvfb, synthetic keystrokes only).

## Checklist and results

### Desktop

| Check | Result |
| --- | --- |
| Every control reachable with Tab / Shift+Tab | Yes. All controls are egui widgets with `Sense::click()`; Tab order = draw order. **Gap:** the connection strip (Check again, Details) is drawn first, so it takes the first two Tab stops. |
| Enter / Space activate | Yes (egui keyboard-activates any focused clickable widget). Keypad keys keep focus after keyboard activation (fixed: they used to jump back to the number field). |
| Arrow keys in lists and calendar grid | Yes: egui moves focus spatially with the arrow keys between focusable widgets (month cells, contact rows, conversation rows, sidebar, keypad). Text fields keep their own arrow keys. |
| Escape closes sheets | Fixed. Forget-phone, Pair-with-QR and the event editor close on Escape (nothing destructive happens). Escape only leaves a focused text field first, so half-typed text is not discarded. The pairing code sheet has no Escape (Decline is a deliberate button). |
| Visible focus ring >= 3:1, both themes | Fixed. The ring was 70% translucent red (accent) and vanished on the red Primary button. It is now a 2 pt full-ink ring (`theme::focus_color`) one point outside the control, never red. Unit-tested >= 3:1 on every surface in light, dark and high contrast. Ring added to calendar cells/chips/blocks/rows, contact and conversation rows, the alphabet index, the calendar list. |
| Incoming-call popup without a mouse | Fixed. On ring, focus moves onto the popup sheet and Tab / Shift+Tab stay inside it (sheet, Decline, Answer). **Enter = Answer, Esc = Decline** on the sheet; when Decline or Answer itself has focus, Enter activates that button (so Tab to Decline + Enter declines). `A` / `D` work when no text field has focus. |
| Hang up is safe by default | Fixed. `H` used to hang up on one keypress. Now the first `H` arms ("Hang up now", hint text); a second `H` or Enter confirms, Esc cancels, it disarms after 5 s. The Hang up button itself stays one deliberate click/activation. Typing in a field never hangs up. Tests: `call_ui::tests::*`. |
| Screen reader exposure | **Not enabled: see below.** Every custom widget already reports `widget_info` (label, role, selected/enabled state) so it is exposed the moment AccessKit is on. |
| Text scaling, minimum font >= 12 pt | Fixed. Smallest sizes were 8 to 11.5 pt (keypad letters, overline labels, calendar chips, message previews, technical details); all are now >= 12. A unit test scans the UI sources for any `FontId::new(<12` / `.size(<12`. Ctrl +/- / Ctrl 0 zoom the whole UI (egui default). Dot-matrix pitch follows the Body size. |
| Reduced motion / no animation loops | Yes. Nothing animates continuously; repaints are event-driven (`nothing.rs`) and the only timers are bounded flashes (copy confirmation, 60 ms during a held 0 key). egui's state fades are now off (`animation_time = 0`). |
| High-contrast mode | Added. `GEEKBRIDGE_HIGH_CONTRAST=1` (or a `GTK_THEME` containing `HighContrast`) turns secondary text to full ink and edges / unlit dots to a strong grey (>= 7:1 text, >= 3:1 edges, tested). Screenshot: `a11y-call-popup-high-contrast.png`. It does not follow a desktop-wide setting automatically (no portal query). |
| Colour is never the only signal | Yes. Status dots pair with text ("Helper not running", "Paired"/"Found, not paired") and shape (hollow vs filled ring); the red "today" is a larger filled disc plus "today" in the cell label; blocked actions give a written reason; Primary red buttons carry words and a key hint (`Decline  Esc`). The red accent is never the only difference. |
| Locale / numbers / dates | **Gap.** Month and weekday names are English, clocks are 24 h `HH:MM`, dates are not localised; the phone feeds are formatted in `localtime.rs`. Numbers are ASCII digits everywhere (correct for dial strings). |
| Right-to-left | **Gap.** egui 0.31 does no bidi shaping or mirrored layout; Arabic / Hebrew text is drawn in logical order and layouts are left-to-right. Android views are RTL-aware (`layoutDirection` is respected in the dot views). |
| Hyprland / tiling | Calls and Share float via `docs/omarchy.md` rules; the others tile and survive forced sizes from 200x260 to 2400x1400 without panics (5 apps checked). |

### Screen reader (AccessKit) on desktop

egui ships AccessKit support behind the `accesskit` feature of `eframe` (-> `egui-winit` -> `accesskit_winit`). The app
currently builds eframe with `default-features = false`, so it is **off**. Turning it on is a one-word change in
`crates/bridge-apps/Cargo.toml` (`features = ["accesskit", ...]`), but it adds `accesskit_winit` and its
AT-SPI / UIA / NSAccessibility dependency tree, which are not in the offline vendor cache of this build environment and
must go through `docs/supply-chain.md` / `cargo deny` review. I did not add them. What is already in place:

- every button, tab, row, calendar cell and chip has `widget_info` with a name and role
  (Button, SelectableLabel, Checkbox, ProgressIndicator, Label);
- dot-matrix text (`nothing::dot_text`) is labelled with the plain string, not dots;
- the incoming sheet is named "Incoming call from NAME" and holds the keyboard focus.

Until the feature is enabled **no desktop element is exposed to Orca / NVDA / VoiceOver**; the desktop apps are
keyboard-only accessible, not screen-reader accessible. This is the largest known gap.

### Android

| Check | Result |
| --- | --- |
| Dot-matrix text reads as words | Yes. `DotText` sets `contentDescription` to the drawn string (never dots or letters); decorative dot words are hidden. Padded numerals ("07") are now spoken as numbers ("7") via `A11yLabels.spoken`. |
| Tile grouping | Fixed. The Home status tile was 3 to 4 TalkBack stops (word, title, subtitle). It is now one stop: "Computer link: Online. Connected to X. details." with children hidden; the transfer label and progress stay separate (the progress has `RangeInfo` percent). Feature tiles read "Name, state" and now add "Needs attention" when the red dot shows, so state is not only a red mark. |
| Live status announcements | Fixed. The Home screen rebuilds on every state change, so a live region would not fire. A change of the link word (Online / Offline / ...) is announced once with `announceForAccessibility`; transfer progress is not announced per tick. |
| TalkBack focus after rebuild | Fixed. Rebuilding Home dropped accessibility focus to the top on every state change (every transfer percent). The focused node is remembered by child path and re-focused. |
| Custom views: roles / actions | `asButton` (class Button) on tiles and day cells, `isSelected` on the chosen day, `ProgressBar` + `RangeInfo` on `DotProgress`, headings on titles (API 28+), expand / collapse `stateDescription`, switch rows as one stop. |
| Switch Access order | Layout order, 48 dp minimum targets (`Tok.TARGET`), each clickable also `isFocusable`. Not tested on hardware (no device / adb in the build sandbox). |
| Large font 2.0 / display size max | `DotText` shrinks its dot pitch (floor 2.4 dp) rather than overflowing and caps its font-scale growth at 1.4; calendar cells scale to 1.6; chips stack under text at >= 1.5; text is `sp`. Reviewed by reading the layout code, **not verified on a device**. |

## Tests added

- `bridge-apps`: `theme::contrast_tests` (focus ring 3:1 on all surfaces in 4 modes, high-contrast strength, no text
  < 12 pt, Tab order follows layout order via a headless egui context), `call_ui::tests` (Enter / Esc / A / D, focused
  button owns Enter, H arms and confirms, typing is safe).
- Android: `A11yLabelsTest` (spoken text, numerals, hero sentence, tile state).

## Known gaps (not fixed)

1. No AccessKit on desktop (dependency review needed; see above).
2. Tab order starts in the connection strip; there is no skip link.
3. Locale: English month / weekday names, 24 h clock; no RTL mirroring or bidi on desktop.
4. High contrast is opt-in by environment variable, not read from the desktop.
5. The incoming sheet focus ring and a few toolbar rings can be clipped by the panel edge by 1 to 2 px.
6. Android Switch Access, font 2.0 and display-size-max were reviewed in code only; run the real-device kit
   (`docs/real-device-test-plan.md`) with TalkBack on and Settings > Font size 2.0 / Display size largest.
7. The desktop pairing-code sheet has no keyboard shortcut for Decline by design.
