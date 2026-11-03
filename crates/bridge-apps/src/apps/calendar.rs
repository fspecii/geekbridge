// SPDX-License-Identifier: GPL-3.0-or-later
//! Calendar: month, week, day and agenda-list views of the calendars the phone exposes. Google
//! Calendar shows up here because Google accounts sync into the phone's calendar provider; this
//! app never talks to Google. Reading is the default. Creating, changing and deleting events is
//! a separate per-phone permission and always names the calendar it writes to.
use crate::a11y::ScrollBars as _;
use crate::{
    calendar_model::{self as model, Fields},
    icons::Icon,
    localtime::{self, Date, Now},
    phone_feed::{self, Feed, FeedState},
    phone_ui,
    shell::{AppView, Env, central},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    phone_view::{
        AgendaView, CalendarEditAction, CalendarItem, DataState, EditItem, EditPhase, EventItem,
        edit_reason_text,
    },
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Margin, Rect, Sense, Stroke, pos2,
    vec2,
};
use std::{collections::HashSet, time::Duration};

/// Keyboard focus ring plus a screen-reader name for a hand-painted, clickable region.
fn focusable(ui: &egui::Ui, pal: &Palette, resp: &egui::Response, label: String) {
    if resp.has_focus() {
        theme::focus_ring(ui, pal, resp.rect, CornerRadius::same(4));
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
}

/// "Monday 3 November, 2 events" for a day cell.
fn day_label(day: Date, today: bool, count: usize) -> String {
    format!(
        "{} {} {}{}{}",
        day.weekday().short(),
        day.day,
        day.month_name(),
        if today { ", today" } else { "" },
        match count {
            0 => String::new(),
            1 => ", 1 event".to_owned(),
            n => format!(", {n} events"),
        }
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Mode {
    Day,
    Week,
    #[default]
    Month,
    List,
}

type Key = (String, String);

#[derive(Default)]
pub struct Calendar {
    mode: Mode,
    /// None follows today, including across midnight.
    picked: Option<Date>,
    feed: Option<Feed<AgendaView>>,
    /// (device id, calendar id) hidden with the sidebar toggles.
    hidden: HashSet<Key>,
    /// (device id, event instance id) shown in the details pane.
    selected: Option<Key>,
    editor: Option<Editor>,
    confirm_delete: bool,
    scroll_to_hour: Option<f32>,
    edit_seq: u64,
    aid_editor_done: bool,
    zone: Option<Option<String>>,
}

struct Editor {
    /// None = a new event; the explicit calendar choice is made in the form.
    existing: Option<Existing>,
    fields: Fields,
    calendar: Option<Key>,
    error: Option<String>,
}

struct Existing {
    device_id: String,
    event_id: String,
    calendar_label: String,
}

const HOUR_HEIGHT: f32 = 48.0;
const LABEL_WIDTH: f32 = 54.0;

/// Drawing context shared by the view helpers.
struct Cx<'a> {
    pal: &'a Palette,
    view: &'a AgendaView,
}

/// What a frame asked for; applied after drawing so the borrows stay simple.
#[derive(Default)]
struct Acts {
    select: Option<Key>,
    goto: Option<(Date, Mode)>,
    new_at: Option<(Date, u32)>,
}

/// First day and number of days to ask the helper for. One day of slack on each side keeps
/// events near midnight correct when the daylight-saving offset changes inside the window.
pub fn window(mode: Mode, shown: Date) -> (Date, u32) {
    match mode {
        Mode::Day => (shown.add_days(-1), 3),
        Mode::Week => (shown.week_start().add_days(-1), 9),
        Mode::Month => (model::month_grid(shown)[0][0].add_days(-1), 44),
        Mode::List => (shown.add_days(-1), 56),
    }
}

fn argb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn off(d: Date) -> i32 {
    localtime::utc_offset_minutes(d)
}

fn day_string(d: Date) -> String {
    model::fmt_date(d)
}

/// Why the event cannot be changed from the computer, or None when it can.
pub fn edit_block(view: &AgendaView, e: &EventItem) -> Option<String> {
    let dev = view.devices.iter().find(|d| d.device_id == e.device_id)?;
    match dev.calendar_edit {
        DataState::ConsentOff => {
            return Some(format!(
                "Editing is off for {}. Allow it in the sidebar.",
                dev.name
            ));
        }
        DataState::Offline => return Some("The phone is not connected right now.".into()),
        DataState::NotOffered => {
            return Some(
                "The phone does not offer editing. Turn on Edit calendar in the GeekBridge app on the phone."
                    .into(),
            );
        }
        DataState::Waiting | DataState::Ready => {}
    }
    if e.recurring {
        return Some("Repeating events can only be changed on the phone.".into());
    }
    if e.event_id.is_none() {
        return Some("This event cannot be named to the phone.".into());
    }
    let cal = view
        .calendars
        .iter()
        .find(|c| c.device_id == e.device_id && Some(&c.id) == e.calendar_id.as_ref());
    match cal {
        Some(c) if c.writable => None,
        Some(_) => Some("This calendar is read-only on the phone.".into()),
        None => Some("This event's calendar is not available for editing.".into()),
    }
}

fn long_time(e: &EventItem, offset_min: i32) -> String {
    let (a, b) = model::local_days(e, &off);
    let (d0, d1) = (Date::from_days(a), Date::from_days(b));
    let nice = |d: Date| format!("{} {} {}", d.weekday().short(), d.day, d.month_name());
    if e.all_day {
        return if a == b {
            format!("{} · all day", nice(d0))
        } else {
            format!("{} – {} · all day", nice(d0), nice(d1))
        };
    }
    let (s, t) = (
        localtime::clock_label(e.start_ms, offset_min),
        localtime::clock_label(e.end_ms, offset_min),
    );
    if a == b {
        format!("{} · {s} – {t}", nice(d0))
    } else {
        format!("{} {s} – {} {t}", nice(d0), nice(d1))
    }
}

impl Calendar {
    fn shown(&self, now: &Now) -> Date {
        self.picked.unwrap_or(now.date)
    }

    fn select_day(&mut self, d: Date, now: &Now) {
        self.picked = (d != now.date).then_some(d);
        self.scroll_to_hour = Some(if d == now.date {
            (now.seconds_into_day as f32 / 3600.0 - 2.0).max(0.0)
        } else {
            7.0
        });
    }

    fn step(&mut self, dir: i32, now: &Now) {
        let shown = self.shown(now);
        let next = match self.mode {
            Mode::Day => shown.add_days(i64::from(dir)),
            Mode::Week => shown.add_days(7 * i64::from(dir)),
            Mode::Month => model::add_months(shown, dir),
            Mode::List => shown.add_days(28 * i64::from(dir)),
        };
        self.select_day(next, now);
    }

    fn title(&self, now: &Now) -> String {
        let s = self.shown(now);
        match self.mode {
            Mode::Month => format!("{} {}", s.month_name(), s.year),
            Mode::Day => format!(
                "{}, {} {} {}",
                s.weekday().name(),
                s.day,
                s.month_name(),
                s.year
            ),
            Mode::Week => {
                let a = s.week_start();
                let b = a.add_days(6);
                if a.month == b.month {
                    format!("{}–{} {} {}", a.day, b.day, b.month_name(), b.year)
                } else {
                    format!(
                        "{} {} – {} {} {}",
                        a.day,
                        crate::calendar_model::month_abbr(a.month_name()),
                        b.day,
                        crate::calendar_model::month_abbr(b.month_name()),
                        b.year
                    )
                }
            }
            Mode::List => format!("From {} {} {}", s.day, s.month_name(), s.year),
        }
    }

    fn cal_of<'a>(&self, view: &'a AgendaView, e: &EventItem) -> Option<(usize, &'a CalendarItem)> {
        view.calendars
            .iter()
            .enumerate()
            .find(|(_, c)| c.device_id == e.device_id && Some(&c.id) == e.calendar_id.as_ref())
    }

    fn color(&self, view: &AgendaView, e: &EventItem, pal: &Palette) -> Color32 {
        match self.cal_of(view, e) {
            Some((i, c)) => argb(model::calendar_color(Some(c), i)),
            None => pal.accent,
        }
    }

    fn is_hidden(&self, e: &EventItem) -> bool {
        e.calendar_id
            .as_ref()
            .is_some_and(|c| self.hidden.contains(&(e.device_id.clone(), c.clone())))
    }

    fn new_op_id(&mut self) -> String {
        self.edit_seq += 1;
        format!(
            "gb{}-{}-{}",
            std::process::id(),
            self.edit_seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis())
        )
    }

    fn zone_name(&mut self) -> Option<String> {
        self.zone.get_or_insert_with(model::local_zone_name).clone()
    }

    fn run(&self, r: Request) {
        if let Some(f) = &self.feed {
            f.run(r);
        }
    }

    // ---------------------------------------------------------------- panels

    fn toolbar(&mut self, ctx: &egui::Context, pal: &Palette, now: &Now, view: &AgendaView) {
        let mut new_event = false;
        let can_create = view
            .devices
            .iter()
            .any(|d| d.calendar_edit == DataState::Ready && d.calendar_writable);
        theme::toolbar(ctx, pal, 46.0, |ui| {
            ui.horizontal_centered(|ui| {
                theme::segmented(
                    ui,
                    pal,
                    &mut self.mode,
                    &[
                        (Mode::Day, "Day"),
                        (Mode::Week, "Week"),
                        (Mode::Month, "Month"),
                        (Mode::List, "Agenda"),
                    ],
                );
                ui.add_space(8.0);
                if theme::tool_button(ui, pal, Icon::ChevronLeft, None, true, "Previous").clicked()
                {
                    self.step(-1, now);
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
                    self.select_day(now.date, now);
                }
                if theme::tool_button(ui, pal, Icon::ChevronRight, None, true, "Next").clicked() {
                    self.step(1, now);
                }
                ui.add_space(8.0);
                ui.label(theme::semibold(self.title(now), 15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tip = if can_create {
                        "Create an event on the phone's calendar"
                    } else {
                        "Allow Edit calendar for a phone (sidebar) to add events"
                    };
                    if theme::pill_button(ui, pal, "New event", ButtonKind::Primary, can_create)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        new_event = true;
                    }
                });
            });
        });
        if new_event {
            self.open_new(self.shown(now), 9);
        }
    }

    fn open_new(&mut self, day: Date, hour: u32) {
        self.editor = Some(Editor {
            existing: None,
            fields: Fields::new_on(day, hour),
            calendar: None,
            error: None,
        });
    }

    fn sidebar(
        &mut self,
        ctx: &egui::Context,
        env: &Env,
        view: &AgendaView,
        state: &FeedState<AgendaView>,
    ) {
        let pal = env.pal;
        let mut action: Option<Request> = None;
        egui::SidePanel::left("calendars")
            .exact_width(236.0)
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
                egui::ScrollArea::vertical()
                    .drag_to_scroll(false)
                    .id_salt("cal-side")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let now = localtime::now();
                        let shown = self.shown(&now);
                        ui.horizontal(|ui| {
                            crate::nothing::dot_text(
                                ui,
                                pal,
                                &format!("{:02}", shown.day),
                                crate::nothing::PITCH_TITLE,
                                pal.text,
                                false,
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} {}",
                                            shown.month_name(),
                                            shown.year
                                        ))
                                        .size(12.5)
                                        .color(pal.accent),
                                    );
                                },
                            );
                        });
                        ui.add_space(6.0);
                        let visible: Vec<&EventItem> =
                            view.events.iter().filter(|e| !self.is_hidden(e)).collect();
                        if let Some(day) = crate::dot_calendar::dot_month(
                            ui,
                            pal,
                            shown,
                            now.date,
                            Some(shown),
                            &|d| !model::on_day(&visible, d, &off).is_empty(),
                        ) {
                            self.select_day(day, &now);
                        }
                        ui.add_space(12.0);
                        ui.label(theme::overline(pal, "Calendars"));
                        ui.add_space(4.0);
                        if view.calendars.is_empty() {
                            ui.add(
                                egui::Label::new(theme::caption(pal, self.empty_text(state, view)))
                                    .wrap(),
                            );
                        }
                        let mut last: Option<(&str, &str)> = None;
                        for (i, c) in view.calendars.iter().enumerate() {
                            let who = (
                                c.device_id.as_str(),
                                c.account.as_deref().unwrap_or("Other"),
                            );
                            if last != Some(who) {
                                let phone = view
                                    .devices
                                    .iter()
                                    .find(|d| d.device_id == c.device_id)
                                    .map_or("", |d| d.name.as_str());
                                ui.add_space(6.0);
                                let google = if model::is_google(c) {
                                    " · Google"
                                } else {
                                    ""
                                };
                                ui.add(
                                    egui::Label::new(theme::caption(
                                        pal,
                                        format!("{}{}", who.1, google),
                                    ))
                                    .truncate(),
                                );
                                if view.devices.len() > 1 {
                                    ui.label(
                                        theme::caption(pal, format!("on {phone}")).color(pal.text3),
                                    );
                                }
                                last = Some(who);
                            }
                            let key = (c.device_id.clone(), c.id.clone());
                            let shown = !self.hidden.contains(&key);
                            let color = argb(model::calendar_color(Some(c), i));
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 26.0),
                                Sense::click(),
                            );
                            if resp.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(6),
                                    pal.control_hover.gamma_multiply(0.5),
                                );
                            }
                            if resp.has_focus() {
                                theme::focus_ring(ui, pal, rect, CornerRadius::same(6));
                            }
                            let box_r = Rect::from_center_size(
                                pos2(rect.left() + 12.0, rect.center().y),
                                vec2(14.0, 14.0),
                            );
                            if shown {
                                ui.painter()
                                    .rect_filled(box_r, CornerRadius::same(4), color);
                                let c = box_r.center();
                                ui.painter().add(egui::Shape::line(
                                    vec![
                                        pos2(c.x - 3.5, c.y),
                                        pos2(c.x - 1.0, c.y + 2.8),
                                        pos2(c.x + 3.8, c.y - 3.0),
                                    ],
                                    Stroke::new(1.8_f32, Color32::WHITE),
                                ));
                            } else {
                                ui.painter().rect_stroke(
                                    box_r,
                                    CornerRadius::same(4),
                                    Stroke::new(1.6_f32, color),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let clip = ui.painter().with_clip_rect(rect.shrink2(vec2(2.0, 0.0)));
                            let mut label = c.name.clone();
                            if !c.writable {
                                label.push_str("  · read-only");
                            }
                            clip.text(
                                pos2(rect.left() + 28.0, rect.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                FontId::new(13.0, FontFamily::Proportional),
                                if shown { pal.text } else { pal.text3 },
                            );
                            resp.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Checkbox,
                                    true,
                                    shown,
                                    format!("{} calendar", c.name),
                                )
                            });
                            if resp.clicked() {
                                if shown {
                                    self.hidden.insert(key);
                                } else {
                                    self.hidden.remove(&key);
                                }
                            }
                        }
                        ui.add_space(14.0);
                        // The heading is only worth showing when a phone has rows to list under it.
                        if env
                            .control
                            .status
                            .as_ref()
                            .is_some_and(|s| !s.paired.is_empty())
                        {
                            ui.label(theme::overline(pal, "This computer may"));
                            ui.add_space(4.0);
                        }
                        action = self.permission_rows(ui, env, view);
                        let recent: Vec<&EditItem> = view.edits.iter().take(5).collect();
                        if !recent.is_empty() {
                            ui.add_space(14.0);
                            ui.label(theme::overline(pal, "Recent changes"));
                            ui.add_space(4.0);
                            for e in recent {
                                if let Some(r) = self.edit_row(ui, pal, e) {
                                    action = Some(r);
                                }
                            }
                        }
                    })
                    .bars(ui.ctx());
            });
        if let Some(r) = action {
            self.run(r);
        }
    }

    fn empty_text(&self, state: &FeedState<AgendaView>, view: &AgendaView) -> String {
        if let Some(p) = &state.problem {
            return p.clone();
        }
        if state.view.is_none() {
            return "Looking for your phone's calendars ...".into();
        }
        view.devices
            .iter()
            .find_map(|d| phone_ui::state_line(d.calendar, phone_ui::Noun::Calendar))
            .unwrap_or_else(|| "No calendar is shared yet. On the phone, choose the calendars to share in the GeekBridge app, or allow Calendar below.".into())
    }

    /// One row per feature and paired phone: what the computer may do with the phone's calendar.
    fn permission_rows(&self, ui: &mut egui::Ui, env: &Env, view: &AgendaView) -> Option<Request> {
        let pal = env.pal;
        let status = env.control.status.as_ref()?;
        let mut out = None;
        for phone in &status.paired {
            for (feature, label, hint) in [
                (
                    Feature::Calendar,
                    "Read the calendar",
                    "Shows the calendars you picked on the phone.",
                ),
                (
                    Feature::CalendarEdit,
                    "Edit the calendar",
                    "Add, change and delete events. The phone writes them and syncs them to Google.",
                ),
                (
                    Feature::CalendarAlerts,
                    "Notify before events",
                    "A desktop notification shortly before an event starts.",
                ),
            ] {
                let Some(on) = phone
                    .consents
                    .iter()
                    .find(|c| c.feature == feature)
                    .map(|c| c.enabled)
                else {
                    continue;
                };
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(
                            egui::Label::new(egui::RichText::new(label).size(13.0).color(pal.text))
                                .truncate(),
                        );
                        if status.paired.len() > 1 {
                            ui.label(theme::caption(pal, phone.name.clone()));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if on { "On" } else { "Off" };
                        let kind = if on {
                            ButtonKind::Primary
                        } else {
                            ButtonKind::Plain
                        };
                        if theme::pill_button(ui, pal, text, kind, true)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            out = Some(Request::SetConsent {
                                device_id: phone.device_id.clone(),
                                feature,
                                enabled: !on,
                            });
                        }
                    });
                });
                if feature == Feature::CalendarEdit
                    && on
                    && let Some(d) = view.devices.iter().find(|d| d.device_id == phone.device_id)
                    && d.calendar_edit != DataState::Ready
                    && let Some(line) =
                        phone_ui::state_line(d.calendar_edit, phone_ui::Noun::CalendarEditing)
                {
                    ui.add(egui::Label::new(theme::caption(pal, line)).wrap());
                }
                ui.add_space(4.0);
            }
        }
        out
    }

    fn edit_row(&self, ui: &mut egui::Ui, pal: &Palette, e: &EditItem) -> Option<Request> {
        let verb = match e.action {
            CalendarEditAction::Create => "Add",
            CalendarEditAction::Update => "Change",
            CalendarEditAction::Delete => "Delete",
        };
        let (line, tone) = match e.phase {
            EditPhase::Queued => ("Waiting to be sent".to_owned(), pal.text3),
            EditPhase::HandedToPhone => ("Sent. Waiting for the phone to confirm.".to_owned(), pal.warn),
            EditPhase::Applied => ("Saved on the phone. Google syncs from there.".to_owned(), pal.ok),
            EditPhase::Failed => (
                format!("Not done: {}", e.reason.as_deref().map_or("the phone refused", edit_reason_text)),
                pal.err,
            ),
            EditPhase::Uncertain => (
                "Unknown whether the phone applied this. Check the phone, or ask again (safe: it is applied once).".to_owned(),
                pal.warn,
            ),
        };
        let mut out = None;
        ui.horizontal(|ui| {
            theme::dot(ui, tone, false);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{verb} “{}”", e.title))
                        .size(12.5)
                        .color(pal.text),
                )
                .truncate(),
            );
        });
        ui.add(egui::Label::new(theme::caption(pal, line)).wrap());
        if matches!(e.phase, EditPhase::Uncertain | EditPhase::Failed)
            && theme::pill_button(ui, pal, "Ask again", ButtonKind::Plain, true).clicked()
        {
            out = Some(Request::CalendarEditRetry {
                device_id: e.device_id.clone(),
                op_id: e.op_id.clone(),
            });
        }
        ui.add_space(4.0);
        out
    }

    fn strip(
        &mut self,
        ctx: &egui::Context,
        env: &Env,
        view: &AgendaView,
        state: &FeedState<AgendaView>,
    ) {
        let pal = env.pal;
        let mut action = None;
        egui::TopBottomPanel::bottom("calendar-status")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for d in &view.devices {
                        let mut line = format!("{}: {} events", d.name, d.events_count);
                        if let Some(ms) = d.calendar_synced_unix_ms {
                            line.push_str(&format!(
                                ", updated {}",
                                phone_ui::age_label(now_ms(), ms)
                            ));
                        }
                        ui.label(theme::caption(pal, line));
                    }
                    if view.devices.iter().any(|d| d.calendar_truncated) {
                        ui.label(theme::caption(
                            pal,
                            "Showing the events nearest today (up to 12 months back and 24 months ahead). \
                             Farther ones stay on the phone.",
                        ));
                    }
                    if view.events_truncated {
                        ui.label(theme::caption(
                            pal,
                            "Too many events to show at once; narrowing the view shows the rest.",
                        ));
                    }
                    if let Some(n) = state.notice.as_ref().or(state.problem.as_ref()) {
                        ui.label(theme::caption(pal, n.clone()));
                    }
                    if !view.devices.is_empty()
                        && theme::pill_button(ui, pal, "Refresh", ButtonKind::Plain, true).clicked()
                    {
                        action = Some(Request::AgendaRefresh { device_id: None });
                    }
                    if let Some(r) =
                        phone_ui::allow_buttons(ui, pal, env, Feature::Calendar, phone_ui::Noun::Calendar)
                    {
                        action = Some(r);
                    }
                });
            });
        if let Some(r) = action {
            self.run(r);
        }
    }

    fn details(&mut self, ctx: &egui::Context, env: &Env, view: &AgendaView) {
        let pal = env.pal;
        let Some(key) = self.selected.clone() else {
            return;
        };
        let Some(e) = view
            .events
            .iter()
            .find(|e| e.device_id == key.0 && e.id == key.1)
            .cloned()
        else {
            self.selected = None;
            return;
        };
        let block = edit_block(view, &e);
        let cal = self.cal_of(view, &e).map(|(_, c)| c.clone());
        let color = self.color(view, &e, pal);
        let mut close = false;
        let mut edit = false;
        let mut delete = None;
        egui::SidePanel::right("event-details")
            .exact_width(300.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(pal.sidebar).inner_margin(Margin::same(16)))
            .show(ctx, |ui| {
                let r = ui.max_rect();
                ui.painter().vline(r.left() - 16.0 - 0.5, r.y_range().expand(20.0), Stroke::new(1.0_f32, pal.separator));
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(6.0, 24.0), Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(3), color);
                    ui.add(egui::Label::new(theme::semibold(&e.title, 18.0)).wrap());
                });
                ui.add_space(6.0);
                ui.label(theme::secondary(pal, long_time(&e, off(Date::from_days(model::local_days(&e, &off).0)))));
                if let Some(tz) = e.tz.as_deref().filter(|_| !e.all_day)
                    && model::local_zone_name().as_deref() != Some(tz)
                    && tz != "UTC"
                {
                    ui.label(theme::caption(pal, format!("Created in time zone {tz}")));
                }
                if let Some(c) = &cal {
                    ui.add_space(8.0);
                    ui.label(theme::overline(pal, "Calendar"));
                    ui.add(egui::Label::new(egui::RichText::new(model::calendar_label(c)).color(pal.text)).wrap());
                    if model::is_google(c) {
                        ui.label(theme::caption(pal, "Google Calendar, synced by the phone"));
                    }
                }
                if let Some(l) = &e.location {
                    ui.add_space(8.0);
                    ui.label(theme::overline(pal, "Location"));
                    ui.add(egui::Label::new(egui::RichText::new(l).color(pal.text)).selectable(true).wrap());
                }
                if !e.reminders_min.is_empty() {
                    ui.add_space(8.0);
                    ui.label(theme::overline(pal, "Reminders"));
                    for m in &e.reminders_min {
                        ui.label(egui::RichText::new(model::reminder_label(*m)).color(pal.text));
                    }
                }
                if e.attendee_count > 0 {
                    ui.add_space(8.0);
                    ui.label(theme::overline(pal, "Attendees"));
                    let shown = e.attendees.join(", ");
                    let more = (e.attendee_count as usize).saturating_sub(e.attendees.len());
                    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
                    ui.add(egui::Label::new(egui::RichText::new(format!("{} total: {shown}{tail}", e.attendee_count)).color(pal.text)).wrap());
                }
                if e.recurring {
                    ui.add_space(8.0);
                    ui.label(theme::caption(pal, "Repeats. Each occurrence is listed on its own date."));
                }
                if let Some(d) = &e.description {
                    ui.add_space(8.0);
                    ui.label(theme::overline(pal, "Notes"));
                    ui.add(egui::Label::new(egui::RichText::new(d).color(pal.text)).selectable(true).wrap());
                }
                ui.add_space(14.0);
                theme::hairline(ui, pal);
                ui.add_space(10.0);
                if self.confirm_delete {
                    ui.add(egui::Label::new(theme::secondary(pal, "Delete this event from the phone's calendar? Google Calendar will drop it once the phone syncs.")).wrap());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if theme::pill_button(ui, pal, "Delete", ButtonKind::Primary, true).clicked() {
                            delete = Some(e.clone());
                        }
                        if theme::pill_button(ui, pal, "Keep", ButtonKind::Plain, true).clicked() {
                            self.confirm_delete = false;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if theme::pill_button(ui, pal, "Edit", ButtonKind::Plain, block.is_none()).clicked() {
                            edit = true;
                        }
                        if theme::pill_button(ui, pal, "Delete", ButtonKind::Plain, block.is_none()).clicked() {
                            self.confirm_delete = true;
                        }
                        if theme::pill_button(ui, pal, "Close", ButtonKind::Plain, true).clicked() {
                            close = true;
                        }
                    });
                    if let Some(why) = &block {
                        ui.add_space(6.0);
                        theme::blocked_note(ui, pal, why);
                    }
                }
            });
        if close {
            self.selected = None;
            self.confirm_delete = false;
        }
        if edit {
            let calendar_label = cal.as_ref().map_or_else(String::new, model::calendar_label);
            self.editor = Some(Editor {
                existing: e.event_id.clone().map(|event_id| Existing {
                    device_id: e.device_id.clone(),
                    event_id,
                    calendar_label,
                }),
                fields: Fields::from_event(&e, &off),
                calendar: None,
                error: None,
            });
        }
        if let Some(e) = delete {
            let op_id = self.new_op_id();
            self.run(Request::CalendarEdit {
                device_id: e.device_id.clone(),
                op_id,
                action: CalendarEditAction::Delete,
                calendar_id: None,
                event_id: e.event_id.clone(),
                draft: None,
            });
            self.selected = None;
            self.confirm_delete = false;
        }
    }

    // ---------------------------------------------------------------- editor

    fn editor_window(&mut self, ctx: &egui::Context, env: &Env, view: &AgendaView) {
        let pal = env.pal;
        let zone = self.zone_name();
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let writable: Vec<(Key, String)> =
            view.calendars
                .iter()
                .filter(|c| {
                    c.writable
                        && view.devices.iter().any(|d| {
                            d.device_id == c.device_id && d.calendar_edit == DataState::Ready
                        })
                })
                .map(|c| {
                    (
                        (c.device_id.clone(), c.id.clone()),
                        model::calendar_label(c),
                    )
                })
                .collect();
        let mut save = false;
        let mut cancel = theme::escape_pressed(ctx);
        let creating = ed.existing.is_none();
        egui::Window::new(if creating { "New event" } else { "Edit event" })
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .fixed_size(vec2(420.0, 0.0))
            .show(ctx, |ui| {
                let f = &mut ed.fields;
                crate::a11y::named(ui.add(egui::TextEdit::singleline(&mut f.title).hint_text(crate::theme::hint(ui, "Title")).desired_width(f32::INFINITY)), "Event title");
                crate::input::limit_chars(&mut f.title, 200);
                ui.add_space(8.0);
                if creating {
                    ui.label(theme::overline(pal, "Add to calendar"));
                    let current = ed.calendar.as_ref().and_then(|k| writable.iter().find(|(w, _)| w == k)).map(|(_, l)| l.clone());
                    egui::ComboBox::from_id_salt("editor-calendar")
                        .selected_text(current.unwrap_or_else(|| "Choose a calendar…".into()))
                        .width(380.0)
                        .show_ui(ui, |ui| {
                            for (k, label) in &writable {
                                ui.selectable_value(&mut ed.calendar, Some(k.clone()), label);
                            }
                        });
                    if writable.is_empty() {
                        theme::blocked_note(ui, pal, "No writable calendar is shared by a phone that allows editing.");
                    }
                } else if let Some(x) = &ed.existing {
                    ui.label(theme::overline(pal, "Calendar"));
                    ui.label(egui::RichText::new(&x.calendar_label).color(pal.text));
                    ui.label(theme::caption(pal, "An event stays in its calendar."));
                }
                ui.add_space(8.0);
                ui.checkbox(&mut f.all_day, "All day");
                ui.add_space(4.0);
                egui::Grid::new("editor-times").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label(theme::caption(pal, "Starts"));
                    crate::a11y::named(ui.add_sized([120.0, 24.0], egui::TextEdit::singleline(&mut f.start_date).hint_text(crate::theme::hint(ui, "YYYY-MM-DD"))), "Start date, YYYY-MM-DD");
                    if !f.all_day {
                        crate::a11y::named(ui.add_sized([64.0, 24.0], egui::TextEdit::singleline(&mut f.start_time).hint_text(crate::theme::hint(ui, "HH:MM"))), "Start time, HH:MM");
                    }
                    ui.end_row();
                    ui.label(theme::caption(pal, if f.all_day { "Last day" } else { "Ends" }));
                    crate::a11y::named(ui.add_sized([120.0, 24.0], egui::TextEdit::singleline(&mut f.end_date).hint_text(crate::theme::hint(ui, "YYYY-MM-DD"))), "End date, YYYY-MM-DD");
                    if !f.all_day {
                        crate::a11y::named(ui.add_sized([64.0, 24.0], egui::TextEdit::singleline(&mut f.end_time).hint_text(crate::theme::hint(ui, "HH:MM"))), "End time, HH:MM");
                    }
                    ui.end_row();
                });
                for s in [&mut f.start_date, &mut f.end_date] {
                    crate::input::limit_chars(s, 10);
                }
                for s in [&mut f.start_time, &mut f.end_time] {
                    crate::input::limit_chars(s, 5);
                }
                if !f.all_day {
                    let note = match &zone {
                        Some(z) => format!("Times are in your time zone ({z})."),
                        None => "This computer's time zone name could not be read; times are saved as the same moment in UTC.".into(),
                    };
                    ui.add(egui::Label::new(theme::caption(pal, note)).wrap());
                }
                ui.add_space(8.0);
                crate::a11y::named(ui.add(egui::TextEdit::singleline(&mut f.location).hint_text(crate::theme::hint(ui, "Location")).desired_width(f32::INFINITY)), "Event location");
                crate::input::limit_chars(&mut f.location, 200);
                ui.add_space(6.0);
                crate::a11y::named(ui.add(egui::TextEdit::multiline(&mut f.notes).hint_text(crate::theme::hint(ui, "Notes")).desired_rows(3).desired_width(f32::INFINITY)), "Event notes");
                crate::input::limit_chars(&mut f.notes, model_notes_limit());
                ui.add_space(6.0);
                egui::ComboBox::from_id_salt("editor-reminder")
                    .selected_text(f.reminder.map_or_else(|| "No reminder".to_owned(), model::reminder_label))
                    .show_ui(ui, |ui| {
                        for r in model::REMINDER_CHOICES {
                            ui.selectable_value(&mut f.reminder, r, r.map_or_else(|| "No reminder".to_owned(), model::reminder_label));
                        }
                    });
                if let Some(e) = &ed.error {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(e).color(pal.err));
                }
                ui.add_space(10.0);
                ui.add(egui::Label::new(theme::caption(pal, "The phone writes this to its calendar; Google Calendar gets it when the phone syncs.")).wrap());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::pill_button(ui, pal, if creating { "Add event" } else { "Save" }, ButtonKind::Primary, true).clicked() {
                        save = true;
                    }
                    if theme::pill_button(ui, pal, "Cancel", ButtonKind::Plain, true).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.editor = None;
            return;
        }
        if !save {
            return;
        }
        let zone = zone.unwrap_or_else(|| "UTC".into());
        let op_id = self.new_op_id();
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let draft = match ed.fields.to_draft(&zone, &off) {
            Ok(d) => d,
            Err(e) => {
                ed.error = Some(e);
                return;
            }
        };
        let request = match &ed.existing {
            None => {
                let Some((device, cal)) = ed.calendar.clone() else {
                    ed.error = Some("Choose which calendar to add the event to.".into());
                    return;
                };
                Request::CalendarEdit {
                    device_id: device,
                    op_id,
                    action: CalendarEditAction::Create,
                    calendar_id: Some(cal),
                    event_id: None,
                    draft: Some(Box::new(draft)),
                }
            }
            Some(x) => Request::CalendarEdit {
                device_id: x.device_id.clone(),
                op_id,
                action: CalendarEditAction::Update,
                calendar_id: None,
                event_id: Some(x.event_id.clone()),
                draft: Some(Box::new(draft)),
            },
        };
        self.run(request);
        self.editor = None;
        self.selected = None;
    }

    // ---------------------------------------------------------------- views

    fn chip(
        &self,
        ui: &mut egui::Ui,
        cx: &Cx,
        e: &EventItem,
        rect: Rect,
        day: Date,
        acts: &mut Acts,
    ) {
        let (pal, view) = (cx.pal, cx.view);
        let color = self.color(view, e, pal);
        let selected = self
            .selected
            .as_ref()
            .is_some_and(|(d, i)| *d == e.device_id && *i == e.id);
        let id = egui::Id::new(("chip", &e.device_id, &e.id, day.to_days()));
        let resp = ui.interact(rect, id, Sense::click());
        let fill = if e.all_day {
            color
        } else if selected {
            color.gamma_multiply(0.30)
        } else {
            color.gamma_multiply(if pal.dark { 0.22 } else { 0.14 })
        };
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            if resp.hovered() && !e.all_day {
                fill.gamma_multiply(1.4)
            } else {
                fill
            },
        );
        let text_color = if e.all_day { Color32::WHITE } else { pal.text };
        let clip = ui.painter().with_clip_rect(rect.shrink2(vec2(3.0, 0.0)));
        let label = if e.all_day {
            e.title.clone()
        } else {
            format!(
                "{} {}",
                localtime::clock_label(e.start_ms, off(day)),
                e.title
            )
        };
        if !e.all_day {
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                CornerRadius::same(2),
                color,
            );
        }
        clip.text(
            pos2(rect.left() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::new(12.0, FontFamily::Proportional),
            text_color,
        );
        focusable(ui, pal, &resp, e.title.clone());
        if resp.clicked() {
            acts.select = Some((e.device_id.clone(), e.id.clone()));
        }
    }

    fn month(&self, ui: &mut egui::Ui, cx: &Cx, events: &[&EventItem], now: &Now, acts: &mut Acts) {
        let pal = cx.pal;
        let shown = self.shown(now);
        let grid = model::month_grid(shown);
        let avail = ui.available_rect_before_wrap();
        let head_h = 24.0;
        let cell_w = avail.width() / 7.0;
        let cell_h = ((avail.height() - head_h) / 6.0).max(60.0);
        let painter = ui.painter().clone();
        for (i, name) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            let pitch = crate::nothing::scaled(ui, 2.0);
            let size = crate::dotmatrix::measure(name, pitch);
            crate::dotmatrix::paint(
                &painter,
                pos2(
                    avail.left() + cell_w * (i as f32 + 0.5) - size.x / 2.0,
                    avail.top() + (head_h - size.y) / 2.0,
                ),
                name,
                pitch,
                pal.text2,
                None,
            );
        }
        let (rect, _) =
            ui.allocate_exact_size(vec2(avail.width(), head_h + cell_h * 6.0), Sense::hover());
        let top = rect.top() + head_h;
        for (w, week) in grid.iter().enumerate() {
            for (i, day) in week.iter().enumerate() {
                let cell = Rect::from_min_size(
                    pos2(rect.left() + cell_w * i as f32, top + cell_h * w as f32),
                    vec2(cell_w, cell_h),
                );
                painter.hline(
                    cell.x_range(),
                    cell.top(),
                    Stroke::new(1.0_f32, pal.separator),
                );
                let in_month = day.month == shown.month;
                let is_today = *day == now.date;
                let resp = ui.interact(
                    cell,
                    egui::Id::new(("month-cell", day.to_days())),
                    Sense::click(),
                );
                if resp.hovered() {
                    painter.rect_filled(cell, 0.0, pal.control_hover.gamma_multiply(0.18));
                }
                let center = pos2(cell.left() + 16.0, cell.top() + 14.0);
                if is_today {
                    painter.circle_filled(center, 11.0, pal.accent_fill);
                }
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    day.day.to_string(),
                    FontId::new(
                        12.5,
                        if is_today {
                            theme::semibold_family()
                        } else {
                            FontFamily::Proportional
                        },
                    ),
                    if is_today {
                        pal.on_accent
                    } else if in_month {
                        pal.text
                    } else {
                        pal.text3
                    },
                );
                let list = model::on_day(events, *day, &off);
                focusable(ui, pal, &resp, day_label(*day, is_today, list.len()));
                let room = (((cell_h - 30.0) / 17.0).floor() as usize).max(1);
                let shown_n = if list.len() > room {
                    room - 1
                } else {
                    list.len()
                };
                for (n, e) in list.iter().take(shown_n).enumerate() {
                    let r = Rect::from_min_size(
                        pos2(cell.left() + 3.0, cell.top() + 28.0 + n as f32 * 17.0),
                        vec2(cell_w - 6.0, 15.0),
                    );
                    self.chip(ui, cx, e, r, *day, acts);
                }
                if list.len() > shown_n {
                    painter.text(
                        pos2(
                            cell.left() + 8.0,
                            cell.top() + 28.0 + shown_n as f32 * 17.0 + 7.0,
                        ),
                        Align2::LEFT_CENTER,
                        format!("+{} more", list.len() - shown_n),
                        FontId::new(12.0, FontFamily::Proportional),
                        pal.text2,
                    );
                }
                if resp.clicked() {
                    acts.goto = Some((*day, Mode::Day));
                }
                if resp.double_clicked() {
                    acts.new_at = Some((*day, 9));
                }
            }
        }
    }

    fn timeline(
        &mut self,
        ui: &mut egui::Ui,
        cx: &Cx,
        events: &[&EventItem],
        days: &[Date],
        now: &Now,
        acts: &mut Acts,
    ) {
        let (pal, view) = (cx.pal, cx.view);
        let cols = days.len();
        // Header row: day names (click to open that day), then the all-day band.
        let width = ui.available_width();
        let col_w = (width - LABEL_WIDTH - 10.0) / cols as f32;
        let (head, _) = ui.allocate_exact_size(
            vec2(width, if cols > 1 { 44.0 } else { 6.0 }),
            Sense::hover(),
        );
        if cols > 1 {
            for (i, d) in days.iter().enumerate() {
                let r = Rect::from_min_size(
                    pos2(head.left() + LABEL_WIDTH + col_w * i as f32, head.top()),
                    vec2(col_w, 44.0),
                );
                let resp =
                    ui.interact(r, egui::Id::new(("week-head", d.to_days())), Sense::click());
                let today = *d == now.date;
                focusable(ui, pal, &resp, day_label(*d, today, 0));
                ui.painter().text(
                    pos2(r.center().x, r.top() + 10.0),
                    Align2::CENTER_CENTER,
                    d.weekday().short().to_uppercase(),
                    FontId::new(12.0, theme::semibold_family()),
                    pal.text3,
                );
                let c = pos2(r.center().x, r.top() + 29.0);
                if today {
                    ui.painter().circle_filled(c, 12.0, pal.accent_fill);
                }
                ui.painter().text(
                    c,
                    Align2::CENTER_CENTER,
                    d.day.to_string(),
                    FontId::new(14.0, FontFamily::Proportional),
                    if today { pal.on_accent } else { pal.text },
                );
                if resp.clicked() {
                    acts.goto = Some((*d, Mode::Day));
                }
            }
        }
        // All-day band.
        let max_rows = days
            .iter()
            .map(|d| {
                model::on_day(events, *d, &off)
                    .iter()
                    .filter(|e| e.all_day)
                    .count()
            })
            .max()
            .unwrap_or(0)
            .min(4);
        if max_rows > 0 {
            let (band, _) =
                ui.allocate_exact_size(vec2(width, max_rows as f32 * 19.0 + 4.0), Sense::hover());
            ui.painter().text(
                pos2(band.left() + LABEL_WIDTH - 8.0, band.top() + 11.0),
                Align2::RIGHT_CENTER,
                "all-day",
                FontId::new(12.0, FontFamily::Proportional),
                pal.text3,
            );
            for (i, d) in days.iter().enumerate() {
                let list: Vec<&EventItem> = model::on_day(events, *d, &off)
                    .into_iter()
                    .filter(|e| e.all_day)
                    .collect();
                for (n, e) in list.iter().take(max_rows).enumerate() {
                    let r = Rect::from_min_size(
                        pos2(
                            band.left() + LABEL_WIDTH + col_w * i as f32 + 2.0,
                            band.top() + 2.0 + n as f32 * 19.0,
                        ),
                        vec2(col_w - 4.0, 17.0),
                    );
                    self.chip(ui, cx, e, r, *d, acts);
                }
            }
        }
        theme::hairline(ui, pal);
        let mut area = egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .id_salt("timeline-scroll")
            .auto_shrink([false, false]);
        if let Some(h) = self.scroll_to_hour.take() {
            area = area.vertical_scroll_offset(h * HOUR_HEIGHT);
        }
        area.show(ui, |ui| {
            let (rect, bg) = ui.allocate_exact_size(
                vec2(ui.available_width(), HOUR_HEIGHT * 24.0 + 12.0),
                Sense::click(),
            );
            let painter = ui.painter().clone();
            let top = rect.top() + 8.0;
            let col_w = (rect.width() - LABEL_WIDTH - 10.0) / cols as f32;
            for h in 0..=24 {
                let y = top + h as f32 * HOUR_HEIGHT;
                painter.hline(
                    (rect.left() + LABEL_WIDTH)..=(rect.right() - 6.0),
                    y,
                    Stroke::new(1.0_f32, pal.separator),
                );
                if h > 0 && h < 24 {
                    painter.text(
                        pos2(rect.left() + LABEL_WIDTH - 8.0, y),
                        Align2::RIGHT_CENTER,
                        localtime::Locale::current().hour_label(h),
                        FontId::new(12.0, FontFamily::Proportional),
                        pal.text3,
                    );
                }
            }
            for i in 0..=cols {
                let x = rect.left() + LABEL_WIDTH + col_w * i as f32;
                painter.vline(
                    x,
                    top..=(top + 24.0 * HOUR_HEIGHT),
                    Stroke::new(1.0_f32, pal.separator),
                );
            }
            for (i, d) in days.iter().enumerate() {
                let offset = off(*d);
                let timed: Vec<&EventItem> = model::on_day(events, *d, &off)
                    .into_iter()
                    .filter(|e| !e.all_day)
                    .collect();
                let spans: Vec<(i64, i64)> = timed
                    .iter()
                    .map(|e| {
                        (
                            model::seconds_into(e.start_ms, *d, offset),
                            model::seconds_into(e.end_ms, *d, offset),
                        )
                    })
                    .collect();
                let (colno, n) = model::columns(&spans);
                let left = rect.left() + LABEL_WIDTH + col_w * i as f32 + 2.0;
                let lane = (col_w - 6.0) / n as f32;
                for ((e, (s, t)), c) in timed.iter().zip(&spans).zip(colno) {
                    let y0 = top + *s as f32 / 3600.0 * HOUR_HEIGHT;
                    let y1 = top + *t as f32 / 3600.0 * HOUR_HEIGHT;
                    let block = Rect::from_min_max(
                        pos2(left + lane * c as f32, y0 + 1.0),
                        pos2(
                            left + lane * (c as f32 + 1.0) - 2.0,
                            (y1 - 1.0).max(y0 + 18.0),
                        ),
                    );
                    let color = self.color(view, e, pal);
                    let selected = self
                        .selected
                        .as_ref()
                        .is_some_and(|(dd, ii)| *dd == e.device_id && *ii == e.id);
                    let resp = ui.interact(
                        block,
                        egui::Id::new(("block", &e.device_id, &e.id, d.to_days())),
                        Sense::click(),
                    );
                    painter.rect_filled(
                        block,
                        CornerRadius::same(5),
                        color.gamma_multiply(if selected {
                            0.38
                        } else if pal.dark {
                            0.24
                        } else {
                            0.17
                        }),
                    );
                    painter.rect_filled(
                        Rect::from_min_size(block.min, vec2(3.0, block.height())),
                        CornerRadius::same(2),
                        color,
                    );
                    let clip = painter.with_clip_rect(block.shrink(1.0));
                    clip.text(
                        block.min + vec2(8.0, 3.0),
                        Align2::LEFT_TOP,
                        &e.title,
                        FontId::new(12.0, theme::semibold_family()),
                        pal.text,
                    );
                    let detail = match &e.location {
                        Some(l) => format!("{} · {l}", localtime::clock_label(e.start_ms, offset)),
                        None => format!(
                            "{} – {}",
                            localtime::clock_label(e.start_ms, offset),
                            localtime::clock_label(e.end_ms, offset)
                        ),
                    };
                    clip.text(
                        block.min + vec2(8.0, 18.0),
                        Align2::LEFT_TOP,
                        detail,
                        FontId::new(12.0, FontFamily::Proportional),
                        pal.text2,
                    );
                    focusable(ui, pal, &resp, e.title.clone());
                    if resp.clicked() {
                        acts.select = Some((e.device_id.clone(), e.id.clone()));
                    }
                }
                if *d == now.date {
                    let y = top + now.seconds_into_day as f32 / 3600.0 * HOUR_HEIGHT;
                    painter.hline(
                        (rect.left() + LABEL_WIDTH + col_w * i as f32)
                            ..=(rect.left() + LABEL_WIDTH + col_w * (i as f32 + 1.0)),
                        y,
                        Stroke::new(1.5_f32, pal.err),
                    );
                    painter.circle_filled(
                        pos2(rect.left() + LABEL_WIDTH + col_w * i as f32, y),
                        3.5,
                        pal.err,
                    );
                }
            }
            if bg.double_clicked()
                && let Some(p) = bg.interact_pointer_pos()
            {
                let col = (((p.x - rect.left() - LABEL_WIDTH) / col_w).floor().max(0.0) as usize)
                    .min(cols - 1);
                let hour = (((p.y - top) / HOUR_HEIGHT).floor().max(0.0) as u32).min(23);
                acts.new_at = Some((days[col], hour));
            }
        })
        .bars(ui.ctx());
    }

    fn list(
        &self,
        ui: &mut egui::Ui,
        pal: &Palette,
        view: &AgendaView,
        events: &[&EventItem],
        now: &Now,
        acts: &mut Acts,
    ) {
        let first = self.shown(now);
        egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .id_salt("agenda-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut any = false;
                // The window is 56 days starting a day early, so 55 are listed.
                for n in 0..55 {
                    let d = first.add_days(n);
                    let list = model::on_day(events, d, &off);
                    if list.is_empty() {
                        continue;
                    }
                    any = true;
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let today = d == now.date;
                        ui.label(
                            theme::semibold(
                                format!("{} {} {}", d.weekday().name(), d.day, d.month_name()),
                                13.5,
                            )
                            .color(if today {
                                pal.accent
                            } else {
                                pal.text
                            }),
                        );
                        if today {
                            ui.label(theme::caption(pal, "Today"));
                        }
                    });
                    theme::hairline(ui, pal);
                    for e in list {
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(6),
                                pal.control_hover.gamma_multiply(0.4),
                            );
                        }
                        let color = self.color(view, e, pal);
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                pos2(rect.left() + 4.0, rect.top() + 5.0),
                                vec2(4.0, 24.0),
                            ),
                            CornerRadius::same(2),
                            color,
                        );
                        let when = if e.all_day {
                            "All day".to_owned()
                        } else {
                            format!(
                                "{} – {}",
                                localtime::clock_label(e.start_ms, off(d)),
                                localtime::clock_label(e.end_ms, off(d))
                            )
                        };
                        ui.painter().text(
                            pos2(rect.left() + 18.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            when,
                            FontId::new(12.5, FontFamily::Proportional),
                            pal.text2,
                        );
                        let clip = ui.painter().with_clip_rect(rect);
                        let mut title = e.title.clone();
                        if let Some(l) = &e.location {
                            title.push_str(&format!("  ·  {l}"));
                        }
                        clip.text(
                            pos2(rect.left() + 140.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            title,
                            FontId::new(13.5, FontFamily::Proportional),
                            pal.text,
                        );
                        focusable(ui, pal, &resp, e.title.clone());
                        if resp.clicked() {
                            acts.select = Some((e.device_id.clone(), e.id.clone()));
                        }
                    }
                }
                if !any {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(theme::semibold("Nothing coming up", 15.0).color(pal.text3))
                    });
                }
            })
            .bars(ui.ctx());
    }
}

fn model_notes_limit() -> usize {
    bridge_core::phone_link::MAX_DRAFT_NOTES
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl AppView for Calendar {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let now = localtime::now();
        let shown = self.shown(&now);
        let (first, days) = window(self.mode, shown);
        let request = Request::AgendaView {
            query: String::new(),
            day: day_string(first),
            utc_offset_minutes: off(first),
            days,
            people_limit: 1,
        };
        match &self.feed {
            Some(feed) => feed.set_params(request),
            None => {
                // Screenshot/testing aid: start in a given view (day, week, month, agenda).
                self.mode = match std::env::var("GEEKBRIDGE_CALENDAR_VIEW").as_deref() {
                    Ok("day") => Mode::Day,
                    Ok("week") => Mode::Week,
                    Ok("agenda") => Mode::List,
                    _ => self.mode,
                };
                let repaint = ctx.clone();
                self.feed = Some(phone_feed::agenda_feed(
                    move || repaint.request_repaint(),
                    request,
                ));
                self.scroll_to_hour = Some((now.seconds_into_day as f32 / 3600.0 - 2.0).max(0.0));
            }
        }
        ctx.request_repaint_after(Duration::from_secs(20));
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        let view = state.view.clone().unwrap_or_default();
        // Screenshot/testing aids: open an event's details by title, or the new-event form.
        if self.selected.is_none()
            && let Ok(title) = std::env::var("GEEKBRIDGE_CALENDAR_SELECT")
            && let Some(e) = view.events.iter().find(|e| e.title == title)
        {
            self.selected = Some((e.device_id.clone(), e.id.clone()));
        }
        if self.editor.is_none()
            && !self.aid_editor_done
            && std::env::var_os("GEEKBRIDGE_CALENDAR_EDITOR").is_some()
            && view
                .devices
                .iter()
                .any(|d| d.calendar_edit == DataState::Ready)
        {
            self.aid_editor_done = true;
            self.open_new(self.shown(&now), 9);
        }
        let events: Vec<&EventItem> = view.events.iter().filter(|e| !self.is_hidden(e)).collect();

        self.toolbar(ctx, pal, &now, &view);
        self.strip(ctx, env, &view, &state);
        self.sidebar(ctx, env, &view, &state);
        self.details(ctx, env, &view);

        let mut acts = Acts::default();
        central(ctx, pal, Margin::ZERO, |ui| {
            if !now.local_zone_known {
                ui.label(theme::caption(
                    pal,
                    "Times use UTC because this computer's time zone couldn't be read.",
                ));
            }
            match self.mode {
                Mode::Month => {
                    egui::Frame::new()
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            self.month(ui, &Cx { pal, view: &view }, &events, &now, &mut acts)
                        });
                }
                Mode::Week => {
                    let ws = shown.week_start();
                    let days: Vec<Date> = (0..7).map(|i| ws.add_days(i)).collect();
                    self.timeline(
                        ui,
                        &Cx { pal, view: &view },
                        &events,
                        &days,
                        &now,
                        &mut acts,
                    );
                }
                Mode::Day => self.timeline(
                    ui,
                    &Cx { pal, view: &view },
                    &events,
                    &[shown],
                    &now,
                    &mut acts,
                ),
                Mode::List => {
                    egui::Frame::new()
                        .inner_margin(Margin::symmetric(20, 6))
                        .show(ui, |ui| self.list(ui, pal, &view, &events, &now, &mut acts));
                }
            }
        });

        if let Some(k) = acts.select {
            self.selected = Some(k);
            self.confirm_delete = false;
        }
        if let Some((d, m)) = acts.goto {
            self.mode = m;
            self.select_day(d, &now);
        }
        if let Some((d, h)) = acts.new_at {
            let can = view
                .devices
                .iter()
                .any(|dev| dev.calendar_edit == DataState::Ready && dev.calendar_writable);
            if can {
                self.open_new(d, h);
            }
        }
        self.editor_window(ctx, env, &view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_model::DAY_MS;
    use bridge_core::phone_view::DataDevice;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date {
            year: y,
            month: m,
            day,
        }
    }

    fn event(recurring: bool, cal: &str) -> EventItem {
        EventItem {
            device_id: "p".into(),
            id: "1@1".into(),
            title: "T".into(),
            all_day: false,
            start_ms: 0,
            end_ms: 1,
            location: None,
            description: None,
            calendar_id: Some(cal.into()),
            event_id: Some("1".into()),
            recurring,
            tz: None,
            reminders_min: vec![],
            attendee_count: 0,
            attendees: vec![],
        }
    }

    fn view(edit: DataState, writable: bool) -> AgendaView {
        let dev = DataDevice {
            device_id: "p".into(),
            name: "Pixel".into(),
            contacts: DataState::Ready,
            calendar: DataState::Ready,
            sms: DataState::ConsentOff,
            calendar_edit: edit,
            contacts_count: 0,
            events_count: 1,
            calendar_names: vec![],
            calendar_writable: writable,
            calendar_truncated: false,
            contacts_capped: false,
            threads_on_phone: None,
            contacts_synced_unix_ms: None,
            calendar_synced_unix_ms: None,
            sms_status_reports: false,
            sms_send: false,
            sims: vec![],
        };
        AgendaView {
            devices: vec![dev],
            calendars: vec![CalendarItem {
                device_id: "p".into(),
                id: "1".into(),
                name: "Mine".into(),
                account: None,
                account_type: None,
                color: None,
                writable,
            }],
            ..AgendaView::default()
        }
    }

    #[test]
    fn editing_is_blocked_with_a_plain_reason_until_everything_allows_it() {
        let e = event(false, "1");
        assert!(
            edit_block(&view(DataState::ConsentOff, true), &e)
                .unwrap()
                .contains("Allow")
        );
        assert!(
            edit_block(&view(DataState::Offline, true), &e)
                .unwrap()
                .contains("not connected")
        );
        assert!(
            edit_block(&view(DataState::NotOffered, true), &e)
                .unwrap()
                .contains("Edit calendar")
        );
        assert!(
            edit_block(&view(DataState::Ready, true), &event(true, "1"))
                .unwrap()
                .contains("Repeating")
        );
        assert!(
            edit_block(&view(DataState::Ready, false), &e)
                .unwrap()
                .contains("read-only")
        );
        assert!(edit_block(&view(DataState::Ready, true), &event(false, "9")).is_some());
        assert_eq!(edit_block(&view(DataState::Ready, true), &e), None);
    }

    #[test]
    fn windows_pad_the_visible_range_and_stay_within_the_service_cap() {
        let shown = d(2026, 11, 15);
        for m in [Mode::Day, Mode::Week, Mode::Month, Mode::List] {
            let (first, days) = window(m, shown);
            assert!(days <= bridge_core::phone_view::MAX_VIEW_DAYS, "{m:?}");
            assert!(first.to_days() <= shown.to_days());
        }
        // The month grid of November 2026 starts 26 October; the window starts a day earlier.
        let (first, days) = window(Mode::Month, shown);
        assert_eq!(first, d(2026, 10, 25));
        assert!(first.add_days(i64::from(days)).to_days() > d(2026, 12, 6).to_days());
        assert_eq!(window(Mode::Week, shown).0, d(2026, 11, 8));
        // Every mode has a day of slack before the first visible day, the agenda list included:
        // the request carries one UTC offset, so events near the first edge must not depend on it.
        for m in [Mode::Day, Mode::Week, Mode::Month, Mode::List] {
            let visible_first = match m {
                Mode::Day | Mode::List => shown,
                Mode::Week => shown.week_start(),
                Mode::Month => model::month_grid(shown)[0][0],
            };
            assert_eq!(window(m, shown).0, visible_first.add_days(-1), "{m:?}");
        }
    }

    #[test]
    fn long_time_reads_naturally_for_timed_all_day_and_multi_day() {
        let day = d(2026, 11, 24);
        let mut e = event(false, "1");
        e.start_ms = (day.to_days() * 86_400 + 13 * 3600) * 1000;
        e.end_ms = e.start_ms + 3_600_000;
        assert!(long_time(&e, 60).ends_with("14:00 – 15:00"));
        let mut a = event(false, "1");
        a.all_day = true;
        a.start_ms = day.to_days() * DAY_MS;
        a.end_ms = a.start_ms + DAY_MS;
        assert!(long_time(&a, 0).ends_with("all day"));
        a.end_ms = a.start_ms + 3 * DAY_MS;
        assert!(long_time(&a, 0).contains('–'));
    }
}

#[cfg(test)]
#[path = "calendar_tests.rs"]
mod render_tests;
