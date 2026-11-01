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
use std::{path::PathBuf, sync::Arc};

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
    pub on_accent: Color32,
    pub selected_bg: Color32,
    pub selected_fg: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    /// Recessed display (Messages status display).
    pub lcd_bg: Color32,
    pub lcd_off: Color32,
}

pub fn palette(app: AppKind, dark: bool) -> Palette {
    match (app, dark) {
        (AppKind::Messages, false) => Palette {
            dark,
            toolbar: rgb(0xEDEDEA),
            sidebar: rgb(0xF3F3F0),
            content: rgb(0xFAFAF8),
            surface: rgb(0xFFFFFF),
            text: rgb(0x0C0C0C),
            text2: rgb(0x6B6B66),
            text3: rgb(0xA4A49E),
            separator: rgb(0xDADAD4),
            control: rgb(0xE6E6E1),
            control_hover: rgb(0xDCDCD6),
            control_active: rgb(0xCFCFC8),
            accent: rgb(0xD71921),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0x0C0C0C),
            selected_fg: rgb(0xFAFAF8),
            ok: rgb(0x2E7D4F),
            warn: rgb(0xA86400),
            err: rgb(0xD71921),
            lcd_bg: rgb(0xE2E2DD),
            lcd_off: rgb(0xD3D3CD),
        },
        (AppKind::Messages, true) => Palette {
            dark,
            toolbar: rgb(0x141414),
            sidebar: rgb(0x0B0B0B),
            content: rgb(0x000000),
            surface: rgb(0x151515),
            text: rgb(0xF4F4F1),
            text2: rgb(0x9A9A95),
            text3: rgb(0x5E5E5A),
            separator: rgb(0x262624),
            control: rgb(0x222221),
            control_hover: rgb(0x2D2D2B),
            control_active: rgb(0x3A3A37),
            accent: rgb(0xFF3B30),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0xF4F4F1),
            selected_fg: rgb(0x0C0C0C),
            ok: rgb(0x58C486),
            warn: rgb(0xE0A030),
            err: rgb(0xFF3B30),
            lcd_bg: rgb(0x0A0A0A),
            lcd_off: rgb(0x242422),
        },
        (_, false) => Palette {
            dark,
            toolbar: rgb(0xECECEE),
            sidebar: rgb(0xF2F2F4),
            content: rgb(0xFFFFFF),
            surface: rgb(0xF4F4F6),
            text: rgb(0x1D1D1F),
            text2: rgb(0x6E6E73),
            text3: rgb(0xA1A1A6),
            separator: rgb(0xD9D9DD),
            control: rgb(0xE6E6EA),
            control_hover: rgb(0xDCDCE1),
            control_active: rgb(0xCECED5),
            accent: rgb(0x0A7AFF),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0xD9D9DE),
            selected_fg: rgb(0x1D1D1F),
            ok: rgb(0x2DB04F),
            warn: rgb(0xB26A00),
            err: rgb(0xD93025),
            lcd_bg: rgb(0xE6E6EA),
            lcd_off: rgb(0xD2D2D8),
        },
        (_, true) => Palette {
            dark,
            toolbar: rgb(0x2A2A2D),
            sidebar: rgb(0x232326),
            content: rgb(0x1C1C1E),
            surface: rgb(0x2A2A2D),
            text: rgb(0xF2F2F7),
            text2: rgb(0xA0A0A8),
            text3: rgb(0x6C6C72),
            separator: rgb(0x3A3A3E),
            control: rgb(0x3A3A3E),
            control_hover: rgb(0x46464B),
            control_active: rgb(0x55555B),
            accent: rgb(0x0A84FF),
            on_accent: Color32::WHITE,
            selected_bg: rgb(0x3C3C41),
            selected_fg: rgb(0xF2F2F7),
            ok: rgb(0x32D74B),
            warn: rgb(0xFFB340),
            err: rgb(0xFF453A),
            lcd_bg: rgb(0x2A2A2D),
            lcd_off: rgb(0x3A3A3E),
        },
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

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut regular_name = None;
    let mut bold_name = None;
    for face in candidate_faces() {
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
            v.selection.bg_fill = pal.accent.gamma_multiply(if dark { 0.45 } else { 0.28 });
            v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
            v.text_cursor.stroke = Stroke::new(1.5_f32, pal.accent);
            let radius = CornerRadius::same(7);
            for (w, fill) in [
                (&mut v.widgets.inactive, pal.control),
                (&mut v.widgets.hovered, pal.control_hover),
                (&mut v.widgets.active, pal.control_active),
            ] {
                w.bg_fill = fill;
                w.weak_bg_fill = fill;
                w.bg_stroke = Stroke::NONE;
                w.corner_radius = radius;
                w.fg_stroke = Stroke::new(1.0_f32, pal.text);
            }
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.separator);
            v.widgets.noninteractive.corner_radius = radius;
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text2);
            v.widgets.open.corner_radius = radius;
            v.window_corner_radius = CornerRadius::same(10);
            v.window_stroke = Stroke::new(1.0_f32, pal.separator);
            style.visuals = v;
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
pub fn caption(pal: &Palette, text: impl Into<String>) -> RichText {
    secondary(pal, text).size(12.0)
}
/// Small uppercase section label, as used above sidebar groups and form fields.
pub fn overline(pal: &Palette, text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .font(FontId::new(10.5, semibold_family()))
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
        Tone::Info => pal.warn,
        Tone::Warning => pal.warn,
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

fn focus_ring(ui: &Ui, pal: &Palette, rect: egui::Rect, radius: impl Into<CornerRadius>) {
    ui.painter().rect_stroke(
        rect.expand(1.0),
        radius,
        Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.7)),
        StrokeKind::Outside,
    );
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
}

/// Rounded pill button. Disabled buttons stay visible but muted.
pub fn pill_button(
    ui: &mut Ui,
    pal: &Palette,
    text: &str,
    kind: ButtonKind,
    enabled: bool,
) -> Response {
    let (fill, fg) = match (kind, enabled) {
        (ButtonKind::Primary, true) => (pal.accent, pal.on_accent),
        (ButtonKind::Primary, false) => (pal.control, pal.text3),
        (ButtonKind::Plain, true) => (pal.control, pal.text),
        (ButtonKind::Plain, false) => (pal.control.gamma_multiply(0.6), pal.text3),
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::new(13.5, semibold_family()), fg);
    let size = vec2(galley.size().x + 28.0, 30.0);
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
            ButtonKind::Primary => fill.gamma_multiply(0.92),
            ButtonKind::Plain => pal.control_hover,
        }
    } else {
        fill
    };
    let radius = CornerRadius::same(15);
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
        ui.painter().circle_stroke(
            rect.center(),
            diameter / 2.0 + 2.0,
            Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.7)),
        );
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

/// macOS-style segmented control. Returns true when the selection changed.
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
    let total = vec2(seg_w * options.len() as f32 + 4.0, 26.0);
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
    let track = CornerRadius::same(8);
    ui.painter().rect_filled(rect, track, pal.control);
    let mut changed = false;
    for (i, ((option, label), galley)) in options.iter().zip(galleys).enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 2.0 + seg_w * i as f32, rect.top() + 2.0),
            vec2(seg_w, 22.0),
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
            ui.painter().rect_filled(seg, CornerRadius::same(6), fill);
            ui.painter().rect_stroke(
                seg,
                CornerRadius::same(6),
                Stroke::new(1.0_f32, pal.separator),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter().rect_filled(
                seg,
                CornerRadius::same(6),
                pal.control_hover.gamma_multiply(0.6),
            );
        }
        if response.has_focus() {
            focus_ring(ui, pal, seg, CornerRadius::same(6));
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
    let radius = CornerRadius::same(7);
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
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.separator))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}
