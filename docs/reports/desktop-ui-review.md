# Desktop UI review (Nothing x Continuity reskin)

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Scope: `crates/bridge-apps`, every screen in `dist/screenshots/desktop/nothing-*.png` (light and
dark), compared with `dist/screenshots/android/nothing-*.png` and the five reference images
(dot-matrix calendar/agenda, widgets, Nothing OS tile sheet, lock-screen/settings, dark launcher).
Fixes are small and targeted; screenshots re-captured with `scripts/screenshots-desktop-nothing.sh`.

## Capture pipeline (found first, it invalidated half the set)

| | Before | After |
|---|---|---|
| Light set | `pair`, `calls-keypad`, `messages` were all-black 1280x860 frames; `settings`, `contacts`, `calendar-*`, `clipboard`, `share` showed "Start the GeekBridge service" / "service couldn't be authenticated"; `calendar-agenda-light` contained a second window (stale Share window in the crop) | Re-shot with a private display and ports; all 26 frames show the connected fixture, correct crop, date is today (2 November) |
| Cause | Script hard-codes display `:94` and ports 17710-12; a parallel agent run `pkill Xvfb :94` mid-run (log: `unable to open X server ':94'`) | `DISP`, `CP`, `AP`, `DP`, `RUN` are now environment-overridable (defaults unchanged) |

Use e.g. `DISP=:194 CP=27711 AP=27710 DP=27712 RUN=$PWD/target/shots-review scripts/screenshots-desktop-nothing.sh`.

## Defects fixed

| # | Defect | Before | After |
|---|---|---|---|
| 1 | Incoming-call sheet spans the whole window: wrapped note label stretched the `Window` to the screen width, rounded corners cut off at the edges (380 px wide Calls window) | `img/review/before-call-incoming-dark.png` | Sheet width = clamp(window - 96, 220, 340); corners and margins intact (`img/review/after-call-incoming-dark.png`). `call_ui.rs` |
| 2 | Text fields had no visible boundary: fill vs sidebar is 1.1:1 dark, white on `#FBFAF7` light; the Contacts search, Share link field, shortcut command box looked like bare text | `img/review/before-contacts-search-dark.png` | 1 px outline, 3.4:1 dark (`#666662`) and 3.3:1 light (`#8A8A84`) (WCAG 1.4.11), hover uses `text3`. `theme.rs` |
| 3 | After #2 the 12 pt widget radius made 22 pt fields pills with text touching the curve | (intermediate) | Stock-widget radius 8; Contacts and Share fields get 8x4 text margin. `theme.rs`, `contacts.rs`, `share_app.rs` (`img/review/after-contacts-search-dark.png`) |
| 4 | Light-theme `err` red `#D71921` was 4.18:1 on control fills (buttons), `ok` green 4.31:1 | below 4.5:1 | `err` = `#B8121A` (5.4:1), `ok` = `#1A6E3E` (5.1:1). Contrast test now also covers ok/warn/err on content, tile and control. `theme.rs` |
| 5 | Home at the 560x480 minimum: sidebar status line ("Background service running") painted over the "Share files" row | `img/review/before-home-min-overlap.png` | Min height 520 so the sidebar always fits. `home_app.rs` |
| 6 | Header title and date overlap when the content column is narrow (large text scale, HiDPI with small window): "HOME" dot-matrix drawn under "2 November" | seen at 1.5x below min | Date is dropped when < 130 pt remain. `home_app.rs` |
| 7 | Messages status LED was red when connected and also red for errors, so red carried no meaning (Android keeps red for attention only) | red LED + "PHONE CONNECTED" | Connected = plain lit dot, warning/error = red, otherwise grey. `messages.rs` |
| 8 | Only Home/Pair/Settings showed shortcuts; app launchers had none and Share had no key at all | no hints | Sidebar shows Ctrl+4..9 (Cmd on macOS); Ctrl+9 opens Share. `home_app.rs` |

## Contrast (computed, WCAG 2.x relative luminance)

Dark (red accent text `#FF5A5A`): 6.9 on content, 6.5 on sidebar, 6.0 on tile, 5.5 on control,
3.8 only on the *pressed* control fill. Light (accent text `#B8121A`): 6.0 content, 6.4 sidebar,
6.7 tile, 5.4 control, 4.2 pressed control. Brand red `#D71921` is used only for fills and dots:
3.55:1 on dark tile and 5.2:1 on white (graphics need 3:1; white label on it is 5.18:1). Body
`text2`/`text3` are >= 5.2:1 in both themes except pressed control (4.0-4.6, transient).
Disabled controls are intentionally low-contrast.

## Checked, fine

- Palette follows the system: `--theme system` maps to egui `ThemePreference::System`; fallback is light.
- Focus: all custom controls (tile, pill, round, segmented, sidebar row, toolbar button) draw a red focus ring and are `Sense::click`, so Tab/Shift+Tab/Space/Enter work in DOM order (sidebar, then content).
- Enter/Escape: only the incoming-call sheet binds them (Enter = answer, Esc = decline), read before text fields so typing a number cannot swallow them; letter shortcuts are ignored while typing. Elsewhere Escape is a no-op by design (no modal to dismiss).
- Reachability from Home: every app is one click (sidebar row or tile); Pair, Settings are one click; in-app features (New event, Day/Week/Agenda, Compose, Recents) are two clicks at most. Nothing needs more.
- Min sizes: Calls 320x560, Messages 640x460, Calendar 760x520, Contacts 560x460, Clipboard 360x440, Home now 560x520. Tiles collapse to one column and scroll; long names are `truncate()`d in tiles and the device card, `…` in sidebar rows.
- Repaint: Home repaints every 1 s (x pace backoff when unfocused/minimised), Share 500 ms, Calendar 20 s, Contacts 30 s; idle wakeups measured 1/s (budget 12). Service idle: 6.5 MB RSS, 1 wakeup/s.

## Open / not fixed

- **Idle RSS of `geekbridge-messages` is 103 MB (budget 90 MB)** under Xvfb (software GL). The same figure appears for an older release build (`target/agent-verify`, built before the reskin), so it is not caused by this pass; owner is the size-trim work. `scripts/perf-check.sh --ui`, all other lines OK.
- 2x HiDPI shot was not captured (the scale run was cut short); 1x and 1.5x below-min frames only. Verify 2x on the real device kit. `WINIT_X11_SCALE_FACTOR=2` reproduces it.
- Settings > Clipboard card has large vertical gaps and a repeated "UPDATES" overline inside the Updates card; Pair repeats "Phone linking" and lists the phone twice (paired + connected rows). Content, not layout bugs, so left for the owners of `update_ui.rs` / `connection.rs`.
- Home leaves ~40 % empty below the tiles at 960x680 (matches widget-grid look, not a defect); Contacts letter rail hit targets are 18x11 pt (below 24 pt).
- RTL and emoji names were not exercised with a fixture (the simulator has none); text is egui-shaped without bidi, so Arabic/Hebrew names will render in logical order. Needs a fixture before claiming support.
- Comparison with Android: tiles, dot-matrix titles/numerals, red-for-attention and the mini dot calendar match; Android uses the dotted font for body time/labels in the agenda where desktop uses the system font for legibility.

## Verification

`cargo fmt -p bridge-apps`, `cargo clippy -p bridge-apps --all-targets -- -D warnings` and `cargo test -p bridge-apps` (79 tests) all pass.
