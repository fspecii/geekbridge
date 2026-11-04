// SPDX-License-Identifier: GPL-3.0-or-later
//! Calls: a dialer in the style of the macOS phone keypad. The number is local text only;
//! placing a call needs the service to offer it (Telephony allowed, the phone permits it, no call going on)
//! and the user to confirm the number in a sheet; the service re-checks everything before sending.
use crate::a11y::ScrollBars as _;
use crate::{
    call_audio_ui::CallAudioPanel,
    call_feed::{CallFeed, FeedState},
    call_ui::{self, CallPanel},
    icons::{self, Icon},
    input,
    shell::{AppView, Env, central},
    theme::{self, Palette},
};
use eframe::egui::{self, Align2, FontFamily, FontId, Margin, Sense, vec2};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Keypad,
    Recents,
}

#[derive(Default)]
pub struct Calls {
    tab: Tab,
    number: String,
    /// The 0 key was held long enough to enter "+"; the release must not also add a "0".
    zero_long_press_done: bool,
    focus_display: bool,
    /// Live call view from the service; started on the first frame.
    feed: Option<CallFeed>,
    panel: CallPanel,
    sources: FeedState,
    /// The number the user pressed Call for; nothing is dialed until they confirm it.
    confirming: Option<String>,
    /// "Call audio on this computer" checklist, shown on the Recents tab.
    audio: CallAudioPanel,
}

const KEYS: [[(char, &str); 3]; 4] = [
    [('1', ""), ('2', "ABC"), ('3', "DEF")],
    [('4', "GHI"), ('5', "JKL"), ('6', "MNO")],
    [('7', "PQRS"), ('8', "TUV"), ('9', "WXYZ")],
    [('*', ""), ('0', "+"), ('#', "")],
];

const LONG_PRESS_SECONDS: f64 = 0.5;

fn key_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    d: f32,
    key: char,
    letters: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(d, d), Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        pal.control_active
    } else if response.hovered() {
        pal.control_hover
    } else {
        pal.control
    };
    ui.painter().circle_filled(rect.center(), d / 2.0, fill);
    if response.has_focus() {
        theme::focus_circle(ui, pal, rect.center(), d / 2.0);
    }
    let main_size = (d * 0.40).clamp(18.0, 28.0);
    let has_letters = !letters.is_empty();
    let main_y = rect.center().y - if has_letters { d * 0.07 } else { 0.0 };
    // `*` sits visually high in most fonts; nudge it down.
    let nudge = if key == '*' { main_size * 0.18 } else { 0.0 };
    ui.painter().text(
        egui::pos2(rect.center().x, main_y + nudge),
        Align2::CENTER_CENTER,
        key,
        FontId::new(main_size, FontFamily::Proportional),
        pal.text,
    );
    if has_letters {
        ui.painter().text(
            egui::pos2(rect.center().x, rect.center().y + d * 0.24),
            Align2::CENTER_CENTER,
            letters,
            FontId::new((d * 0.14).clamp(12.0, 12.0), theme::semibold_family()),
            pal.text2,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Key {key}"))
    });
    response
}

fn small_round(
    ui: &mut egui::Ui,
    pal: &Palette,
    d: f32,
    icon: Icon,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(d, d), sense);
    let hot = enabled && response.hovered();
    if hot {
        ui.painter().circle_filled(
            rect.center(),
            d / 2.0,
            pal.control_hover.gamma_multiply(0.7),
        );
    }
    if response.has_focus() {
        theme::focus_circle(ui, pal, rect.center(), d / 2.0);
    }
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), vec2(d * 0.36, d * 0.36)),
        icon,
        if enabled {
            pal.text2
        } else {
            pal.text3.gamma_multiply(0.6)
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    response.on_hover_text(label)
}

impl Calls {
    fn keypad(&mut self, ui: &mut egui::Ui, env: &Env) {
        let pal = env.pal;
        let blocker = env
            .blocked_line("call")
            .or_else(|| call_ui::dial_blocker(self.sources.status.as_ref(), &self.number));
        // The no-Bluetooth warning sits above the keys so it is on screen at the smallest window too.
        let banner_shown = self.audio.banner(ui, pal);
        if banner_shown {
            ui.add_space(4.0);
        }
        let avail = ui.available_size();
        let note_h = 26.0;
        let display_h = 62.0;
        let gap = 12.0;
        let by_height = (avail.y - display_h - note_h - 5.0 * gap - 10.0) / 5.0;
        let by_width = (avail.x - 32.0 - 2.0 * gap) / 3.0;
        let d = by_height.min(by_width).clamp(44.0, 76.0);
        let grid_w = 3.0 * d + 2.0 * gap;
        let left = ((avail.x - grid_w) / 2.0).max(0.0);

        // --- number display: a real text field, so typing and pasting work too
        let len = self.number.chars().count();
        let size = match len {
            0..=11 => 32.0,
            12..=15 => 26.0,
            16..=21 => 21.0,
            _ => 17.0,
        };
        let edit = crate::a11y::named(
            ui.add_sized(
                [avail.x, display_h - 8.0],
                egui::TextEdit::singleline(&mut self.number)
                    .frame(false)
                    .hint_text(egui::RichText::new("Enter a number").color(pal.text3))
                    .font(FontId::new(size, FontFamily::Proportional))
                    .horizontal_align(egui::Align::Center)
                    .vertical_align(egui::Align::Center)
                    .desired_width(f32::INFINITY),
            ),
            "Phone number",
        );
        theme::ring_when_focused(ui, pal, &edit);
        if edit.changed() {
            self.number = input::sanitize_dial(&self.number);
        }
        if self.focus_display {
            edit.request_focus();
            self.focus_display = false;
        }
        ui.add_space(gap);

        // --- keypad
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        for row in KEYS {
            ui.horizontal(|ui| {
                ui.add_space(left);
                for (key, letters) in row {
                    let letters = if key == '0' { "+" } else { letters };
                    let response = key_button(ui, pal, d, key, letters);
                    if key == '0' {
                        self.handle_zero(ui, &response);
                    } else if response.clicked() {
                        input::push_dial_char(&mut self.number, key);
                        // Only a mouse click returns to the display; keyboard activation keeps focus
                        // on the key so Tab/Enter can keep typing digits.
                        self.focus_display = response.hovered();
                    }
                }
            });
        }

        // --- action row: copy | call | delete
        ui.horizontal(|ui| {
            ui.add_space(left);
            let has_number = !self.number.is_empty();
            ui.allocate_ui_with_layout(
                vec2(d, d),
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    if small_round(ui, pal, d * 0.8, Icon::Copy, has_number, "Copy number")
                        .clicked()
                    {
                        ui.ctx().copy_text(self.number.clone());
                    }
                },
            );
            let can_call = blocker.is_none() && has_number;
            if theme::round_button(ui, pal, d, pal.ok, Icon::Phone, can_call, "Call with phone")
                .clicked()
            {
                // Never dials by itself: this opens the confirmation.
                self.confirming = Some(self.number.clone());
            }
            ui.allocate_ui_with_layout(
                vec2(d, d),
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    if small_round(
                        ui,
                        pal,
                        d * 0.8,
                        Icon::Backspace,
                        has_number,
                        "Delete last digit",
                    )
                    .clicked()
                    {
                        self.number.pop();
                        self.focus_display = ui.rect_contains_pointer(ui.max_rect());
                    }
                },
            );
        });

        ui.add_space(2.0);
        if let Some(reason) = blocker {
            ui.vertical_centered(|ui| theme::blocked_note(ui, pal, &reason));
        }
        if !banner_shown {
            ui.vertical_centered(|ui| call_ui::audio_note(ui, pal, &self.sources));
        }
        if self.audio.found_card(ui, pal) {
            self.tab = Tab::Recents;
        }
    }

    /// "Call +49...?" sheet. Esc or Cancel drops it; only the Call button sends the request.
    fn confirm_dial(&mut self, ctx: &egui::Context, pal: &Palette, feed: &CallFeed) {
        let Some(number) = self.confirming.clone() else {
            return;
        };
        let state = feed.state();
        let dial = state.status.as_ref().and_then(|s| s.view.dial.clone());
        // The offer can vanish while the sheet is open (call started, phone left): then drop it.
        let Some(dial) = dial else {
            self.confirming = None;
            return;
        };
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.confirming = None;
            return;
        }
        let (question, consequence) = call_ui::dial_confirmation(&dial, &number);
        let mut decision = None;
        egui::Window::new(crate::i18n::tr("call.dial.confirm.title"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(
                egui::Frame::new()
                    .fill(pal.tile)
                    .stroke(egui::Stroke::new(1.0_f32, pal.separator))
                    .corner_radius(egui::CornerRadius::same(crate::nothing::R_TILE))
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(300.0);
                ui.vertical_centered(|ui| {
                    ui.add(egui::Label::new(theme::semibold(&question, 20.0)).wrap());
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(theme::secondary(pal, &consequence)).wrap());
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let w = (ui.available_width() - 12.0) / 2.0;
                        if theme::pill_button_sized(
                            ui,
                            pal,
                            crate::i18n::tr("call.dial.confirm.cancel"),
                            theme::ButtonKind::Plain,
                            true,
                            w,
                        )
                        .clicked()
                        {
                            decision = Some(false);
                        }
                        if theme::pill_button_sized(
                            ui,
                            pal,
                            crate::i18n::tr("call.dial.confirm.go"),
                            theme::ButtonKind::Inverse,
                            true,
                            w,
                        )
                        .clicked()
                        {
                            decision = Some(true);
                        }
                    });
                });
            });
        match decision {
            Some(true) => {
                if let Some(request) = state
                    .status
                    .as_ref()
                    .and_then(|s| call_ui::dial_request(&s.view, &number))
                {
                    feed.send(request);
                    self.number.clear();
                }
                self.confirming = None;
            }
            Some(false) => self.confirming = None,
            None => {}
        }
    }

    /// The 0 key types "0"; holding it types "+", like a phone dialer.
    fn handle_zero(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.is_pointer_button_down_on() {
            let held = ui.input(|i| {
                i.pointer
                    .press_start_time()
                    .map(|start| i.time - start)
                    .unwrap_or(0.0)
            });
            if held >= LONG_PRESS_SECONDS && !self.zero_long_press_done {
                self.zero_long_press_done = true;
                if self.number.is_empty() {
                    input::push_dial_char(&mut self.number, '+');
                }
                self.focus_display = true;
            } else if !self.zero_long_press_done {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        if response.clicked() {
            if !self.zero_long_press_done {
                input::push_dial_char(&mut self.number, '0');
                self.focus_display = response.hovered();
            }
            self.zero_long_press_done = false;
        } else if !response.is_pointer_button_down_on() {
            self.zero_long_press_done = false;
        }
    }

    fn recents(&mut self, ui: &mut egui::Ui, env: &Env) {
        let pal = env.pal;
        let sources = self.sources.clone();
        ui.add_space(6.0);
        ui.label(theme::semibold("Recent Calls", 20.0));
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 24.0, pal.control);
            icons::paint(
                ui.painter(),
                egui::Rect::from_center_size(rect.center(), vec2(22.0, 22.0)),
                Icon::Clock,
                pal.text3,
            );
            ui.add_space(6.0);
            ui.label(theme::semibold("No call history", 15.0));
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    "Nothing has been read from a phone, so there's nothing to list.",
                ))
                .wrap(),
            );
        });
        ui.add_space(24.0);
        let banner_shown = self.audio.banner(ui, pal);
        if !banner_shown {
            call_ui::audio_note(ui, pal, &sources);
        }
        ui.add_space(8.0);
        call_ui::sources(ui, pal, &sources, banner_shown);
        ui.add_space(18.0);
        ui.label(theme::overline(pal, crate::i18n::tr("callaudio.title")));
        ui.add_space(6.0);
        self.audio.show(ui, pal, true);
        ui.add_space(12.0);
    }
}

/// Screenshot aid: a scripted phone report instead of the service (`GEEKBRIDGE_CALLS_FIXTURE`).
pub(crate) fn fixture_feed(kind: &str, wake: egui::Context) -> CallFeed {
    let ringing = kind == "ringing";
    use bridge_core::{
        calls::{CallState, Sensitive},
        calls_link::{LinkCallReport, PhoneState},
        calls_view::{self, SourceSnapshot},
        ipc::CallsStatus,
    };
    let report = LinkCallReport {
        state: if kind == "ended" {
            PhoneState::Idle
        } else if ringing {
            PhoneState::Ringing
        } else {
            PhoneState::OffHook
        },
        number: Some(Sensitive::new("+1 555 0100")),
        name: Some(Sensitive::new("Ada Lovelace")),
        can_answer: true,
        can_end: true,
        can_dial: false,
    };
    let mut state = CallState::default();
    report.apply(&mut state);
    let view = calls_view::build(&[SourceSnapshot {
        key: "link:fixture".into(),
        label: "Pixel 9".into(),
        caps: report.capabilities(),
        state,
    }]);
    let status = CallsStatus {
        view,
        notice: None,
        sources: Vec::new(),
        audio_note: None,
        wifi: wifi_fixture(kind),
    };
    CallFeed::spawn_with(
        std::time::Duration::from_secs(3600),
        move || Ok(status.clone()),
        |_| Ok(()),
        move || wake.request_repaint(),
    )
}

/// The experimental Wi-Fi call audio part of a screenshot fixture: `active-wifi` offers the button,
/// `listening-wifi` shows the indicator with "Speak (experimental)", `speaking-wifi` the microphone warning.
fn wifi_fixture(kind: &str) -> Option<bridge_core::call_wifi_view::CallWifiView> {
    use bridge_core::call_wifi_view::{CallWifiPhone, CallWifiSession, CallWifiView};
    let session = |speaking| CallWifiSession {
        device_id: "fixture".into(),
        name: "Pixel 9".into(), // i18n-ok: screenshot fixture
        state: "live".into(),
        speaking,
        frames: 4200,
        lost: 3,
        depth_ms: 60,
        seconds: 83,
    };
    let session = match kind {
        "listening-wifi" => Some(session(false)),
        "speaking-wifi" => Some(session(true)),
        "active-wifi" => None,
        _ => return None,
    };
    Some(CallWifiView {
        supported: true,
        phones: vec![CallWifiPhone {
            device_id: "fixture".into(),
            name: "Pixel 9".into(), // i18n-ok: screenshot fixture
            connected: true,
            allowed: true,
            ready: true,
            listen: true,
            speak: true,
            test: "works".into(),
            in_call: true,
        }],
        session,
        ..CallWifiView::default()
    })
}

impl AppView for Calls {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
        if self.number.is_empty()
            && let Some(n) = crate::prefill::take_dial()
        {
            self.number = input::sanitize_dial(&n);
        }
        let feed = self.feed.take().unwrap_or_else(|| {
            // Screenshots: open on the Recents tab, where the call audio checklist is.
            if std::env::var("GEEKBRIDGE_CALLS_TAB").as_deref() == Ok("recents") {
                self.tab = Tab::Recents;
            }
            let wake = ctx.clone();
            match std::env::var("GEEKBRIDGE_CALLS_FIXTURE").as_deref() {
                Ok(
                    kind @ ("ringing" | "active" | "active-wifi" | "listening-wifi"
                    | "speaking-wifi"),
                ) => fixture_feed(kind, wake),
                _ => CallFeed::spawn(move || wake.request_repaint()),
            }
        });
        self.sources = feed.state();
        theme::toolbar(ctx, pal, 46.0, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                theme::segmented(
                    ui,
                    pal,
                    &mut self.tab,
                    &[(Tab::Keypad, "Keypad"), (Tab::Recents, "Recents")],
                );
            });
        });
        self.panel.top(ctx, pal, &feed);
        central(ctx, pal, Margin::symmetric(16, 10), |ui| match self.tab {
            Tab::Keypad => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .drag_to_scroll(false)
                    .id_salt("calls-keypad")
                    .show(ui, |ui| self.keypad(ui, env))
                    .bars(ui.ctx());
            }
            Tab::Recents => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .drag_to_scroll(false)
                    .id_salt("calls-recents")
                    .show(ui, |ui| self.recents(ui, env))
                    .bars(ui.ctx());
            }
        });
        self.panel.popup(ctx, pal, &feed);
        self.confirm_dial(ctx, pal, &feed);
        self.feed = Some(feed);
    }
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod render_tests;
