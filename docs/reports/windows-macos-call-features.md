# Windows and macOS: call audio, incoming-call card, tray

Scope: the Linux-verified call features of 2026-11-04 (Bluetooth call audio, the incoming-call card,
the call-audio tooltip line) reviewed and fixed for the Windows and macOS builds. No real Windows or
Mac was available: everything below is proven by unit tests (Linux), by the Windows/macOS compile
and the smoke test in `build-binaries.yml` (GitHub runners, no Bluetooth, no display), and by reading
the winit 0.30 / egui-winit 0.31 sources. Real-machine checks are listed at the end.

## Findings and fixes

| # | Platform | Finding | Fix |
|---|----------|---------|-----|
| 1 | both | The incoming-call card could never start: `real_display_env()` demanded `WAYLAND_DISPLAY`/`DISPLAY` or a Wayland socket and failed with `no-display`. `doctor` said "cannot: no-display (no Wayland or X11...)". | `needs_display_variables(os)`: Windows and macOS skip the check. Failed-ring hint no longer names `systemctl`. Tests. |
| 2 | both | `geekbridge-ctl call setup` returned "the guided call-audio setup is for Linux" as an error (exit 1). | Prints the platform text and exits 0: Windows "pair in Settings > Bluetooth & devices, turn on Phone calls, set the default communications device"; macOS "answer here and talk on the phone". |
| 3 | both | `calling` printed `platform_unavailable (unavailable), telephony service: absent ...`, and `doctor` warned "platform_unavailable". | Platform lines (`doctor::native_calls_line`); `doctor` "Calls" is ok with the call-audio hint; backend reason text names no Linux tool (`unavailable_reason`). |
| 4 | Windows | `calling`/`doctor` had no adapter/pairing detection: `HearCalls` had no steps. | `HearCalls` is built from the existing `Get-PnpDevice` parsers (`build_windows`): adapter, pairing, Phone calls (hands-free node), default communications device (cannot be read: "skip", never a failure). |
| 5 | Windows | A PC without Bluetooth: `Get-PnpDevice -Class Bluetooth` errors, PowerShell exits 1, the read counted as failed so the checklist showed "unknown". Seen in CI. | Script sets `$ErrorActionPreference='SilentlyContinue'` and `exit 0`; empty output is an empty list, so CI now prints "No Bluetooth on this computer". |
| 6 | Windows | The PowerShell read (a few seconds of start-up) exceeded the control client's deadline, and a console window would flash for each query from the windowless service. | Read once in the background (`Refreshing<T>`, fresh for 20 s, callers wait at most 2.5 s, stale value served meanwhile), both queries in parallel, warmed at service start, `CREATE_NO_WINDOW`. Tests for the cache. |
| 7 | Windows | `doctor` said "Ready: ... plays on its headphones" when nothing could be read. | Warns when no step could be read; uses the platform headline when ready. |
| 8 | Windows | `call-audio` on a PC without Bluetooth said "kernel btusb driver"; the test error was Linux-worded. | Windows text ("Windows needs a Bluetooth adapter with hands-free (Phone calls) support") and the default-communications-device guide; `call-audio test` explains that Windows chooses the devices. |
| 9 | both | `call audio-devices` said "only implemented on Linux". | Platform sentence. |
| 10 | Windows | The card is not activated when shown (`with_active(false)`), but a click would activate it. | `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, `WS_EX_APPWINDOW` cleared, applied on the first frame through `eframe::Frame`'s window handle (`windows-sys`, `raw-window-handle`, both already in the lock file). `WindowLevel::AlwaysOnTop` (WS_EX_TOPMOST) and the tool-window/no-task-bar flag were already set by the viewport. |
| 11 | macOS | The card ran as a regular app (Dock icon, activation of its own app). The card at y=16 sat under the menu bar. | `ActivationPolicy::Accessory` through `NativeOptions::event_loop_builder` (winit 0.30, same version already linked by eframe; no new package). Level `AlwaysOnTop` = floating panel level, `orderFront` without key from `with_active(false)`. Card is placed `MAC_MENU_BAR` (28 pt) lower. objc2 is not a dependency of `bridge-apps`, so no raw NSPanel (NSWindowStyleMaskNonactivatingPanel) is attempted. |
| 12 | Windows | Tray tooltip: `szTip` truncation lived only in the Windows module. | `tray::model::tip_units` shared and tested on Linux for every call-audio hint, long and non-BMP names (never half a surrogate pair, always zero-terminated). macOS tooltip is the same strings in an `NSString`. On both systems the call-audio hint is `Unknown`, so no new tooltip line appears. |

Not executing on Windows or macOS (checked by reading): `pactl`, `parec`, `paplay`, `pw-play`,
`wpctl`, `bluetoothctl`, `rfkill`, `systemctl`, `notify-send` and every D-Bus use are reachable only
from `cfg(target_os = "linux")` code (`call_audio_host::host`, `call_control::linux`,
`bridge_calls::{pipewire, setup}`, `readiness::detect`) or from the Linux branch of
`SystemHost::collect`. The only other command run is PowerShell, on Windows.

## DPI and multi-monitor geometry

egui-winit divides the monitor's physical size by the window's scale factor to get `monitor_size`
(logical points) and multiplies an `OuterPosition` by the same factor, so one scale is applied once.
`card_position_for` works in logical points; `card_position_physical` is the reference in physical
pixels. Tests (`bridge-core::call_overlay`):

* 1920x1080, 3840x2160, 2560x1440 at 100/125/150/200 %: the logical path, multiplied by the scale,
  equals the physical reference within 1 px; the right margin is the scaled 16 px. Known values for
  1920 wide: 1544 / 1450 / 1356 / 1168.
* Negative origins: a monitor left of the primary (-1920,0) at 125 % gives x = -470; above (0,-1440)
  at 200 %; both negative at 150 %; the card never leaves the left edge of a narrow monitor.
* A failed scale query (0, negative, NaN, infinity) reads as 100 %. macOS: 28 pt lower; 2x Retina.

Limitation: egui reports the monitor's size but not its origin. The card is placed on the monitor
the window opens on, assumed at origin (0,0), which is true for the primary monitor on Windows and
macOS (the system puts a new window there). It is not moved to the monitor of the focused window.

## CI evidence (run 41779873064 on 4010e541f; artifacts in a fresh directory, checksums verified, not executed)

Smoke test now also runs `ctl calling`, `call-audio`, `call setup`, `call audio-devices`, requires
exit 0 and fails on any of `pactl parec wpctl bluetoothctl rfkill btusb PipeWire WirePlumber D-Bus
systemctl WAYLAND sudo`. All four jobs green; zero Linux-only matches in the Windows and macOS logs.

Windows (runner with no Bluetooth):
```
[ ok ] Calls: calls are shown and answered through the phone link; Windows' own Bluetooth Phone calls carries the voice ...
[warn] Hear calls here: This computer has no Bluetooth. ... Full checklist: geekbridge-ctl calling
[ ok ] Incoming-call overlay: can launch (geekbridge-ctl call-overlay-test shows it)
calling:  1. [FAIL] Bluetooth adapter found: This computer has no Bluetooth. ...
call-audio: Call audio on this computer: No Bluetooth on this computer ... Windows carries the call's voice itself ...
call setup: Windows pairs and routes call audio itself, so there is nothing to set up here. ...
```
macOS:
```
[ ok ] Calls: calls are shown and answered through the phone link; the voice stays on the phone, so answer here and talk on the phone
[ ok ] Incoming-call overlay: can launch (...)
call-audio: Not available on a Mac yet ... - Answer here and talk on the phone.
call setup: On a Mac the voice of a call stays on the phone: answer the call here and talk on the phone ...
```
(Before the fixes the same run showed an all-"unknown" Windows checklist, "Ready" in `doctor`, and
would have shown "no-display" for the overlay.)

Local gates on Linux: `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings` and `cargo test --workspace --locked` exit 0.

## Needs a real machine

* Windows: that Phone calls produces the `BTHENUM\{0000111F-...}` node as assumed (unverified by
  Microsoft's documentation); that the card does not take focus when clicked; the card at 125/150 %
  on a two-monitor desk; a console window never flashes.
* macOS: that the accessory-policy card is above full-screen apps and does not steal focus; that it
  clears the menu bar on a notch Mac.
