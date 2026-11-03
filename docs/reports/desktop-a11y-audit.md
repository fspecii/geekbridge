# Desktop accessibility and keyboard audit

Scope: the egui apps in `crates/bridge-apps` (Home, Calls, Messages, Contacts, Calendar, Clipboard, Share, the pair UI and the
incoming-call popup), AccessKit enabled. Method: headless AccessKit trees and egui shapes from the existing harness
(`a11y_tests.rs`, `capture`), numbers computed from the real theme colours as `ThemeContrastTest` does on Android. No window
system, live service, real config or Hyprland was touched. An earlier pass (`docs/accessibility.md`, 2026-11-02) already
covered naming, Esc, Tab order and the call popup; this audit re-verified those and looked for what was left.

## Results

| Area | Result |
| --- | --- |
| Tab reaches every interactive widget | Yes (all `Sense::click`/text fields). Defect fixed: every scroll area had an extra **unnamed** Tab stop (egui's drag-to-scroll surface). All scroll areas now use `drag_to_scroll(false)`; wheel, touch-pad, scroll bar and arrow keys still scroll. Touch-screen drag scrolling is gone on the desktop apps. |
| Visible focus ring | Test `every_tab_stop_draws_a_focus_indicator_in_both_themes` Tabs through five apps in light and dark. Defects fixed: stock buttons (Check phone linking, Pair, update buttons) showed focus only as a fill change; the frameless fields (Calls number display, Clipboard text, Messages reply / recipient / message) and the Technical details header showed none. Stock buttons now take a 2 pt ink outline in their active state (`widgets.active.bg_stroke`), the others call `theme::ring_when_focused` / `focus_ring`. |
| Known remainder | egui's own scroll bar handle is a Tab stop (named "Vertical scroll bar") and draws no ring; it cannot be restyled or removed from Tab order without forking egui. It does nothing for the keyboard, which scrolls with arrows / PageUp / PageDown. Test exempts it explicitly. |
| Enter / Space | egui activates any focused clickable on both keys; covered by the existing key tests (`calls_tests`, call popup tests). |
| Esc closes dialogs | Forget phone, Pair with QR, event editor, dial confirmation and the call popup (Esc declines) close on Esc; the pairing-code sheet has no Esc by design (Decline is explicit). Existing tests in `connection_tests`, `pair_ui_tests`, `calendar_tests`, `call_ui`. No change. |
| Names and roles | `every_control_in_the_shell_apps_has_a_name` and the Home tests: no unnamed button, toggle, field. Icon-only buttons (Previous, Next, Copy number, Delete last digit, Show or hide the sidebar, round call buttons) carry labels. The remaining unnamed Tab stops were the scroll surfaces above. |
| Decorative dots | New test `dot_matrix_and_status_dots_add_no_operable_nodes`: dot-matrix lettering is one text node holding the words; status dots add no node, no focus stop. |
| Incoming call announced | Existing `incoming_call_sheet_is_an_announced_alert`: `AlertDialog` role, assertive live region, named Answer / Decline. Confirmed. |
| Contrast, text | `text`, `text2`, `text3`, accent, ok / warn / err are >= 4.5:1 on every surface in light and dark (existing test), >= 7:1 in high contrast. **Defect fixed:** stock placeholders (Calendar editor, Share, Contacts search, pairing fields) used egui's default half-strength ink, **3.46:1** in light. They now use `theme::hint` (`text3`, >= 4.5:1). Test `placeholders_never_use_egui_default_weak_colour` fails if a plain-string `hint_text` comes back. |
| Contrast, non-text | Text-field outline >= 3:1 on content, sidebar and field fill, both themes; focus ring >= 3:1 (existing); link colour >= 4.5:1; text on the selection fill >= 4.5:1 (new `derived_text_and_control_outlines_meet_aa_in_both_themes`). |
| Target size | WCAG 2.2 SC 2.5.8 (24 px). A point is a CSS px, so the size in points must not shrink with scale: `operable_targets_are_at_least_24_points_at_100_and_150_percent` renders Home and the five shell apps (details open and closed) at 1.0 and 1.5 and checks every operable node; `target_size_in_points_does_not_shrink_with_scale` checks the layout is scale independent. **Defect fixed:** the segmented controls (Keypad / Recents, Day / Week / Month / Agenda) were 22 pt tall (22 px at 100%); now 26 pt (39 px at 150%). Smallest remaining targets: calendar day cells 30x26, Check again 30x28, Previous / Next 30x28. None reaches the 44 pt AAA size; that would change the dense desktop layout and was left alone. Scroll bars are exempt (2 pt strip when idle, other ways to scroll). |

## Tests added

All in `crates/bridge-apps`: `a11y_tests.rs` (`capture_scaled`, target size at 100 / 150 %, scale independence, focus
indicator per Tab stop in both themes, decorative dots) and `theme.rs` `contrast_tests` (derived colours, placeholder
source scan, disabled-label legibility).

## Not done / limits

- Right-to-left layout and a desktop-wide high-contrast setting remain open as in `docs/accessibility.md`.
- No live AT-SPI run this time (`scripts/a11y-dump.sh` was not re-run); the AccessKit tree is checked directly.
- Golden images: 44 desktop goldens changed (taller segmented controls, darker placeholders, ring on Technical details); regenerated with `scripts/golden-desktop.sh --update` in a separate worktree, reviewed from the diff images, `--check` then 0 failing of 76.
