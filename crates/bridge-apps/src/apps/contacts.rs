//! Contacts: the phone's phonebook on the computer. Alphabetical with letter sections, search,
//! a details pane with Call / Message / Copy, and nothing stored: the list is a bounded view of
//! what the phone shared, held in memory by the helper service.
use crate::{
    icons::{self, Icon},
    localtime,
    phone_feed::{self, Feed},
    phone_ui, prefill,
    shell::{AppView, Env, central},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    phone_view::{AgendaView, MAX_PEOPLE_LIMIT, MAX_PEOPLE_SHOWN, Person},
};
use eframe::egui::{self, Align2, CornerRadius, FontFamily, FontId, Margin, Sense, Stroke, vec2};

const ROW_HEIGHT: f32 = 34.0;
const HEADER_HEIGHT: f32 = 24.0;
const LIST_WIDTH: f32 = 290.0;

#[derive(Default)]
pub struct Contacts {
    feed: Option<Feed<AgendaView>>,
    query: String,
    limit: usize,
    /// (device id, contact id) shown on the right.
    person: Option<(String, String)>,
    /// Scroll the list to this offset on the next frame (letter rail).
    jump: Option<f32>,
    notice: Option<(String, f64)>,
}

/// One alphabetical section: its letter and the people it holds (indices into the list).
#[derive(Debug, PartialEq, Eq)]
pub struct Section {
    pub letter: char,
    pub range: std::ops::Range<usize>,
}

/// Section letter of a name: its first letter in capitals, `#` for digits and symbols.
pub fn letter_of(name: &str) -> char {
    name.trim()
        .chars()
        .next()
        .filter(|c| c.is_alphabetic())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('#')
}

/// Groups a list that is already sorted by name into consecutive sections.
pub fn sections(people: &[Person]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for (i, p) in people.iter().enumerate() {
        let l = letter_of(&p.name);
        match out.last_mut() {
            Some(s) if s.letter == l => s.range.end = i + 1,
            _ => out.push(Section {
                letter: l,
                range: i..i + 1,
            }),
        }
    }
    out
}

/// Scroll offset of the section header for `letter` in the list drawn by [`Contacts::list`].
pub fn offset_of(sections: &[Section], letter: char) -> Option<f32> {
    let mut y = 0.0;
    for s in sections {
        if s.letter == letter {
            return Some(y);
        }
        y += HEADER_HEIGHT + s.range.len() as f32 * ROW_HEIGHT;
    }
    None
}

pub fn initials(name: &str) -> String {
    let mut it = name
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()));
    let a = it.next();
    let b = it.next_back();
    match (a, b) {
        (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

fn avatar(ui: &mut egui::Ui, pal: &Palette, name: &str, diameter: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(diameter, diameter), Sense::hover());
    // A stable tint per name, so the same person always looks the same.
    let hue = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
        % 360;
    let tint = egui::ecolor::Hsva::new(
        hue as f32 / 360.0,
        0.45,
        if pal.dark { 0.55 } else { 0.75 },
        1.0,
    );
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, egui::Color32::from(tint));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initials(name),
        FontId::new(diameter * 0.38, theme::semibold_family()),
        egui::Color32::WHITE,
    );
}

impl Contacts {
    fn view(&self) -> (phone_feed::FeedState<AgendaView>, AgendaView) {
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        let view = state.view.clone().unwrap_or_default();
        (state, view)
    }

    fn run(&self, r: Request) {
        if let Some(f) = &self.feed {
            f.run(r);
        }
    }

    fn flash(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        self.notice = Some((text.into(), ctx.input(|i| i.time)));
    }

    fn list(
        &mut self,
        ctx: &egui::Context,
        env: &Env,
        state: &phone_feed::FeedState<AgendaView>,
        view: &AgendaView,
    ) {
        let pal = env.pal;
        egui::SidePanel::left("people")
            .exact_width(LIST_WIDTH)
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
                    ui.label(theme::semibold("All Contacts", 15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(theme::secondary(pal, view.people_total.to_string()));
                    });
                });
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search name, number or email")
                        .margin(Margin::symmetric(8, 4))
                        .desired_width(f32::INFINITY),
                );
                crate::input::limit_chars(&mut self.query, 64);
                ui.add_space(8.0);
                if view.people.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        badge(ui, pal);
                        ui.add_space(6.0);
                        ui.label(theme::semibold("No contacts yet", 14.0));
                        ui.add(
                            egui::Label::new(theme::caption(pal, self.empty_text(state, view)))
                                .wrap(),
                        );
                    });
                    return;
                }
                let secs = sections(&view.people);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("people-list")
                    .auto_shrink([false, false]);
                if let Some(y) = self.jump.take() {
                    area = area.vertical_scroll_offset(y);
                }
                area.show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for s in &secs {
                            ui.allocate_ui_with_layout(
                                vec2(ui.available_width(), HEADER_HEIGHT),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space(8.0);
                                    ui.label(
                                        theme::semibold(s.letter.to_string(), 12.0)
                                            .color(pal.accent),
                                    );
                                },
                            );
                            for p in &view.people[s.range.clone()] {
                                let selected = self
                                    .person
                                    .as_ref()
                                    .is_some_and(|(d, i)| *d == p.device_id && *i == p.id);
                                if person_row(ui, pal, p, selected).clicked() {
                                    self.person = Some((p.device_id.clone(), p.id.clone()));
                                }
                            }
                        }
                        if view.people_total > view.people.len() {
                            ui.add_space(8.0);
                            let more = self.limit.max(MAX_PEOPLE_SHOWN) < MAX_PEOPLE_LIMIT;
                            ui.label(theme::caption(
                                pal,
                                format!(
                                    "Showing {} of {}. Search to narrow down.",
                                    view.people.len(),
                                    view.people_total
                                ),
                            ));
                            if more
                                && theme::pill_button(ui, pal, "Show more", ButtonKind::Plain, true)
                                    .clicked()
                            {
                                self.limit = (self.limit.max(MAX_PEOPLE_SHOWN) + MAX_PEOPLE_SHOWN)
                                    .min(MAX_PEOPLE_LIMIT);
                            }
                        }
                    });
                });
            });
        // Letter rail: only the letters that exist; click to jump.
        if view.people.is_empty() {
            return;
        }
        let secs = sections(&view.people);
        egui::SidePanel::left("people-rail")
            .exact_width(18.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(0, 12)),
            )
            .show(ctx, |ui| {
                ui.add_space(78.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                let step =
                    ((ui.available_height() - 30.0) / secs.len().max(1) as f32).clamp(11.0, 16.0);
                for s in &secs {
                    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, step), Sense::click());
                    if resp.has_focus() {
                        theme::focus_ring(ui, pal, rect, CornerRadius::same(4));
                    }
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        s.letter.to_string(),
                        FontId::new(12.0, FontFamily::Proportional),
                        if resp.hovered() {
                            pal.accent
                        } else {
                            pal.text3
                        },
                    );
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            format!("Jump to {}", s.letter),
                        )
                    });
                    if resp.clicked() {
                        self.jump = offset_of(&secs, s.letter);
                    }
                }
            });
    }

    fn empty_text(&self, state: &phone_feed::FeedState<AgendaView>, view: &AgendaView) -> String {
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

    fn strip(
        &mut self,
        ctx: &egui::Context,
        env: &Env,
        state: &phone_feed::FeedState<AgendaView>,
        view: &AgendaView,
    ) {
        let pal = env.pal;
        let mut action = None;
        egui::TopBottomPanel::bottom("contacts-consent")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for d in &view.devices {
                        let mut line = format!("{}: {} contacts", d.name, d.contacts_count);
                        if let Some(ms) = d.contacts_synced_unix_ms {
                            line.push_str(&format!(
                                ", updated {}",
                                phone_ui::age_label(now_ms(), ms)
                            ));
                        }
                        ui.label(theme::caption(pal, line));
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
                        phone_ui::allow_buttons(ui, pal, env, Feature::Contacts, "Contacts")
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
        let chosen = self
            .person
            .as_ref()
            .and_then(|(d, i)| view.people.iter().find(|p| p.device_id == *d && p.id == *i))
            .cloned();
        let mut toast = None;
        let mut copy = None;
        central(ctx, pal, Margin::same(28), |ui| {
            let Some(p) = chosen else {
                ui.centered_and_justified(|ui| {
                    ui.label(theme::semibold("No Contact Selected", 17.0).color(pal.text3));
                });
                return;
            };
            ui.horizontal(|ui| {
                avatar(ui, pal, &p.name, 64.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(theme::semibold(&p.name, 24.0));
                    if let Some(o) = &p.organization {
                        ui.label(theme::secondary(pal, o));
                    }
                });
            });
            ui.add_space(14.0);
            theme::hairline(ui, pal);
            ui.add_space(8.0);
            for number in &p.phones {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [64.0, 22.0],
                        egui::Label::new(theme::overline(pal, "Phone")),
                    );
                    ui.add(
                        egui::Label::new(egui::RichText::new(number).size(15.0).color(pal.text))
                            .selectable(true),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_space(68.0);
                    if theme::pill_button(ui, pal, "Call", ButtonKind::Primary, true).clicked() {
                        toast = Some(
                            match prefill::launch_with(
                                "geekbridge-calls",
                                prefill::DIAL_VAR,
                                number,
                            ) {
                                Ok(()) => "Opened Calls with the number ready.".to_owned(),
                                Err(e) => e,
                            },
                        );
                    }
                    if theme::pill_button(ui, pal, "Message", ButtonKind::Plain, true).clicked() {
                        toast = Some(
                            match prefill::launch_with(
                                "geekbridge-messages",
                                prefill::MESSAGE_VAR,
                                number,
                            ) {
                                Ok(()) => "Opened Messages with the number ready.".to_owned(),
                                Err(e) => e,
                            },
                        );
                    }
                    if theme::pill_button(ui, pal, "Copy number", ButtonKind::Plain, true).clicked()
                    {
                        copy = Some(number.clone());
                    }
                });
                ui.add_space(8.0);
            }
            for mail in &p.emails {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [64.0, 22.0],
                        egui::Label::new(theme::overline(pal, "Email")),
                    );
                    ui.add(
                        egui::Label::new(egui::RichText::new(mail).size(15.0).color(pal.text))
                            .selectable(true),
                    );
                    if theme::pill_button(ui, pal, "Copy", ButtonKind::Plain, true).clicked() {
                        copy = Some(mail.clone());
                    }
                });
                ui.add_space(6.0);
            }
            if p.phones.is_empty() && p.emails.is_empty() {
                ui.label(theme::secondary(
                    pal,
                    "This contact has no number or email.",
                ));
            }
            ui.add_space(14.0);
            ui.label(theme::caption(
                pal,
                "Read-only copy from your phone, kept in memory only. Edit contacts on the phone.",
            ));
            if let Some((text, at)) = &self.notice
                && ui.ctx().input(|i| i.time) - at < 4.0
            {
                ui.add_space(8.0);
                ui.label(theme::caption(pal, text.clone()));
            }
        });
        if let Some(t) = copy {
            ctx.copy_text(t);
            toast = Some("Copied.".into());
        }
        if let Some(t) = toast {
            self.flash(ctx, t);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn badge(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 22.0, pal.control);
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), vec2(20.0, 20.0)),
        Icon::Person,
        pal.text3,
    );
}

fn person_row(ui: &mut egui::Ui, pal: &Palette, p: &Person, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let radius = CornerRadius::same(7);
    if selected {
        ui.painter().rect_filled(rect, radius, pal.selected_bg);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, radius, pal.control_hover.gamma_multiply(0.55));
    }
    if resp.has_focus() {
        theme::focus_ring(ui, pal, rect, radius);
    }
    let fg = if selected { pal.selected_fg } else { pal.text };
    let clip = ui.painter().with_clip_rect(rect.shrink2(vec2(4.0, 0.0)));
    clip.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        &p.name,
        FontId::new(13.5, FontFamily::Proportional),
        fg,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, p.name.clone()));
    resp
}

impl AppView for Contacts {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let request = Request::AgendaView {
            query: self.query.clone(),
            day: {
                let d = localtime::now().date;
                format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
            },
            utc_offset_minutes: 0,
            days: 1,
            people_limit: self.limit,
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
        let (state, view) = self.view();
        // Screenshot/testing aid: open the first contact on start.
        if self.person.is_none()
            && std::env::var_os("GEEKBRIDGE_CONTACTS_SELECT_FIRST").is_some()
            && let Some(p) = view.people.first()
        {
            self.person = Some((p.device_id.clone(), p.id.clone()));
        }
        theme::toolbar(ctx, pal, 46.0, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(theme::semibold("Contacts", 15.0));
                ui.label(theme::secondary(pal, "Your phone's phonebook"));
            });
        });
        self.strip(ctx, env, &state, &view);
        self.list(ctx, env, &state, &view);
        self.details(ctx, env, &view);
        ctx.request_repaint_after(std::time::Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Person {
        Person {
            device_id: "d".into(),
            id: name.into(),
            name: name.into(),
            phones: vec![],
            emails: vec![],
            organization: None,
        }
    }

    #[test]
    fn people_group_into_alphabetical_sections() {
        let list = [
            p("1Password"),
            p("Ada"),
            p("adam"),
            p("Béla"),
            p("Zoe"),
            p("émile"),
        ];
        assert_eq!(letter_of("adam"), 'A');
        assert_eq!(letter_of("1Password"), '#');
        assert_eq!(letter_of("  émile"), 'É');
        assert_eq!(letter_of(""), '#');
        let s = sections(&list);
        assert_eq!(
            s.iter()
                .map(|s| (s.letter, s.range.clone()))
                .collect::<Vec<_>>(),
            vec![
                ('#', 0..1),
                ('A', 1..3),
                ('B', 3..4),
                ('Z', 4..5),
                ('É', 5..6)
            ]
        );
        // The rail jumps to the section header: headers and rows have fixed heights.
        assert_eq!(offset_of(&s, '#'), Some(0.0));
        assert_eq!(offset_of(&s, 'A'), Some(HEADER_HEIGHT + ROW_HEIGHT));
        assert_eq!(offset_of(&s, 'Q'), None);
        assert!(sections(&[]).is_empty());
    }

    #[test]
    fn initials_use_first_and_last_word() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("Grace Brewster Murray Hopper"), "GH");
        assert_eq!(initials("madonna"), "M");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("+44 20"), "42");
    }
}
