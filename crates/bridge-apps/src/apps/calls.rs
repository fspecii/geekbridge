//! Calls: a dialer in the style of the macOS phone keypad. The number is local text only;
//! placing a call stays blocked until a call backend is authorized for a paired phone.
use crate::{
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
        ui.painter().circle_stroke(
            rect.center(),
            d / 2.0 + 2.0,
            egui::Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.7)),
        );
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
            FontId::new((d * 0.14).clamp(8.0, 10.0), theme::semibold_family()),
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
        ui.painter().circle_stroke(
            rect.center(),
            d / 2.0 + 1.0,
            egui::Stroke::new(2.0_f32, pal.accent.gamma_multiply(0.7)),
        );
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
        let blocker = env.blocked_line("call");
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
        let edit = ui.add_sized(
            [avail.x, display_h - 8.0],
            egui::TextEdit::singleline(&mut self.number)
                .frame(false)
                .hint_text(egui::RichText::new("Enter a number").color(pal.text3))
                .font(FontId::new(size, FontFamily::Proportional))
                .horizontal_align(egui::Align::Center)
                .vertical_align(egui::Align::Center)
                .desired_width(f32::INFINITY),
        );
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
                        self.focus_display = true;
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
                // Reached only once a call backend authorizes this phone.
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
                        self.focus_display = true;
                    }
                },
            );
        });

        ui.add_space(2.0);
        if let Some(reason) = blocker {
            ui.vertical_centered(|ui| theme::blocked_note(ui, pal, &reason));
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
                self.focus_display = true;
            }
            self.zero_long_press_done = false;
        } else if !response.is_pointer_button_down_on() {
            self.zero_long_press_done = false;
        }
    }

    fn recents(&mut self, ui: &mut egui::Ui, env: &Env) {
        let pal = env.pal;
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
    }
}

impl AppView for Calls {
    fn show(&mut self, ctx: &egui::Context, env: &Env) {
        let pal = env.pal;
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
        central(ctx, pal, Margin::symmetric(16, 10), |ui| match self.tab {
            Tab::Keypad => self.keypad(ui, env),
            Tab::Recents => self.recents(ui, env),
        });
    }
}
