//! Agenda: a people view and a day view. Dates and the "now" line come from this computer's
//! clock; people and events stay empty until a phone shares them.
use crate::{
    icons::{self, Icon},
    localtime::{self, Date, Now},
    shell::{AppView, Env, central},
    theme::{self, Palette},
};
use eframe::egui::{self, Align2, CornerRadius, FontFamily, FontId, Margin, Sense, Stroke, vec2};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    People,
    Day,
}

#[derive(Default)]
pub struct Agenda {
    tab: Tab,
    /// None follows today, including across midnight.
    picked: Option<Date>,
    scroll_to_hour: Option<f32>,
}

const HOUR_HEIGHT: f32 = 52.0;
const LABEL_WIDTH: f32 = 66.0;

fn time_label(seconds: u32) -> String {
    format!("{:02}:{:02}", seconds / 3600, seconds / 60 % 60)
}

impl Agenda {
    fn shown(&self, now: &Now) -> Date {
        self.picked.unwrap_or(now.date)
    }

    fn select(&mut self, date: Date, now: &Now) {
        self.picked = (date != now.date).then_some(date);
        self.scroll_to_hour = Some(if date == now.date {
            (now.seconds_into_day as f32 / 3600.0 - 2.0).max(0.0)
        } else {
            7.0
        });
    }

    fn day_nav(&mut self, ui: &mut egui::Ui, pal: &Palette, now: &Now) {
        let shown = self.shown(now);
        if theme::tool_button(ui, pal, Icon::ChevronRight, None, true, "Next day").clicked() {
            self.select(shown.add_days(1), now);
        }
        if theme::tool_button(
            ui,
            pal,
            Icon::Calendar,
            Some("Today"),
            true,
            "Jump to today",
        )
        .clicked()
        {
            self.select(now.date, now);
        }
        if theme::tool_button(ui, pal, Icon::ChevronLeft, None, true, "Previous day").clicked() {
            self.select(shown.add_days(-1), now);
        }
    }

    fn people(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        egui::SidePanel::left("people")
            .exact_width(250.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(14, 12)),
            )
            .show(ctx, |ui| {
                let r = ui.max_rect();
                ui.painter().vline(
                    r.right() + 14.0 + 0.5,
                    r.y_range().expand(20.0),
                    Stroke::new(1.0_f32, pal.separator),
                );
                ui.horizontal(|ui| {
                    ui.label(theme::semibold("All People", 15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(theme::secondary(pal, "0"));
                    });
                });
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(20.0);
                    badge(ui, pal, Icon::Person);
                    ui.add_space(6.0);
                    ui.label(theme::semibold("No people yet", 14.0));
                    ui.add(
                        egui::Label::new(theme::caption(
                            pal,
                            "Nothing has been synced from a phone.",
                        ))
                        .wrap(),
                    );
                });
            });
        central(ctx, pal, Margin::same(24), |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(theme::semibold("No Person Selected", 17.0).color(pal.text3));
            });
        });
    }

    fn day(&mut self, ctx: &egui::Context, env: &Env, now: &Now) {
        let pal = env.pal;
        let shown = self.shown(now);
        let is_today = shown == now.date;
        central(ctx, pal, Margin::ZERO, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(20, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::semibold(shown.day.to_string(), 38.0));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(theme::semibold(shown.weekday().name(), 15.0));
                            ui.label(theme::secondary(
                                pal,
                                format!("{} {}", shown.month_name(), shown.year),
                            ));
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(theme::caption(
                        pal,
                        if now.local_zone_known {
                            "No events to show. Calendar sync isn't available yet."
                        } else {
                            "No events to show. Dates use UTC because this computer's time zone couldn't be read."
                        },
                    ));
                    ui.add_space(8.0);
                    self.week_strip(ui, pal, now);
                });
            theme::hairline(ui, pal);

            let mut area = egui::ScrollArea::vertical()
                .id_salt("day-scroll")
                .auto_shrink([false, false]);
            if let Some(hour) = self.scroll_to_hour.take() {
                area = area.vertical_scroll_offset(hour * HOUR_HEIGHT);
            }
            area.show(ui, |ui| {
                let width = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(vec2(width, HOUR_HEIGHT * 24.0 + 12.0), Sense::hover());
                let painter = ui.painter();
                let top = rect.top() + 8.0;
                for hour in 0..24 {
                    let y = top + hour as f32 * HOUR_HEIGHT;
                    painter.hline(
                        (rect.left() + LABEL_WIDTH)..=(rect.right() - 12.0),
                        y,
                        Stroke::new(1.0_f32, pal.separator),
                    );
                    let near_now = is_today
                        && (y - (top + now.seconds_into_day as f32 / 3600.0 * HOUR_HEIGHT)).abs()
                            < 12.0;
                    if hour > 0 && !near_now {
                        painter.text(
                            egui::pos2(rect.left() + LABEL_WIDTH - 10.0, y),
                            Align2::RIGHT_CENTER,
                            format!("{hour:02}:00"),
                            FontId::new(11.5, FontFamily::Proportional),
                            pal.text3,
                        );
                    }
                }
                painter.hline(
                    (rect.left() + LABEL_WIDTH)..=(rect.right() - 12.0),
                    top + 24.0 * HOUR_HEIGHT,
                    Stroke::new(1.0_f32, pal.separator),
                );
                if is_today {
                    let y = top + now.seconds_into_day as f32 / 3600.0 * HOUR_HEIGHT;
                    let red = pal.err;
                    painter.hline(
                        (rect.left() + LABEL_WIDTH - 4.0)..=(rect.right() - 12.0),
                        y,
                        Stroke::new(1.5_f32, red),
                    );
                    painter.circle_filled(egui::pos2(rect.left() + LABEL_WIDTH - 4.0, y), 3.5, red);
                    let chip = egui::Rect::from_center_size(
                        egui::pos2(rect.left() + LABEL_WIDTH - 12.0 - 21.0, y),
                        vec2(42.0, 18.0),
                    );
                    painter.rect_filled(chip, CornerRadius::same(9), red);
                    painter.text(
                        chip.center(),
                        Align2::CENTER_CENTER,
                        time_label(now.seconds_into_day),
                        FontId::new(11.0, theme::semibold_family()),
                        egui::Color32::WHITE,
                    );
                }
            });
        });
    }

    fn week_strip(&mut self, ui: &mut egui::Ui, pal: &Palette, now: &Now) {
        let shown = self.shown(now);
        let start = shown.week_start();
        let width = ui.available_width();
        let cell = width / 7.0;
        let (rect, _) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
        for i in 0..7 {
            let date = start.add_days(i);
            let cell_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left() + cell * i as f32, rect.top()),
                vec2(cell, 52.0),
            );
            let response = ui.interact(cell_rect, ui.id().with(("day", i)), Sense::click());
            let selected = date == shown;
            let today = date == now.date;
            let center = egui::pos2(cell_rect.center().x, cell_rect.top() + 36.0);
            if selected {
                ui.painter().circle_filled(
                    center,
                    14.0,
                    if today { pal.accent } else { pal.selected_bg },
                );
            } else if response.hovered() {
                ui.painter()
                    .circle_filled(center, 14.0, pal.control_hover.gamma_multiply(0.6));
            }
            if response.has_focus() {
                ui.painter().circle_stroke(
                    center,
                    16.0,
                    Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.7)),
                );
            }
            ui.painter().text(
                egui::pos2(cell_rect.center().x, cell_rect.top() + 9.0),
                Align2::CENTER_CENTER,
                date.weekday().short().to_uppercase(),
                FontId::new(10.5, theme::semibold_family()),
                pal.text3,
            );
            let fg = if selected {
                if today {
                    pal.on_accent
                } else {
                    pal.selected_fg
                }
            } else if today {
                pal.accent
            } else {
                pal.text
            };
            ui.painter().text(
                center,
                Align2::CENTER_CENTER,
                date.day.to_string(),
                FontId::new(14.0, FontFamily::Proportional),
                fg,
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!(
                        "{} {} {}",
                        date.weekday().name(),
                        date.day,
                        date.month_name()
                    ),
                )
            });
            if response.clicked() {
                self.select(date, now);
            }
        }
    }
}

fn badge(ui: &mut egui::Ui, pal: &Palette, icon: Icon) {
    let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 22.0, pal.control);
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), vec2(20.0, 20.0)),
        icon,
        pal.text3,
    );
}

impl AppView for Agenda {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let now = localtime::now();
        // Keep the "now" line and the date honest without repainting every frame.
        ctx.request_repaint_after(Duration::from_secs(20));
        theme::toolbar(ctx, pal, 46.0, |ui| {
            ui.horizontal_centered(|ui| {
                let before = self.tab;
                theme::segmented(
                    ui,
                    pal,
                    &mut self.tab,
                    &[(Tab::People, "People"), (Tab::Day, "Day")],
                );
                if before != self.tab && self.tab == Tab::Day && self.scroll_to_hour.is_none() {
                    self.scroll_to_hour =
                        Some((now.seconds_into_day as f32 / 3600.0 - 2.0).max(0.0));
                }
                if self.tab == Tab::Day {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.day_nav(ui, pal, &now);
                    });
                }
            });
        });
        match self.tab {
            Tab::People => self.people(ctx, env),
            Tab::Day => self.day(ctx, env, &now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_label_is_zero_padded() {
        assert_eq!(time_label(0), "00:00");
        assert_eq!(time_label(9 * 3600 + 5 * 60 + 59), "09:05");
        assert_eq!(time_label(23 * 3600 + 59 * 60), "23:59");
    }

    #[test]
    fn picking_today_follows_the_clock() {
        let now = Now {
            date: Date {
                year: 2026,
                month: 11,
                day: 1,
            },
            seconds_into_day: 10 * 3600,
            local_zone_known: true,
        };
        let mut agenda = Agenda::default();
        agenda.select(now.date.add_days(3), &now);
        assert_eq!(agenda.shown(&now), now.date.add_days(3));
        agenda.select(now.date, &now);
        assert_eq!(agenda.picked, None);
        assert_eq!(agenda.shown(&now), now.date);
    }
}
