//! Messages: an iTunes-style window (toolbar with a status display, library sidebar with a
//! devices group, list/detail content) drawn in a monochrome dot-matrix look with a single
//! red accent. Drafts and copying are real and local; sending stays blocked until a messaging
//! backend is authorized for a paired phone. Nothing here is sample data.
use crate::{
    dotmatrix,
    icons::{self, Icon},
    input,
    shell::{AppView, Env, central},
    state::{DeviceInfo, DiscoveryModel, Tone},
    theme::{self, ButtonKind, Palette},
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
                if theme::sidebar_item(
                    ui,
                    pal,
                    Icon::Chat,
                    "Conversations",
                    Some("0"),
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

    fn conversations(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // List header in the style of a library column header; these are labels, not sort buttons.
        let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        ui.painter()
            .rect_filled(header, 0.0, pal.toolbar.gamma_multiply(0.6));
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.separator),
        );
        let narrow = header.width() < 480.0;
        for (text, x, align) in [
            ("Contact", header.left() + 22.0, egui::Align2::LEFT_CENTER),
            (
                "Last message",
                header.left() + header.width() * if narrow { 0.42 } else { 0.34 },
                egui::Align2::LEFT_CENTER,
            ),
            ("Time", header.right() - 22.0, egui::Align2::RIGHT_CENTER),
        ] {
            ui.painter().text(
                egui::pos2(x, header.center().y),
                align,
                text.to_uppercase(),
                FontId::new(10.5, theme::semibold_family()),
                pal.text3,
            );
        }

        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.2).clamp(20.0, 90.0));
            let pitch = 6.5;
            let size = dotmatrix::measure("0", pitch);
            let (rect, _) = ui.allocate_exact_size(size + vec2(0.0, 2.0), Sense::hover());
            dotmatrix::paint(ui.painter(), rect.min, "0", pitch, pal.text, None);
            ui.add_space(14.0);
            ui.label(theme::semibold("No conversations", 17.0));
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    "Nothing has been read from a phone yet, so there's nothing to list. You can still write a draft.",
                ))
                .wrap(),
            );
            ui.add_space(10.0);
            if theme::pill_button(ui, pal, "New Message", ButtonKind::Plain, true).clicked() {
                self.open_compose();
            }
        });
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

        let blocker = env.blocked_line("send");
        if let Some(reason) = &blocker {
            theme::blocked_note(ui, pal, reason);
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
                {
                    // Reached only once a messaging backend authorizes this phone.
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
                row(
                    ui,
                    "Messages",
                    "Unavailable. Pairing isn't available yet, so nothing is read from or sent to this phone."
                        .into(),
                );
            });
    }
}

impl AppView for Messages {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
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
                central(ctx, pal, Margin::ZERO, |ui| self.conversations(ui, pal))
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
