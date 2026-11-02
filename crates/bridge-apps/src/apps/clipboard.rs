//! Clipboard: per-phone sync switches and status, "paste from phone", and a small scratchpad.
//! Computer-to-phone copying is automatic for phones you turn on; phone-to-computer is pulled with a
//! shortcut because Android does not let apps read the clipboard in the background.
use crate::{
    clipboard_link::{self, Action, ClipLink},
    input,
    shell::{AppView, Env, central},
    theme::{self, ButtonKind},
};
use bridge_core::hotkey::Hotkey;
use eframe::egui::{self, FontFamily, FontId, Margin, RichText, ScrollArea};

const FLASH_SECONDS: f64 = 1.6;

#[derive(Default)]
pub struct Clipboard {
    text: String,
    copied_at: Option<f64>,
    link: Option<ClipLink>,
    hotkey_draft: Option<String>,
}

/// True when the configured shortcut was pressed in this window this frame.
fn shortcut_pressed(ctx: &egui::Context, hotkey: &str) -> bool {
    let Ok(h) = Hotkey::parse(hotkey) else {
        return false;
    };
    let Some(key) = egui::Key::from_name(&h.key) else {
        return false;
    };
    let wanted = egui::Modifiers {
        alt: h.alt,
        ctrl: h.ctrl,
        shift: h.shift,
        mac_cmd: false,
        command: false,
    };
    let _ = h.meta;
    ctx.input_mut(|i| i.consume_key(wanted, key))
}

impl AppView for Clipboard {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        let link = self.link.get_or_insert_with(|| {
            let repaint = ctx.clone();
            ClipLink::spawn(move || repaint.request_repaint())
        });
        link.update();
        // Request a repaint every second so counters and the pull result stay fresh.
        ctx.request_repaint_after(crate::pace::scaled(std::time::Duration::from_secs(1)));
        let hotkey = link
            .state
            .status
            .as_ref()
            .map_or("Ctrl+Shift+V".to_owned(), |s| s.hotkey.clone());
        if shortcut_pressed(ctx, &hotkey) {
            link.send(Action::Pull);
        }
        central(ctx, pal, Margin::symmetric(20, 16), |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let now = ui.input(|i| i.time);
                    let flashing = self.copied_at.is_some_and(|t| now - t < FLASH_SECONDS);
                    if flashing {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(200));
                    } else {
                        self.copied_at = None;
                    }

                    ui.label(theme::overline(pal, "This computer"));
                    theme::group(ui, pal, |ui| {
                        let rows =
                            ((ui.ctx().screen_rect().height() - 420.0) / 20.0).clamp(3.0, 24.0);
                        let edit = ui.add(
                            egui::TextEdit::multiline(&mut self.text)
                                .frame(false)
                                .hint_text(
                                    RichText::new("Type or paste text here (Ctrl/Cmd+V)")
                                        .color(pal.text3),
                                )
                                .font(FontId::new(14.5, FontFamily::Proportional))
                                .desired_rows(rows as usize)
                                .desired_width(f32::INFINITY),
                        );
                        if edit.changed() {
                            input::limit_chars(&mut self.text, input::MAX_CLIPBOARD_CHARS);
                        }
                        ui.add_space(2.0);
                        let count = self.text.chars().count();
                        ui.label(theme::caption(
                            pal,
                            format!(
                                "{count} character{}  ·  Text stays in this window",
                                if count == 1 { "" } else { "s" }
                            ),
                        ));
                    });

                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let label = if flashing {
                            "Copied"
                        } else {
                            "Copy to this computer"
                        };
                        if theme::pill_button(
                            ui,
                            pal,
                            label,
                            ButtonKind::Primary,
                            !self.text.is_empty(),
                        )
                        .clicked()
                        {
                            ui.ctx().copy_text(self.text.clone());
                            self.copied_at = Some(now);
                        }
                        if theme::pill_button(
                            ui,
                            pal,
                            "Clear",
                            ButtonKind::Plain,
                            !self.text.is_empty(),
                        )
                        .clicked()
                        {
                            self.text.clear();
                        }
                    });
                    ui.label(theme::caption(
                        pal,
                        "This scratchpad is only for typing text to copy. It is never sent anywhere.",
                    ));

                    ui.add_space(10.0);
                    sync_section(ui, pal, self.link.as_mut().expect("created above"), &mut self.hotkey_draft);
                });
        });
    }
}

pub(crate) fn sync_section(
    ui: &mut egui::Ui,
    pal: &theme::Palette,
    link: &mut ClipLink,
    draft: &mut Option<String>,
) {
    ui.label(theme::overline(pal, "Phone sync"));
    sync_body(ui, pal, link, draft);
}

/// The sync status, per-phone switches, paste/send buttons, shortcut and window settings.
pub(crate) fn sync_body(
    ui: &mut egui::Ui,
    pal: &theme::Palette,
    link: &mut ClipLink,
    draft: &mut Option<String>,
) {
    let state = link.state.clone();
    let Some(status) = state.status else {
        theme::group(ui, pal, |ui| {
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    state
                        .error
                        .as_deref()
                        .unwrap_or("Checking the GeekBridge service…"),
                ))
                .wrap(),
            );
        });
        return;
    };
    theme::group(ui, pal, |ui| {
        ui.add(egui::Label::new(theme::caption(
            pal,
            "From this computer to the phone: automatic for phones you turn on below (text and links; passwords from password managers are skipped).",
        )).wrap());
        ui.add_space(2.0);
        ui.add(egui::Label::new(theme::caption(
            pal,
            "From the phone to this computer: you ask for it. Android doesn't let apps read the clipboard in the background, so the phone shares only what you copied recently and only while it's unlocked.",
        )).wrap());
        ui.add_space(4.0);
        ui.add(egui::Label::new(theme::caption(pal, clipboard_link::backend_line(&status))).wrap());
    });

    ui.add_space(6.0);
    theme::group(ui, pal, |ui| {
        if status.devices.is_empty() {
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    "No paired phones yet. Pair one from the GeekBridge window: Pair with QR code.",
                ))
                .wrap(),
            );
        }
        for d in &status.devices {
            ui.horizontal_wrapped(|ui| {
                let mut on = d.enabled;
                let label = clipboard_link::device_name(&d.name);
                if ui.checkbox(&mut on, label).changed() {
                    link.send(Action::SetEnabled {
                        device_id: d.device_id.clone(),
                        enabled: on,
                    });
                }
                let line = if !d.enabled {
                    "Off · tick the box to turn Clipboard on for this phone".to_owned()
                } else if !d.connected {
                    "Waiting for the phone to connect".to_owned()
                } else if !d.negotiated {
                    "Connected, but its app doesn't do clipboard sync".to_owned()
                } else {
                    format!("On · sent {} · received {}", d.sent, d.received)
                };
                ui.label(theme::caption(pal, line));
            });
            if d.enabled
                && let Some(skip) = &d.last_skip
            {
                ui.label(theme::caption(
                    pal,
                    format!("   {}", clipboard_link::skip_line(skip)),
                ));
            }
        }
    });

    ui.add_space(6.0);
    theme::group(ui, pal, |ui| {
        let any_ready = status
            .devices
            .iter()
            .any(|d| d.enabled && d.connected && d.negotiated);
        ui.horizontal_wrapped(|ui| {
            if theme::pill_button(ui, pal, "Paste from phone", ButtonKind::Primary, any_ready)
                .clicked()
            {
                link.send(Action::Pull);
            }
            if theme::pill_button(
                ui,
                pal,
                "Send clipboard to phone",
                ButtonKind::Plain,
                any_ready,
            )
            .clicked()
            {
                link.send(Action::Push);
            }
        });
        if let Some(p) = &status.last_pull {
            ui.add(egui::Label::new(theme::secondary(pal, clipboard_link::pull_line(p))).wrap());
        }
        ui.add(egui::Label::new(theme::caption(pal, clipboard_link::hotkey_line(&status))).wrap());
        let mut command: &str = clipboard_link::PASTE_COMMAND;
        ui.add(egui::TextEdit::singleline(&mut command).desired_width(f32::INFINITY));
        if let Some(n) = &state.notice {
            ui.add(egui::Label::new(theme::caption(pal, n)).wrap());
        }
    });

    ui.add_space(6.0);
    theme::group(ui, pal, |ui| {
        ui.add(
            egui::Label::new(theme::caption(
                pal,
                clipboard_link::window_line(status.window_ms),
            ))
            .wrap(),
        );
        let mut secs = (status.window_ms / 1000) as f32;
        let slider = ui.add(
            egui::Slider::new(&mut secs, 5.0..=600.0)
                .logarithmic(true)
                .suffix(" s")
                .text("Time window"),
        );
        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
            link.send(Action::SetWindowSeconds(secs as u64));
        }
        ui.horizontal(|ui| {
            ui.label(theme::caption(pal, "Shortcut"));
            let text = draft.get_or_insert_with(|| status.hotkey.clone());
            let edit = ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
            if (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.small_button("Save").clicked()
            {
                link.send(Action::SetHotkey(text.clone()));
                *draft = None;
            }
        });
        ui.label(theme::caption(pal, format!(
            "Images (PNG/JPEG) up to {} MB travel as a separate stream, never inline. Nothing is kept as history.",
            status.max_image_bytes / (1024 * 1024)
        )));
    });
}
