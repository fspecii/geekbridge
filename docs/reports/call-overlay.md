# Incoming-call card (call overlay)

## Problem

On the owner's Omarchy/Hyprland desktop an incoming call produced nothing on screen: the service signalled a ring only
through a `notify-send` banner (dropped or silenced by the notification plugin, which also draws no action buttons), and
the Calls window opened only when that banner was clicked.

## What was built

* **`geekbridge-calls --overlay`** (`crates/bridge-apps/src/call_overlay.rs`): a 360x96 borderless, always-on-top card
  with the caller (name; number only when allowed), the phone it rings on, **Answer** (green), **Reject** (red) and an X
  that hides the card while the call keeps ringing. Same helper binary as the Calls window, so no packaging change.
  App id `dev.geekbridge.CallOverlay` (the Calls window keeps `dev.geekbridge.Calls`).
* **Started by the service on every ring** (`bridge-service/src/call_overlay.rs`, called from the call notifier on Linux and
  the banner notifier on Windows/macOS): once per call (the hub's announced-set), one card at a time, not dependent on the
  banner. The banner stays as an additional signal. The card closes itself when the call ends (1.5 s grace for flicker),
  after the outcome line, or after 8 s if the service cannot be reached.
* **Answer / Reject use the existing path**: `call_ui::request_for` over the merged view -> `Request::CallAction`
  -> `CallHub::act`. A button exists only if the service offers that target. The card then shows what the service says:
  "Answering...", then "Answered on phone" (or "Answered. The voice comes to this computer." on the Bluetooth path) once
  the ring stops and a live call exists, "Call rejected", "Call ended", or "Couldn't answer: <reason>" /
  "Couldn't reject: <reason>" from the service's refusal (the buttons come back after 6 s if it still rings).
* **Keyboard**: the window is created inactive and never grabs focus. Enter answers and Esc hides the card, only while it
  has the keyboard. Esc does not reject (a stray key must not hang up).
* **Privacy**: `GEEKBRIDGE_CALL_OVERLAY=off|name|on`, or the same word in `<state dir>/call-overlay`. `name` shows the
  contact name only and "Unknown caller" otherwise, never the number. The card shows what the Telephony-consented view
  carries and nothing more; no sound (the phone rings).
* **Accessibility / i18n / theme**: `overlay.*` strings in `crates/bridge-i18n/i18n/en.properties`; the card is an
  `Alert` node, an assertive live region ("Incoming call from Ada", then each outcome); buttons are named
  ("Answer the call from Ada"); the egui theme (dot-matrix INCOMING, `tile` fill, brand red, white text on 5.1:1 green).
* **Robust launch** (owner priority): the card finds the display even if the service started without session variables
  (falls back to the `wayland-*` socket in `XDG_RUNTIME_DIR` / `/run/user/UID`, then an X11 socket). When it cannot start, one
  line `incoming-call overlay: cannot: <code>` goes to the service log and `<state dir>/call-overlay-status` records it
  (`disabled`, `not-configured`, `helper-missing`, `no-display`, `spawn-failed: ...`).
* **`geekbridge-ctl doctor`** gets "Incoming-call overlay: can launch / cannot: <reason>" (checked from the ctl's session,
  plus the service's last recorded outcome).
* **`geekbridge-ctl call-overlay-test`**: a "Test call" card for 10 seconds, no phone needed, Answer/Reject drawn but off,
  with "This is a test. Nobody is calling."
* **Placement**:
  * Hyprland: `geekbridge-ctl hotkey install` (dry run diff, `--apply`) now also writes the card's rule in all three
    syntaxes (`hypr_config.rs::overlay_rules`): float, pin, size 360x96, `move (monitor_w-window_w-16) 16`
    (`100%-w-16 16` legacy), `no_initial_focus`, `no_anim`. Deviation from the brief: no `nofocus`; on Hyprland it can also stop
    the click on Answer, and `no_initial_focus` already keeps typing uninterrupted. The card is pinned even with `--no-pin`.
  * X11 / Windows / macOS: winit hints (`WindowLevel::AlwaysOnTop`, no decorations, not in the task bar, inactive,
    `_NET_WM_WINDOW_TYPE_NOTIFICATION`) plus `card_position()` (bridge-core, unit-tested) applied as an outer position as soon as
    the monitor size is known (assumes the monitor's origin at 0,0, i.e. the primary monitor).
  * Other Wayland compositors (sway, GNOME, KDE): a client cannot place itself; the card appears where the compositor puts
    a floating window (usually centred). It still works. Layer-shell was not used: smithay-client-toolkit/wayland-client would
    mean a second GUI/EGL stack next to egui; the window rules give the same place and stacking.
* **Fallback**: unchanged. The Calls window (with its own popup) and the banner still work whether or not the card could
  be placed or started.

## Tests

* Unit (bridge-core): placement geometry (1x/2x, second monitor origin, tiny monitor), mode words.
* Unit (bridge-apps): the card state machine (offered buttons, answer path + confirmation, refusal reason and recovery,
  reject, flicker vs end, dismiss and a new call, service down), privacy lines, drawing, window options.
* Unit (bridge-service): Hyprland rules in Lua / `.conf` current / legacy, diff and idempotence, display discovery, status
  file, launch arguments. Also the existing rule tests (pin) adjusted.
* Xvfb (headless): `geekbridge-calls --overlay --test` verified at X=904 Y=16 size 360x96 on a 1280x900 screen, window type
  NOTIFICATION, class `dev.geekbridge.CallOverlay`. `scripts/e2e/call-ui.sh` has a new `overlay` stage (ring via
  `adb emu gsm call`, card appears by itself, geometry, type, colour of the buttons, X hides while ringing, auto-close, Answer
  -> off-hook + "Answered" text, Reject -> idle).
* Goldens: `call-overlay-{light,dark}-{std,min}.png` through `scripts/golden-desktop.sh` (pseudo app
  `geekbridge-calls+overlay`), captured in a separate worktree.

## What could and could not be verified

| Platform | Status |
|---|---|
| X11 under Xvfb, no window manager | placement, size, window type verified (above) |
| X11 with a real window manager | not run; the manager may override the requested position |
| Hyprland (real or nested) | **not verified**: the rules were written against the Lua names Omarchy itself uses (`no_initial_focus`, `move` expressions) and are only unit-tested as text. Run `geekbridge-ctl hotkey install`, then `geekbridge-ctl call-overlay-test`, and look |
| sway / GNOME / KDE Wayland | no placement by design; not run |
| Windows (topmost, no-activate) / macOS (floating panel) | only the winit hints; `WS_EX_NOACTIVATE` / non-activating NSPanel are not set explicitly; not run |
| Real phone ring on the Fold5 | not run here (another agent owns call detection) |

Known limits: a second call that rings within ~2.5 s after the first one ended can find the old card still closing and
get none (the Calls window and banner still show it). SIM / line label: the call view has no SIM field yet, so the card
shows the phone's name ("on Pixel 9"); when the dual-SIM work adds a line label to `CallRow`, `Card::body` is the one place
to show it. `stepping_and_jumping_move_the_shown_day_in_every_mode` (calendar) fails on 4 Nov 2026 independent of this
change (date dependent).
