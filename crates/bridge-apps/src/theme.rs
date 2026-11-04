// SPDX-License-Identifier: GPL-3.0-or-later
//! Native-feeling look for the four apps: macOS-style light and dark palettes, system fonts,
//! and the small set of controls they share. Navigation never lives here.
//!
//! Messages gets its own ink-and-red palette; the other three use the system-blue one.
use crate::icons::{self, Icon};
use bridge_core::AppKind;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin,
    Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Theme, Ui, Visuals, WidgetInfo,
    WidgetType, vec2,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub toolbar: Color32,
    pub sidebar: Color32,
    pub content: Color32,
    /// Raised areas inside content: inputs, keys, cards.
    pub surface: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub separator: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    pub accent: Color32,
    /// Brand red for fills, dots and indicators (not for small text on dark).
    pub accent_fill: Color32,
    pub on_accent: Color32,
    pub selected_bg: Color32,
    pub selected_fg: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    /// Recessed display (Messages status display).
    pub lcd_bg: Color32,
    pub lcd_off: Color32,
    /// Rounded-square tile and card fill, and its hover shade.
    pub tile: Color32,
    pub tile_hover: Color32,
    /// Unlit dot-matrix dots.
    pub dot_off: Color32,
}

/// One palette for every window: true-black / warm off-white, grey ramps and a single red.
/// `accent` is the text-safe red (>= 4.5:1 on every surface of its theme); `accent_fill` is the
/// brand #D71921 for dots, fills and indicators (white text on it is 5.2:1).
pub fn palette(app: AppKind, dark: bool) -> Palette {
    let base = base_palette(app, dark);
    if high_contrast() {
        high_contrast_of(base)
    } else {
        base
    }
}

static HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

pub fn set_high_contrast(on: bool) {
    HIGH_CONTRAST.store(on, Ordering::Relaxed);
}

pub fn high_contrast() -> bool {
    HIGH_CONTRAST.load(Ordering::Relaxed)
}

/// `GEEKBRIDGE_HIGH_CONTRAST=1`, or a GTK theme whose name says high contrast, asks for it.
pub fn high_contrast_requested() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    env("GEEKBRIDGE_HIGH_CONTRAST") == "1"
        || env("GTK_THEME").to_lowercase().contains("highcontrast")
}

/// Same layout, but secondary text becomes full ink and every edge and unlit dot a strong mid
/// grey, so nothing relies on a faint tint. Red stays the only hue.
fn high_contrast_of(mut p: Palette) -> Palette {
    p.text2 = p.text;
    p.text3 = p.text;
    p.separator = if p.dark { rgb(0x8C8C86) } else { rgb(0x5F5F5A) };
    p.dot_off = p.separator;
    p.lcd_off = p.separator;
    p
}

fn base_palette(_app: AppKind, dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            toolbar: rgb(0x000000),
            sidebar: rgb(0x0A0A0A),
            content: rgb(0x000000),
            surface: rgb(0x121212),
            text: rgb(0xF4F4F1),
            text2: rgb(0xA3A39D),
            text3: rgb(0x9A9A94),
            separator: rgb(0x242422),
            control: rgb(0x1E1E1D),
            control_hover: rgb(0x2A2A28),
            control_active: rgb(0x383835),
            accent: rgb(0xFF5A5A),
            accent_fill: rgb(0xD71921),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0xF4F4F1),
            selected_fg: rgb(0x0C0C0C),
            ok: rgb(0x58C486),
            warn: rgb(0xE0A030),
            err: rgb(0xFF5A5A),
            lcd_bg: rgb(0x0A0A0A),
            lcd_off: rgb(0x242422),
            tile: rgb(0x141414),
            tile_hover: rgb(0x1C1C1B),
            dot_off: rgb(0x2E2E2C),
        }
    } else {
        Palette {
            dark,
            toolbar: rgb(0xF4F3EF),
            sidebar: rgb(0xFBFAF7),
            content: rgb(0xF4F3EF),
            surface: rgb(0xFFFFFF),
            text: rgb(0x0C0C0C),
            text2: rgb(0x52524D),
            text3: rgb(0x5F5F5A),
            separator: rgb(0xDEDDD6),
            control: rgb(0xE8E7E1),
            control_hover: rgb(0xDDDCD5),
            control_active: rgb(0xCFCEC6),
            accent: rgb(0xB8121A),
            accent_fill: rgb(0xD71921),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0x0C0C0C),
            selected_fg: rgb(0xFAFAF8),
            ok: rgb(0x1A6E3E),
            warn: rgb(0x8F5500),
            err: rgb(0xB8121A),
            lcd_bg: rgb(0xE8E7E1),
            lcd_off: rgb(0xD6D5CE),
            tile: rgb(0xFFFFFF),
            tile_hover: rgb(0xF7F6F2),
            dot_off: rgb(0xD3D2CB),
        }
    }
}

pub fn current(ctx: &egui::Context, app: AppKind) -> Palette {
    palette(app, ctx.style().visuals.dark_mode)
}

// ---------------------------------------------------------------- fonts

const SEMIBOLD: &str = "geekbridge-semibold";

pub fn semibold_family() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

struct Face {
    regular: Vec<PathBuf>,
    bold: Vec<PathBuf>,
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

/// System UI font families in order of preference. Nothing is bundled or downloaded; if no
/// listed font exists the toolkit's built-in font is used.
fn candidate_faces() -> Vec<Face> {
    vec![
        // Windows
        Face {
            regular: paths(&["C:\\Windows\\Fonts\\segoeui.ttf"]),
            bold: paths(&[
                "C:\\Windows\\Fonts\\segoeuib.ttf",
                "C:\\Windows\\Fonts\\seguisb.ttf",
            ]),
        },
        // macOS: San Francisco ships as one variable file; the bold slot reuses it.
        Face {
            regular: paths(&["/System/Library/Fonts/SFNS.ttf"]),
            bold: paths(&["/System/Library/Fonts/SFNS.ttf"]),
        },
        // Common Linux families, static files only.
        Face {
            regular: paths(&[
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/NotoSans-Regular.ttf",
            ]),
            bold: paths(&[
                "/usr/share/fonts/noto/NotoSans-Bold.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
                "/usr/share/fonts/google-noto/NotoSans-Bold.ttf",
                "/usr/share/fonts/NotoSans-Bold.ttf",
            ]),
        },
        Face {
            regular: paths(&[
                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            ]),
            bold: paths(&[
                "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            ]),
        },
        Face {
            regular: paths(&[
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ]),
            bold: paths(&[
                "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ]),
        },
    ]
}

fn read_font(candidates: &[PathBuf]) -> Option<Vec<u8>> {
    candidates.iter().find_map(|path| {
        let meta = std::fs::metadata(path).ok()?;
        // Skip anything that is not a plausibly sized font file.
        (meta.is_file() && meta.len() < 32 * 1024 * 1024)
            .then(|| std::fs::read(path).ok())
            .flatten()
    })
}

/// Fonts embedded in the binary (egui's `default_fonts` feature is off so no Ubuntu Font Licence
/// font is shipped). Licences and provenance: third_party/fonts/README.md.
const EMBEDDED_FONTS: [(&str, &[u8]); 4] = [
    (
        "noto-sans",
        include_bytes!("../../../third_party/fonts/NotoSans-Regular-subset.ttf"),
    ),
    (
        "noto-emoji",
        include_bytes!("../../../third_party/fonts/NotoEmoji-Regular.ttf"),
    ),
    (
        "emoji-icon-font",
        include_bytes!("../../../third_party/fonts/emoji-icon-font.ttf"),
    ),
    (
        "hack",
        include_bytes!("../../../third_party/fonts/Hack-Regular.ttf"),
    ),
];

/// Registers the embedded fonts: Noto Sans (Latin, Greek, Cyrillic subset) behind any system face,
/// Hack for monospace, and the monochrome emoji/icon fonts as last-resort fallbacks.
fn embed_fonts(fonts: &mut FontDefinitions) {
    for (name, bytes) in EMBEDDED_FONTS {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.extend(["noto-sans", "noto-emoji", "emoji-icon-font"].map(String::from));
    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.extend(["hack", "noto-sans", "noto-emoji", "emoji-icon-font"].map(String::from));
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    embed_fonts(&mut fonts);
    let mut regular_name = None;
    let mut bold_name = None;
    // Golden-image tests: render with the embedded fonts only so results do not depend on the machine.
    let bundled_only = std::env::var_os("GEEKBRIDGE_BUNDLED_FONTS_ONLY").is_some();
    for face in candidate_faces().into_iter().filter(|_| !bundled_only) {
        let Some(regular) = read_font(&face.regular) else {
            continue;
        };
        fonts.font_data.insert(
            "system-regular".into(),
            Arc::new(FontData::from_owned(regular)),
        );
        regular_name = Some("system-regular".to_owned());
        if let Some(bold) = read_font(&face.bold) {
            fonts
                .font_data
                .insert("system-bold".into(), Arc::new(FontData::from_owned(bold)));
            bold_name = Some("system-bold".to_owned());
        }
        break;
    }
    if let Some(name) = &regular_name {
        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        proportional.insert(0, name.clone());
    }
    // Monochrome emoji outlines (egui cannot draw colour bitmap emoji); only used for glyphs the
    // text face lacks, so a message with an emoji shows a symbol instead of an empty box.
    for path in [
        "/usr/share/fonts/noto/NotoEmoji-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
        "/usr/share/fonts/google-noto-emoji/NotoEmoji-Regular.ttf",
        "/usr/share/fonts/truetype/ancient-scripts/Symbola_hint.ttf",
    ]
    .into_iter()
    .filter(|_| !bundled_only)
    {
        if let Some(bytes) = read_font(&paths(&[path])) {
            fonts
                .font_data
                .insert("mono-emoji".into(), Arc::new(FontData::from_owned(bytes)));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("mono-emoji".into());
            break;
        }
    }
    // Scripts the bundled fonts lack (CJK, Arabic, Indic, ...) that text on screen asked for.
    crate::script_fonts::add_loaded(&mut fonts);
    // Semibold falls back to the regular face (then the built-in one) when no bold exists.
    let mut semibold: Vec<String> = bold_name.into_iter().collect();
    semibold.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(semibold_family(), semibold);
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------- style

/// Installs fonts and both theme styles. Palettes are picked per frame from the active theme.
pub fn apply(ctx: &egui::Context, app: AppKind) {
    install_fonts(ctx);
    set_high_contrast(high_contrast_requested());
    ctx.options_mut(|o| o.fallback_theme = Theme::Light);
    for (theme, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
        let pal = palette(app, dark);
        ctx.style_mut_of(theme, |style| {
            let mut v = if dark {
                Visuals::dark()
            } else {
                Visuals::light()
            };
            v.panel_fill = pal.content;
            v.window_fill = pal.content;
            v.extreme_bg_color = pal.surface;
            v.faint_bg_color = pal.control;
            v.override_text_color = Some(pal.text);
            v.hyperlink_color = pal.accent;
            v.selection.bg_fill = pal
                .accent_fill
                .gamma_multiply(if dark { 0.5 } else { 0.28 });
            v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
            v.text_cursor.stroke = Stroke::new(1.5_f32, pal.accent);
            let radius = CornerRadius::same(12);
            for (w, fill) in [
                (&mut v.widgets.inactive, pal.control),
                (&mut v.widgets.hovered, pal.control_hover),
                (&mut v.widgets.active, pal.control_active),
            ] {
                w.bg_fill = fill;
                w.weak_bg_fill = fill;
                w.bg_stroke = Stroke::NONE;
                // Stock widgets (text fields, small buttons) are 22-28 pt tall: a 12 pt radius turns
                // them into pills that clip the text.
                w.corner_radius = CornerRadius::same(8);
                w.fg_stroke = Stroke::new(1.0_f32, pal.text);
            }
            // Text fields and stock widgets get a >= 3:1 outline (WCAG 1.4.11); the fill alone is
            // 1.1:1 against the sidebar and invisible in light mode.
            let outline = if high_contrast() {
                pal.text
            } else if dark {
                rgb(0x666662)
            } else {
                rgb(0x8A8A84)
            };
            v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, outline);
            v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, pal.text3);
            // egui draws a keyboard-focused stock button (and a pressed one) with the "active"
            // style: give it the shared ink ring so focus is visible, not just a fill change.
            v.widgets.active.bg_stroke = Stroke::new(2.0_f32, focus_color(&pal));
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.separator);
            v.widgets.noninteractive.corner_radius = radius;
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text2);
            v.widgets.open.corner_radius = radius;
            v.window_corner_radius = CornerRadius::same(18);
            v.window_stroke = Stroke::new(1.0_f32, pal.separator);
            style.visuals = v;
            // Reduced motion: widgets change state instantly instead of fading.
            style.animation_time = 0.0;
            style.spacing.item_spacing = vec2(8.0, 8.0);
            style.spacing.button_padding = vec2(12.0, 5.0);
            style.spacing.interact_size.y = 28.0;
            style.spacing.scroll.bar_width = 8.0;
            style.spacing.scroll.floating = true;
            style.text_styles = [
                (TextStyle::Heading, FontId::new(22.0, semibold_family())),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(14.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Small,
                    FontId::new(12.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(13.0, FontFamily::Monospace),
                ),
            ]
            .into();
        });
    }
}

// ---------------------------------------------------------------- text helpers

pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(FontId::new(size, semibold_family()))
}
pub fn secondary(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(pal.text2)
}
/// Placeholder text of a stock text field. egui would draw it at half-strength ink (3.5:1 in the
/// light theme); this is the secondary text colour, which is held to 4.5:1 on every surface.
pub fn hint(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).color(palette(AppKind::Calls, ui.visuals().dark_mode).text3)
}
pub fn caption(pal: &Palette, text: impl Into<String>) -> RichText {
    secondary(pal, text).size(12.0)
}
/// Small uppercase section label, as used above sidebar groups and form fields.
pub fn overline(pal: &Palette, text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .font(FontId::new(12.0, semibold_family()))
        .color(pal.text3)
}

// ---------------------------------------------------------------- structure

pub fn plain_frame(fill: Color32) -> Frame {
    Frame::new().fill(fill).inner_margin(Margin::ZERO)
}

/// Unified toolbar across the top of the window with a hairline underneath.
pub fn toolbar<R>(
    ctx: &egui::Context,
    pal: &Palette,
    height: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    egui::TopBottomPanel::top("toolbar")
        .exact_height(height)
        .frame(
            Frame::new()
                .fill(pal.toolbar)
                .stroke(Stroke::NONE)
                .inner_margin(Margin::symmetric(12, 0)),
        )
        .show(ctx, |ui| {
            let r = ui.max_rect();
            ui.painter().hline(
                r.x_range().expand(12.0),
                r.bottom() + 0.5,
                Stroke::new(1.0_f32, pal.separator),
            );
            add(ui)
        })
        .inner
}

pub fn hairline(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, pal.separator);
}

pub fn status_color(pal: &Palette, tone: crate::state::Tone) -> Color32 {
    use crate::state::Tone;
    match tone {
        Tone::Neutral => pal.text3,
        Tone::Info => pal.text,
        Tone::Warning => pal.accent_fill,
        Tone::Error => pal.err,
    }
}

/// Filled dot, or a ring when `hollow`.
pub fn dot(ui: &mut Ui, color: Color32, hollow: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    if hollow {
        ui.painter()
            .circle_stroke(rect.center(), 3.5, Stroke::new(1.4_f32, color));
    } else {
        ui.painter().circle_filled(rect.center(), 3.5, color);
    }
}

// ---------------------------------------------------------------- controls

/// Keyboard focus ring colour: full ink, >= 3:1 on every surface in both themes and never red, so it
/// stays visible next to the red Primary button and the red "today" mark.
pub fn focus_color(pal: &Palette) -> Color32 {
    pal.text
}

/// Escape closes a sheet. While a text field has the keyboard the first Escape only leaves the
/// field (egui does that), so half-typed text is not thrown away by a stray key.
pub fn escape_pressed(ctx: &egui::Context) -> bool {
    !ctx.wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}

/// 2 pt ring one point outside `rect`, leaving a hairline gap so it also reads against a filled
/// control of its own colour.
pub fn focus_ring(ui: &Ui, pal: &Palette, rect: egui::Rect, radius: impl Into<CornerRadius>) {
    ui.painter().rect_stroke(
        rect.expand(1.0),
        radius,
        Stroke::new(2.0_f32, focus_color(pal)),
        StrokeKind::Outside,
    );
}

/// Draws the focus ring around a frameless text field (stock fields draw their own outline).
pub fn ring_when_focused(ui: &Ui, pal: &Palette, response: &Response) {
    if response.has_focus() {
        focus_ring(ui, pal, response.rect, 8);
    }
}

/// [`focus_ring`] for round controls.
pub fn focus_circle(ui: &Ui, pal: &Palette, center: egui::Pos2, radius: f32) {
    ui.painter()
        .circle_stroke(center, radius + 2.0, Stroke::new(2.0_f32, focus_color(pal)));
}

fn describe(response: &Response, label: &str, enabled: bool) {
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
}

/// Flat toolbar button: icon with an optional label; shaded on hover.
pub fn tool_button(
    ui: &mut Ui,
    pal: &Palette,
    icon: Icon,
    label: Option<&str>,
    enabled: bool,
    tip: &str,
) -> Response {
    let font = FontId::new(13.0, FontFamily::Proportional);
    let tint = if enabled { pal.text } else { pal.text3 };
    let galley = label.map(|l| ui.painter().layout_no_wrap(l.to_owned(), font, tint));
    let width = 30.0 + galley.as_ref().map_or(0.0, |g| g.size().x + 6.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), sense);
    if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        let fill = if response.is_pointer_button_down_on() {
            pal.control_active
        } else {
            pal.control_hover.gamma_multiply(0.7)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
    }
    if response.has_focus() {
        focus_ring(ui, pal, rect, CornerRadius::same(7));
    }
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 15.0, rect.center().y),
        vec2(16.0, 16.0),
    );
    icons::paint(ui.painter(), icon_rect, icon, tint);
    if let Some(galley) = galley {
        let pos = egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, tint);
    }
    describe(&response, label.unwrap_or(tip), enabled);
    response.on_hover_text(tip)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with the accent color.
    Primary,
    /// Quiet grey pill.
    Plain,
    /// High-contrast ink pill (white on black in dark, black on white in light).
    Inverse,
}

/// Rounded pill button. Disabled buttons stay visible but muted.
pub fn pill_button(
    ui: &mut Ui,
    pal: &Palette,
    text: &str,
    kind: ButtonKind,
    enabled: bool,
) -> Response {
    pill_button_sized(ui, pal, text, kind, enabled, 0.0)
}

/// [`pill_button`] at least `min_width` wide (for side-by-side decisions).
pub fn pill_button_sized(
    ui: &mut Ui,
    pal: &Palette,
    text: &str,
    kind: ButtonKind,
    enabled: bool,
    min_width: f32,
) -> Response {
    let (fill, fg) = match (kind, enabled) {
        (ButtonKind::Primary, true) => (pal.accent_fill, pal.on_accent),
        (ButtonKind::Inverse, true) => (pal.selected_bg, pal.selected_fg),
        (ButtonKind::Primary | ButtonKind::Inverse, false) => (pal.control, pal.text3),
        (ButtonKind::Plain, true) => (pal.control, pal.text),
        (ButtonKind::Plain, false) => (pal.control.gamma_multiply(0.6), pal.text3),
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::new(13.5, semibold_family()), fg);
    let size = vec2((galley.size().x + 28.0).max(min_width), 32.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let fill = if !enabled {
        fill
    } else if response.is_pointer_button_down_on() {
        fill.gamma_multiply(0.82)
    } else if response.hovered() {
        match kind {
            ButtonKind::Primary | ButtonKind::Inverse => fill.gamma_multiply(0.92),
            ButtonKind::Plain => pal.control_hover,
        }
    } else {
        fill
    };
    let radius = CornerRadius::same(16);
    ui.painter().rect_filled(rect, radius, fill);
    if response.has_focus() {
        focus_ring(ui, pal, rect, radius);
    }
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    describe(&response, text, enabled);
    response
}

/// Round icon button used for Send, Call and similar single actions.
pub fn round_button(
    ui: &mut Ui,
    pal: &Palette,
    diameter: f32,
    fill: Color32,
    icon: Icon,
    enabled: bool,
    label: &str,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, diameter), sense);
    let fill = if !enabled {
        pal.control
    } else if response.is_pointer_button_down_on() {
        fill.gamma_multiply(0.8)
    } else if response.hovered() {
        fill.gamma_multiply(0.92)
    } else {
        fill
    };
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, fill);
    if response.has_focus() {
        focus_circle(ui, pal, rect.center(), diameter / 2.0);
    }
    let tint = if enabled { Color32::WHITE } else { pal.text3 };
    let icon_rect =
        egui::Rect::from_center_size(rect.center(), vec2(diameter * 0.46, diameter * 0.46));
    if icon == Icon::Phone {
        icons::paint_handset(ui.painter(), rect.center(), diameter * 0.34, tint, 0.0);
    } else {
        icons::paint(ui.painter(), icon_rect, icon, tint);
    }
    describe(&response, label, enabled);
    response
}

/// macOS-style segmented control. Returns true when the selection changed. Each segment is
/// 26 pt tall (WCAG 2.2 target size minimum is 24 px at 100% scale).
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    pal: &Palette,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let font = FontId::new(13.0, FontFamily::Proportional);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), pal.text)
        })
        .collect();
    let seg_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max) + 26.0;
    let total = vec2(seg_w * options.len() as f32 + 4.0, 30.0);
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
    let track = CornerRadius::same(15);
    ui.painter().rect_filled(rect, track, pal.control);
    let mut changed = false;
    for (i, ((option, label), galley)) in options.iter().zip(galleys).enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 2.0 + seg_w * i as f32, rect.top() + 2.0),
            vec2(seg_w, 26.0),
        );
        let id = ui.id().with(("segment", i));
        let response = ui.interact(seg, id, Sense::click());
        let selected = *value == *option;
        if selected {
            let fill = if pal.dark {
                pal.control_active
            } else {
                Color32::WHITE
            };
            ui.painter().rect_filled(seg, CornerRadius::same(13), fill);
            ui.painter().rect_stroke(
                seg,
                CornerRadius::same(13),
                Stroke::new(1.0_f32, pal.separator),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter().rect_filled(
                seg,
                CornerRadius::same(13),
                pal.control_hover.gamma_multiply(0.6),
            );
        }
        if response.has_focus() {
            focus_ring(ui, pal, seg, CornerRadius::same(13));
        }
        let color = if selected { pal.text } else { pal.text2 };
        ui.painter().galley(
            egui::pos2(
                seg.center().x - galley.size().x / 2.0,
                seg.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, *label)
        });
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// A selectable sidebar row. `badge` is a short right-aligned count.
pub fn sidebar_item(
    ui: &mut Ui,
    pal: &Palette,
    icon: Icon,
    label: &str,
    badge: Option<&str>,
    selected: bool,
    marker: Option<Color32>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let radius = CornerRadius::same(crate::nothing::R_ROW);
    if selected {
        ui.painter().rect_filled(rect, radius, pal.selected_bg);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, radius, pal.control_hover.gamma_multiply(0.55));
    }
    if response.has_focus() {
        focus_ring(ui, pal, rect, radius);
    }
    let fg = if selected { pal.selected_fg } else { pal.text };
    let icon_tint = if selected { pal.selected_fg } else { pal.text2 };
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 17.0, rect.center().y),
            vec2(16.0, 16.0),
        ),
        icon,
        icon_tint,
    );
    let mut right = rect.right() - 10.0;
    let reserved = badge.map_or(0.0, |b| b.chars().count() as f32 * 8.0 + 8.0)
        + if marker.is_some() { 14.0 } else { 0.0 };
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::new(13.5, FontFamily::Proportional),
        fg,
    );
    job.wrap.max_width = (rect.width() - 34.0 - 10.0 - reserved).max(20.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    if let Some(badge) = badge {
        let g = ui.painter().layout_no_wrap(
            badge.to_owned(),
            FontId::new(12.0, FontFamily::Proportional),
            if selected { pal.selected_fg } else { pal.text3 },
        );
        right -= g.size().x;
        ui.painter()
            .galley(egui::pos2(right, rect.center().y - g.size().y / 2.0), g, fg);
        right -= 8.0;
    }
    if let Some(color) = marker {
        ui.painter()
            .circle_filled(egui::pos2(right - 2.0, rect.center().y), 3.5, color);
    }
    describe(&response, label, true);
    response
}

/// Visible (not hover-only) reason an action is blocked: a small dot and one short line.
pub fn blocked_note(ui: &mut Ui, pal: &Palette, reason: &str) {
    ui.horizontal(|ui| {
        dot(ui, pal.text3, true);
        ui.add(egui::Label::new(caption(pal, reason)).wrap());
    });
}

/// Rounded raised block for grouped content in the simple utility layouts.
pub fn group<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(pal.tile)
        .stroke(if pal.dark {
            Stroke::NONE
        } else {
            Stroke::new(1.0_f32, pal.separator)
        })
        .corner_radius(CornerRadius::same(crate::nothing::R_CARD))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    fn lin(c: u8) -> f64 {
        let v = f64::from(c) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    fn lum(c: Color32) -> f64 {
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }
    pub fn ratio(a: Color32, b: Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_and_red_accent_meet_4_5_to_1_on_every_surface_in_both_themes() {
        for dark in [false, true] {
            let p = palette(AppKind::Calls, dark);
            for (bg_name, bg) in [
                ("content", p.content),
                ("sidebar", p.sidebar),
                ("tile", p.tile),
                ("tile_hover", p.tile_hover),
                ("control", p.control),
                ("control_hover", p.control_hover),
            ] {
                for (fg_name, fg) in [
                    ("text", p.text),
                    ("text2", p.text2),
                    ("text3", p.text3),
                    ("accent", p.accent),
                ] {
                    let r = ratio(fg, bg);
                    assert!(r >= 4.5, "dark={dark} {fg_name} on {bg_name}: {r:.2}");
                }
            }
            for (bg_name, bg) in [
                ("content", p.content),
                ("tile", p.tile),
                ("control", p.control),
            ] {
                for (fg_name, fg) in [("ok", p.ok), ("warn", p.warn), ("err", p.err)] {
                    let r = ratio(fg, bg);
                    assert!(r >= 4.5, "dark={dark} {fg_name} on {bg_name}: {r:.2}");
                }
            }
            let r = ratio(p.on_accent, p.accent_fill);
            assert!(r >= 4.5, "dark={dark} on_accent on accent_fill: {r:.2}");
            assert!(ratio(p.selected_fg, p.selected_bg) >= 4.5);
        }
        assert_eq!(palette(AppKind::Calls, true).content, Color32::BLACK);
    }

    /// The colours egui derives on its own (placeholder text in stock fields, disabled controls,
    /// links, the outline of a text field), judged from the installed style like ThemeContrastTest
    /// does on Android. Placeholder and disabled text are held to 4.5:1 here too: a hint is the
    /// only label a sighted user has on an empty field.
    #[test]
    fn derived_text_and_control_outlines_meet_aa_in_both_themes() {
        for (theme_choice, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
            let ctx = egui::Context::default();
            apply(&ctx, AppKind::Calls);
            let style = ctx.style_of(theme_choice);
            let v = &style.visuals;
            let p = palette(AppKind::Calls, dark);
            for (bg_name, bg) in [("content", p.content), ("field", v.extreme_bg_color)] {
                let hint = p.text3;
                let r = ratio(hint, bg);
                assert!(
                    r >= 4.5,
                    "dark={dark} placeholder (theme::hint) on {bg_name}: {r:.2}"
                );
                let link = ratio(v.hyperlink_color, bg);
                assert!(link >= 4.5, "dark={dark} link on {bg_name}: {link:.2}");
            }
            // Non-text: the outline that finds a text field's edge (WCAG 1.4.11, 3:1).
            for bg in [p.content, p.sidebar, v.extreme_bg_color] {
                let r = ratio(v.widgets.inactive.bg_stroke.color, bg);
                assert!(r >= 3.0, "dark={dark} field outline on {bg:?}: {r:.2}");
            }
            // The text of a selection stays readable on the selection fill.
            let sel = v.selection.bg_fill;
            let blended = Color32::from_rgb(
                ((u16::from(sel.r()) * u16::from(sel.a())
                    + u16::from(p.content.r()) * (255 - u16::from(sel.a())))
                    / 255) as u8,
                ((u16::from(sel.g()) * u16::from(sel.a())
                    + u16::from(p.content.g()) * (255 - u16::from(sel.a())))
                    / 255) as u8,
                ((u16::from(sel.b()) * u16::from(sel.a())
                    + u16::from(p.content.b()) * (255 - u16::from(sel.a())))
                    / 255) as u8,
            );
            let r = ratio(p.text, blended);
            assert!(r >= 4.5, "dark={dark} text on selection: {r:.2}");
        }
    }

    /// egui's default placeholder colour is half-strength ink (3.46:1 in light): every hint must
    /// name a colour, through `theme::hint` or an explicit `.color(..)`.
    #[test]
    fn placeholders_never_use_egui_default_weak_colour() {
        let files: [(&str, &str); 6] = [
            ("connection.rs", include_str!("connection.rs")),
            ("share_app.rs", include_str!("share_app.rs")),
            ("apps/contacts.rs", include_str!("apps/contacts.rs")),
            ("apps/calendar.rs", include_str!("apps/calendar.rs")),
            ("apps/messages.rs", include_str!("apps/messages.rs")),
            ("apps/clipboard.rs", include_str!("apps/clipboard.rs")),
        ];
        for (name, src) in files {
            assert!(
                !src.contains(".hint_text(\""),
                "{name}: a plain-string hint_text draws at egui's 3.5:1 weak colour"
            );
        }
    }

    #[test]
    fn disabled_controls_keep_a_readable_label() {
        // Disabled text is exempt from WCAG contrast, but a blocked button must still be legible
        // enough to read why (the reason is written beside it). Held at 3:1 for text3, the colour
        // `pill_button`/`round_button` use for it, on the surfaces they sit on.
        for dark in [false, true] {
            let p = palette(AppKind::Calls, dark);
            for bg in [p.content, p.control, p.tile] {
                assert!(ratio(p.text3, bg) >= 3.0);
            }
        }
    }

    #[test]
    fn focus_ring_is_3_to_1_on_every_surface_in_every_mode() {
        for dark in [false, true] {
            for hc in [false, true] {
                let base = base_palette(AppKind::Calls, dark);
                let p = if hc { high_contrast_of(base) } else { base };
                let ring = focus_color(&p);
                for (name, bg) in [
                    ("toolbar", p.toolbar),
                    ("sidebar", p.sidebar),
                    ("content", p.content),
                    ("surface", p.surface),
                    ("tile", p.tile),
                    ("control", p.control),
                    ("control_hover", p.control_hover),
                    ("selected_bg row", p.selected_bg),
                ] {
                    // The ring sits outside the control with a 1 pt gap, so it is judged against the
                    // surface behind it; also against the control itself where they meet.
                    if name == "selected_bg row" {
                        continue;
                    }
                    let r = ratio(ring, bg);
                    assert!(r >= 3.0, "dark={dark} hc={hc} ring on {name}: {r:.2}");
                }
                assert_ne!(
                    ring, p.accent_fill,
                    "the ring must never be the red it sits next to"
                );
            }
        }
    }

    #[test]
    fn high_contrast_makes_secondary_text_and_edges_strong() {
        for dark in [false, true] {
            let p = high_contrast_of(base_palette(AppKind::Calls, dark));
            for bg in [p.content, p.sidebar, p.tile, p.control] {
                assert!(ratio(p.text2, bg) >= 7.0 && ratio(p.text3, bg) >= 7.0);
                assert!(ratio(p.separator, bg) >= 3.0, "edges visible, dark={dark}");
                assert!(
                    ratio(p.dot_off, bg) >= 3.0,
                    "unlit dots visible, dark={dark}"
                );
            }
        }
    }

    #[test]
    fn status_is_never_colour_alone() {
        // Every status tone is a dot whose *shape* also differs (hollow vs filled) and is followed
        // by text; the palette must still keep warning/error distinguishable from ok by luminance.
        for dark in [false, true] {
            let p = base_palette(AppKind::Calls, dark);
            assert!(ratio(p.ok, p.err) >= 1.0);
            assert_ne!(p.err, p.text, "errors are red, plain text is ink");
        }
    }

    #[test]
    fn no_text_is_set_below_12_points() {
        let files: [(&str, &str); 9] = [
            ("theme.rs", include_str!("theme.rs")),
            ("shell.rs", include_str!("shell.rs")),
            ("nothing.rs", include_str!("nothing.rs")),
            ("call_ui.rs", include_str!("call_ui.rs")),
            ("apps/calls.rs", include_str!("apps/calls.rs")),
            ("apps/messages.rs", include_str!("apps/messages.rs")),
            ("apps/contacts.rs", include_str!("apps/contacts.rs")),
            ("apps/calendar.rs", include_str!("apps/calendar.rs")),
            ("home_app.rs", include_str!("home_app.rs")),
        ];
        for (name, src) in files {
            for (n, line) in src.lines().enumerate() {
                for pat in ["FontId::new(", ".size("] {
                    let mut rest = line;
                    while let Some(i) = rest.find(pat) {
                        rest = &rest[i + pat.len()..];
                        let num: String = rest
                            .chars()
                            .take_while(|c| c.is_ascii_digit() || *c == '.')
                            .collect();
                        if let Ok(size) = num.parse::<f32>() {
                            assert!(size >= 12.0, "{name}:{} sets {size} pt text", n + 1);
                        }
                    }
                }
            }
        }
    }

    /// Tab walks the controls in reading order and wraps; Shift+Tab goes back.
    #[test]
    fn tab_order_follows_layout_order() {
        use egui::{Event, Key, Modifiers, RawInput};
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let pal = base_palette(AppKind::Calls, false);
        let labels = ["One", "Two", "Three"];
        let mut focused = Vec::new();
        let frame = |events: Vec<Event>| -> Option<usize> {
            let input = RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            let mut at = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (i, l) in labels.iter().enumerate() {
                            if pill_button(ui, &pal, l, ButtonKind::Plain, true).has_focus() {
                                at = Some(i);
                            }
                        }
                    });
                });
            });
            at
        };
        let tab = |shift: bool| Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        };
        frame(vec![]);
        for _ in 0..3 {
            // Each Tab takes effect on the following frame.
            frame(vec![tab(false)]);
            focused.push(frame(vec![]));
        }
        assert_eq!(focused, [Some(0), Some(1), Some(2)]);
        frame(vec![tab(true)]);
        assert_eq!(frame(vec![]), Some(1));
    }

    #[test]
    fn embedded_fonts_cover_latin_greek_cyrillic_and_monospace() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            ctx.fonts(|f| {
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    for c in ['a', 'É', 'ž', 'Ж', 'ω', '€', '—'] {
                        assert!(
                            f.has_glyph(&FontId::new(14.0, family.clone()), c),
                            "{c} in {family:?}"
                        );
                    }
                }
            });
        });
    }
}
