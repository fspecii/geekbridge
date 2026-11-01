//! Messages: an iTunes-style window (toolbar with a status display, library sidebar with a
//! devices group, list/detail content) drawn in a monochrome dot-matrix look with a single
//! red accent. Drafts and copying are real and local; sending stays blocked until a messaging
//! backend is authorized for a paired phone. Nothing here is sample data.
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
    phone_view::{
        ConversationItem, DataDevice, DataState, MessagesView, SelectedThread, SendItem, SendPhase,
    },
};
use eframe::egui::{
    self, Align, CornerRadius, FontFamily, FontId, Layout, Margin, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, vec2,
};
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
}

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
        let led = if tone == Tone::Neutral && matches!(env.model, DiscoveryModel::Checking) {
            pal.text3
        } else {
            pal.accent
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
                let count = self.view().0.conversations.len().to_string();
                if theme::sidebar_item(
                    ui,
                    pal,
                    Icon::Chat,
                    "Conversations",
                    Some(&count),
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
        let offset = localtime::utc_offset_minutes(localtime::now().date);
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
                egui::ScrollArea::vertical()
                    .id_salt("thread-list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for c in &view.conversations {
                            let selected = self
                                .thread
                                .as_ref()
                                .is_some_and(|(d, t)| *d == c.device_id && *t == c.thread_id);
                            if thread_row(ui, pal, c, selected, now_ms, offset).clicked() {
                                self.thread = Some((c.device_id.clone(), c.thread_id));
                            }
                        }
                    });
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
            });
        theme::hairline(ui, pal);
        let footer = 150.0;
        let body_h = (ui.available_height() - footer).max(80.0);
        let offset = localtime::utc_offset_minutes(localtime::now().date);
        egui::ScrollArea::vertical()
            .id_salt("thread-scroll")
            .max_height(body_h)
            .min_scrolled_height(body_h)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                if sel.may_have_older
                    && theme::pill_button(ui, pal, "Load older messages", ButtonKind::Plain, true)
                        .clicked()
                {
                    self.run(Request::MessagesRefresh {
                        device_id: device.clone(),
                        thread_id: Some(thread),
                    });
                }
                for m in &sel.messages {
                    bubble(ui, pal, m, offset);
                }
                ui.add_space(6.0);
            });
        theme::hairline(ui, pal);
        let mine: Vec<&SendItem> = view
            .sends
            .iter()
            .filter(|s| {
                s.device_id == device
                    && conv
                        .is_some_and(|c| c.addresses.iter().any(|a| same_number(a, &s.recipient)))
            })
            .take(3)
            .collect();
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
                && theme::pill_button(ui, pal, "Send again", ButtonKind::Plain, true)
                    .on_hover_text("The recipient may get this twice.")
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
        let ready = dev.is_some_and(|d| d.sms == DataState::Ready);
        let to = conv.and_then(|c| c.addresses.first().cloned());
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.reply)
                .frame(false)
                .hint_text(egui::RichText::new("Reply").color(pal.text3))
                .font(FontId::new(15.0, FontFamily::Proportional))
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        input::limit_chars(&mut self.reply, input::MAX_DRAFT_CHARS);
        let blocker = if !ready {
            Some(
                dev.and_then(|d| phone_ui::state_line(d.sms, "Messages"))
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
            ui.label(theme::caption(
                pal,
                format!("{}/{}", self.reply.chars().count(), input::MAX_DRAFT_CHARS),
            ));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::round_button(
                    ui,
                    pal,
                    34.0,
                    pal.accent,
                    Icon::Send,
                    can,
                    "Send as text message",
                )
                .clicked()
                    && let (Some(to), Some(d)) = (to, dev)
                {
                    let submission_id = self.submission();
                    self.run(Request::MessagesSend {
                        device_id: d.device_id.clone(),
                        recipient: to,
                        text: self.reply.clone(),
                        submission_id,
                    });
                    self.reply.clear();
                }
            });
        });
        let _ = sel;
    }

    fn conversations_empty(&mut self, ui: &mut egui::Ui, env: &Env, view: &MessagesView) {
        let pal = env.pal;
        let state = self.feed.as_ref().map(Feed::state).unwrap_or_default();
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
                    .find_map(|d| phone_ui::state_line(d.sms, "Messages"))
                    .unwrap_or_else(|| "Nothing has been shared from a phone yet.".into())
            };
            ui.add(egui::Label::new(theme::secondary(pal, why)).wrap());
            if let Some(n) = &state.notice {
                ui.label(theme::caption(pal, n.clone()));
            }
            if let Some(r) = phone_ui::allow_buttons(ui, pal, env, Feature::Sms, "Messages") {
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
            let to = ui.add(
                egui::TextEdit::singleline(&mut self.recipient)
                    .frame(false)
                    .hint_text(RichText::new("Phone number").color(pal.text3))
                    .font(FontId::new(16.0, FontFamily::Proportional))
                    .vertical_align(Align::Center)
                    .desired_width(f32::INFINITY),
            );
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

        let footer_h = 142.0;
        let body_h = (ui.available_height() - footer_h).max(70.0);
        egui::ScrollArea::vertical()
            .id_salt("draft-scroll")
            .max_height(body_h)
            .min_scrolled_height(body_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let rows = (body_h / 22.0).floor().max(3.0) as usize;
                let body = ui.add(
                    egui::TextEdit::multiline(&mut self.draft)
                        .frame(false)
                        .hint_text(RichText::new("Write a message").color(pal.text3))
                        .font(FontId::new(15.0, FontFamily::Proportional))
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY),
                );
                if body.changed() {
                    input::limit_chars(&mut self.draft, input::MAX_DRAFT_CHARS);
                }
                if matches!(self.focus, Some(Focus::Draft)) {
                    body.request_focus();
                    self.focus = None;
                }
            });

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
            .find(|d| d.sms == DataState::Ready)
            .cloned();
        let own_block = target.is_none().then(|| {
            view.devices
                .iter()
                .find_map(|d| phone_ui::state_line(d.sms, "Messages"))
                .unwrap_or_else(|| {
                    "Messages are not allowed for any phone yet. Open Conversations to allow them."
                        .to_owned()
                })
        });
        let blocker = env.blocked_line("send").or(own_block);
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
                    pal.accent,
                    Icon::Send,
                    can_send,
                    "Send as text message",
                )
                .clicked()
                    && let Some(d) = &target
                {
                    let submission_id = self.submission();
                    self.run(Request::MessagesSend {
                        device_id: d.device_id.clone(),
                        recipient: self.recipient.clone(),
                        text: self.draft.clone(),
                        submission_id,
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
                            phone_ui::state_line(d.sms, "Messages")
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
                send.reason.as_ref().map_or(String::new(), |r| format!(" ({r})"))
            ),
            true,
        ),
        SendPhase::Uncertain => (
            "unknown: the connection dropped after it was handed over, so it may or may not have been sent".into(),
            true,
        ),
    }
}

fn thread_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    c: &ConversationItem,
    selected: bool,
    now_ms: u64,
    offset: i32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
    let radius = CornerRadius::same(8);
    if selected {
        ui.painter().rect_filled(rect, radius, pal.selected_bg);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, radius, pal.control_hover.gamma_multiply(0.55));
    }
    let fg = if selected { pal.selected_fg } else { pal.text };
    let sub = if selected { pal.selected_fg } else { pal.text2 };
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
        format!("{prefix}{}", short(&c.last_body.replace('\n', " "), 40)),
        FontId::new(12.5, FontFamily::Proportional),
        sub,
    );
    let when = if now_ms.saturating_sub(c.last_date_ms) < 86_400_000 {
        localtime::clock_label(i64::try_from(c.last_date_ms).unwrap_or(0), offset)
    } else {
        phone_ui::age_label(now_ms, c.last_date_ms)
    };
    ui.painter().text(
        egui::pos2(rect.right() - 12.0, rect.top() + 18.0),
        egui::Align2::RIGHT_CENTER,
        when,
        FontId::new(11.0, FontFamily::Proportional),
        sub,
    );
    if c.unread > 0 {
        ui.painter().circle_filled(
            egui::pos2(rect.right() - 16.0, rect.bottom() - 16.0),
            5.0,
            pal.accent,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{}: {}", c.title, c.last_body),
        )
    });
    response
}

fn bubble(ui: &mut egui::Ui, pal: &Palette, m: &bridge_core::phone_view::MessageItem, offset: i32) {
    let layout = if m.outgoing {
        Layout::right_to_left(Align::TOP)
    } else {
        Layout::left_to_right(Align::TOP)
    };
    ui.with_layout(layout, |ui| {
        ui.add_space(14.0);
        let (fill, fg) = if m.outgoing {
            (pal.accent, pal.on_accent)
        } else {
            (pal.control, pal.text)
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.78).max(120.0));
                let mut text = m.body.clone();
                if m.attachments > 0 {
                    text.push_str(&format!("\n[{} attachment(s) not shown]", m.attachments));
                }
                ui.add(egui::Label::new(RichText::new(text).color(fg).size(14.0)).wrap());
                ui.label(
                    RichText::new(localtime::clock_label(
                        i64::try_from(m.date_ms).unwrap_or(0),
                        offset,
                    ))
                    .color(fg.gamma_multiply(0.7))
                    .size(10.5),
                );
            });
    });
    ui.add_space(3.0);
}

impl AppView for Messages {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let request = Request::MessagesView {
            device_id: self.thread.as_ref().map(|(d, _)| d.clone()),
            thread_id: self.thread.as_ref().map(|(_, t)| *t),
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
        let (view, _) = self.view();
        if self.thread.is_none()
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
                if !view.conversations.is_empty() {
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

    #[test]
    fn capitalizes_device_kind() {
        assert_eq!(capitalize("phone"), "Phone");
        assert_eq!(capitalize(""), "");
    }
}
