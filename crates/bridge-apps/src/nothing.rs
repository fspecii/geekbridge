//! Design tokens and widget-like components for the Nothing-inspired look: rounded-square
//! tiles, dot-matrix titles and numerals, dotted status/progress rows. Everything is painted in
//! code with the shared dot-matrix renderer; no fonts, images or logos are involved.
//!
//! Painting is event-driven: nothing here animates or requests repaints.
use crate::{dotmatrix, theme::Palette};
use eframe::egui::{
    self, Color32, CornerRadius, Margin, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    WidgetInfo, WidgetType, pos2, vec2,
};

// ---------------------------------------------------------------- tokens

/// Corner radii. Tiles are "rounded squares"; sheets and cards sit one step below.
pub const R_TILE: u8 = 22;
pub const R_CARD: u8 = 16;
pub const R_ROW: u8 = 12;

/// 4-pt spacing scale.
pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S5: f32 = 24.0;

/// Type scale (points) for the system UI font: title, headline, body, footnote, caption.
pub const T_TITLE: f32 = 22.0;
pub const T_HEAD: f32 = 15.0;
pub const T_BODY: f32 = 14.0;
pub const T_NOTE: f32 = 12.5;

/// Dot pitch (distance between dot centres) for the three dot-matrix sizes at 100% text.
pub const PITCH_NUMERAL: f32 = 5.0;
pub const PITCH_TITLE: f32 = 3.2;
pub const PITCH_LABEL: f32 = 2.2;

/// Dot-matrix pitch follows the user's text scale (body text size relative to the default).
pub fn scaled(ui: &Ui, pitch: f32) -> f32 {
    let body = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .map_or(T_BODY, |f| f.size);
    pitch * (body / T_BODY).clamp(0.8, 2.0)
}

// ---------------------------------------------------------------- dot-matrix text

/// A run of dot-matrix text. Reserves its size, paints lit dots in `color` and (optionally) the
/// unlit grid faintly, and is announced to screen readers as the plain text.
pub fn dot_text(ui: &mut Ui, pal: &Palette, text: &str, pitch: f32, color: Color32, grid: bool) {
    let pitch = scaled(ui, pitch);
    let size = dotmatrix::measure(text, pitch) + vec2(pitch * 0.8, pitch * 0.8);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let origin = pos2(rect.left() + pitch * 0.4, rect.top() + pitch * 0.4);
    let off = grid.then(|| pal.dot_off);
    dotmatrix::paint(ui.painter(), origin, text, pitch, color, off);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
}

/// Like [`dot_text`], but picks the longest candidate that fits `max_width`.
pub fn dot_text_fit(
    ui: &mut Ui,
    pal: &Palette,
    candidates: &[&str],
    pitch: f32,
    max_width: f32,
    color: Color32,
    grid: bool,
) {
    let p = scaled(ui, pitch);
    let text = dotmatrix::fit(candidates, p, max_width);
    dot_text(ui, pal, text, pitch, color, grid);
}

/// A row of dots, `lit` of `total` filled in `on`; the rest are the faint "off" dots.
pub fn dot_progress(ui: &mut Ui, pal: &Palette, total: usize, lit: usize, on: Color32, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    dotmatrix::meter(ui.painter(), rect, total, lit, on, pal.dot_off);
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::ProgressIndicator, true, format!("{lit} of {total}"))
    });
}

/// Small status dot with a label: filled when `on`, hollow ring otherwise. Red only when `alert`.
pub fn status_dot(ui: &mut Ui, pal: &Palette, on: bool, alert: bool, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = S2;
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        let color = if alert { pal.accent_fill } else { pal.text };
        if on {
            ui.painter().circle_filled(rect.center(), 3.5, color);
        } else {
            ui.painter()
                .circle_stroke(rect.center(), 3.2, Stroke::new(1.3_f32, pal.text3));
        }
        ui.label(egui::RichText::new(label).size(T_NOTE).color(pal.text2));
    });
}

// ---------------------------------------------------------------- tiles

/// A rounded-square tile. `add` lays out the content inside a [`S4`] inset. Returns the tile's
/// own response, which reports clicks when `clickable`. Hover and press shade the fill; the
/// focus ring follows the keyboard.
pub fn tile(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    size: egui::Vec2,
    clickable: bool,
    add: impl FnOnce(&mut Ui),
) -> Response {
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let fill = if clickable && response.is_pointer_button_down_on() {
        pal.control_active
    } else if clickable && response.hovered() {
        pal.tile_hover
    } else {
        pal.tile
    };
    let radius = CornerRadius::same(R_TILE);
    ui.painter().rect_filled(rect, radius, fill);
    if !pal.dark {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, pal.separator),
            StrokeKind::Inside,
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            radius,
            Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.8)),
            StrokeKind::Outside,
        );
    }
    let inner = rect.shrink(S4);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.shrink(2.0));
    add(&mut child);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, clickable, label));
    response
}

/// Number of equal columns that fit `width` when each tile wants at least `min_tile` points.
pub fn columns_for(width: f32, min_tile: f32, gap: f32) -> usize {
    (((width + gap) / (min_tile + gap)).floor() as usize).clamp(1, 4)
}

/// Width of one of `cols` equal tiles in `width`.
pub fn tile_width(width: f32, cols: usize, gap: f32) -> f32 {
    ((width - gap * (cols as f32 - 1.0)) / cols as f32).max(40.0)
}

/// Section label above a group: small, uppercase, quiet.
pub fn section(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(S2);
    ui.label(crate::theme::overline(pal, text));
    ui.add_space(S1);
}

/// A card for grouped rows (settings, sheets): one step less round than a tile.
pub fn card<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(pal.tile)
        .stroke(if pal.dark {
            Stroke::NONE
        } else {
            Stroke::new(1.0_f32, pal.separator)
        })
        .corner_radius(CornerRadius::same(R_CARD))
        .inner_margin(Margin::same(S4 as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Rect helper for painting inside a tile corner.
pub fn top_right(rect: Rect, inset: f32) -> egui::Pos2 {
    pos2(rect.right() - inset, rect.top() + inset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_math_is_clamped_and_fills_the_width() {
        assert_eq!(columns_for(300.0, 220.0, 12.0), 1);
        assert_eq!(columns_for(480.0, 220.0, 12.0), 2);
        assert_eq!(columns_for(5000.0, 220.0, 12.0), 4);
        let w = tile_width(480.0, 2, 12.0);
        assert!((w * 2.0 + 12.0 - 480.0).abs() < 0.01);
    }
}
