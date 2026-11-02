// SPDX-License-Identifier: GPL-3.0-or-later
//! Dot-matrix month grid, the desktop twin of the Android Calendar card: weekday initials in the
//! dot font, one dim dot per day, a bright dot where something is scheduled, and a red disc on
//! today. Drawn once per change; no animation.
use crate::{calendar_model as model, dotmatrix, localtime::Date, theme::Palette};
use eframe::egui::{self, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};

const HEAD_H: f32 = 22.0;
const ROW_H: f32 = 26.0;

/// Height needed for a six-row month at the given width.
pub fn height() -> f32 {
    HEAD_H + ROW_H * 6.0
}

/// Draws the month containing `shown`. `busy(day)` says whether the day has events. Returns the
/// day the user clicked (or activated from the keyboard), if any.
pub fn dot_month(
    ui: &mut Ui,
    pal: &Palette,
    shown: Date,
    today: Date,
    selected: Option<Date>,
    busy: &dyn Fn(Date) -> bool,
) -> Option<Date> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height()), Sense::hover());
    let cell_w = rect.width() / 7.0;
    let painter = ui.painter().clone();
    for (i, letter) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
        let pitch = 1.7;
        let size = dotmatrix::measure(letter, pitch);
        let x = rect.left() + cell_w * (i as f32 + 0.5) - size.x / 2.0;
        dotmatrix::paint(
            &painter,
            pos2(x, rect.top() + (HEAD_H - size.y) / 2.0 - 2.0),
            letter,
            pitch,
            pal.text2,
            None,
        );
    }
    let mut picked = None;
    let top = rect.top() + HEAD_H;
    for (w, week) in model::month_grid(shown).iter().enumerate() {
        for (i, day) in week.iter().enumerate() {
            let cell = Rect::from_min_size(
                pos2(rect.left() + cell_w * i as f32, top + ROW_H * w as f32),
                vec2(cell_w, ROW_H),
            );
            let in_month = day.month == shown.month;
            let is_today = *day == today;
            let has = busy(*day);
            let resp = ui.interact(
                cell,
                egui::Id::new(("dot-month", day.to_days())),
                Sense::click(),
            );
            let c = cell.center();
            if is_today {
                painter.circle_filled(c, 7.5, pal.accent_fill);
            } else if !in_month {
                painter.circle_filled(c, 1.6, pal.dot_off);
            } else if has {
                painter.circle_filled(c, 3.3, pal.text);
            } else {
                painter.circle_filled(c, 2.4, pal.text3.gamma_multiply(0.55));
            }
            if is_today && has {
                painter.circle_filled(c, 2.4, pal.on_accent);
            }
            if selected == Some(*day) && !is_today {
                painter.circle_stroke(c, 8.0, Stroke::new(1.3_f32, pal.text2));
            }
            if resp.hovered() && !is_today {
                painter.circle_stroke(c, 8.0, Stroke::new(1.0_f32, pal.separator));
            }
            if resp.has_focus() {
                crate::theme::focus_circle(ui, pal, c, 8.0);
            }
            resp.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    true,
                    format!(
                        "{} {}{}{}",
                        day.day,
                        day.month_name(),
                        if is_today { ", today" } else { "" },
                        if has { ", has events" } else { "" }
                    ),
                )
            });
            if resp.clicked() {
                picked = Some(*day);
            }
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_height_is_six_rows_plus_header() {
        assert_eq!(height(), HEAD_H + 6.0 * ROW_H);
    }
}
