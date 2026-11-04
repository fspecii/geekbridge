// SPDX-License-Identifier: GPL-3.0-or-later
//! The incoming-call card: the small notification-style window that appears in the top-right corner
//! of the screen while a call rings (`geekbridge-calls --overlay`).
//!
//! The service, the window and the compositor rules (`geekbridge-ctl hotkey install`) must agree on
//! the window's identity and size, so the numbers live here, with the placement arithmetic that
//! needs no window system and is tested as plain data.

/// Wayland app_id / X11 WM_CLASS of the card. Different from the Calls window's, so compositor
/// rules for one never catch the other.
pub const APP_ID: &str = "dev.geekbridge.CallOverlay";
/// Logical size of the card.
pub const CARD_WIDTH: f32 = 360.0;
pub const CARD_HEIGHT: f32 = 96.0;
/// Gap between the card and the screen edges.
pub const MARGIN: f32 = 16.0;

/// A monitor in logical (scale-corrected) pixels, origin included for multi-monitor layouts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Outer position of the card: top-right corner of `monitor`, `MARGIN` from the top and right edges.
/// A monitor narrower than the card (nonsense, or a failed query) keeps the card on the monitor's
/// left edge instead of pushing it off screen.
pub fn card_position(monitor: Monitor) -> (f32, f32) {
    let x = (monitor.x + monitor.width - CARD_WIDTH - MARGIN).max(monitor.x);
    (x, monitor.y + MARGIN)
}

/// What the card may show of the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reveal {
    /// Name and number (when the phone sent them).
    #[default]
    Full,
    /// The contact name only; an unknown caller is shown as "Unknown caller" without the number.
    NameOnly,
}

/// Whether and how the card is shown. Set with `GEEKBRIDGE_CALL_OVERLAY=off|name|on`, or the same
/// word in `<state dir>/call-overlay`; the variable wins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    On,
    NameOnly,
    Off,
}

impl Mode {
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "" | "on" | "full" | "1" | "yes" | "true" => Some(Self::On),
            "name" | "name-only" | "names" => Some(Self::NameOnly),
            "off" | "0" | "no" | "false" | "none" => Some(Self::Off),
            _ => None,
        }
    }

    /// `env` is the variable's value, `file` the settings file's text; an unreadable word falls
    /// back to the next source and finally to `On`.
    pub fn resolve(env: Option<&str>, file: Option<&str>) -> Self {
        env.and_then(Self::parse)
            .or_else(|| file.and_then(Self::parse))
            .unwrap_or_default()
    }

    pub fn reveal(self) -> Reveal {
        match self {
            Self::NameOnly => Reveal::NameOnly,
            _ => Reveal::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: f32, y: f32, w: f32, h: f32) -> Monitor {
        Monitor {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn the_card_sits_top_right_with_a_margin() {
        assert_eq!(card_position(mon(0.0, 0.0, 1920.0, 1080.0)), (1544.0, 16.0));
        // A 2x display reports 1280x720 logical pixels.
        assert_eq!(card_position(mon(0.0, 0.0, 1280.0, 720.0)), (904.0, 16.0));
    }

    #[test]
    fn a_second_monitor_keeps_its_own_origin() {
        assert_eq!(
            card_position(mon(1920.0, 0.0, 2560.0, 1440.0)),
            (1920.0 + 2560.0 - 360.0 - 16.0, 16.0)
        );
        assert_eq!(
            card_position(mon(-1280.0, 200.0, 1280.0, 720.0)),
            (-376.0, 216.0)
        );
    }

    #[test]
    fn a_tiny_monitor_never_pushes_the_card_off_the_left_edge() {
        assert_eq!(card_position(mon(0.0, 0.0, 300.0, 200.0)), (0.0, 16.0));
        assert_eq!(card_position(mon(100.0, 0.0, 0.0, 0.0)), (100.0, 16.0));
    }

    #[test]
    fn the_mode_words() {
        assert_eq!(Mode::resolve(None, None), Mode::On);
        assert_eq!(Mode::resolve(Some("off"), Some("on")), Mode::Off);
        assert_eq!(
            Mode::resolve(Some("garbage"), Some("name\n")),
            Mode::NameOnly
        );
        assert_eq!(Mode::resolve(None, Some("nonsense")), Mode::On);
        assert_eq!(Mode::NameOnly.reveal(), Reveal::NameOnly);
        assert_eq!(Mode::On.reveal(), Reveal::Full);
    }
}
