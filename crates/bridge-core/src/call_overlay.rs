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

/// Extra space above the card on macOS: the menu bar (24 pt, 37 pt on a Mac with a camera notch)
/// is not part of a borderless window's frame, so without it the card would sit under the bar.
pub const MAC_MENU_BAR: f32 = 28.0;

/// Like [`card_position`], for the operating system named by `std::env::consts::OS`. Logical
/// pixels in, logical pixels out, so the caller's window system applies the scale factor once.
pub fn card_position_for(os: &str, monitor: Monitor) -> (f32, f32) {
    let (x, y) = card_position(monitor);
    (x, y + if os == "macos" { MAC_MENU_BAR } else { 0.0 })
}

/// A monitor as the window system reports it before scaling: physical pixels, origin included.
/// Windows reports exactly this (a monitor left of or above the primary has a negative origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMonitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The monitor in logical pixels at `scale` (1.25 for "125 %"). A zero or negative scale reads as 1.
pub fn logical_monitor(m: PhysicalMonitor, scale: f64) -> Monitor {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    Monitor {
        x: (f64::from(m.x) / s) as f32,
        y: (f64::from(m.y) / s) as f32,
        width: (f64::from(m.width) / s) as f32,
        height: (f64::from(m.height) / s) as f32,
    }
}

/// Where the card goes in physical pixels: top-right of `m`, the card and its margins scaled by
/// `scale`. This is what Windows ends up with once `card_position` (logical) has been multiplied
/// by the scale factor; computing it directly is the reference the tests hold the logical path to.
pub fn card_position_physical(os: &str, m: PhysicalMonitor, scale: f64) -> (i32, i32) {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let card_w = (f64::from(CARD_WIDTH + MARGIN) * s).round() as i64;
    let top = f64::from(MARGIN + if os == "macos" { MAC_MENU_BAR } else { 0.0 });
    let x = (i64::from(m.x) + i64::from(m.width) - card_w).max(i64::from(m.x));
    (x as i32, (i64::from(m.y) + (top * s).round() as i64) as i32)
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

    /// What egui-winit does with `place()`: the monitor size is divided by the window's scale
    /// factor, the position goes out in logical points and is multiplied by the same factor.
    fn through_the_window_system(os: &str, m: PhysicalMonitor, scale: f64) -> (f64, f64) {
        let l = logical_monitor(m, scale);
        // egui reports the monitor size only; the origin is the primary monitor's, (0, 0).
        let (x, y) = card_position_for(
            os,
            Monitor {
                x: 0.0,
                y: 0.0,
                width: l.width,
                height: l.height,
            },
        );
        (f64::from(x) * scale, f64::from(y) * scale)
    }

    fn phys(x: i32, y: i32, w: u32, h: u32) -> PhysicalMonitor {
        PhysicalMonitor {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn windows_scale_factors_put_the_card_in_the_same_physical_place() {
        // 100 / 125 / 150 / 200 % on a 1920 x 1080 and a 3840 x 2160 panel.
        for (w, h) in [(1920_u32, 1080_u32), (3840, 2160), (2560, 1440)] {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let m = phys(0, 0, w, h);
                let (px, py) = card_position_physical("windows", m, scale);
                let (gx, gy) = through_the_window_system("windows", m, scale);
                assert!(
                    (f64::from(px) - gx).abs() <= 1.0,
                    "x {w}x{h} @{scale}: {px} vs {gx}"
                );
                assert!(
                    (f64::from(py) - gy).abs() <= 1.0,
                    "y {w}x{h} @{scale}: {py} vs {gy}"
                );
                // The whole card (scaled) lies on the monitor with a scaled margin on the right.
                let card_w = (f64::from(CARD_WIDTH) * scale).round() as i64;
                let right = i64::from(px) + card_w;
                let margin = (f64::from(MARGIN) * scale).round() as i64;
                assert!(
                    (i64::from(w) - right - margin).abs() <= 1,
                    "{w}x{h} @{scale}"
                );
            }
        }
    }

    #[test]
    fn known_physical_positions_at_each_scale() {
        // 1920 x 1080 panel: 100 % -> 1544, 125 % -> 1450, 150 % -> 1356, 200 % -> 1168.
        let m = phys(0, 0, 1920, 1080);
        assert_eq!(card_position_physical("windows", m, 1.0), (1544, 16));
        assert_eq!(card_position_physical("windows", m, 1.25), (1450, 20));
        assert_eq!(card_position_physical("windows", m, 1.5), (1356, 24));
        assert_eq!(card_position_physical("windows", m, 2.0), (1168, 32));
    }

    #[test]
    fn a_monitor_left_of_or_above_the_primary_has_a_negative_origin() {
        // A 1920 x 1080 monitor to the left of the primary at 125 %.
        let left = phys(-1920, 0, 1920, 1080);
        assert_eq!(card_position_physical("windows", left, 1.25), (-470, 20));
        // Above the primary, at 200 %.
        let above = phys(0, -1440, 2560, 1440);
        assert_eq!(
            card_position_physical("windows", above, 2.0),
            (1808, -1440 + 32)
        );
        // Straddling both negative, 150 %.
        let both = phys(-2560, -1080, 2560, 1080);
        assert_eq!(
            card_position_physical("windows", both, 1.5),
            (-2560 + 2560 - 564, -1080 + 24)
        );
        // The logical path keeps the origin too.
        let l = logical_monitor(left, 1.25);
        assert_eq!((l.x, l.width), (-1536.0, 1536.0));
        let (x, _) = card_position(l);
        assert_eq!(f64::from(x) * 1.25, -470.0);
    }

    #[test]
    fn a_failed_scale_query_reads_as_100_percent() {
        let m = phys(0, 0, 1920, 1080);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(card_position_physical("windows", m, bad), (1544, 16));
            assert_eq!(logical_monitor(m, bad).width, 1920.0);
        }
    }

    #[test]
    fn a_monitor_narrower_than_the_card_keeps_the_left_edge_in_physical_pixels() {
        assert_eq!(
            card_position_physical("windows", phys(-300, 0, 300, 200), 1.0),
            (-300, 16)
        );
    }

    #[test]
    fn the_mac_card_clears_the_menu_bar() {
        let m = Monitor {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        };
        assert_eq!(card_position_for("macos", m), (1064.0, 16.0 + MAC_MENU_BAR));
        assert_eq!(card_position_for("windows", m), (1064.0, 16.0));
        assert_eq!(card_position_for("linux", m), card_position(m));
        // A Retina display reports points, so 2x changes nothing in logical terms.
        let (px, py) = card_position_physical("macos", phys(0, 0, 2880, 1800), 2.0);
        assert_eq!((px, py), (2128, 88));
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
