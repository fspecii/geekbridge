# Accessibility

Audit of the desktop apps (`crates/bridge-apps`, eframe/egui 0.31) and the Android app (TalkBack, Switch Access,
large text). Scope: keyboard operation, screen-reader exposure, contrast, text size, motion, colour independence,
locale, window management. Date: 2026-11-02. Screenshots of focus states: `dist/screenshots/desktop/a11y-*.png`
(regenerate with `dist/screenshots/desktop/a11y-capture.sh`; Xvfb, synthetic keystrokes only).

## Checklist and results

### Desktop

| Check | Result |
| --- | --- |
| Every control reachable with Tab / Shift+Tab | Yes. All controls are egui widgets with `Sense::click()`; Tab order = creation order. Fixed: the connection strip (Details, Check again) used to be drawn first and took the first two stops. `Shell` now only reserves its bottom panel, draws the app, then draws the strip controls into the reserved rect, so Tab reaches the app first and the strip last (one frame of lag when the strip's height changes, e.g. opening Details). Test: `a11y_tests::tab_reaches_the_app_before_the_connection_strip`. |
| Enter / Space activate | Yes (egui keyboard-activates any focused clickable widget). Keypad keys keep focus after keyboard activation (fixed: they used to jump back to the number field). |
| Arrow keys in lists and calendar grid | Yes: egui moves focus spatially with the arrow keys between focusable widgets (month cells, contact rows, conversation rows, sidebar, keypad). Text fields keep their own arrow keys. |
| Escape closes sheets | Fixed. Forget-phone, Pair-with-QR and the event editor close on Escape (nothing destructive happens). Escape only leaves a focused text field first, so half-typed text is not discarded. The pairing code sheet has no Escape (Decline is a deliberate button). |
| Visible focus ring >= 3:1, both themes | Fixed. The ring was 70% translucent red (accent) and vanished on the red Primary button. It is now a 2 pt full-ink ring (`theme::focus_color`) one point outside the control, never red. Unit-tested >= 3:1 on every surface in light, dark and high contrast. Ring added to calendar cells/chips/blocks/rows, contact and conversation rows, the alphabet index, the calendar list. |
| Incoming-call popup without a mouse | Fixed. On ring, focus moves onto the popup sheet and Tab / Shift+Tab stay inside it (sheet, Decline, Answer). **Enter = Answer, Esc = Decline** on the sheet; when Decline or Answer itself has focus, Enter activates that button (so Tab to Decline + Enter declines). `A` / `D` work when no text field has focus. |
| Hang up is safe by default | Fixed. `H` used to hang up on one keypress. Now the first `H` arms ("Hang up now", hint text); a second `H` or Enter confirms, Esc cancels, it disarms after 5 s. The Hang up button itself stays one deliberate click/activation. Typing in a field never hangs up. Tests: `call_ui::tests::*`. |
| Screen reader exposure | **Enabled** (AT-SPI verified on Linux; see below). Every control reports `widget_info`, text fields, scroll bars and the window are named, and the call sheet / status line / transfer progress are live regions. |
| Text scaling, minimum font >= 12 pt | Fixed. Smallest sizes were 8 to 11.5 pt (keypad letters, overline labels, calendar chips, message previews, technical details); all are now >= 12. A unit test scans the UI sources for any `FontId::new(<12` / `.size(<12`. Ctrl +/- / Ctrl 0 zoom the whole UI (egui default). Dot-matrix pitch follows the Body size. |
| Reduced motion / no animation loops | Yes. Nothing animates continuously; repaints are event-driven (`nothing.rs`) and the only timers are bounded flashes (copy confirmation, 60 ms during a held 0 key). egui's state fades are now off (`animation_time = 0`). |
| High-contrast mode | Added. `GEEKBRIDGE_HIGH_CONTRAST=1` (or a `GTK_THEME` containing `HighContrast`) turns secondary text to full ink and edges / unlit dots to a strong grey (>= 7:1 text, >= 3:1 edges, tested). Screenshot: `a11y-call-popup-high-contrast.png`. It does not follow a desktop-wide setting automatically (no portal query). |
| Colour is never the only signal | Yes. Status dots pair with text ("Helper not running", "Paired"/"Found, not paired") and shape (hollow vs filled ring); the red "today" is a larger filled disc plus "today" in the cell label; blocked actions give a written reason; Primary red buttons carry words and a key hint (`Decline  Esc`). The red accent is never the only difference. |
| Locale / numbers / dates | Partly fixed. `localtime::Locale` reads `LC_ALL` / `LC_TIME` / `LANG` once: month and weekday names for en, de, fr, es, pt, it (table in `localtime.rs`, English for anything else) and a 12 h clock ("2:05 PM", hour grid "2 PM") for `*_US`, `_CA`, `_AU`, `_NZ`, `_IN`, `_PH`, `_MX`, `_CO`, `_EG`, `_SA`, `_PK`, `_BD` and `hi`/`ar`/`ko`; `GEEKBRIDGE_CLOCK=12|24` overrides. The button and label **text** of the apps stays English (not translated), editable time fields stay `HH:MM`, date order stays "day month". Windows and macOS GUI processes carry no `LANG`, so they show English / 24 h until a platform query is added. Numbers are ASCII digits everywhere (correct for dial strings). Tests: `localtime::tests::*` (the test build always uses English / 24 h). |
| Right-to-left | **Gap.** egui 0.31 does no bidi shaping or mirrored layout; Arabic / Hebrew text is drawn in logical order and layouts are left-to-right. Android views are RTL-aware (`layoutDirection` is respected in the dot views). |
| Hyprland / tiling | Calls and Share float via `docs/omarchy.md` rules; the others tile and survive forced sizes from 200x260 to 2400x1400 without panics (5 apps checked). |

### Screen reader (AccessKit) on desktop

**Enabled** (2026-11-02). `crates/bridge-apps` has a cargo feature `accesskit` (on by default) that turns on
`eframe/accesskit` -> `egui-winit` -> `accesskit_winit` 0.23, which brings AT-SPI on Linux (`accesskit_unix` over
`zbus`), UI Automation on Windows (`accesskit_windows`) and NSAccessibility on macOS (`accesskit_macos`). egui builds
the tree only while an assistive technology is attached, so an unattended window pays only for the extra threads.
Build without it with `cargo build -p bridge-apps --no-default-features` (every call site is `cfg`-gated; CI-checked
with clippy). The service binary does not depend on `bridge-apps` and is unaffected.

Cost, measured on this machine (release, `opt-level = "s"`, fat LTO; idle messages window; median of two runs on the
real X display, Xvfb in brackets):

| | without | with `accesskit` | change |
| --- | --- | --- | --- |
| Binary size (stripped), messages | 6 164 352 B | 8 049 424 B | +1.9 MB (+31%) for every GUI binary |
| Idle RSS, messages | 121-122 MB (111 MB) | 123-124 MB (116 MB) | +1.5 to 2.5 MB (+2%) |
| Threads at idle | 6 | 9 | +3 (AT-SPI bus connection) |
| Idle wakeups (`scripts/perf-check.sh --ui` method) | ~1/s | ~3/s | budget is 12/s |
| New crates | | 33 (`accesskit*`, `atspi*`, a second `zbus`/`zvariant` 4.x line, `rand` 0.8, `nix`, ...) | all MIT or Apache-2.0, already allowed in `deny.toml` |

The 90 MB idle-RSS budget of `scripts/perf-check.sh --ui` is **exceeded on this machine with or without the feature**
(105-124 MB here; the 65.7 MB in `docs/performance.md` was taken on a different GL stack). It is dominated by Mesa and
the X/Wayland libraries, not by AccessKit, so the feature stays on. If a build must stay small, turn it off with
`--no-default-features`; `scripts/build-desktop.sh` uses the defaults. `cargo deny` was not installed in this
environment: run `cargo deny check` once (the older `zvariant`/`zbus_names` 3.x/4.x lines may need entries beside the
5.x ones already ignored for RUSTSEC-2026-0370..0372).

If the session has no AT-SPI registry but the accessibility bus is advertised as enabled, `accesskit_unix` 0.13 panics
inside its own background thread (`context.rs:54`); the release profile unwinds, so the window keeps working and only
screen-reader support is lost. Nothing else in the app waits on that thread.

#### Verified with a real AT-SPI client

`scripts/a11y-dump.sh calls messages ...` starts a private session bus, `at-spi-bus-launcher` + `at-spi2-registryd`
and Xvfb, launches the release binary and walks its tree with `scripts/atspi_dump.py` (PyGObject `Atspi`, run with
`python3 -I`). Nothing touches the real session. Result for the idle, service-not-running state (roles are AT-SPI
role names; `[focusable]` = Tab stop; disabled buttons are not focusable):

**Home (`geekbridge`)**

```text
application [disabled]
  frame "GeekBridge" [focusable]
    label "GEEKBRIDGE"
    label "Looking for the service"
    label "Checking the background service"
    progress bar "0 of 12"
    button "Home" [focusable]
    button "Pair a phone" [focusable]
    button "Settings" [focusable]
    label "OPEN"
    button "Calls" [focusable]
    button "Messages" [focusable]
    button "Contacts" [focusable]
    button "Calendar" [focusable]
    button "Clipboard" [focusable]
    button "Share files" [focusable]
    label "Background service not running"
    label "HOME"
    label "2 November"
    button "Phone link" [focusable]
    label "loading"
    label "Looking for the service"
    label "Checking the background service"
    button "Calls" [focusable]
    label "not available"
    label "Calls"
    label "Needs a linked phone"
    button "Messages" [focusable]
    label "not available"
    label "Messages"
    label "Needs a linked phone"
    button "Clipboard" [focusable]
    label "CLIP"
    label "Clipboard"
    label "Nothing received yet"
    button "Send files" [focusable]
    label "SEND"
    label "Send files"
    label "Drop files anywhere here"
    button "Calendar" [focusable]
    label "02"
    label "November"
    label "Calendar"
    label "Needs a linked phone"
    ...
```

**Calls**

```text
application [disabled]
  frame "GeekBridge Calls" [focusable]
    toggle button "Keypad" [focusable]
    toggle button "Recents" [focusable]
    entry "Phone number" [focusable,editable]
    button "Key 1" [focusable]
    button "Key 2" [focusable]
    button "Key 3" [focusable]
    button "Key 4" [focusable]
    button "Key 5" [focusable]
    button "Key 6" [focusable]
    button "Key 7" [focusable]
    button "Key 8" [focusable]
    button "Key 9" [focusable]
    button "Key *" [focusable]
    button "Key 0" [focusable]
    button "Key #" [focusable]
    button "Copy number"
    button "Call with phone"
    button "Delete last digit"
    label "Can't call yet: the GeekBridge helper isn't running."
    label "Helper not running"
    button "Details" [focusable]
    button "Check again" [focusable]
```

**Messages**

```text
application [disabled]
  frame "GeekBridge Messages" [focusable]
    button "New Message" [focusable]
    button "Show or hide the sidebar" [focusable]
    label "LIBRARY"
    button "Conversations" [focusable]
    button "New Message" [focusable]
    label "DEVICES"
    label "No devices"
    label "No conversations"
    label "Start the GeekBridge service to set up phone linking."
    button "New Message" [focusable]
    label "Messages come from the phone's SMS store, only while you allow them and the phone app has SMS access. No RCS or picture messages."
    label "Helper not running"
    button "Details" [focusable]
    button "Check again" [focusable]
```

**Calendar (`geekbridge-agenda`)**

```text
application [disabled]
  frame "GeekBridge Calendar" [focusable]
    toggle button "Day" [focusable]
    toggle button "Week" [focusable]
    toggle button "Month" [focusable]
    toggle button "Agenda" [focusable]
    button "Previous" [focusable]
    button "Today" [focusable]
    button "Next" [focusable]
    label "November 2026"
    button "New event"
    label "Start the GeekBridge service to set up phone linking."
    label "02"
    label "November 2026"
    button "26 October" [focusable]
    button "27 October" [focusable]
    ... (38 more day cells, one per day, named like "2 November, today")
    button "2 November, today" [focusable]
    label "CALENDARS"
    label "Start the GeekBridge service to set up phone linking."
    label "THIS COMPUTER MAY"
    label "Helper not running"
    button "Details" [focusable]
    button "Check again" [focusable]
```

**Contacts**

```text
application [disabled]
  frame "GeekBridge Contacts" [focusable]
    label "Contacts"
    label "Your phone's phonebook"
    label "Start the GeekBridge service to set up phone linking."
    label "All Contacts"
    label "0"
    entry "Search contacts" [focusable,editable]
    label "No contacts yet"
    label "Start the GeekBridge service to set up phone linking."
    label "No Contact Selected"
    label "Helper not running"
    button "Details" [focusable]
    button "Check again" [focusable]
```

**Clipboard**

```text
application [disabled]
  frame "GeekBridge Clipboard" [focusable]
    label "THIS COMPUTER"
    entry "Text to send to the phone" [focusable,editable]
    label "0 characters  ·  Text stays in this window"
    button "Copy to this computer"
    button "Clear"
    label "This scratchpad is only for typing text to copy. It is never sent anywhere."
    label "PHONE SYNC"
    label "Start the GeekBridge service to set up phone linking."
    label "Helper not running"
    button "Details" [focusable]
    button "Check again" [focusable]
```

What this shows and what was fixed to get here:

- every window has a name (the title; egui leaves the root unnamed, `a11y::window_title` sets it);
- dot-matrix text exposes the real words; placeholder runs are spoken as meaning ("---" -> "not available",
  "..." -> "loading") through `nothing::spoken`; the Home progress dots read "0 of 12";
- icon-only buttons are named (`Check again`, `Delete last digit`, `Key 7`, `Previous` / `Next`);
- every text field has a name (egui does not use the hint text as one): `a11y::named` wraps the 22 `TextEdit`s;
- egui's scroll bars are Tab stops and carried no name: `a11y::ScrollBars::bars` names them "Vertical scroll bar";
- the invisible full-screen backdrop behind the incoming-call sheet was an unnamed, focusable zero-size area; it is now
  non-interactive;
- **live regions**: the incoming-call sheet is an `AlertDialog` with `live = assertive` and the name
  "Incoming call from NAME"; the connection status line at the bottom of Calls / Messages / Calendar / Contacts /
  Clipboard is `live = polite`; the Share window's transfer progress bars are named "FILE state" with a percent value
  and `live = polite`. egui exposes no setter for these, so they go through `Context::accesskit_node_builder`
  (`crates/bridge-apps/src/a11y.rs`).

Not covered by the tree dump (needs a phone or fixtures): rows inside Messages threads, Contacts people, calendar
events and the Share transfer list. Their widgets use the same `widget_info` labelled helpers, and the headless test
below walks the same code, but a populated window has not been dumped through AT-SPI. Windows UIA and macOS
NSAccessibility are only type-checked (`scripts/xcheck.sh`), not run. **Orca itself has not been used.**

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

- `a11y_tests` (needs the `accesskit` feature; headless, no window): walks the AccessKit `TreeUpdate` of Calls, Messages,
  Calendar, Contacts and Clipboard (checking, paired and service-down models, details closed and open) and of the Home
  window, and fails if any control (button, toggle, text field, link, anything clickable that is not plain text) has
  no label; asserts the window is named, the status line is a polite live region, dot-matrix text is exposed as words,
  and Tab visits the app before the connection strip. `call_ui::tests::incoming_call_sheet_is_an_announced_alert`
  checks the ringing sheet is an assertive `AlertDialog` with named Answer / Decline. `cargo test -p bridge-apps --lib
  dump_outlines -- --ignored --nocapture` prints the same trees as text.
- `localtime::tests`: locale parsing and precedence, localised names, 12 / 24 h clocks, `nothing::tests::placeholder_dots_are_spoken_as_meaning`.
- `bridge-apps`: `theme::contrast_tests` (focus ring 3:1 on all surfaces in 4 modes, high-contrast strength, no text
  < 12 pt, Tab order follows layout order via a headless egui context), `call_ui::tests` (Enter / Esc / A / D, focused
  button owns Enter, H arms and confirms, typing is safe).
- Android: `A11yLabelsTest` (spoken text, numerals, hero sentence, tile state).

## Known gaps (not fixed)

1. Screen reader: verified through AT-SPI only on an idle, unpaired window; not tried with Orca, NVDA or VoiceOver, and
   populated lists (threads, events, contacts, transfers) have not been dumped. `cargo deny check` still to run for
   the 33 new crates (licences checked by hand).
2. No skip link (Tab order itself is fixed: the strip comes last).
3. Locale: names and clocks follow the Unix locale variables for 6 languages; UI strings are not translated; Windows /
   macOS do not read the system locale; no RTL mirroring or bidi on desktop.
4. High contrast is opt-in by environment variable, not read from the desktop.
5. The incoming sheet focus ring and a few toolbar rings can be clipped by the panel edge by 1 to 2 px.
6. Android Switch Access, font 2.0 and display-size-max were reviewed in code only; run the real-device kit
   (`docs/real-device-test-plan.md`) with TalkBack on and Settings > Font size 2.0 / Display size largest.
7. The desktop pairing-code sheet has no keyboard shortcut for Decline by design.
