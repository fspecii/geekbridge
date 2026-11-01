//! Agenda: a people view and a day view. Dates and the "now" line come from this computer's
//! clock; people and events stay empty until a phone shares them.
use crate::{
    icons::{self, Icon},
    localtime::{self, Date, Now},
    phone_feed::{self, Feed},
    phone_ui,
    shell::{AppView, Env, central},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    phone_view::{AgendaView, EventItem, Person},
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
    feed: Option<Feed<AgendaView>>,
    query: String,
    /// (device id, contact id) of the person shown on the right.
    person: Option<(String, String)>,
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
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        let view = state.view.clone().unwrap_or_default();
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
                        ui.label(theme::secondary(pal, view.people_total.to_string()));
                    });
                });
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
                crate::input::limit_chars(&mut self.query, 64);
                ui.add_space(8.0);
                if view.people.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        badge(ui, pal, Icon::Person);
                        ui.add_space(6.0);
                        ui.label(theme::semibold("No people yet", 14.0));
                        ui.add(
                            egui::Label::new(theme::caption(pal, self.empty_people(&state, &view)))
                                .wrap(),
                        );
                    });
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("people-list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for p in &view.people {
                            let selected = self
                                .person
                                .as_ref()
                                .is_some_and(|(d, i)| *d == p.device_id && *i == p.id);
                            if theme::sidebar_item(
                                ui,
                                pal,
                                Icon::Person,
                                &p.name,
                                None,
                                selected,
                                None,
                            )
                            .clicked()
                            {
                                self.person = Some((p.device_id.clone(), p.id.clone()));
                            }
                        }
                        if view.people_total > view.people.len() {
                            ui.add_space(6.0);
                            ui.label(theme::caption(
                                pal,
                                format!(
                                    "Showing {} of {}. Search to narrow down.",
                                    view.people.len(),
                                    view.people_total
                                ),
                            ));
                        }
                    });
            });
        central(ctx, pal, Margin::same(24), |ui| {
            let chosen = self
                .person
                .as_ref()
                .and_then(|(d, i)| view.people.iter().find(|p| p.device_id == *d && p.id == *i));
            match chosen {
                Some(p) => person_card(ui, pal, p),
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(theme::semibold("No Person Selected", 17.0).color(pal.text3));
                    });
                }
            }
        });
    }

    fn empty_people(&self, state: &phone_feed::FeedState<AgendaView>, view: &AgendaView) -> String {
        if let Some(problem) = &state.problem {
            return problem.clone();
        }
        if state.view.is_none() {
            return "Looking for your phone's contacts ...".into();
        }
        if !self.query.is_empty() && view.devices.iter().any(|d| d.contacts_count > 0) {
            return "Nobody matches this search.".into();
        }
        view.devices
            .iter()
            .find_map(|d| phone_ui::state_line(d.contacts, "Contacts"))
            .unwrap_or_else(|| {
                "Nothing has been shared from a phone yet. Allow Contacts for a paired phone below."
                    .into()
            })
    }

    /// Bottom strip: sync status, plus the explicit "Allow ..." buttons for phones that have not
    /// allowed this kind of data yet, plus a refresh button.
    fn consent_strip(
        &mut self,
        ctx: &egui::Context,
        env: &Env,
        view: &AgendaView,
        feature: Feature,
        noun: &str,
    ) {
        let pal = env.pal;
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        let mut action = None;
        egui::TopBottomPanel::bottom("agenda-consent")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for d in &view.devices {
                        ui.label(theme::caption(pal, phone_ui::device_summary(d)));
                    }
                    if let Some(n) = state.notice.as_ref().or(state.problem.as_ref()) {
                        ui.label(theme::caption(pal, n.clone()));
                    }
                    if !view.devices.is_empty()
                        && theme::pill_button(ui, pal, "Refresh", ButtonKind::Plain, true).clicked()
                    {
                        action = Some(Request::AgendaRefresh { device_id: None });
                    }
                    if let Some(r) = phone_ui::allow_buttons(ui, pal, env, feature, noun) {
                        action = Some(r);
                    }
                });
            });
        if let (Some(r), Some(feed)) = (action, &self.feed) {
            feed.run(r);
        }
    }

    fn day(&mut self, ctx: &egui::Context, env: &Env, now: &Now) {
        let pal = env.pal;
        let shown = self.shown(now);
        let is_today = shown == now.date;
        let view = self
            .feed
            .as_ref()
            .and_then(|f| f.state().view)
            .unwrap_or_default();
        self.consent_strip(ctx, env, &view, Feature::Calendar, "Calendar");
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
                    let note = day_note(&view, now.local_zone_known);
                    ui.label(theme::caption(pal, note));
                    ui.add_space(4.0);
                    let all_day: Vec<&EventItem> =
                        view.events.iter().filter(|e| e.all_day).collect();
                    for e in all_day.iter().take(4) {
                        ui.horizontal(|ui| {
                            theme::dot(ui, pal.accent, false);
                            ui.add(egui::Label::new(theme::semibold(&e.title, 13.0)).truncate());
                            ui.label(theme::caption(pal, "all day"));
                        });
                    }
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
                let offset = localtime::utc_offset_minutes(shown);
                let day_start = shown.to_days() * 86_400;
                let mut timed: Vec<&EventItem> =
                    view.events.iter().filter(|e| !e.all_day).collect();
                timed.sort_by_key(|e| e.start_ms);
                let mut column_ends: Vec<i64> = Vec::new();
                let placed: Vec<(&EventItem, usize)> = timed
                    .into_iter()
                    .map(|e| {
                        let col = column_ends
                            .iter()
                            .position(|end| *end <= e.start_ms)
                            .unwrap_or_else(|| {
                                column_ends.push(0);
                                column_ends.len() - 1
                            });
                        column_ends[col] = e.end_ms;
                        (e, col)
                    })
                    .collect();
                let columns = column_ends.len().max(1) as f32;
                let left = rect.left() + LABEL_WIDTH + 6.0;
                let lane = (rect.right() - 14.0 - left) / columns;
                for (e, col) in placed {
                    let local_start = e.start_ms / 1000 + i64::from(offset) * 60 - day_start;
                    let local_end = e.end_ms / 1000 + i64::from(offset) * 60 - day_start;
                    let y0 = top + (local_start.clamp(0, 86_400) as f32 / 3600.0) * HOUR_HEIGHT;
                    let y1 = top + (local_end.clamp(0, 86_400) as f32 / 3600.0) * HOUR_HEIGHT;
                    let block = egui::Rect::from_min_max(
                        egui::pos2(left + lane * col as f32, y0 + 1.0),
                        egui::pos2(
                            left + lane * (col as f32 + 1.0) - 3.0,
                            (y1 - 1.0).max(y0 + 20.0),
                        ),
                    );
                    painter.rect_filled(
                        block,
                        CornerRadius::same(6),
                        pal.accent.gamma_multiply(0.16),
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_size(block.min, vec2(3.0, block.height())),
                        CornerRadius::same(2),
                        pal.accent,
                    );
                    let clip = painter.with_clip_rect(block.shrink(2.0));
                    clip.text(
                        block.min + vec2(9.0, 3.0),
                        Align2::LEFT_TOP,
                        &e.title,
                        FontId::new(12.5, theme::semibold_family()),
                        pal.text,
                    );
                    let detail = match &e.location {
                        Some(l) => {
                            format!("{} · {}", localtime::clock_label(e.start_ms, offset), l)
                        }
                        None => localtime::clock_label(e.start_ms, offset),
                    };
                    clip.text(
                        block.min + vec2(9.0, 19.0),
                        Align2::LEFT_TOP,
                        detail,
                        FontId::new(11.0, FontFamily::Proportional),
                        pal.text2,
                    );
                }
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

fn day_note(view: &AgendaView, zone_known: bool) -> String {
    let zone = if zone_known {
        ""
    } else {
        " Dates use UTC because this computer's time zone couldn't be read."
    };
    if !view.events.is_empty() {
        return format!(
            "{} event{} today.{zone}",
            view.events.len(),
            if view.events.len() == 1 { "" } else { "s" }
        );
    }
    let line = view
        .devices
        .iter()
        .find_map(|d| phone_ui::state_line(d.calendar, "Calendar"));
    match line {
        Some(l) => format!("No events to show. {l}{zone}"),
        None if view.devices.is_empty() => {
            format!("No events to show. Allow Calendar for a paired phone in the People tab.{zone}")
        }
        None => format!("No events on this day.{zone}"),
    }
}

fn person_card(ui: &mut egui::Ui, pal: &Palette, p: &Person) {
    ui.label(theme::semibold(&p.name, 24.0));
    if let Some(o) = &p.organization {
        ui.label(theme::secondary(pal, o));
    }
    ui.add_space(12.0);
    theme::hairline(ui, pal);
    ui.add_space(8.0);
    egui::Grid::new("person-info")
        .num_columns(2)
        .spacing([22.0, 10.0])
        .show(ui, |ui| {
            for phone in &p.phones {
                ui.label(theme::overline(pal, "Phone"));
                ui.add(
                    egui::Label::new(egui::RichText::new(phone).color(pal.text)).selectable(true),
                );
                ui.end_row();
            }
            for mail in &p.emails {
                ui.label(theme::overline(pal, "Email"));
                ui.add(
                    egui::Label::new(egui::RichText::new(mail).color(pal.text)).selectable(true),
                );
                ui.end_row();
            }
        });
    ui.add_space(14.0);
    ui.label(theme::caption(
        pal,
        "Read-only copy from your phone, kept in memory only.",
    ));
}

impl AppView for Agenda {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let now = localtime::now();
        let shown = self.shown(&now);
        let request = Request::AgendaView {
            query: self.query.clone(),
            day: format!("{:04}-{:02}-{:02}", shown.year, shown.month, shown.day),
            utc_offset_minutes: localtime::utc_offset_minutes(shown),
        };
        match &self.feed {
            Some(feed) => feed.set_params(request),
            None => {
                let repaint = ctx.clone();
                self.feed = Some(phone_feed::agenda_feed(
                    move || repaint.request_repaint(),
                    request,
                ));
            }
        }
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
