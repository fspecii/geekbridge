// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages: an iTunes-style window (toolbar with a status display, library sidebar with a
//! devices group, list/detail content) drawn in a monochrome dot-matrix look with a single
//! red accent. Conversations, names and unread counts come from the paired phone through the
//! service (memory only, bounded); sends are shown with the phone's real state and are never
//! resent on their own. Nothing here is sample data.
use crate::a11y::ScrollBars as _;
use crate::{
    dotmatrix,
    icons::{self, Icon},
    input, localtime,
    phone_feed::{self, Feed},
    phone_ui,
    shell::{AppView, Env, central},
    state::{DeviceInfo, DiscoveryModel, Tone},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    messages::{SmsEncoding, sms_segments},
    phone_view::{
        ConversationItem, DataDevice, DataState, MAX_MESSAGES_LIMIT, MAX_MESSAGES_SHOWN,
        MessageItem, MessagesView, SelectedThread, SendItem, SendPhase, SimItem, send_reason_text,
    },
};
use eframe::egui::{
    self, Align, CornerRadius, FontFamily, FontId, Layout, Margin, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, vec2,
};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Default)]
enum Section {
    #[default]
    Conversations,
    Compose,
    Device(String),
}

#[derive(Default)]
pub struct Messages {
    section: Section,
    recipient: String,
    draft: String,
    sidebar_hidden: bool,
    focus: Option<Focus>,
    copied_at: Option<f64>,
    feed: Option<Feed<MessagesView>>,
    /// (device id, thread id) shown in the conversation pane.
    thread: Option<(String, i64)>,
    reply: String,
    /// A send was clicked; the draft is cleared only once the service accepted it.
    awaiting: bool,
    clicks: u64,
    /// A number handed over by Contacts: open its conversation, or a new message to it.
    open_number: Option<String>,
    open_number_checked: bool,
    /// Text in the search field of the conversation list.
    search: String,
    /// SIM subscription chosen for the next send; `None` = the phone's default.
    sim: Option<i32>,
    /// Conversations already marked read on this computer, with how many messages they held then.
    marked: HashMap<(String, i64), usize>,
    env_checked: bool,
    /// Conversations whose earlier messages were already requested on opening.
    history: std::collections::HashSet<(String, i64)>,
    /// How many of the open thread's newest messages the view asks for (0 = the default); grows with
    /// "Load older messages" and resets when another conversation opens.
    message_limit: usize,
    limit_for: Option<(String, i64)>,
    /// Measured height of each bubble (with its day heading), by message and pane width.
    heights: HashMap<(i64, u32), f32>,
}
/// Height assumed for a bubble that has not been drawn yet, and how far past the viewport bubbles are still drawn.
const ESTIMATED_BUBBLE: f32 = 52.0;
const OVERSCAN: f32 = 240.0;

#[derive(Clone, Copy)]
enum Focus {
    Recipient,
    Draft,
}

const SIDEBAR_WIDTH: f32 = 204.0;
const FLASH_SECONDS: f64 = 1.6;
const METER_DOTS: usize = 16;

/// Candidate texts for the status display, longest first; the display picks what fits.
fn lcd_candidates(model: &DiscoveryModel) -> Vec<String> {
    match model {
        DiscoveryModel::Checking => vec!["SEARCHING".into()],
        DiscoveryModel::ServiceUnavailable { .. } => {
            vec!["HELPER NOT RUNNING".into(), "HELPER OFF".into()]
        }
        DiscoveryModel::Ready {
            discovery_error: Some(_),
            ..
        } => vec!["SEARCH UNAVAILABLE".into(), "NO SEARCH".into()],
        DiscoveryModel::Ready { devices, .. } if devices.is_empty() => {
            vec!["NO PHONE FOUND".into(), "NO PHONE".into()]
        }
        DiscoveryModel::Ready { devices, .. } => {
            let n = devices.len();
            vec![
                format!("{n} PHONE{} NEARBY", if n == 1 { "" } else { "S" }),
                "PHONE NEARBY".into(),
                "PHONE".into(),
            ]
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl Messages {
    fn open_compose(&mut self) {
        self.section = Section::Compose;
        self.focus = Some(if self.recipient.is_empty() {
            Focus::Recipient
        } else {
            Focus::Draft
        });
    }

    fn has_draft(&self) -> bool {
        !self.draft.is_empty() || !self.recipient.is_empty()
    }

    // ------------------------------------------------------------ toolbar

    fn toolbar(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        theme::toolbar(ctx, pal, 54.0, |ui| {
            let full = ui.max_rect();
            let lcd_w = (full.width() - 2.0 * 150.0).clamp(140.0, 390.0);
            let lcd = egui::Rect::from_center_size(full.center(), vec2(lcd_w, 36.0));
            let left = egui::Rect::from_min_max(full.min, egui::pos2(lcd.left() - 8.0, full.max.y));
            let right =
                egui::Rect::from_min_max(egui::pos2(lcd.right() + 8.0, full.min.y), full.max);

            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(left)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    if theme::tool_button(
                        ui,
                        pal,
                        Icon::Compose,
                        Some("New Message"),
                        true,
                        "Write a new message",
                    )
                    .clicked()
                    {
                        self.open_compose();
                    }
                },
            );
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(right)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    if theme::tool_button(
                        ui,
                        pal,
                        Icon::Sidebar,
                        None,
                        true,
                        "Show or hide the sidebar",
                    )
                    .clicked()
                    {
                        self.sidebar_hidden = !self.sidebar_hidden;
                    }
                },
            );
            self.status_display(ui, pal, env, lcd);
        });
    }

    /// The recessed display in the middle of the toolbar: real connection status in dot-matrix.
    fn status_display(&self, ui: &mut egui::Ui, pal: &Palette, env: &Env, rect: egui::Rect) {
        let response = ui.interact(rect, ui.id().with("lcd"), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(9), pal.lcd_bg);
        painter.rect_stroke(
            rect,
            CornerRadius::same(9),
            Stroke::new(1.0_f32, pal.separator),
            StrokeKind::Inside,
        );
        let tone = env.control.strip(env.model).tone;
        // Red is reserved for "needs attention"; a healthy link is a plain lit dot.
        let led = if env.control.connected() {
            pal.text
        } else if matches!(tone, Tone::Warning | Tone::Error) {
            pal.accent
        } else {
            pal.text3
        };
        let led_center = egui::pos2(rect.left() + 17.0, rect.center().y);
        painter.circle_filled(led_center, 4.0, led);
        painter.circle_stroke(
            led_center,
            6.5,
            Stroke::new(1.0_f32, led.gamma_multiply(0.35)),
        );
        let pitch = 2.9;
        let room = rect.width() - 34.0 - 14.0;
        let candidates = if env.control.connected() {
            vec!["PHONE CONNECTED".to_owned(), "CONNECTED".to_owned()]
        } else {
            lcd_candidates(env.model)
        };
        let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let text = dotmatrix::fit(&refs, pitch, room);
        let size = dotmatrix::measure(text, pitch);
        let origin = egui::pos2(
            rect.left() + 34.0 + (room - size.x).max(0.0) / 2.0,
            rect.center().y - size.y / 2.0,
        );
        dotmatrix::paint(painter, origin, text, pitch, pal.text, Some(pal.lcd_off));
        response.on_hover_text(env.model.guidance(env.api_port));
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ctx: &egui::Context, env: &Env) {
        if self.sidebar_hidden {
            return;
        }
        let pal = env.pal;
        egui::SidePanel::left("library")
            .exact_width(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ctx, |ui| {
                let r = ui.max_rect();
                ui.painter().vline(
                    r.right() + 10.5,
                    r.y_range().expand(20.0),
                    Stroke::new(1.0_f32, pal.separator),
                );
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(theme::overline(pal, "Library"));
                ui.add_space(4.0);
                let unread: u32 = self.view().0.conversations.iter().map(|c| c.unread).sum();
                let count = unread.to_string();
                if theme::sidebar_item(
                    ui,
                    pal,
                    Icon::Chat,
                    "Conversations",
                    (unread > 0).then_some(count.as_str()),
                    self.section == Section::Conversations,
                    None,
                )
                .clicked()
                {
                    self.section = Section::Conversations;
                }
                let marker = self.has_draft().then_some(pal.accent);
                if theme::sidebar_item(
                    ui,
                    pal,
                    Icon::Compose,
                    "New Message",
                    None,
                    self.section == Section::Compose,
                    marker,
                )
                .clicked()
                {
                    self.open_compose();
                }

                ui.add_space(16.0);
                ui.label(theme::overline(pal, "Devices"));
                ui.add_space(4.0);
                let devices = env.model.devices();
                if devices.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        theme::dot(ui, pal.text3, true);
                        ui.add(egui::Label::new(theme::caption(pal, "No devices")).wrap());
                    });
                }
                for device in devices {
                    let selected = self.section == Section::Device(device.id.clone());
                    if theme::sidebar_item(ui, pal, Icon::Phone, &device.name, None, selected, None)
                        .clicked()
                    {
                        self.section = Section::Device(device.id.clone());
                    }
                }
            });
    }

    // ------------------------------------------------------------ content

    fn view(&self) -> (MessagesView, phone_feed::FeedState<MessagesView>) {
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        (state.view.clone().unwrap_or_default(), state)
    }

    fn submission(&mut self) -> String {
        self.clicks += 1;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        format!("m{ms}-{}", self.clicks)
    }

    fn run(&self, request: Request) {
        if let Some(feed) = &self.feed {
            feed.run(request);
        }
    }

    /// The list of conversations from every allowed phone, newest first.
    fn thread_list(&mut self, ctx: &egui::Context, env: &Env, view: &MessagesView) {
        let pal = env.pal;
        let now_ms = now_ms();
        egui::SidePanel::left("threads")
            .exact_width(310.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(8, 8)),
            )
            .show(ctx, |ui| {
                crate::a11y::named(ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(RichText::new("Search messages").color(pal.text3))
                        .desired_width(f32::INFINITY),
                ), "Search messages");
                input::limit_chars(&mut self.search, 64);
                ui.add_space(4.0);
                if view.conversations.is_empty() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        ui.label(theme::secondary(pal, "No conversation matches."));
                        ui.add(
                            egui::Label::new(theme::caption(
                                pal,
                                "Search covers the messages loaded here. Open a conversation to load its history.",
                            ))
                            .wrap(),
                        );
                    });
                }
                egui::ScrollArea::vertical().drag_to_scroll(false)
                    .id_salt("thread-list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for c in &view.conversations {
                            let selected = self
                                .thread
                                .as_ref()
                                .is_some_and(|(d, t)| *d == c.device_id && *t == c.thread_id);
                            if thread_row(ui, pal, c, selected, now_ms).clicked() {
                                self.thread = Some((c.device_id.clone(), c.thread_id));
                            }
                        }
                        if let Some(note) = conversations_note(view) {
                            ui.add_space(6.0);
                            ui.add(egui::Label::new(theme::caption(pal, note)).wrap());
                        }
                    }).bars(ui.ctx());
            });
    }

    fn conversations(&mut self, ui: &mut egui::Ui, env: &Env, view: &MessagesView) {
        let pal = env.pal;
        let Some((device, thread)) = self.thread.clone() else {
            return self.conversations_empty(ui, env, view);
        };
        let Some(sel) = view
            .selected
            .as_ref()
            .filter(|s| s.device_id == device && s.thread_id == thread)
        else {
            ui.centered_and_justified(|ui| {
                ui.label(theme::secondary(pal, "Loading the conversation ..."));
            });
            return;
        };
        let conv = view
            .conversations
            .iter()
            .find(|c| c.device_id == device && c.thread_id == thread);
        let dev = view.devices.iter().find(|d| d.device_id == device);
        // Opening a conversation fetches its earlier messages once (the phone first lists only
        // the newest one of each); "Load older messages" asks for more.
        if sel.may_have_older && sel.held < 20 && self.history.insert((device.clone(), thread)) {
            self.run(Request::MessagesRefresh {
                device_id: device.clone(),
                thread_id: Some(thread),
            });
        }
        // Reading a conversation marks it read here (the phone is not told). Asked again only
        // when more messages arrived since.
        if let Some(c) = conv
            && c.unread > 0
            && self.marked.get(&(device.clone(), thread)) != Some(&c.message_count)
        {
            self.marked
                .insert((device.clone(), thread), c.message_count);
            self.run(Request::MessagesMarkRead {
                device_id: device.clone(),
                thread_id: thread,
            });
        }
        egui::Frame::new()
            .inner_margin(Margin::symmetric(18, 10))
            .show(ui, |ui| {
                ui.label(theme::semibold(
                    conv.map_or("Conversation", |c| c.title.as_str()),
                    16.0,
                ));
                if let Some(c) = conv
                    && c.addresses.first().is_some_and(|a| *a != c.title)
                {
                    ui.label(theme::caption(pal, c.addresses.join(", ")));
                }
                ui.label(theme::caption(
                    pal,
                    "Read marks stay on this computer; the phone keeps its own.",
                ));
            });
        theme::hairline(ui, pal);
        let mine: Vec<&SendItem> = conv.map_or_else(Vec::new, |c| {
            pending_sends(&view.sends, &device, &c.addresses, &sel.messages, 3)
        });
        let ready = dev.is_some_and(|d| send_blocker(d).is_none());
        let footer = 128.0
            + 24.0 * mine.len() as f32
            + if dev.is_some_and(|d| d.sims.len() > 1) {
                28.0
            } else {
                0.0
            }
            + if ready { 0.0 } else { 74.0 };
        let body_h = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical().drag_to_scroll(false)
            .id_salt("thread-scroll")
            .max_height(body_h)
            .min_scrolled_height(body_h)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let list_origin = ui.min_rect().top();
                ui.add_space(8.0);
                // Messages held here but not shown yet come first; the phone is asked only when
                // everything held is already on screen.
                let hidden = sel.held.saturating_sub(sel.messages.len());
                if (hidden > 0 || sel.may_have_older)
                    && theme::pill_button(ui, pal, "Load older messages", ButtonKind::Plain, true)
                        .clicked()
                {
                    self.message_limit = (sel.messages.len().max(MAX_MESSAGES_SHOWN) + 200)
                        .min(MAX_MESSAGES_LIMIT);
                    if hidden == 0 {
                        self.run(Request::MessagesRefresh {
                            device_id: device.clone(),
                            thread_id: Some(thread),
                        });
                    }
                }
                if hidden > 0 {
                    ui.vertical_centered(|ui| {
                        ui.label(theme::caption(
                            pal,
                            format!(
                                "Showing the newest {} of {} messages loaded.",
                                sel.messages.len(),
                                sel.held
                            ),
                        ));
                    });
                }
                if sel.capped {
                    ui.vertical_centered(|ui| {
                        ui.label(theme::caption(
                            pal,
                            format!(
                                "Showing the latest {} messages of this conversation. Older ones stay on the phone.",
                                sel.held
                            ),
                        ));
                    });
                }
                thread_bubbles(
                    ui,
                    pal,
                    &mut self.heights,
                    &sel.messages,
                    viewport,
                    list_origin,
                );
                ui.add_space(6.0);
            }).bars(ui.ctx());
        theme::hairline(ui, pal);
        for send in mine {
            self.send_line(ui, pal, send, dev);
        }
        self.reply_box(ui, env, dev, conv, sel);
    }

    fn send_line(
        &self,
        ui: &mut egui::Ui,
        pal: &Palette,
        send: &SendItem,
        dev: Option<&DataDevice>,
    ) {
        let reports = dev.is_some_and(|d| d.sms_status_reports);
        let (text, again) = send_phase_text(send, reports);
        ui.horizontal(|ui| {
            theme::dot(
                ui,
                pal.text3,
                matches!(send.phase, SendPhase::HandedToPhone | SendPhase::Queued),
            );
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    format!("\"{}\": {text}", short(&send.text, 28)),
                ))
                .wrap(),
            );
            if again
                && theme::pill_button(ui, pal, "Send again as new", ButtonKind::Plain, true)
                    .on_hover_text(
                        "Creates a new message. If the first one did go out, the recipient gets it twice and your plan may bill both.",
                    )
                    .clicked()
            {
                let submission_id = format!("r{}-{}", send.send_id, now_ms());
                self.run(Request::MessagesResend {
                    device_id: send.device_id.clone(),
                    send_id: send.send_id,
                    submission_id,
                });
            }
        });
    }

    fn reply_box(
        &mut self,
        ui: &mut egui::Ui,
        env: &Env,
        dev: Option<&DataDevice>,
        conv: Option<&ConversationItem>,
        sel: &SelectedThread,
    ) {
        let pal = env.pal;
        let ready = dev.is_some_and(|d| send_blocker(d).is_none());
        let to = conv.and_then(|c| c.addresses.first().cloned());
        ui.add_space(6.0);
        let reply = crate::a11y::named(
            ui.add(
                egui::TextEdit::multiline(&mut self.reply)
                    .frame(false)
                    .hint_text(egui::RichText::new("Reply").color(pal.text3))
                    .font(FontId::new(15.0, FontFamily::Proportional))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            ),
            "Reply",
        );
        theme::ring_when_focused(ui, pal, &reply);
        input::limit_chars(&mut self.reply, input::MAX_DRAFT_CHARS);
        if let Some(d) = dev {
            self.sim_picker(ui, pal, d);
        }
        let blocker = if !ready {
            Some(
                dev.and_then(send_blocker)
                    .unwrap_or_else(|| "Messages are not available for this phone.".into()),
            )
        } else if to.is_none() {
            Some("This conversation has no address to reply to.".into())
        } else {
            None
        };
        if let Some(b) = &blocker {
            theme::blocked_note(ui, pal, b);
        }
        let can = blocker.is_none() && !self.reply.trim().is_empty();
        ui.horizontal(|ui| {
            ui.label(theme::caption(pal, segment_label(&self.reply)))
                .on_hover_text(segment_hint(&self.reply));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::round_button(
                    ui,
                    pal,
                    34.0,
                    pal.accent_fill,
                    Icon::Send,
                    can,
                    "Send as text message",
                )
                .clicked()
                    && let (Some(to), Some(d)) = (to, dev)
                {
                    let submission_id = self.submission();
                    let sub_id = self.chosen_sim(d);
                    self.run(Request::MessagesSend {
                        device_id: d.device_id.clone(),
                        recipient: to,
                        text: self.reply.clone(),
                        submission_id,
                        sub_id,
                    });
                    self.reply.clear();
                }
            });
        });
        let _ = sel;
    }

    /// The SIM to send with: only one the phone reported, else the phone's default.
    fn chosen_sim(&self, d: &DataDevice) -> Option<i32> {
        self.sim.filter(|id| d.sims.iter().any(|s| s.id == *id))
    }

    /// "Send with: Phone default | SIM 1 | SIM 2", shown only when the phone reported several SIMs.
    fn sim_picker(&mut self, ui: &mut egui::Ui, pal: &Palette, d: &DataDevice) {
        if d.sims.len() < 2 {
            return;
        }
        let current = self.chosen_sim(d);
        if current != self.sim {
            self.sim = current;
        }
        ui.horizontal(|ui| {
            ui.label(theme::caption(pal, "Send with"));
            let text = d
                .sims
                .iter()
                .find(|s| Some(s.id) == self.sim)
                .map_or_else(|| "Phone default".to_owned(), sim_label);
            egui::ComboBox::from_id_salt("sim-choice")
                .selected_text(text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sim, None, "Phone default");
                    for s in &d.sims {
                        ui.selectable_value(&mut self.sim, Some(s.id), sim_label(s));
                    }
                });
        });
    }

    fn conversations_empty(&mut self, ui: &mut egui::Ui, env: &Env, view: &MessagesView) {
        let pal = env.pal;
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
        if !self.search.trim().is_empty() && state.view.is_some() {
            ui.centered_and_justified(|ui| {
                ui.label(theme::secondary(
                    pal,
                    if view.conversations.is_empty() {
                        "Nothing matches this search."
                    } else {
                        "Pick a conversation from the results."
                    },
                ));
            });
            return;
        }
        let mut action = None;
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.2).clamp(20.0, 90.0));
            let pitch = 6.5;
            let size = dotmatrix::measure("0", pitch);
            let (rect, _) = ui.allocate_exact_size(size + vec2(0.0, 2.0), Sense::hover());
            dotmatrix::paint(ui.painter(), rect.min, "0", pitch, pal.text, None);
            ui.add_space(14.0);
            ui.label(theme::semibold("No conversations", 17.0));
            ui.add_space(2.0);
            let why = if let Some(p) = &state.problem {
                p.clone()
            } else if state.view.is_none() {
                "Looking for your phone's messages ...".into()
            } else {
                view.devices
                    .iter()
                    .find_map(sms_blocker)
                    .unwrap_or_else(|| "Nothing has been shared from a phone yet.".into())
            };
            ui.add(egui::Label::new(theme::secondary(pal, why)).wrap());
            if let Some(n) = &state.notice {
                ui.label(theme::caption(pal, n.clone()));
            }
            if let Some(r) = phone_ui::allow_buttons(ui, pal, env, Feature::Sms, phone_ui::Noun::Messages) {
                action = Some(r);
            }
            ui.add_space(10.0);
            if theme::pill_button(ui, pal, "New Message", ButtonKind::Plain, true).clicked() {
                self.open_compose();
            }
            ui.add_space(14.0);
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    "Messages come from the phone's SMS store, only while you allow them and the phone app has SMS access. No RCS or picture messages.",
                ))
                .wrap(),
            );
        });
        if let Some(r) = action {
            self.run(r);
        }
    }

    fn compose(&mut self, ui: &mut egui::Ui, env: &Env) {
        let pal = env.pal;
        let now = ui.input(|i| i.time);
        let flashing = self.copied_at.is_some_and(|t| now - t < FLASH_SECONDS);
        if flashing {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        } else {
            self.copied_at = None;
        }

        // To: [number]
        ui.horizontal(|ui| {
            ui.add_sized([34.0, 28.0], egui::Label::new(theme::overline(pal, "To")));
            let to = crate::a11y::named(
                ui.add(
                    egui::TextEdit::singleline(&mut self.recipient)
                        .frame(false)
                        .hint_text(RichText::new("Phone number").color(pal.text3))
                        .font(FontId::new(16.0, FontFamily::Proportional))
                        .vertical_align(Align::Center)
                        .desired_width(f32::INFINITY),
                ),
                "Recipient phone number",
            );
            theme::ring_when_focused(ui, pal, &to);
            if to.changed() {
                self.recipient = input::sanitize_dial(&self.recipient);
            }
            if matches!(self.focus, Some(Focus::Recipient)) {
                to.request_focus();
                self.focus = None;
            }
        });
        theme::hairline(ui, pal);
        ui.add_space(6.0);

        let sims = self
            .view()
            .0
            .devices
            .iter()
            .any(|d| d.sims.len() > 1 && send_blocker(d).is_none());
        let footer_h = 176.0 + if sims { 34.0 } else { 0.0 };
        let body_h = (ui.available_height() - footer_h).max(70.0);
        egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .id_salt("draft-scroll")
            .max_height(body_h)
            .min_scrolled_height(body_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let rows = (body_h / 22.0).floor().max(3.0) as usize;
                let body = crate::a11y::named(
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft)
                            .frame(false)
                            .hint_text(RichText::new("Write a message").color(pal.text3))
                            .font(FontId::new(15.0, FontFamily::Proportional))
                            .desired_rows(rows)
                            .desired_width(f32::INFINITY),
                    ),
                    "Message text",
                );
                theme::ring_when_focused(ui, pal, &body);
                if body.changed() {
                    input::limit_chars(&mut self.draft, input::MAX_DRAFT_CHARS);
                }
                if matches!(self.focus, Some(Focus::Draft)) {
                    body.request_focus();
                    self.focus = None;
                }
            })
            .bars(ui.ctx());

        theme::hairline(ui, pal);
        ui.add_space(8.0);
        self.counter(ui, pal);
        ui.add_space(4.0);

        let (view, state) = self.view();
        if self.awaiting {
            if state.notice.is_some() {
                self.draft.clear();
                self.recipient.clear();
                self.awaiting = false;
                self.section = Section::Conversations;
            } else if state.problem.is_some() {
                self.awaiting = false;
            }
        }
        let target = view
            .devices
            .iter()
            .find(|d| send_blocker(d).is_none())
            .cloned();
        if let Some(d) = &target {
            self.sim_picker(ui, pal, d);
        }
        let own_block = target.is_none().then(|| {
            view.devices
                .iter()
                .find_map(send_blocker)
                .unwrap_or_else(|| {
                    "Messages are not allowed for any phone yet. Open Conversations to allow them."
                        .to_owned()
                })
        });
        // The service knows whether this phone can send; the generic discovery gate does not.
        let blocker = own_block;
        if let Some(reason) = &blocker {
            theme::blocked_note(ui, pal, reason);
        }
        if let Some(why) = &state.problem {
            theme::blocked_note(ui, pal, why);
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let can_send =
                blocker.is_none() && !self.recipient.is_empty() && !self.draft.trim().is_empty();
            if theme::pill_button(
                ui,
                pal,
                if flashing { "Copied" } else { "Copy draft" },
                ButtonKind::Plain,
                !self.draft.is_empty(),
            )
            .clicked()
            {
                ui.ctx().copy_text(self.draft.clone());
                self.copied_at = Some(now);
            }
            if theme::pill_button(ui, pal, "Discard", ButtonKind::Plain, self.has_draft()).clicked()
            {
                self.draft.clear();
                self.recipient.clear();
                self.focus = Some(Focus::Recipient);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::round_button(
                    ui,
                    pal,
                    38.0,
                    pal.accent_fill,
                    Icon::Send,
                    can_send,
                    "Send as text message",
                )
                .clicked()
                    && let Some(d) = &target
                {
                    let submission_id = self.submission();
                    let sub_id = self.chosen_sim(d);
                    self.run(Request::MessagesSend {
                        device_id: d.device_id.clone(),
                        recipient: self.recipient.clone(),
                        text: self.draft.clone(),
                        submission_id,
                        sub_id,
                    });
                    self.awaiting = true;
                }
            });
        });
    }

    /// Dot-matrix meter and count for the draft length.
    fn counter(&self, ui: &mut egui::Ui, pal: &Palette) {
        let used = self.draft.chars().count();
        let lit = (used * METER_DOTS).div_ceil(input::MAX_DRAFT_CHARS);
        let hot = lit >= METER_DOTS - 2;
        ui.horizontal(|ui| {
            let (meter, _) =
                ui.allocate_exact_size(vec2(METER_DOTS as f32 * 7.0, 14.0), Sense::hover());
            dotmatrix::meter(
                ui.painter(),
                meter,
                METER_DOTS,
                lit,
                if hot { pal.accent } else { pal.text },
                pal.lcd_off,
            );
            let pitch = 2.3;
            let text = format!("{used}/{}", input::MAX_DRAFT_CHARS);
            let size = dotmatrix::measure(&text, pitch);
            let (rect, response) = ui.allocate_exact_size(size + vec2(0.0, 2.0), Sense::hover());
            dotmatrix::paint(
                ui.painter(),
                rect.min + vec2(0.0, 1.0),
                &text,
                pitch,
                pal.text2,
                None,
            );
            response.on_hover_text(format!(
                "{used} of {} characters. The draft is kept only in this window.",
                input::MAX_DRAFT_CHARS
            ));
            ui.label(theme::caption(pal, "Draft stays in this window"));
        });
        ui.label(theme::caption(pal, segment_label(&self.draft)))
            .on_hover_text(segment_hint(&self.draft));
    }

    fn device(&mut self, ui: &mut egui::Ui, env: &Env, device: &DeviceInfo) {
        let pal = env.pal;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(14), pal.control);
            icons::paint(
                ui.painter(),
                egui::Rect::from_center_size(rect.center(), vec2(26.0, 26.0)),
                Icon::Phone,
                pal.text,
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(theme::semibold(&device.name, 20.0)).truncate());
                ui.horizontal(|ui| {
                    theme::dot(ui, pal.accent, !device.trusted);
                    ui.label(theme::secondary(
                        pal,
                        if device.trusted {
                            "Paired"
                        } else {
                            "Found nearby, not paired"
                        },
                    ));
                });
            });
        });
        ui.add_space(14.0);
        theme::hairline(ui, pal);
        ui.add_space(8.0);
        egui::Grid::new("device-info")
            .num_columns(2)
            .spacing([22.0, 10.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, key: &str, value: String| {
                    ui.label(theme::overline(pal, key));
                    ui.add(egui::Label::new(RichText::new(value).color(pal.text)).wrap());
                    ui.end_row();
                };
                row(ui, "Type", capitalize(&device.kind));
                row(ui, "Address", device.address.clone());
                row(
                    ui,
                    "Messaging",
                    if device.advertises_feature {
                        "The phone says it supports messaging".into()
                    } else {
                        "The phone didn't mention messaging".into()
                    },
                );
                let line = self
                    .view()
                    .0
                    .devices
                    .iter()
                    .find(|d| d.device_id == device.id)
                    .map_or_else(
                        || {
                            "Not shared. Pair this phone and allow Messages from the Conversations page."
                                .to_owned()
                        },
                        |d| {
                            phone_ui::state_line(d.sms, phone_ui::Noun::Messages)
                                .unwrap_or_else(|| "Shared with this computer".into())
                        },
                    );
                row(ui, "Messages", line);
            });
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Same number when the last seven digits agree (country codes and spacing differ between sources).
fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (digits(a), digits(b));
    let n = 7.min(a.len()).min(b.len());
    n >= 3 && a[a.len() - n..] == b[b.len() - n..]
}

fn short(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// What the user is told about a send, and whether "Send again" is offered. Only the phone's own
/// reports (or an exact echo of the message in its SMS store) can raise it above "handed to phone".
fn send_phase_text(send: &SendItem, phone_reports: bool) -> (String, bool) {
    match send.phase {
        SendPhase::Queued => ("not sent yet".into(), false),
        SendPhase::HandedToPhone if phone_reports => {
            ("handed to the phone, waiting for it to accept".into(), false)
        }
        SendPhase::HandedToPhone => (
            "handed to the phone. This phone does not report back; it counts as sent once the message shows up in the conversation".into(),
            false,
        ),
        SendPhase::AcceptedByPhone => ("accepted by the phone, sending".into(), false),
        SendPhase::Sent => ("sent from the phone (not a delivery receipt)".into(), false),
        SendPhase::Delivered => ("delivered".into(), false),
        SendPhase::Failed => (
            format!(
                "failed{}",
                send.reason
                    .as_deref()
                    .map_or(String::new(), |r| format!(": {}", send_reason_text(r)))
            ),
            true,
        ),
        SendPhase::Uncertain => (
            "unknown: the connection dropped after it was handed over, so it may or may not have been sent".into(),
            true,
        ),
    }
}

/// "1 SMS, 12 of 160" / "2 SMS (Unicode: emoji or accents), 38 left" for the compose counter.
fn segment_label(text: &str) -> String {
    let info = sms_segments(text);
    if info.segments == 0 {
        return "Empty".into();
    }
    let kind = match info.encoding {
        SmsEncoding::Gsm7 => "",
        SmsEncoding::Ucs2 => " (Unicode: emoji or accents)",
    };
    format!(
        "{} SMS{kind}, {} left in the last one",
        info.segments, info.remaining
    )
}

/// Longer messages and Unicode cost more texts; the phone and carrier have the last word.
fn segment_hint(text: &str) -> String {
    let info = sms_segments(text);
    format!(
        "{} {} used. One text holds {} here; longer messages are split into parts of {} and may be billed as several texts.",
        info.units,
        match info.encoding {
            SmsEncoding::Gsm7 => "characters",
            SmsEncoding::Ucs2 => "UTF-16 units",
        },
        match info.encoding {
            SmsEncoding::Gsm7 => 160,
            SmsEncoding::Ucs2 => 70,
        },
        match info.encoding {
            SmsEncoding::Gsm7 => 153,
            SmsEncoding::Ucs2 => 67,
        },
    )
}

fn sim_label(s: &SimItem) -> String {
    match (s.label.is_empty(), s.slot) {
        (true, 0) => format!("SIM {}", s.id),
        (true, n) => format!("SIM {n}"),
        (false, 0) => s.label.clone(),
        (false, n) => format!("{} (SIM {n})", s.label),
    }
}

/// Heading for a run of messages from one local day.
fn day_label(day: localtime::Date, today: localtime::Date) -> String {
    match today.to_days() - day.to_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => day.weekday().name().into(),
        _ => format!(
            "{} {} {}{}",
            day.weekday().short(),
            day.day,
            crate::calendar_model::month_abbr(day.month_name()),
            if day.year == today.year {
                String::new()
            } else {
                format!(" {}", day.year)
            }
        ),
    }
}

fn local_day(ms: u64) -> localtime::Date {
    let utc_ms = i64::try_from(ms).unwrap_or(0);
    localtime::split_unix(
        utc_ms.div_euclid(1000),
        localtime::utc_offset_minutes_at(utc_ms).saturating_mul(60),
    )
    .0
}

/// Sends of this conversation that still need a line of their own: the ones whose text has not
/// shown up as an outgoing message on the phone yet, plus failed or uncertain ones, which keep
/// their "Send again" choice. Newest last, at most `max`.
fn pending_sends<'a>(
    sends: &'a [SendItem],
    device: &str,
    addresses: &[String],
    messages: &[MessageItem],
    max: usize,
) -> Vec<&'a SendItem> {
    let mut v: Vec<&SendItem> = sends
        .iter()
        .filter(|s| s.device_id == device && addresses.iter().any(|a| same_number(a, &s.recipient)))
        .filter(|s| {
            matches!(s.phase, SendPhase::Failed | SendPhase::Uncertain)
                || !messages.iter().any(|m| {
                    m.outgoing
                        && m.body.chars().take(160).eq(s.text.chars())
                        && m.date_ms + 120_000 >= s.created_unix_ms
                })
        })
        .collect();
    v.sort_by_key(|s| s.created_unix_ms);
    let skip = v.len().saturating_sub(max);
    v.split_off(skip)
}

/// Why this phone cannot be used for Messages right now, with the exact steps; `None` when it can.
fn sms_blocker(d: &DataDevice) -> Option<String> {
    match d.sms {
        DataState::Ready => None,
        DataState::NotOffered => Some(
            "The phone is connected but does not offer Messages. On the phone: GeekBridge, Share with computer, switch on Messages and allow SMS. If Android blocks the permission (Android 13 and newer, app installed outside a store): Settings, Apps, GeekBridge, the three-dot menu, Allow restricted settings; then Permissions, SMS, Allow. GeekBridge does not need to be your default SMS app: picture messages (MMS) and RCS stay in your messaging app."
                .into(),
        ),
        other => phone_ui::state_line(other, phone_ui::Noun::Messages),
    }
}

/// `Some(reason)` when this phone can be read but not asked to send.
fn send_blocker(d: &DataDevice) -> Option<String> {
    if d.sms != DataState::Ready {
        return sms_blocker(d);
    }
    (!d.sms_send).then(|| {
        "This phone lets you read messages but does not accept texts to send. Turn on sending in the GeekBridge app, allow the SMS permission, and reconnect."
            .into()
    })
}

fn thread_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    c: &ConversationItem,
    selected: bool,
    now_ms: u64,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
    let radius = CornerRadius::same(8);
    if selected {
        ui.painter().rect_filled(rect, radius, pal.selected_bg);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, radius, pal.control_hover.gamma_multiply(0.55));
    }
    if response.has_focus() {
        theme::focus_ring(ui, pal, rect, radius);
    }
    let fg = if selected { pal.selected_fg } else { pal.text };
    let sub = if selected { pal.selected_fg } else { pal.text2 };
    let preview = match &c.matched {
        Some(m) => m.clone(),
        None => short(&c.last_body.replace('\n', " "), 40),
    };
    let clip = ui.painter().with_clip_rect(rect.shrink2(vec2(6.0, 0.0)));
    clip.text(
        rect.left_top() + vec2(14.0, 9.0),
        egui::Align2::LEFT_TOP,
        &c.title,
        FontId::new(14.0, theme::semibold_family()),
        fg,
    );
    let prefix = if c.last_outgoing { "You: " } else { "" };
    clip.text(
        rect.left_top() + vec2(14.0, 30.0),
        egui::Align2::LEFT_TOP,
        format!("{prefix}{preview}"),
        FontId::new(12.5, FontFamily::Proportional),
        sub,
    );
    let when = if now_ms.saturating_sub(c.last_date_ms) < 86_400_000 {
        localtime::clock_label_local(i64::try_from(c.last_date_ms).unwrap_or(0))
    } else {
        phone_ui::age_label(now_ms, c.last_date_ms)
    };
    ui.painter().text(
        egui::pos2(rect.right() - 12.0, rect.top() + 18.0),
        egui::Align2::RIGHT_CENTER,
        when,
        FontId::new(12.0, FontFamily::Proportional),
        sub,
    );
    if c.unread > 0 {
        let label = if c.unread > 99 {
            "99+".to_owned()
        } else {
            c.unread.to_string()
        };
        let w = if label.len() > 1 { 26.0 } else { 20.0 };
        let badge = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 12.0 - w / 2.0, rect.bottom() - 15.0),
            vec2(w, 18.0),
        );
        ui.painter()
            .rect_filled(badge, CornerRadius::same(9), pal.accent_fill);
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            label,
            FontId::new(12.0, theme::semibold_family()),
            pal.on_accent,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!(
                "{}{}: {}",
                c.title,
                if c.unread > 0 {
                    format!(", {} unread", c.unread)
                } else {
                    String::new()
                },
                c.last_body
            ),
        )
    });
    response
}

/// The bubbles of one thread inside a scroll area, laying out only those near `viewport`.
/// `list_origin` is the top of the scroll content (`ui.min_rect().top()` before anything was added).
fn thread_bubbles(
    ui: &mut egui::Ui,
    pal: &Palette,
    heights: &mut HashMap<(i64, u32), f32>,
    messages: &[MessageItem],
    viewport: egui::Rect,
    list_origin: f32,
) {
    let today = localtime::now().date;
    // Only the bubbles near the viewport are laid out. Heights are measured the first time a
    // bubble is drawn and remembered; unseen ones count as an estimate, so a frame costs
    // O(visible) layout plus O(n) additions, whatever the thread length.
    let top = viewport.min.y;
    let bottom = viewport.max.y;
    let width_bucket = (ui.available_width() / 8.0) as u32;
    let base = ui.cursor().top() - list_origin;
    let mut days = Vec::with_capacity(messages.len());
    let mut last_day = None;
    for m in messages {
        let day = local_day(m.date_ms);
        days.push((last_day != Some(day)).then_some(day));
        last_day = Some(day);
    }
    let height_of = |heights: &HashMap<(i64, u32), f32>, i: usize, m: &MessageItem| {
        heights
            .get(&(m.uid, width_bucket))
            .copied()
            .unwrap_or(ESTIMATED_BUBBLE + if days[i].is_some() { 34.0 } else { 0.0 })
    };
    let mut y = base;
    let (mut first, mut last) = (0, 0);
    let mut before = 0.0;
    let mut after = 0.0;
    for (i, m) in messages.iter().enumerate() {
        let h = height_of(heights, i, m);
        if y + h < top - OVERSCAN {
            first = i + 1;
            before += h;
        } else if y <= bottom + OVERSCAN {
            last = i + 1;
        } else {
            after += h;
        }
        y += h;
    }
    let last = last.max(first);
    ui.add_space(before);
    for (i, m) in messages.iter().enumerate().take(last).skip(first) {
        let start = ui.cursor().top();
        if let Some(day) = days[i] {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(theme::caption(pal, day_label(day, today)));
                ui.add_space(4.0);
            });
        }
        bubble(ui, pal, m);
        let measured = ui.cursor().top() - start;
        let known = heights.insert((m.uid, width_bucket), measured);
        if known.is_none_or(|k| (k - measured).abs() > 0.5) {
            ui.ctx().request_repaint();
        }
    }
    ui.add_space(after);
    if heights.len() > 8 * MAX_MESSAGES_LIMIT {
        heights.clear();
    }
}

/// Says so when the list is cut: this computer shows the newest conversations only; the rest stay on the phone.
fn conversations_note(view: &MessagesView) -> Option<String> {
    let shown = view.conversations.len();
    let on_phone = view
        .devices
        .iter()
        .filter_map(|d| d.threads_on_phone)
        .sum::<usize>()
        .max(view.conversations_total);
    if view.conversations_total > shown {
        Some(format!(
            "Showing the newest {} of {} conversations. Search to find older ones.",
            fmt_count(shown),
            fmt_count(on_phone)
        ))
    } else if on_phone > shown && view.conversations_total == shown {
        Some(format!(
            "Showing the newest {} of {} conversations; older ones stay on the phone.",
            fmt_count(shown),
            fmt_count(on_phone)
        ))
    } else {
        None
    }
}

/// 1234 -> "1,234".
fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn bubble(ui: &mut egui::Ui, pal: &Palette, m: &bridge_core::phone_view::MessageItem) {
    let layout = if m.outgoing {
        Layout::right_to_left(Align::TOP)
    } else {
        Layout::left_to_right(Align::TOP)
    };
    ui.with_layout(layout, |ui| {
        ui.add_space(14.0);
        let (fill, fg) = if m.outgoing {
            (pal.accent_fill, pal.on_accent)
        } else {
            (pal.control, pal.text)
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width() * 0.78).max(120.0));
                    if !m.body.is_empty() {
                        ui.add(
                            egui::Label::new(RichText::new(m.body.clone()).color(fg).size(14.0))
                                .wrap(),
                        );
                    }
                    if m.attachments > 0 || m.mms {
                        let n = m.attachments.max(1);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Attachment{}",
                                    if n > 1 {
                                        format!(" ({n})")
                                    } else {
                                        String::new()
                                    }
                                ))
                                .color(fg)
                                .size(13.0)
                                .strong(),
                            )
                            .wrap(),
                        );
                        ui.label(
                            RichText::new("Picture message: open it on the phone")
                                .color(fg.gamma_multiply(0.75))
                                .size(12.0),
                        );
                    }
                    ui.label(
                        RichText::new(localtime::clock_label_local(
                            i64::try_from(m.date_ms).unwrap_or(0),
                        ))
                        .color(fg.gamma_multiply(0.7))
                        .size(12.0),
                    );
                });
            });
    });
    ui.add_space(3.0);
}

impl AppView for Messages {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        if self.limit_for != self.thread {
            self.limit_for.clone_from(&self.thread);
            self.message_limit = 0;
        }
        let request = Request::MessagesView {
            device_id: self.thread.as_ref().map(|(d, _)| d.clone()),
            thread_id: self.thread.as_ref().map(|(_, t)| *t),
            query: Some(self.search.trim().to_owned()).filter(|q| !q.is_empty()),
            message_limit: self.message_limit,
        };
        match &self.feed {
            Some(feed) => feed.set_params(request),
            None => {
                let repaint = ctx.clone();
                self.feed = Some(phone_feed::messages_feed(
                    move || repaint.request_repaint(),
                    request,
                ));
            }
        }
        if !self.env_checked {
            // Screenshot and test hooks: start in a given state (never set by the launchers).
            self.env_checked = true;
            if let Ok(q) = std::env::var("GEEKBRIDGE_MESSAGES_SEARCH") {
                self.search = q.chars().take(64).collect();
            }
            if std::env::var("GEEKBRIDGE_MESSAGES_COMPOSE").is_ok() {
                self.draft = std::env::var("GEEKBRIDGE_MESSAGES_DRAFT").unwrap_or_default();
                self.recipient = "+44 20 7946 0001".into();
                self.open_compose();
            }
        }
        let (view, state) = self.view();
        if !self.open_number_checked {
            self.open_number_checked = true;
            self.open_number = crate::prefill::take_message_to();
        }
        if let Some(n) = self.open_number.take_if(|_| state.view.is_some()) {
            let found = view
                .conversations
                .iter()
                .find(|c| c.addresses.iter().any(|a| same_number(a, &n)));
            match found {
                Some(c) => {
                    self.thread = Some((c.device_id.clone(), c.thread_id));
                    self.section = Section::Conversations;
                }
                None => {
                    self.recipient = n;
                    self.open_compose();
                }
            }
        }
        // A search can hide the open conversation: follow the first result instead of a stale pane.
        let hidden = self.thread.as_ref().is_some_and(|(d, t)| {
            !self.search.trim().is_empty()
                && !view
                    .conversations
                    .iter()
                    .any(|c| c.device_id == *d && c.thread_id == *t)
        });
        if hidden {
            self.thread = None;
        }
        if self.thread.is_none()
            && self.search.trim().is_empty()
            && let Some(first) = view.conversations.first()
        {
            self.thread = Some((first.device_id.clone(), first.thread_id));
        }
        self.toolbar(ctx, env);
        self.sidebar(ctx, env);
        // A device that disappeared from the network can't stay selected.
        if let Section::Device(id) = &self.section
            && !env.model.devices().iter().any(|d| &d.id == id)
        {
            self.section = Section::Conversations;
        }
        match self.section.clone() {
            Section::Conversations => {
                if !view.conversations.is_empty() || !self.search.trim().is_empty() {
                    self.thread_list(ctx, env, &view);
                }
                central(ctx, pal, Margin::ZERO, |ui| {
                    self.conversations(ui, env, &view)
                })
            }
            Section::Compose => central(
                ctx,
                pal,
                Margin {
                    left: 28,
                    right: 28,
                    top: 14,
                    bottom: 14,
                },
                |ui| self.compose(ui, env),
            ),
            Section::Device(id) => central(ctx, pal, Margin::same(28), |ui| {
                if let Some(device) = env.model.devices().iter().find(|d| d.id == id) {
                    self.device(ui, env, device);
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::{AppKind, Snapshot};

    #[test]
    fn display_text_follows_real_state_and_offers_shorter_fallbacks() {
        let checking = lcd_candidates(&DiscoveryModel::Checking);
        assert_eq!(checking, ["SEARCHING"]);
        let empty = DiscoveryModel::from_snapshot(AppKind::Messages, &Snapshot::empty(1716));
        assert_eq!(lcd_candidates(&empty), ["NO PHONE FOUND", "NO PHONE"]);
        let down = DiscoveryModel::service_unavailable("refused".into());
        assert_eq!(lcd_candidates(&down).last().unwrap(), "HELPER OFF");
        for model in [&DiscoveryModel::Checking, &empty, &down] {
            let list = lcd_candidates(model);
            assert!(list.windows(2).all(|w| w[0].len() >= w[1].len()));
            assert!(
                list.iter()
                    .all(|t| t.chars().all(|c| c.is_ascii_uppercase() || c == ' '))
            );
        }
    }

    #[test]
    fn compose_focus_goes_to_the_first_empty_field() {
        let mut messages = Messages::default();
        messages.open_compose();
        assert!(matches!(messages.focus, Some(Focus::Recipient)));
        messages.recipient = "+15550100".into();
        messages.open_compose();
        assert!(matches!(messages.focus, Some(Focus::Draft)));
        assert!(messages.has_draft());
    }

    fn send(phase: SendPhase, text: &str, created: u64) -> SendItem {
        SendItem {
            send_id: created,
            device_id: "d".into(),
            recipient: "+1 555 123 4567".into(),
            text: text.into(),
            phase,
            reason: None,
            created_unix_ms: created,
            updated_unix_ms: created,
        }
    }
    fn item(outgoing: bool, body: &str, date: u64) -> MessageItem {
        MessageItem {
            uid: 1,
            mms: false,
            outgoing,
            body: body.into(),
            date_ms: date,
            attachments: 0,
            sub_id: None,
        }
    }

    #[test]
    fn segment_labels_count_gsm_and_unicode() {
        assert_eq!(segment_label(""), "Empty");
        assert_eq!(segment_label("hello"), "1 SMS, 155 left in the last one");
        assert!(segment_label(&"a".repeat(161)).starts_with("2 SMS,"));
        let emoji = segment_label("party 🎉");
        assert!(emoji.contains("Unicode"), "{emoji}");
        assert!(segment_label(&"é".repeat(5)).starts_with("1 SMS,"));
        assert!(segment_label(&"я".repeat(71)).starts_with("2 SMS (Unicode"));
        assert!(segment_hint("🎉").contains("UTF-16"));
    }

    #[test]
    fn a_queued_or_unconfirmed_send_is_never_called_delivered() {
        for phase in [
            SendPhase::Queued,
            SendPhase::HandedToPhone,
            SendPhase::AcceptedByPhone,
            SendPhase::Sent,
            SendPhase::Failed,
            SendPhase::Uncertain,
        ] {
            for reports in [false, true] {
                let (text, _) = send_phase_text(&send(phase, "x", 1), reports);
                assert!(!text.starts_with("delivered"), "{phase:?}: {text}");
            }
        }
        assert_eq!(
            send_phase_text(&send(SendPhase::Delivered, "x", 1), true).0,
            "delivered"
        );
        // Only failed and uncertain sends offer a retry, and the retry is a new record.
        assert!(send_phase_text(&send(SendPhase::Failed, "x", 1), true).1);
        assert!(send_phase_text(&send(SendPhase::Uncertain, "x", 1), true).1);
        assert!(!send_phase_text(&send(SendPhase::Sent, "x", 1), true).1);
        assert!(!send_phase_text(&send(SendPhase::HandedToPhone, "x", 1), false).1);
    }

    #[test]
    fn failure_text_is_plain_language_not_a_code() {
        let mut s = send(SendPhase::Failed, "x", 1);
        s.reason = Some("no_service".into());
        assert!(send_phase_text(&s, true).0.contains("no mobile signal"));
    }

    #[test]
    fn day_labels_are_relative_then_dated() {
        let today = localtime::Date {
            year: 2026,
            month: 11,
            day: 4,
        };
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(today.add_days(-1), today), "Yesterday");
        assert_eq!(day_label(today.add_days(-3), today), "Sunday");
        assert_eq!(day_label(today.add_days(-30), today), "Mon 5 Oct");
        assert_eq!(
            day_label(
                localtime::Date {
                    year: 2025,
                    month: 12,
                    day: 31
                },
                today
            ),
            "Wed 31 Dec 2025"
        );
    }

    #[test]
    fn local_sends_stay_visible_until_the_phone_echoes_them() {
        let msgs = vec![item(false, "hi", 1_000)];
        let addrs = vec!["+15551234567".to_string()];
        let queued = send(SendPhase::HandedToPhone, "on my way", 5_000);
        let sends = vec![queued.clone()];
        assert_eq!(pending_sends(&sends, "d", &addrs, &msgs, 3).len(), 1);
        // The message now appears in the thread: the line goes away.
        let echoed = vec![item(true, "on my way", 6_000)];
        assert!(pending_sends(&sends, "d", &addrs, &echoed, 3).is_empty());
        // A failed or uncertain send keeps its line (and its retry) even if a twin exists.
        let failed = vec![send(SendPhase::Failed, "on my way", 5_000)];
        assert_eq!(pending_sends(&failed, "d", &addrs, &echoed, 3).len(), 1);
        // Another phone or another person's number never mixes in.
        assert!(pending_sends(&sends, "other", &addrs, &msgs, 3).is_empty());
        assert!(pending_sends(&sends, "d", &["+4930123456".to_string()], &msgs, 3).is_empty());
        // Bounded, newest kept.
        let many: Vec<_> = (0..9)
            .map(|i| send(SendPhase::Failed, "t", 100 + i))
            .collect();
        let got = pending_sends(&many, "d", &addrs, &msgs, 3);
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].created_unix_ms, 108);
    }

    #[test]
    fn blockers_explain_missing_permission_restricted_settings_and_read_only_phones() {
        let mut d = DataDevice {
            device_id: "d".into(),
            name: "Pixel".into(),
            contacts: DataState::ConsentOff,
            calendar: DataState::ConsentOff,
            sms: DataState::NotOffered,
            calendar_edit: DataState::ConsentOff,
            contacts_count: 0,
            events_count: 0,
            calendar_names: vec![],
            calendar_writable: false,
            calendar_truncated: false,
            contacts_capped: false,
            threads_on_phone: None,
            contacts_synced_unix_ms: None,
            calendar_synced_unix_ms: None,
            sms_status_reports: false,
            sms_send: false,
            sims: vec![],
        };
        let text = sms_blocker(&d).unwrap();
        assert!(text.contains("Allow restricted settings"), "{text}");
        assert!(text.contains("default SMS app"));
        d.sms = DataState::Ready;
        assert!(sms_blocker(&d).is_none());
        assert!(send_blocker(&d).unwrap().contains("read messages"));
        d.sms_send = true;
        assert!(send_blocker(&d).is_none());
    }

    #[test]
    fn sims_are_labelled_by_name_and_slot() {
        let sim = |slot, label: &str| SimItem {
            id: 3,
            slot,
            label: label.into(),
        };
        assert_eq!(sim_label(&sim(2, "Work")), "Work (SIM 2)");
        assert_eq!(sim_label(&sim(1, "")), "SIM 1");
        assert_eq!(sim_label(&sim(0, "")), "SIM 3");
    }

    #[test]
    fn capitalizes_device_kind() {
        assert_eq!(capitalize("phone"), "Phone");
        assert_eq!(capitalize(""), "");
    }

    /// Frame cost of a conversation pane with many bubbles (headless: shapes built and tessellated, nothing drawn).
    /// `--nocapture` prints the numbers; `GEEKBRIDGE_SCALE=1` runs 2,000 messages and enforces a budget.
    #[test]
    fn thread_frame_cost_with_many_messages() {
        use std::time::Instant;
        let big = std::env::var("GEEKBRIDGE_SCALE").as_deref() == Ok("1");
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        theme::apply(&ctx, bridge_core::AppKind::Messages);
        let pal = theme::palette(bridge_core::AppKind::Messages, true);
        let messages = |n: usize| -> Vec<MessageItem> {
            (0..n)
                .map(|i| MessageItem {
                    uid: i as i64,
                    mms: false,
                    outgoing: i % 2 == 0,
                    body: format!("message {i} about lunch 🎉 Café 東京 the quick brown fox jumps over the lazy dog"),
                    date_ms: 1_793_523_600_000 + i as u64 * 60_000,
                    attachments: 0,
                    sub_id: None,
                })
                .collect()
        };
        let heights = std::cell::RefCell::new(HashMap::new());
        let cost = |list: &[MessageItem]| -> f64 {
            heights.borrow_mut().clear();
            let mut total = 0.0;
            for round in 0..8 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 640.0),
                    )),
                    ..Default::default()
                };
                let t = Instant::now();
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .drag_to_scroll(false)
                            .stick_to_bottom(true)
                            .show_viewport(ui, |ui, viewport| {
                                let origin = ui.min_rect().top();
                                thread_bubbles(
                                    ui,
                                    &pal,
                                    &mut heights.borrow_mut(),
                                    list,
                                    viewport,
                                    origin,
                                );
                            });
                    });
                });
                ctx.tessellate(out.shapes, out.pixels_per_point);
                if round >= 3 {
                    total += t.elapsed().as_secs_f64() * 1000.0;
                }
            }
            total / 5.0
        };
        let (small, large) = (
            cost(&messages(100)),
            cost(&messages(if big { 2000 } else { 400 })),
        );
        println!(
            "scale thread: 100 bubbles {small:.2} ms/frame, {} bubbles {large:.2} ms/frame",
            if big { 2000 } else { 400 }
        );
        if big && !cfg!(debug_assertions) {
            assert!(large < 16.0, "{large:.2} ms for 2000 bubbles");
        }
    }

    #[test]
    fn a_cut_conversation_list_says_how_much_is_shown() {
        let mut v = MessagesView::default();
        assert_eq!(conversations_note(&v), None);
        v.conversations = (0..100)
            .map(|i| ConversationItem {
                thread_id: i,
                ..ConversationItem::default()
            })
            .collect();
        v.conversations_total = 100;
        assert_eq!(conversations_note(&v), None);
        v.conversations_total = 300;
        assert_eq!(
            conversations_note(&v).unwrap(),
            "Showing the newest 100 of 300 conversations. Search to find older ones."
        );
        let d = DataDevice {
            threads_on_phone: Some(1234),
            ..DataDevice::default()
        };
        v.devices.push(d);
        assert!(
            conversations_note(&v)
                .unwrap()
                .contains("of 1,234 conversations")
        );
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(20000), "20,000");
    }
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod render_tests;
