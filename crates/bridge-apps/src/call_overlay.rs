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
    call_ui::{Decision, clock, request_for},
    call_wifi_ui::WifiCard,
    cli::ThemeChoice,
    i18n::{tr, tr_args},
    nothing, theme,
};
use bridge_core::{
    AppKind,
    call_overlay::{self as geometry, Monitor, Reveal},
    call_wifi_view::CallWifiAction,
    calls::CallPhase,
    calls_view::{AudioRoute, CallRow, ControlPath},
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
/// "Call ended 02:13" stays this long (the service keeps the process alive a little longer).
const ENDED_SHOW: Duration = Duration::from_secs(4);
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
    /// How long a closing outcome is shown.
    show: Duration,
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
    /// The call is going on (answered here or on the phone): the same card, now an in-call card.
    Live {
        title: String,
        /// "IN CALL 02:13", "ON HOLD 02:13" or "CALLING".
        header: String,
        /// Where the voice is: "Audio is on the phone".
        route: String,
        /// The audio is on the phone and nothing can move it here: say what to set up.
        setup_hint: bool,
        can_hangup: bool,
        /// Mute is offered only while this computer carries the voice; `Some(muted)`.
        mute: Option<bool>,
        /// "Use computer audio" is offered.
        computer_audio: bool,
        /// The service offers keypad tones for this call: the card gets a Keypad button that opens the Calls window
        /// on the keypad (the card is a small window that never takes the keyboard).
        keypad: bool,
        wifi: Option<WifiCard>,
        /// A refused hang-up or mute.
        problem: Option<String>,
    },
    /// The call is over or taken: one line, then the card closes.
    Done { text: String, ok: bool },
}

pub struct Card {
    key: Option<String>,
    parts: Vec<String>,
    dismissed: bool,
    pending: Option<Pending>,
    outcome: Option<Outcome>,
    seen_ringing: bool,
    idle_since: Instant,
    reveal: Reveal,
    /// When the card first saw this call going on (the timer's zero).
    live_since: Option<Instant>,
    live_secs: u64,
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
            parts: Vec::new(),
            dismissed: false,
            pending: None,
            outcome: None,
            seen_ringing: false,
            idle_since: now,
            reveal,
            live_since: None,
            live_secs: 0,
        }
    }

    /// Screenshot aid: the card as it looks right after a call of 02:13 ended.
    pub fn ended_fixture(reveal: Reveal, now: Instant) -> Self {
        let mut c = Self::new(reveal, now);
        c.outcome = Some(Outcome {
            text: tr_args("overlay.ended.after", &[("time", &clock(133))]),
            ok: true,
            closes: true,
            at: now,
            show: ENDED_SHOW,
        });
        c
    }

    /// The request for a button on the in-call card (hang up, mute, computer audio), if the
    /// service offers it.
    pub fn live_request(&self, state: &FeedState, d: Decision) -> Option<Request> {
        request_for(&state.status.as_ref()?.view, d)
    }

    /// The card is hidden by the user (the call keeps ringing).
    /// A call is ringing on the card (Enter answers only then).
    pub fn is_ringing(&self, state: &FeedState) -> bool {
        state
            .status
            .as_ref()
            .is_some_and(|s| s.view.ringing().is_some())
    }

    fn reveal(&self) -> Reveal {
        self.reveal
    }

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
                // The row's key changes when a second source (Bluetooth after the link) starts
                // seeing the same call; its parts say whether it is still the call on the card.
                let same = self.key.as_deref().is_some_and(|k| row.is(k))
                    || self.parts.iter().any(|k| row.is(k));
                // An outcome that closes the card means its ring was over: whatever rings now (the
                // next ring on the same phone has the same key) is a new call.
                let over = self.outcome.as_ref().is_some_and(|o| o.closes);
                if !same || over {
                    // Another call rings: a fresh card.
                    self.dismissed = false;
                    self.pending = None;
                    self.outcome = None;
                }
                self.key = Some(row.key.clone());
                self.parts = row.parts.clone();
                if let Some(p) = self.pending {
                    if let Some(problem) = &state.problem {
                        let key = match p.choice {
                            Choice::Answer => "overlay.failed.answer",
                            Choice::Reject => "overlay.failed.decline",
                        };
                        self.outcome = Some(Outcome {
                            text: tr_args(key, &[("reason", &problem.as_str())]),
                            ok: false,
                            closes: false,
                            at: now,
                            show: DONE_SHOW,
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
            None if view.is_some_and(|v| v.live().is_some()) => {
                let row = view.and_then(|v| v.live()).expect("checked");
                let same = self.key.as_deref().is_some_and(|k| row.is(k))
                    || self.parts.iter().any(|k| row.is(k));
                // A closing outcome ("Call ended") belongs to the call before this one.
                let over = self.outcome.as_ref().is_some_and(|o| o.closes);
                if !same || over {
                    self.dismissed = false;
                    self.live_since = None;
                }
                self.outcome = None;
                self.pending = None;
                self.seen_ringing = true;
                self.idle_since = now;
                self.key = Some(row.key.clone());
                self.parts = row.parts.clone();
                let since = *self.live_since.get_or_insert(now);
                self.live_secs = crate::call_ui::fixed_call_seconds()
                    .unwrap_or_else(|| now.saturating_duration_since(since).as_secs());
                Verdict::Keep
            }
            None => {
                if let Some(since) = self.live_since.take() {
                    // The call was going on and is over: say how long it lasted.
                    self.pending = None;
                    self.outcome = Some(Outcome {
                        text: tr_args(
                            "overlay.ended.after",
                            &[(
                                "time",
                                &clock(now.saturating_duration_since(since).as_secs()),
                            )],
                        ),
                        ok: true,
                        closes: true,
                        at: now,
                        show: ENDED_SHOW,
                    });
                }
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
                        (Choice::Reject, _) => (tr("overlay.declined").to_owned(), true),
                        (Choice::Answer, false) => (tr("overlay.ended").to_owned(), true),
                    };
                    self.outcome = Some(Outcome {
                        text,
                        ok,
                        closes: true,
                        at: now,
                        show: DONE_SHOW,
                    });
                }
                // A refusal that outlived its ring is stale.
                if self.outcome.as_ref().is_some_and(|o| !o.closes) {
                    self.outcome = None;
                }
                if let Some(o) = &self.outcome {
                    return if now.duration_since(o.at) >= o.show {
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
        let status_ref = state.status.as_ref()?;
        let Some(row) = status_ref.view.ringing() else {
            return self.live_body(state);
        };
        let (title, number) = caller_lines(row, self.reveal);
        let subtitle =
            number.unwrap_or_else(|| tr_args("overlay.on_phone", &[("phone", &row.phone)]));
        let status = match (&self.pending, &self.outcome) {
            (Some(p), _) => Some((
                match p.choice {
                    Choice::Answer => tr("overlay.answering"),
                    Choice::Reject => tr("overlay.declining"),
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

impl Card {
    fn live_body(&self, state: &FeedState) -> Option<Body> {
        let status = state.status.as_ref()?;
        let view = &status.view;
        let row = view.live()?;
        let (title, _) = caller_lines(row, self.reveal);
        let time = clock(self.live_secs);
        let header = match row.phase {
            CallPhase::Held => format!("{} {time}", tr("overlay.live.hold")),
            CallPhase::Dialing | CallPhase::Alerting => tr("overlay.live.calling").to_owned(),
            _ => format!("{} {time}", tr("overlay.live.title")),
        };
        let route = match view.audio.route {
            AudioRoute::Computer { .. } => tr("overlay.audio.computer"),
            AudioRoute::Connecting => tr("overlay.audio.moving"),
            AudioRoute::Phone | AudioRoute::NoCall => tr("overlay.audio.phone"),
        }
        .to_owned();
        let computer_audio = view.audio.connect.is_some();
        Some(Body::Live {
            title,
            header,
            route,
            setup_hint: view.audio.route == AudioRoute::Phone && !computer_audio,
            can_hangup: row.hangup.is_some(),
            mute: view.mute.as_ref().map(|m| m.muted == Some(true)),
            computer_audio,
            keypad: view.tones.is_some(),
            wifi: crate::call_wifi_ui::card(status, Some(row)),
            problem: state.problem.clone(),
        })
    }
}

// ---------------------------------------------------------------- drawing

/// What the user did on the card this frame.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Action {
    pub choice: Option<Choice>,
    pub dismiss: bool,
    pub live: Option<LiveChoice>,
}

/// A button of the in-call card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveChoice {
    HangUp,
    ToggleMute,
    ComputerAudio,
    /// Open the Calls window on the keypad.
    Keypad,
    Wifi(CallWifiAction),
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
            Body::Live {
                title,
                header,
                route,
                setup_hint,
                can_hangup,
                mute,
                computer_audio,
                keypad,
                wifi,
                problem,
            } => {
                ui.horizontal(|ui| {
                    let text_w = ui.available_width() - BUTTON_W - 12.0;
                    ui.allocate_ui_with_layout(
                        vec2(text_w, ui.available_height()),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.horizontal(|ui| {
                                nothing::dot_text(
                                    ui,
                                    pal,
                                    header,
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
                            ui.add(egui::Label::new(theme::semibold(title, 15.0)).truncate());
                            ui.add(egui::Label::new(theme::caption(pal, route)).truncate());
                            let small = |t: &str| {
                                egui::Button::new(egui::RichText::new(t).size(12.0)).small()
                            };
                            match (problem, wifi) {
                                (Some(p), _) => {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(p).size(12.0).color(pal.err),
                                        )
                                        .truncate(),
                                    );
                                }
                                (
                                    None,
                                    Some(WifiCard::Running {
                                        speaking,
                                        can_speak,
                                        ..
                                    }),
                                ) => {
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(tr(if *speaking {
                                                    "overlay.wifi.speaking"
                                                } else {
                                                    "overlay.wifi.listening"
                                                }))
                                                .size(12.0)
                                                .color(pal.err),
                                            )
                                            .truncate(),
                                        );
                                        // Speak only where the phone reported a route and the session is live.
                                        if (*can_speak || *speaking)
                                            && ui
                                                .add(small(tr(if *speaking {
                                                    "overlay.wifi.speak.off"
                                                } else {
                                                    "overlay.wifi.speak"
                                                })))
                                                .clicked()
                                        {
                                            action.live =
                                                Some(LiveChoice::Wifi(CallWifiAction::Speak {
                                                    on: !*speaking,
                                                }));
                                        }
                                        if ui.add(small(tr("overlay.wifi.stop"))).clicked() {
                                            action.live =
                                                Some(LiveChoice::Wifi(CallWifiAction::Stop));
                                        }
                                    });
                                }
                                (None, wifi) => {
                                    ui.horizontal(|ui| {
                                        if *computer_audio
                                            && ui.add(small(tr("overlay.audio.use"))).clicked()
                                        {
                                            action.live = Some(LiveChoice::ComputerAudio);
                                        }
                                        if *keypad {
                                            let r = ui.add(small(tr("overlay.keypad")));
                                            let name = tr_args(
                                                "overlay.keypad.name",
                                                &[("caller", title)],
                                            );
                                            r.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::WidgetType::Button,
                                                    true,
                                                    name.clone(),
                                                )
                                            });
                                            if r.clicked() {
                                                action.live = Some(LiveChoice::Keypad);
                                            }
                                        }
                                        if let Some(WifiCard::Offer { device_id, phone }) = wifi {
                                            let r = ui.add(small(tr("overlay.wifi.listen")));
                                            let name = tr_args(
                                                "callwifi.listen.name",
                                                &[("phone", phone)],
                                            );
                                            r.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::WidgetType::Button,
                                                    true,
                                                    name.clone(),
                                                )
                                            });
                                            if r.clicked() {
                                                action.live = Some(LiveChoice::Wifi(
                                                    CallWifiAction::Listen {
                                                        device_id: device_id.clone(),
                                                    },
                                                ));
                                            }
                                        }
                                        if *setup_hint && wifi.is_none() {
                                            ui.add(
                                                egui::Label::new(theme::caption(
                                                    pal,
                                                    tr("overlay.audio.setup"),
                                                ))
                                                .truncate(),
                                            );
                                        }
                                    });
                                }
                            }
                        },
                    );
                    ui.add_space(12.0 - ui.spacing().item_spacing.x.min(12.0));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.add_space(3.0);
                        if *can_hangup
                            && card_button(
                                false,
                                ui,
                                pal,
                                tr("overlay.hangup"),
                                &tr_args("overlay.hangup.name", &[("caller", title)]),
                                pal.accent_fill,
                            )
                            .clicked()
                        {
                            action.live = Some(LiveChoice::HangUp);
                        }
                        if let Some(muted) = mute {
                            let (label, name) = if *muted {
                                (tr("overlay.unmute"), tr("overlay.unmute.name"))
                            } else {
                                (tr("overlay.mute"), tr("overlay.mute.name"))
                            };
                            if card_button(false, ui, pal, label, name, pal.control_hover).clicked()
                            {
                                action.live = Some(LiveChoice::ToggleMute);
                            }
                        }
                    });
                });
                announce(ui, card_rect, &format!("{header}, {title}"));
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
            let kind = std::env::var("GEEKBRIDGE_CALLS_FIXTURE").unwrap_or_default();
            if matches!(
                kind.as_str(),
                "ringing" | "active" | "keypad" | "active-wifi" | "listening-wifi" | "ended"
            ) {
                self.fixture = true;
                if kind == "ended" {
                    self.card = Card::ended_fixture(self.card.reveal(), Instant::now());
                }
                crate::apps::calls::fixture_feed(&kind, wake)
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
        if enter && self.card.is_ringing(&state) {
            action.choice = action.choice.or(Some(Choice::Answer));
        }
        if let Some(live) = action.live.take() {
            let request = match live {
                LiveChoice::Keypad => {
                    // The card never takes the keyboard, so the keys live in the Calls window.
                    let _ = crate::prefill::launch_with(
                        "geekbridge-calls",
                        crate::prefill::KEYPAD_VAR,
                        "1",
                    );
                    None
                }
                LiveChoice::HangUp => self.card.live_request(&state, Decision::HangUp),
                LiveChoice::ToggleMute => self.card.live_request(&state, Decision::ToggleMute),
                LiveChoice::ComputerAudio => {
                    self.card.live_request(&state, Decision::UseComputerAudio)
                }
                LiveChoice::Wifi(a) => Some(Request::CallWifi { action: a }),
            };
            if let Some(r) = request {
                feed.send(r);
            }
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
            can_tones: false,
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
                wifi: None,
                replies: Default::default(),
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
        // The phone picked up: no longer ringing, a live call. The card stays and turns into the
        // in-call card (it used to say "Answered on phone" and close).
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        assert_eq!(c.update(&live, at(t, 1200)), Verdict::Keep);
        let Some(Body::Live {
            title,
            header,
            can_hangup,
            ..
        }) = c.body(&live)
        else {
            panic!("an in-call card, got {:?}", c.body(&live))
        };
        assert_eq!(title, "Ada");
        assert_eq!(header, "IN CALL 00:00");
        assert!(can_hangup);
        // Minutes later it is still there, with a running timer.
        assert_eq!(c.update(&live, at(t, 1200 + 133_000)), Verdict::Keep);
        let Some(Body::Live { header, .. }) = c.body(&live) else {
            panic!()
        };
        assert_eq!(header, "IN CALL 02:13");
    }

    fn idle() -> FeedState {
        FeedState {
            status: Some(CallsStatus {
                view: calls_view::CallsView::empty(),
                notice: None,
                sources: vec![],
                audio_note: None,
                wifi: None,
                replies: Default::default(),
            }),
            problem: None,
        }
    }

    #[test]
    fn the_ended_call_lingers_with_its_length_then_closes() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        c.choose(&st, Choice::Answer, at(t, 100)).unwrap();
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        c.update(&live, at(t, 500));
        c.update(&live, at(t, 500 + 133_000));
        let idle = idle();
        let end = at(t, 500 + 133_400);
        assert_eq!(c.update(&idle, end), Verdict::Keep);
        assert_eq!(
            c.body(&idle),
            Some(Body::Done {
                text: "Call ended 02:13".into(),
                ok: true
            })
        );
        assert_eq!(c.update(&idle, end + Duration::from_secs(3)), Verdict::Keep);
        assert_eq!(
            c.update(&idle, end + Duration::from_secs(5)),
            Verdict::Close
        );
    }

    #[test]
    fn a_call_taken_on_the_phone_without_a_ring_is_an_in_call_card_too() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        assert_eq!(c.update(&live, t), Verdict::Keep);
        assert!(matches!(c.body(&live), Some(Body::Live { .. })));
        // And a new ring after "Call ended" gets a fresh card with buttons.
        let idle = idle();
        c.update(&idle, at(t, 5000));
        let st = ringing();
        c.update(&st, at(t, 6000));
        assert!(matches!(
            c.body(&st),
            Some(Body::Ringing {
                can_answer: true,
                ..
            })
        ));
    }

    #[test]
    fn in_call_buttons_follow_what_the_service_offers() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        c.update(&live, t);
        let Some(Body::Live {
            route,
            setup_hint,
            mute,
            computer_audio,
            wifi,
            ..
        }) = c.body(&live)
        else {
            panic!()
        };
        // Answered through the link only: the voice is on the phone, no Bluetooth to move it,
        // so no mute and no "use computer audio", and the card says what to set up.
        assert_eq!(route, "Audio is on the phone");
        assert!(setup_hint && mute.is_none() && !computer_audio && wifi.is_none());
        assert!(matches!(
            c.live_request(&live, Decision::HangUp),
            Some(Request::CallAction {
                action: bridge_core::ipc::CallAction::Hangup,
                ..
            })
        ));
        assert!(c.live_request(&live, Decision::ToggleMute).is_none());
        // Without a hang-up path there is no button.
        let no_end = state_of(PhoneState::OffHook, false, Some("Ada"), None);
        let Some(Body::Live { can_hangup, .. }) = c.body(&no_end) else {
            panic!()
        };
        assert!(!can_hangup);
    }

    #[test]
    fn the_keypad_button_exists_only_when_the_service_offers_tones() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let mut live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        c.update(&live, t);
        let keypad = |c: &Card, st: &FeedState| match c.body(st) {
            Some(Body::Live { keypad, .. }) => keypad,
            other => panic!("{other:?}"),
        };
        assert!(!keypad(&c, &live));
        // The same call, the phone able to play tones.
        let view = &mut live.status.as_mut().unwrap().view;
        view.tones = Some(calls_view::TonesView {
            source: "link:d".into(),
            path: calls_view::ControlPath::Link,
        });
        assert!(keypad(&c, &live));
        // It draws inside the card with its named button.
        let mut rig = Rig::new(AppKind::Calls, geometry::CARD_WIDTH, geometry::CARD_HEIGHT);
        let body = c.body(&live).unwrap();
        let pal = theme::current(&rig.ctx, AppKind::Calls);
        for _ in 0..3 {
            let input = rig.input(vec![]);
            let _ = rig.ctx.run(input, |ctx| {
                assert_eq!(show(ctx, &pal, &body), Action::default());
            });
        }
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
                wifi: None,
                replies: Default::default(),
            }),
            problem: None,
        };
        c.update(&idle, at(t, 300));
        assert_eq!(
            c.body(&idle),
            Some(Body::Done {
                text: "Call declined".into(),
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

    /// The same ring once Bluetooth also sees it: the row's primary source and key change.
    fn ringing_with_hfp_joined() -> FeedState {
        let mut st = ringing();
        let view = &mut st.status.as_mut().unwrap().view;
        let row = &mut view.rows[0];
        let link_key = row.key.clone();
        row.key = "hfp:/org/bluez/hci0/dev_X|call1".into();
        row.parts = vec![row.key.clone(), link_key];
        st
    }

    #[test]
    fn a_pending_answer_survives_bluetooth_joining_the_same_ring() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        c.choose(&st, Choice::Answer, at(t, 100)).unwrap();
        let joined = ringing_with_hfp_joined();
        c.update(&joined, at(t, 300));
        let Some(Body::Ringing { status, .. }) = c.body(&joined) else {
            panic!()
        };
        assert_eq!(
            status.unwrap().0,
            "Answering…",
            "same call, not a fresh card"
        );
        assert!(c.choose(&joined, Choice::Answer, at(t, 400)).is_none());
    }

    #[test]
    fn the_next_ring_with_the_same_key_after_a_decline_gets_its_buttons() {
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let st = ringing();
        c.update(&st, t);
        c.choose(&st, Choice::Reject, t).unwrap();
        let idle = FeedState {
            status: Some(CallsStatus {
                view: calls_view::CallsView::empty(),
                notice: None,
                sources: vec![],
                audio_note: None,
                wifi: None,
                replies: Default::default(),
            }),
            problem: None,
        };
        c.update(&idle, at(t, 300));
        assert!(matches!(c.body(&idle), Some(Body::Done { .. })));
        // The caller rings again at once: the link calls it `link-current` again.
        assert_eq!(c.update(&st, at(t, 800)), Verdict::Keep);
        let Some(Body::Ringing {
            can_answer,
            can_reject,
            status,
            ..
        }) = c.body(&st)
        else {
            panic!("the new ring is shown, not the old outcome")
        };
        assert!(can_answer && can_reject && status.is_none());
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
            s.view.rows[0].parts = vec!["other".into()];
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
    fn the_in_call_card_draws_inside_its_window() {
        let mut rig = Rig::new(AppKind::Calls, geometry::CARD_WIDTH, geometry::CARD_HEIGHT);
        let t = Instant::now();
        let mut c = Card::new(Reveal::Full, t);
        let live = state_of(PhoneState::OffHook, true, Some("Ada"), None);
        c.update(&live, t);
        let body = c.body(&live).unwrap();
        let pal = theme::current(&rig.ctx, AppKind::Calls);
        for _ in 0..3 {
            let input = rig.input(vec![]);
            let _ = rig.ctx.run(input, |ctx| {
                assert_eq!(show(ctx, &pal, &body), Action::default());
            });
        }
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
