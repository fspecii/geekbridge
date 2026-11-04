// SPDX-License-Identifier: GPL-3.0-or-later
//! The incoming-call card: `geekbridge-calls --overlay`.
//!
//! A small borderless window (360 x 96) in the top-right corner of the screen, like a macOS
//! notification, with Answer and Reject. The service starts it when a call rings (so it is not
//! inside the dialer window) and it closes itself when the call is over or was answered elsewhere.
//!
//! Decisions go through exactly the path the Calls window uses (`call_ui::request_for` over the
//! service's merged view), so a button exists only when something can really answer, and the
//! outcome comes back from the service, not from this window's guess: "Answered on phone" once the
//! call stops ringing, "Couldn't answer: <reason>" when the service or phone refused.
//!
//! The state machine ([`Card`]) and the text ([`caller_lines`]) are plain data, tested without a
//! window. Placement is [`bridge_core::call_overlay::card_position`]; on Wayland the compositor
//! decides (docs/omarchy.md has the Hyprland rules).
use crate::{
    call_feed::{CallFeed, FeedState},
    call_ui::{Decision, request_for},
    cli::ThemeChoice,
    i18n::{tr, tr_args},
    nothing, theme,
};
use bridge_core::{
    AppKind,
    call_overlay::{self as geometry, Monitor, Reveal},
    calls_view::{CallRow, ControlPath},
    ipc::Request,
};
use eframe::egui::{self, Align, Color32, Layout, Margin, Sense, Stroke, ThemePreference, vec2};
use std::time::{Duration, Instant};

/// How long the card waits for the first answer of the service before giving up.
const STARTUP_GRACE: Duration = Duration::from_secs(8);
/// A ring that is no longer reported keeps the card up this long (a flicker is not a hang-up).
const GONE_GRACE: Duration = Duration::from_millis(1500);
/// "Answered on phone" and its kin stay visible this long, then the card closes.
const DONE_SHOW: Duration = Duration::from_millis(2500);
/// A refusal is shown this long; the buttons come back if the call still rings.
const FAIL_SHOW: Duration = Duration::from_secs(6);
/// An accepted decision that the phone never confirms stops looking pending after this.
/// How long `--test` shows its fake ring.
const TEST_SECONDS: Duration = Duration::from_secs(10);
const PENDING_MAX: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Answer,
    Reject,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    choice: Choice,
    path: ControlPath,
    since: Instant,
}

#[derive(Clone, Debug)]
struct Outcome {
    text: String,
    ok: bool,
    /// Closes the card when it has been shown long enough (a refusal does not).
    closes: bool,
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    Close,
}

/// What the card shows right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Ringing {
        title: String,
        /// Number (when allowed) or the line the call arrives on.
        subtitle: String,
        /// Status under the caller: "Answering…" or a refusal; replaces the subtitle.
        status: Option<(String, bool)>,
        can_answer: bool,
        can_reject: bool,
        /// Buttons drawn but off (the test card).
        disabled: bool,
    },
    /// The call is over or taken: one line, then the card closes.
    Done { text: String, ok: bool },
}

pub struct Card {
    key: Option<String>,
    dismissed: bool,
    pending: Option<Pending>,
    outcome: Option<Outcome>,
    seen_ringing: bool,
    idle_since: Instant,
    reveal: Reveal,
}

/// Title (name, else number, else "Unknown caller") and, when allowed and different, the number.
pub fn caller_lines(row: &CallRow, reveal: Reveal) -> (String, Option<String>) {
    let name = row.name.as_ref().map(|n| n.expose().to_owned());
    let number = row.number.as_ref().map(|n| n.expose().to_owned());
    match (name, number, reveal) {
        (Some(n), Some(num), Reveal::Full) if n != num => (n, Some(num)),
        (Some(n), _, _) => (n, None),
        (None, Some(num), Reveal::Full) => (num, None),
        _ => (tr("overlay.unknown").to_owned(), None),
    }
}

impl Card {
    pub fn new(reveal: Reveal, now: Instant) -> Self {
        Self {
            key: None,
            dismissed: false,
            pending: None,
            outcome: None,
            seen_ringing: false,
            idle_since: now,
            reveal,
        }
    }

    /// The card is hidden by the user (the call keeps ringing).
    pub fn hidden(&self) -> bool {
        self.dismissed
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// The user chose `choice`. Returns the service request, or `None` if nothing offers it.
    pub fn choose(&mut self, state: &FeedState, choice: Choice, now: Instant) -> Option<Request> {
        if self.pending.is_some() {
            return None;
        }
        let view = &state.status.as_ref()?.view;
        let (decision, target) = match choice {
            Choice::Answer => (Decision::Answer, view.ringing()?.answer.clone()?),
            Choice::Reject => (Decision::Decline, view.ringing()?.reject.clone()?),
        };
        let request = request_for(view, decision)?;
        self.pending = Some(Pending {
            choice,
            path: target.path,
            since: now,
        });
        self.outcome = None;
        Some(request)
    }

    /// Advance with the latest service state.
    pub fn update(&mut self, state: &FeedState, now: Instant) -> Verdict {
        let view = state.status.as_ref().map(|s| &s.view);
        match view.and_then(|v| v.ringing()) {
            Some(row) => {
                self.seen_ringing = true;
                self.idle_since = now;
                if self.key.as_deref() != Some(row.key.as_str()) {
                    // Another call rings: a fresh card.
                    self.key = Some(row.key.clone());
                    self.dismissed = false;
                    self.pending = None;
                    self.outcome = None;
                }
                if let Some(p) = self.pending {
                    if let Some(problem) = &state.problem {
                        let key = match p.choice {
                            Choice::Answer => "overlay.failed.answer",
                            Choice::Reject => "overlay.failed.reject",
                        };
                        self.outcome = Some(Outcome {
                            text: tr_args(key, &[("reason", &problem.as_str())]),
                            ok: false,
                            closes: false,
                            at: now,
                        });
                        self.pending = None;
                    } else if now.duration_since(p.since) > PENDING_MAX {
                        self.pending = None;
                    }
                }
                if self
                    .outcome
                    .as_ref()
                    .is_some_and(|o| !o.closes && now.duration_since(o.at) > FAIL_SHOW)
                {
                    self.outcome = None;
                }
                Verdict::Keep
            }
            None => {
                if let Some(p) = self.pending.take() {
                    let live = view.is_some_and(|v| v.live().is_some());
                    let (text, ok) = match (p.choice, live) {
                        (Choice::Answer, true) => (
                            match p.path {
                                ControlPath::Link => tr("overlay.answered.link"),
                                ControlPath::Hfp => tr("overlay.answered.hfp"),
                            }
                            .to_owned(),
                            true,
                        ),
                        (Choice::Reject, _) => (tr("overlay.rejected").to_owned(), true),
                        (Choice::Answer, false) => (tr("overlay.ended").to_owned(), true),
                    };
                    self.outcome = Some(Outcome {
                        text,
                        ok,
                        closes: true,
                        at: now,
                    });
                }
                // A refusal that outlived its ring is stale.
                if self.outcome.as_ref().is_some_and(|o| !o.closes) {
                    self.outcome = None;
                }
                if let Some(o) = &self.outcome {
                    return if now.duration_since(o.at) >= DONE_SHOW {
                        Verdict::Close
                    } else {
                        Verdict::Keep
                    };
                }
                let waited = now.duration_since(self.idle_since);
                let limit = if self.seen_ringing || view.is_some() {
                    GONE_GRACE
                } else {
                    STARTUP_GRACE
                };
                // The first answer can arrive after a refused connection: wait out the start-up
                // grace before giving up, but close fast once a ring has ended.
                if waited >= limit {
                    Verdict::Close
                } else {
                    Verdict::Keep
                }
            }
        }
    }

    pub fn body(&self, state: &FeedState) -> Option<Body> {
        if let Some(o) = self.outcome.as_ref().filter(|o| o.closes) {
            return Some(Body::Done {
                text: o.text.clone(),
                ok: o.ok,
            });
        }
        let row = state.status.as_ref()?.view.ringing()?;
        let (title, number) = caller_lines(row, self.reveal);
        let subtitle =
            number.unwrap_or_else(|| tr_args("overlay.on_phone", &[("phone", &row.phone)]));
        let status = match (&self.pending, &self.outcome) {
            (Some(p), _) => Some((
                match p.choice {
                    Choice::Answer => tr("overlay.answering"),
                    Choice::Reject => tr("overlay.rejecting"),
                }
                .to_owned(),
                true,
            )),
            (None, Some(o)) => Some((o.text.clone(), o.ok)),
            _ => row
                .missing_control
                .as_ref()
                .filter(|_| row.answer.is_none() && row.reject.is_none())
                .map(|_| (tr("overlay.no_control").to_owned(), false)),
        };
        let idle = self.pending.is_none();
        Some(Body::Ringing {
            title,
            subtitle,
            status,
            can_answer: idle && row.answer.is_some(),
            can_reject: idle && row.reject.is_some(),
            disabled: false,
        })
    }
}

// ---------------------------------------------------------------- drawing

/// What the user did on the card this frame.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Action {
    pub choice: Option<Choice>,
    pub dismiss: bool,
}

const GREEN: Color32 = Color32::from_rgb(0x1B, 0x7F, 0x46); // white text on it is 5.1:1
const BUTTON_W: f32 = 104.0;
const BUTTON_H: f32 = 30.0;

fn card_button(
    disabled: bool,
    ui: &mut egui::Ui,
    pal: &theme::Palette,
    text: &str,
    name: &str,
    fill: Color32,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::new(13.5, theme::semibold_family()),
        Color32::WHITE,
    );
    let sense = if disabled {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(BUTTON_W, BUTTON_H), sense);
    let fill = if disabled {
        pal.control
    } else if response.is_pointer_button_down_on() {
        fill.gamma_multiply(0.8)
    } else if response.hovered() {
        fill.gamma_multiply(0.92)
    } else {
        fill
    };
    ui.painter().rect_filled(rect, 15, fill);
    if response.has_focus() {
        theme::focus_ring(ui, pal, rect, 15);
    }
    let fg = if disabled { pal.text3 } else { Color32::WHITE };
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    let name = name.to_owned();
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone()));
    response
}

fn close_button(ui: &mut egui::Ui, pal: &theme::Palette) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .circle_filled(rect.center(), 10.0, pal.control_hover);
    }
    if response.has_focus() {
        theme::focus_circle(ui, pal, rect.center(), 10.0);
    }
    let c = rect.center();
    let stroke = Stroke::new(
        1.6_f32,
        if response.hovered() {
            pal.text
        } else {
            pal.text2
        },
    );
    ui.painter()
        .line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], stroke);
    ui.painter()
        .line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], stroke);
    let name = tr("overlay.dismiss");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    response
}

/// Draw the card filling the window. Returns what the user asked for.
pub fn show(ctx: &egui::Context, pal: &theme::Palette, body: &Body) -> Action {
    let mut action = Action::default();
    let frame = egui::Frame::new()
        .fill(pal.tile)
        .stroke(Stroke::new(1.0_f32, pal.separator))
        .inner_margin(Margin::symmetric(14, 10));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let card_rect = ui.max_rect().expand2(vec2(14.0, 10.0));
        match body {
            Body::Ringing {
                title,
                subtitle,
                status,
                can_answer,
                can_reject,
                disabled,
            } => {
                ui.horizontal(|ui| {
                    let text_w = ui.available_width() - BUTTON_W - 12.0;
                    ui.allocate_ui_with_layout(
                        vec2(text_w, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.horizontal(|ui| {
                                nothing::dot_text(
                                    ui,
                                    pal,
                                    tr("overlay.title"),
                                    nothing::PITCH_LABEL,
                                    pal.accent,
                                    false,
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if close_button(ui, pal).clicked() {
                                        action.dismiss = true;
                                    }
                                });
                            });
                            ui.add(egui::Label::new(theme::semibold(title, 16.0)).truncate());
                            match status {
                                Some((text, ok)) => {
                                    let color = if *ok { pal.text2 } else { pal.err };
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(text).size(12.5).color(color),
                                        )
                                        .truncate(),
                                    );
                                }
                                None => {
                                    ui.add(
                                        egui::Label::new(theme::caption(pal, subtitle)).truncate(),
                                    );
                                }
                            }
                        },
                    );
                    ui.add_space(12.0 - ui.spacing().item_spacing.x.min(12.0));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.add_space(3.0);
                        if *can_answer
                            && card_button(
                                *disabled,
                                ui,
                                pal,
                                tr("overlay.answer"),
                                &tr_args("overlay.answer.name", &[("caller", title)]),
                                GREEN,
                            )
                            .clicked()
                        {
                            action.choice = Some(Choice::Answer);
                        }
                        if *can_reject
                            && card_button(
                                *disabled,
                                ui,
                                pal,
                                tr("overlay.reject"),
                                &tr_args("overlay.reject.name", &[("caller", title)]),
                                pal.accent_fill,
                            )
                            .clicked()
                        {
                            action.choice = Some(Choice::Reject);
                        }
                    });
                });
                announce(
                    ui,
                    card_rect,
                    &tr_args("overlay.announce", &[("caller", title)]),
                );
            }
            Body::Done { text, ok } => {
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        let color = if *ok { pal.text } else { pal.err };
                        ui.label(egui::RichText::new(text).size(16.0).color(color));
                    },
                );
                announce(ui, card_rect, text);
            }
        }
    });
    action
}

/// A named, non-interactive node covering the card, marked as a live region so a screen reader
/// reads a new ring (assertive) and each outcome without the window taking focus.
fn announce(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    let id = egui::Id::new("call-overlay-card");
    let node = ui.interact(rect, id, Sense::hover());
    let text = text.to_owned();
    node.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.clone()));
    crate::a11y::live_region(ui.ctx(), node.id, crate::a11y::Live::Assertive);
    #[cfg(feature = "accesskit")]
    crate::a11y::set_role(ui.ctx(), node.id, eframe::egui::accesskit::Role::Alert);
}

// ---------------------------------------------------------------- the window

struct OverlayApp {
    card: Card,
    feed: Option<CallFeed>,
    fixture: bool,
    /// `--test`: a fake ring, no service, closes by itself.
    test: Option<Instant>,
    placed: bool,
    hidden_sent: bool,
    /// The native window style has been applied (once, on the first frame).
    styled: bool,
}

impl eframe::App for OverlayApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::memtrim::cap_atlas(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.styled {
            self.styled = true;
            native::never_activate(frame);
        }
        if let Some(started) = self.test {
            self.place(ctx);
            let pal = theme::current(ctx, AppKind::Calls);
            let a = show(
                ctx,
                &pal,
                &Body::Ringing {
                    title: tr("overlay.test.title").to_owned(),
                    subtitle: String::new(),
                    status: Some((tr("overlay.test.note").to_owned(), true)),
                    can_answer: true,
                    can_reject: true,
                    disabled: true,
                },
            );
            if a.dismiss || started.elapsed() > TEST_SECONDS {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        }
        let feed = self.feed.get_or_insert_with(|| {
            let wake = ctx.clone();
            if std::env::var("GEEKBRIDGE_CALLS_FIXTURE").as_deref() == Ok("ringing") {
                self.fixture = true;
                crate::apps::calls::fixture_feed(true, wake)
            } else {
                CallFeed::spawn(move || wake.request_repaint())
            }
        });
        crate::a11y::window_title(ctx, tr("overlay.window_title"));
        let now = Instant::now();
        let state = feed.state();
        if self.card.update(&state, now) == Verdict::Close && !self.fixture {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.place(ctx);
        // Keys count only while the card has the keyboard (clicking it, or the user moving to it):
        // Enter answers, Esc hides the card. Esc never rejects, so a stray key cannot hang up.
        let focused = ctx.input(|i| i.focused);
        let (enter, esc) = ctx.input(|i| {
            (
                focused && i.key_pressed(egui::Key::Enter),
                focused && i.key_pressed(egui::Key::Escape),
            )
        });
        let feed = self.feed.as_ref().expect("set above");
        let mut action = Action::default();
        if let Some(body) = self.card.body(&state) {
            let pal = theme::current(ctx, AppKind::Calls);
            action = show(ctx, &pal, &body);
        }
        if enter {
            action.choice = action.choice.or(Some(Choice::Answer));
        }
        action.dismiss |= esc;
        if let Some(choice) = action.choice
            && let Some(request) = self.card.choose(&state, choice, now)
        {
            feed.send(request);
        }
        if action.dismiss {
            self.card.dismiss();
        }
        if self.card.hidden() != self.hidden_sent {
            self.hidden_sent = self.card.hidden();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(!self.hidden_sent));
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl OverlayApp {
    /// Top-right of the monitor, once the window system tells its size. A no-op on Wayland, where
    /// a client cannot place itself and the compositor's window rules do it.
    fn place(&mut self, ctx: &egui::Context) {
        if self.placed {
            return;
        }
        let Some(size) = ctx.input(|i| i.viewport().monitor_size) else {
            return;
        };
        if size.x < 1.0 || size.y < 1.0 {
            return;
        }
        let (x, y) = geometry::card_position_for(
            std::env::consts::OS,
            Monitor {
                x: 0.0,
                y: 0.0,
                width: size.x,
                height: size.y,
            },
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        self.placed = true;
    }
}

/// The window options of the card: borderless, always on top, not in the task bar, not activated
/// when it appears (typing elsewhere is not interrupted), and typed as a notification for X11
/// window managers.
pub fn viewport() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(tr("overlay.window_title"))
        .with_app_id(geometry::APP_ID)
        .with_inner_size([geometry::CARD_WIDTH, geometry::CARD_HEIGHT])
        .with_resizable(false)
        .with_decorations(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_active(false)
        .with_close_button(false)
        .with_minimize_button(false)
        .with_maximize_button(false)
        .with_window_type(egui::X11WindowType::Notification)
}

/// Operating-system details of a card that must not take the keyboard from what the user is
/// typing. Windows gets the "no activate" extended style; macOS runs the card's process as an
/// accessory application. Linux relies on the window type and the compositor's rules.
#[cfg_attr(not(windows), allow(dead_code))]
mod native {
    /// `WS_EX_NOACTIVATE`: a click on the card does not make it the foreground window.
    pub const WS_EX_NOACTIVATE: isize = 0x0800_0000;
    /// `WS_EX_TOOLWINDOW`: not in the task bar or Alt+Tab.
    pub const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
    /// `WS_EX_APPWINDOW`: forces a task bar button; must be off for a card.
    pub const WS_EX_APPWINDOW: isize = 0x0004_0000;

    /// The extended window style of the card, from the one the window has now.
    pub fn card_ex_style(current: isize) -> isize {
        (current | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW
    }

    #[cfg(windows)]
    pub fn never_activate(frame: &eframe::Frame) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW,
        };
        let Ok(handle) = frame.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(h) = handle.as_raw() else {
            return;
        };
        let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        // SAFETY: `hwnd` is this process's own live top-level window, valid while `frame` is
        // borrowed; reading and writing its extended style has no other preconditions.
        unsafe {
            let now = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, card_ex_style(now));
        }
    }

    #[cfg(not(windows))]
    pub fn never_activate(_frame: &eframe::Frame) {}

    #[cfg(target_os = "macos")]
    pub fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        Some(Box::new(|builder| {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
        None
    }
}

pub fn run(theme_choice: ThemeChoice, reveal: Reveal, test: bool) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: viewport(),
        event_loop_builder: native::event_loop_hook(),
        ..Default::default()
    };
    eframe::run_native(
        tr("overlay.window_title"),
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, AppKind::Calls);
            cc.egui_ctx.set_theme(match theme_choice {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Light => ThemePreference::Light,
                ThemeChoice::Dark => ThemePreference::Dark,
            });
            Ok(Box::new(OverlayApp {
                card: Card::new(reveal, Instant::now()),
                feed: None,
                fixture: false,
                test: test.then(Instant::now),
                placed: false,
                hidden_sent: false,
                styled: false,
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Rig;
    use bridge_core::{
        calls::{CallState, Sensitive},
        calls_link::{LinkCallReport, PhoneState},
        calls_view::{self, SourceSnapshot},
        ipc::CallsStatus,
    };

    fn state_of(
        phone: PhoneState,
        can: bool,
        name: Option<&str>,
        problem: Option<&str>,
    ) -> FeedState {
        let r = LinkCallReport {
            state: phone,
            number: Some(Sensitive::new("+15550100")),
            name: name.map(Sensitive::new),
            can_answer: can,
            can_end: can,
            can_dial: false,
        };
        let mut s = CallState::default();
        r.apply(&mut s);
        let view = calls_view::build(&[SourceSnapshot {
            key: "link:d".into(),
            label: "Pixel".into(),
            caps: r.capabilities(),
            state: s,
        }]);
        FeedState {
            status: Some(CallsStatus {
                view,
                notice: None,
                sources: vec![],
                audio_note: None,
            }),
            problem: problem.map(str::to_owned),
        }
    }

    fn ringing() -> FeedState {
        state_of(PhoneState::Ringing, true, Some("Ada"), None)
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn lines_follow_the_privacy_mode() {
        let st = ringing();
        let row = st.status.as_ref().unwrap().view.ringing().unwrap().clone();
        assert_eq!(
            caller_lines(&row, Reveal::Full),
            ("Ada".into(), Some("+15550100".into()))
        );
        assert_eq!(caller_lines(&row, Reveal::NameOnly), ("Ada".into(), None));
        let anon = state_of(PhoneState::Ringing, true, None, None);
        let row = anon
            .status
            .as_ref()
            .unwrap()
            .view
            .ringing()
            .unwrap()
            .clone();
        assert_eq!(caller_lines(&row, Reveal::Full), ("+15550100".into(), None));
        assert_eq!(
            caller_lines(&row, Reveal::NameOnly),
            ("Unknown caller".into(), None),
            "name-only never shows the number, not even as the title"
        );
    }

    #[test]
    fn a_ringing_call_shows_buttons_only_for_what_is_offered() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        assert_eq!(c.update(&st, t), Verdict::Keep);
        let Some(Body::Ringing {
            title,
            subtitle,
            can_answer,
            can_reject,
            status,
            ..
        }) = c.body(&st)
        else {
            panic!("no body");
        };
        assert_eq!((title.as_str(), subtitle.as_str()), ("Ada", "+15550100"));
        assert!(can_answer && can_reject && status.is_none());
        // The phone cannot be controlled: no buttons, and the card says why.
        let no = state_of(PhoneState::Ringing, false, Some("Ada"), None);
        let mut c = Card::new(Reveal::Full, t);
        c.update(&no, t);
        let Some(Body::Ringing {
            can_answer,
            can_reject,
            ..
        }) = c.body(&no)
        else {
            panic!()
        };
        assert!(!can_answer && !can_reject);
        assert!(c.choose(&no, Choice::Answer, t).is_none());
    }

    #[test]
    fn answer_uses_the_call_action_path_and_confirms_when_the_ring_ends() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        let req = c.choose(&st, Choice::Answer, at(t, 100)).expect("offered");
        assert_eq!(
            req,
            request_for(&st.status.as_ref().unwrap().view, Decision::Answer).unwrap()
        );
        assert!(
            c.choose(&st, Choice::Reject, at(t, 120)).is_none(),
            "one decision at a time"
        );
        // While waiting the card says so and offers no second click.
        c.update(&st, at(t, 200));
        let Some(Body::Ringing {
            status,
            can_answer,
            can_reject,
            ..
        }) = c.body(&st)
        else {
            panic!()
        };
        assert_eq!(status.unwrap().0, "Answering…");
        assert!(!can_answer && !can_reject);
        // The phone picked up: no longer ringing, a live call.
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        assert_eq!(c.update(&live, at(t, 1200)), Verdict::Keep);
        assert_eq!(
            c.body(&live),
            Some(Body::Done {
                text: "Answered on phone".into(),
                ok: true
            })
        );
        assert_eq!(c.update(&live, at(t, 1200 + 2600)), Verdict::Close);
    }

    #[test]
    fn a_refused_answer_shows_the_reason_and_the_buttons_come_back() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        c.choose(&st, Choice::Answer, t).unwrap();
        let refused = state_of(
            PhoneState::Ringing,
            true,
            Some("Ada"),
            Some("The phone is busy."),
        );
        assert_eq!(c.update(&refused, at(t, 500)), Verdict::Keep);
        let Some(Body::Ringing {
            status, can_answer, ..
        }) = c.body(&refused)
        else {
            panic!()
        };
        assert_eq!(
            status,
            Some(("Couldn't answer: The phone is busy.".into(), false))
        );
        assert!(can_answer, "the user can try again");
        // The refusal fades, the plain card is back.
        c.update(&st, at(t, 500 + 6500));
        let Some(Body::Ringing { status, .. }) = c.body(&st) else {
            panic!()
        };
        assert!(status.is_none());
    }

    #[test]
    fn reject_confirms_and_a_missed_call_closes_the_card() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        assert!(c.choose(&st, Choice::Reject, t).is_some());
        let idle = FeedState {
            status: Some(CallsStatus {
                view: calls_view::CallsView::empty(),
                notice: None,
                sources: vec![],
                audio_note: None,
            }),
            problem: None,
        };
        c.update(&idle, at(t, 300));
        assert_eq!(
            c.body(&idle),
            Some(Body::Done {
                text: "Call rejected".into(),
                ok: true
            })
        );
        // Another card: the caller hangs up, answered elsewhere, nothing was pressed.
        let mut c = Card::new(Reveal::Full, t);
        c.update(&st, t);
        assert_eq!(
            c.update(&idle, at(t, 1000)),
            Verdict::Keep,
            "a flicker is not the end"
        );
        assert_eq!(c.update(&idle, at(t, 1700)), Verdict::Close);
        assert_eq!(c.body(&idle), None);
    }

    #[test]
    fn dismiss_hides_the_card_but_the_next_call_shows_it_again() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        c.dismiss();
        assert!(c.hidden());
        assert_eq!(
            c.update(&st, at(t, 5000)),
            Verdict::Keep,
            "the call keeps ringing"
        );
        // A different call (new key) brings the card back.
        let mut other = ringing();
        if let Some(s) = other.status.as_mut() {
            s.view.rows[0].key = "other".into();
        }
        c.update(&other, at(t, 6000));
        assert!(!c.hidden());
    }

    #[test]
    fn without_a_service_the_card_gives_up_after_the_start_up_grace() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let down = FeedState {
            status: None,
            problem: Some("down".into()),
        };
        assert_eq!(c.update(&down, at(t, 3000)), Verdict::Keep);
        assert_eq!(c.update(&down, at(t, 8100)), Verdict::Close);
    }

    #[test]
    fn the_card_draws_inside_its_window_with_named_buttons() {
        let mut rig = Rig::new(AppKind::Calls, geometry::CARD_WIDTH, geometry::CARD_HEIGHT);
        let st = ringing();
        let mut c = Card::new(Reveal::Full, Instant::now());
        c.update(&st, Instant::now());
        let body = c.body(&st).unwrap();
        let pal = theme::current(&rig.ctx, AppKind::Calls);
        let mut drawn = 0;
        for _ in 0..3 {
            let input = rig.input(vec![]);
            let _ = rig.ctx.run(input, |ctx| {
                let a = show(ctx, &pal, &body);
                assert_eq!(a, Action::default());
                drawn += 1;
            });
        }
        assert_eq!(drawn, 3);
    }

    #[test]
    fn the_windows_style_is_no_activate_tool_window_and_keeps_the_rest() {
        let plain = 0x0000_0100; // WS_EX_WINDOWEDGE
        let s = native::card_ex_style(plain | native::WS_EX_APPWINDOW);
        assert_ne!(s & native::WS_EX_NOACTIVATE, 0);
        assert_ne!(s & native::WS_EX_TOOLWINDOW, 0);
        assert_eq!(s & native::WS_EX_APPWINDOW, 0);
        assert_ne!(s & plain, 0);
        assert_eq!(native::card_ex_style(s), s, "idempotent");
    }

    #[test]
    fn the_window_is_borderless_topmost_unfocused_and_card_sized() {
        let v = viewport();
        assert_eq!(v.app_id.as_deref(), Some("dev.geekbridge.CallOverlay"));
        assert_eq!(v.inner_size, Some(egui::vec2(360.0, 96.0)));
        assert_eq!(v.decorations, Some(false));
        assert_eq!(v.window_level, Some(egui::WindowLevel::AlwaysOnTop));
        assert_eq!(v.taskbar, Some(false));
        assert_eq!(v.active, Some(false));
        assert_eq!(v.window_type, Some(egui::X11WindowType::Notification));
        assert_eq!(v.resizable, Some(false));
    }
}
