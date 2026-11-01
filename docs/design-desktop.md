# Desktop app design

Scope: the desktop apps in `crates/bridge-apps` (Home, Calls, Messages, Contacts, Calendar, Clipboard, Share; the Calendar executable is still `geekbridge-agenda`). Each is its own
executable, window and app ID (`dev.geekbridge.<App>`); there is no app switcher and no unified shell. Windows use
the operating system's own title bar and window controls (`with_decorations(true)`).

Status of this document (updated 2026-11-02): the sections up to "Nothing look" record the first UI pass, written
before the service backends existed; where they say a control is always blocked or that no phone data exists, that is
the **no-phone state**, not the current behaviour. With a paired phone and the per-feature opt-ins, the apps now show
and act on live data (SMS, contacts, calendar, clipboard, file sharing, call answering). The current look is
"Nothing look" below; the screenshots are in `docs/img/` and `dist/screenshots/desktop/`. Verified on Linux (X11 under
Xvfb and Hyprland) with a scripted phone and an emulator; Windows and macOS builds are compiled in CI definitions only
and have not been run on real hardware.

## What the UI does and does not claim

- Without a paired phone and its opt-in, every phone action (call, send SMS, send clipboard, read contacts/events) is **blocked**. The control stays visible,
  is disabled, and a one-line reason is shown next to it ("Can't send yet: no phone found."). The reason comes from
  `DiscoveryModel::blocked_line`, which has no code path that returns "allowed"; discovery is never authorization.
- No sample contacts, messages, calls, events or devices exist in the live app (the capture fixtures are test-only). Empty lists say what is true
  ("Nothing has been read from a phone yet"). Devices in the Messages sidebar and the connection details are only
  what the local discovery service actually reported, labelled "Found nearby, not paired".
- The connection strip only says "connected" for a paired phone with a live authenticated link; before that its states
  are: looking, helper not running, can't search, no phone found, phone(s) nearby · not paired.
- Real, local-only functions: typing/validating a phone number, copying it; writing and copying a message draft and a
  draft character count; typing/pasting clipboard text and copying it to this computer; navigating days and the
  "now" line in Agenda (computer clock and time zone). None of it is persisted or sent anywhere.

## Shared structure (`shell.rs`, `theme.rs`)

```
┌──────────────────────────────────────────────┐  native title bar (OS)
│ toolbar (per app)                            │  theme::toolbar
├────────────┬─────────────────────────────────┤
│ sidebar    │ content                         │  app-owned panels
│ (optional) │                                 │
├────────────┴─────────────────────────────────┤
│ ● No phone found            ⟳  ⌃ Details     │  shell: connection strip
└──────────────────────────────────────────────┘
```

- `AppView::show(ctx, &Env)` replaces the old `ui()` hook: an app adds its own panels (toolbar, side panel, central
  panel). The shell only claims the bottom strip. `Env` carries the app kind, API port, discovery model and palette.
- **Connection strip**: one 30 px row (dot, short status, refresh, Details). Details expands a drawer above it:
  plain-language status and next step, what already works without a phone, nearby phones, and a collapsed
  "Technical details" section (helper address, discovery error text, device id/address, whether the phone announced
  the app's feature). Product text avoids KDE/schema/JNI terms; only the technical section may name internals. A
  unit test enforces the product strings.
- **Palettes** (`theme::palette`): macOS-style light/dark for Calls, Agenda, Clipboard (system blue accent, grey
  controls); a separate ink/warm-white/red palette for Messages. Appearance follows the system theme, falling back to
  light; `--theme light|dark|system` overrides it.
- **Fonts**: nothing is bundled or downloaded. `install_fonts` looks for installed UI fonts (Segoe UI, San Francisco
  system font, Noto Sans, Liberation Sans, DejaVu Sans) and falls back to egui's built-in font. Semibold text uses
  the family's bold file when one exists and the regular face otherwise (macOS San Francisco ships as a single
  variable file, so headings there are regular weight until a better approach is tested on a Mac).
- **Icons and graphics** are painted in code (`icons.rs`, `dotmatrix.rs`): original line icons and a 5x7 dot-matrix
  glyph set drawn with circles. No images, icon fonts or trademarked assets.
- **Controls** drawn in `theme.rs` (segmented control, pill/round/toolbar buttons, sidebar rows) register
  accessibility labels via `widget_info` and show a focus ring for keyboard navigation.
- **Responsive**: `identity::min_size` gives each app a minimum window (Calls 320x560, Messages 640x460, Agenda
  560x460, Clipboard 360x440); layouts were checked at those sizes (`*-min-size.png`). The keypad scales its keys to
  the available height; Messages' status display falls back to shorter text; Clipboard scrolls if squeezed further.

## Messages: iTunes layout x Nothing OS styling

iTunes (10-12 era) contributes structure, not branding:

| iTunes idea | Messages |
| --- | --- |
| Unified toolbar with a recessed "LCD" display in the middle | Toolbar: New Message (left), status display (centre), sidebar toggle (right) |
| Source list sidebar: LIBRARY and DEVICES groups | Library: Conversations, New Message (red marker when a draft exists). Devices: phones the discovery service saw |
| Column-header list view | Conversations list with Contact / Last message / Time headers (labels, not sort buttons) |
| Device summary page | Selecting a nearby phone shows a summary: type, address, whether it announced messaging, and why messages are unavailable |
| Bottom status bar | The shared connection strip |

No transport, volume, album art or other music controls exist.

Nothing OS contributes the look: monochrome ink on warm white (or white on black in dark mode), thin hairlines, small
uppercase labels, a dot-matrix status display and counters, and **red only as a focal accent** (status LED, draft
marker, send button, a nearly-full draft meter). Selected sidebar rows are solid ink pills.

- The status display's text is derived from the real discovery state (`SEARCHING`, `HELPER NOT RUNNING`, `NO PHONE
  FOUND`, `1 PHONE NEARBY`) with shorter fallbacks for narrow windows.
- Compose view: underlined "To" field (digits, `*`, `#`, leading `+` only), a full-height borderless body, a 16-dot
  meter plus dot-matrix `used/4000` count, Copy draft, Discard, and a round Send button that stays disabled with its
  reason visible. Draft stays in memory in this window only.
- Dot-matrix is used in three places only (status display, empty-list numeral, draft counter) to keep it restrained.

## Calls

macOS keypad: Keypad / Recents segmented control, large auto-shrinking number field (a real text field, so typing and
pasting work), round keys with letters, long-press 0 for `+`, copy and delete round buttons, and a round green Call
button that stays disabled with its reason ("Can't call yet: no phone found."). Recents is an honest empty state; there
is no call history without a phone.

## Contacts and Calendar (formerly Agenda)

People / Day segmented control. People: macOS Contacts-style two panes (list with count; "No Person Selected"
detail). Day: large date header, week strip (Monday start; tap a day), previous/next/Today, 24-hour timeline with a
red now-line and time chip when viewing today. Date and time come from the system clock; the zone is read on
Linux/macOS/Windows (`localtime.rs`; the Windows path uses the system time-zone API and falls back to UTC, saying so, if
that fails). This People/Day layout is the first pass; Contacts and Calendar are now separate apps with Day / Week /
Month / Agenda views (see "Nothing look"). There are no events or contacts until a phone provides them.

## Clipboard

A small utility, not a dashboard: one text area (with character count), Copy to this computer / Clear, then a Phone
group with Send to phone (blocked with its reason) and a one-line "From phone: Nothing received yet". It never reads
the system clipboard on its own; text arrives only by typing or the paste shortcut.

## Nothing look (2026-11-01 restyle)

All desktop windows now share one look that matches the Android app: **true black** (dark) or **warm off-white**
(light), grey ramps, and a single red. The theme follows the OS setting (`--theme light|dark|system` still overrides).

- **Tokens** (`theme::palette`, `nothing.rs`): one palette for every window. `accent` is the text-safe red (#B8121A on
  light, #FF5A5A on dark); `accent_fill` is the brand #D71921 for dots, discs and pill fills (white on it is 5.2:1).
  Radii: tile 22, card 16, row 12, controls pill-shaped. 4-pt spacing scale. A unit test
  (`theme::contrast_tests`) asserts text, secondary text and the red accent are >= 4.5:1 on every surface in both themes.
- **Fonts**: system UI font only. Dot-matrix numerals, titles and status words use the code-drawn 5x7 renderer
  (`dotmatrix.rs`), the same idea as the Android app; no proprietary fonts or logos. Dot size scales with text size.
- **Components** (`nothing.rs`, `dot_calendar.rs`): `tile` (rounded-square, hover/press shade, focus ring, screen-reader
  label), `card`, `dot_text`, `dot_progress`, `status_dot`, `dot_month` (month grid of dots with a red disc on today).
- **Home** (`home_app.rs`): translucent-feel sidebar (device card with a dotted link meter, Home / Pair a phone /
  Settings, and launchers for each app) plus a tile grid: phone link, calls (ringing/in call), messages (unread), clipboard
  (last paste from phone), send files (drop target), calendar (next events). Tiles only show what the service reported;
  battery is not shown because the desktop is not told the phone's battery level. Phone-data feeds (calls, agenda,
  messages) start only once a phone is paired, so a fresh install polls nothing extra.
- **Sheets, not dialogs**: the incoming-call popup is a rounded sheet over a dimmed backdrop; the in-call panel and the
  update banner are rounded cards. Decisions are pills (red Decline/Hang up, inverse Answer).
- **Keyboard**: Ctrl/Cmd+1..3 switch Home / Pair / Settings (Ctrl/Cmd+, opens Settings), Ctrl/Cmd+4..8 open Calls,
  Messages, Contacts, Calendar, Clipboard. Call keys (Enter/Esc/A/D/H/M) are unchanged. Tab reaches every tile, row and
  dot-calendar day with a visible focus ring.
- **Calendar**: sidebar gets the dot-matrix month card (day numeral, red month, bright dots on days with events, red
  today); the main month grid uses dot weekday initials and hairline rows.
- **Performance**: nothing animates; windows repaint on change or the existing slow timers. Idle RSS of the UI is
  unchanged from before the restyle (about 103 MB under Xvfb/software GL against a 90 MB budget that was already exceeded
  before this change; the service and ctl binary-size budgets are over because of other in-flight work).
- **Not in this crate**: there is no tray icon or menu popover in `bridge-apps`; the Hyprland/waybar integration lives
  in `bridge-service` and was not restyled here. The update banner could not be triggered in the offline capture, so it
  has no screenshot.
- **Screenshots**: `scripts/screenshots-desktop-nothing.sh [light|dark]` (private Xvfb, scripted phone, optional
  `ONLY=regex`) writes `dist/screenshots/desktop/nothing-*.png`. `GEEKBRIDGE_CALLS_FIXTURE=ringing|active` shows a scripted
  call in the Calls window for capture only.

## Reference review

What was looked at, and what it changed:

- **KDE Connect's SMS app** (public documentation and UI): conversation list + conversation display + a sending area.
  Messages keeps that list / thread / composer split.
- **Combined-shell phone companions** put Messages / Calls / Contacts in one window with a device rail. That is the
  "single shell" pattern the brief rejects, so each GeekBridge app is its own window and process, with flat rows.
- The Calls dialer follows the macOS/iOS keypad conventions.

No third-party code, images or fonts were copied.

## Backend integration points

Only the UI changed; no core/service/calls crate was touched. When a backend exists:

1. Replace `DiscoveryModel::action_blocker` with a per-device, per-operation authorization result. Every blocked
   control already routes through `blocked_line`, so reasons stay visible without further UI work.
2. Fill the empty click handlers marked "Reached only once a ... backend authorizes this phone" in `calls.rs`,
   `messages.rs`, `clipboard.rs`.
3. Messages: render real conversations under the existing list header (`Messages::conversations`) and add a thread view
   beside the composer; Agenda: feed `people()` and the day timeline (`HOUR_HEIGHT` rows) with events; Calls: fill
   Recents.
4. Provide the Windows time-zone lookup in `localtime.rs` (`utc_offset_seconds`).

*Update 2026-11-02:* items 1-4 are done (live data flows through the service control API; Windows uses the system
time-zone API). Call history (Recents) remains empty by design.

## Reproducing the screenshots

`dist/screenshots/desktop/capture.sh` (uses `dist/linux` binaries, or a directory you pass) starts a private
`geekbridge-service` on alternate ports (API 17481, discovery 17416) and a disposable Xvfb display (`:97`), launches each
real window, sends plain xdotool input, and writes PNGs plus `evidence.txt` (window geometry, WM_CLASS, title and
decoration hints). Nothing is announced to the service, so all windows show the genuine "no phone found" state; the
`*-helper-off` image points the app at a closed port. Xvfb here has no window manager, so title bars are not drawn in
the PNGs; the evidence file records that each window requests decorations. The captures include typed text
(`calls-number`, `messages-compose`, `clipboard-text`), which is local draft input, not phone data.


## Parent integration update

Windows Agenda now uses the system dynamic time-zone conversion APIs rather than always falling back to UTC. Epoch-conversion boundaries have host tests; actual Windows time-zone behavior remains a native validation item. The existing explicit UTC fallback remains for API failure.

Parent validation: the updated UI passes 27 host unit tests. An isolated crate using the real unmodified `localtime.rs` was checked with `--target x86_64-pc-windows-msvc --all-targets`; it passed, including the Win32 time-zone binding code and tests. This is type checking, not Windows runtime execution. A subsequent workspace-resolved Clippy attempt was temporarily blocked by an in-progress macOS adapter dependency feature selection, not a reported UI lint failure. Full workspace checks will be rerun after platform integration settles.
