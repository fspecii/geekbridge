// SPDX-License-Identifier: GPL-3.0-or-later
//! The incoming-call popup and the ongoing-call panel of the Calls window.
//!
//! Everything shown here comes from the service's merged view (`bridge_core::calls_view`): a button
//! exists only if the service offered a target for it, so **Answer is never shown unless something
//! can really answer**. The decision logic ([`request_for`], [`route_text`], [`clock`]) is plain data
//! and tested without a window.
use crate::{
    call_feed::{CallFeed, FeedState},
    nothing,
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    calls::CallPhase,
    calls_view::{AudioRoute, AudioRouteView, CallRow, CallsView, ControlPath},
    ipc::{CallAction, Request},
};
use eframe::egui::{self, Align2, Color32, Key, Margin, RichText, vec2};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// How long a first `H` waits for its confirmation before the shortcut disarms.
const HANGUP_CONFIRM: Duration = Duration::from_secs(5);

/// A keyboard or button decision, before it is matched against what the service offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Answer,
    Decline,
    HangUp,
    ToggleMute,
    UseComputerAudio,
}

/// Turn a decision into the service request, or `None` if nothing offers it right now.
pub fn request_for(view: &CallsView, d: Decision) -> Option<Request> {
    let ringing = view.ringing();
    let live = view.live();
    match d {
        Decision::Answer => Some(Request::CallAction {
            target: ringing?.answer.clone()?,
            action: CallAction::Answer,
        }),
        Decision::Decline => Some(Request::CallAction {
            target: ringing?.reject.clone()?,
            action: CallAction::Reject,
        }),
        Decision::HangUp => Some(Request::CallAction {
            target: live?.hangup.clone()?,
            action: CallAction::Hangup,
        }),
        Decision::ToggleMute => {
            let m = view.mute.as_ref()?;
            Some(Request::CallMute {
                source: m.source.clone(),
                muted: !m.muted.unwrap_or(false),
            })
        }
        Decision::UseComputerAudio => Some(Request::CallAudio {
            source: view.audio.connect.clone()?,
        }),
    }
}

/// One line for the audio route, with the honest reason when it is on the phone.
pub fn route_text(a: &AudioRouteView) -> String {
    match a.route {
        AudioRoute::NoCall => String::new(),
        AudioRoute::Computer { codec } => format!("Audio on this computer ({codec:?})"),
        AudioRoute::Connecting => "Moving the audio to this computer…".to_owned(),
        AudioRoute::Phone => match &a.hint {
            Some(h) => format!("Audio on the phone. {h}"),
            None => "Audio on the phone".to_owned(),
        },
    }
}

pub fn clock(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// "Answered on phone" means the answer went through the phone link, so the voice stays there.
pub fn via_label(path: ControlPath) -> &'static str {
    match path {
        ControlPath::Hfp => "Bluetooth",
        ControlPath::Link => "phone link",
    }
}

/// Per-window state: when each live call was first seen (for the timer) and which ring has already
/// pulled the window forward.
#[derive(Default)]
pub struct CallPanel {
    started: BTreeMap<String, Instant>,
    raised_for: Option<String>,
    /// The `H` shortcut was pressed once; a second `H` or Enter hangs up, Esc cancels.
    hangup_armed: Option<Instant>,
    /// Focus ring of the popup, in Tab order: the sheet itself, then Decline and Answer when
    /// offered. While a call rings, Tab and Shift+Tab cycle inside it (a modal sheet traps focus).
    popup_ring: Vec<egui::Id>,
    /// A new ring: move keyboard focus onto the popup sheet.
    focus_sheet: bool,
}

fn sheet_id() -> egui::Id {
    egui::Id::new("incoming-sheet")
}

impl CallPanel {
    fn seconds(&mut self, key: &str) -> u64 {
        self.started
            .entry(key.to_owned())
            .or_insert_with(Instant::now)
            .elapsed()
            .as_secs()
    }

    fn forget_gone(&mut self, view: &CallsView) {
        self.started
            .retain(|k, _| view.rows.iter().any(|r| &r.key == k));
        if self
            .raised_for
            .as_ref()
            .is_some_and(|k| !view.rows.iter().any(|r| &r.key == k && r.ringing()))
        {
            self.raised_for = None;
        }
    }

    /// Reads shortcuts first (so a focused text field cannot swallow Enter), then draws the ongoing
    /// panel, and returns the popup to draw after the central area. Call once per frame, after the
    /// toolbar and before the central panel.
    pub fn top(&mut self, ctx: &egui::Context, pal: &Palette, feed: &CallFeed) {
        let state = feed.state();
        let Some(status) = &state.status else {
            return;
        };
        let view = &status.view;
        self.forget_gone(view);

        if let Some(row) = view.ringing()
            && self.raised_for.as_deref() != Some(row.key.as_str())
        {
            self.raised_for = Some(row.key.clone());
            self.focus_sheet = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        if view.live().is_none()
            || self
                .hangup_armed
                .is_some_and(|t| t.elapsed() >= HANGUP_CONFIRM)
        {
            self.hangup_armed = None;
        }
        // Enter belongs to a focused Decline/Answer button; on the sheet itself it answers.
        let button_focused = ctx
            .memory(|m| m.focused())
            .is_some_and(|f| self.popup_ring.iter().skip(1).any(|id| *id == f));
        let keys = pressed(ctx, view, self.hangup_armed.is_some(), button_focused);
        for d in keys.decisions {
            if d == Decision::HangUp {
                self.hangup_armed = None;
            }
            if let Some(request) = request_for(view, d) {
                feed.send(request);
            }
        }
        if keys.cancel_hangup {
            self.hangup_armed = None;
        }
        if keys.arm_hangup && view.live().is_some_and(|r| r.hangup.is_some()) {
            self.hangup_armed = Some(Instant::now());
            ctx.request_repaint_after(HANGUP_CONFIRM);
        }

        if let Some(row) = view.live() {
            let secs = self.seconds(&row.key);
            egui::TopBottomPanel::top("ongoing-call")
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(pal.content)
                        .inner_margin(Margin::symmetric(12, 8)),
                )
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.tile)
                        .stroke(egui::Stroke::new(1.0_f32, pal.separator))
                        .corner_radius(egui::CornerRadius::same(nothing::R_CARD))
                        .inner_margin(Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ongoing(
                                ui,
                                pal,
                                feed,
                                &state,
                                row,
                                secs,
                                self.hangup_armed.is_some(),
                            )
                        });
                });
        }
    }

    /// The incoming-call popup, centred over the window. Draw last.
    pub fn popup(&mut self, ctx: &egui::Context, pal: &Palette, feed: &CallFeed) {
        let state = feed.state();
        let Some(status) = &state.status else {
            return;
        };
        let Some(row) = status.view.ringing() else {
            return;
        };
        // A sheet, not a system dialog: rounded, dim backdrop, decisions as pills.
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("incoming-backdrop"))
            .order(egui::Order::Middle)
            // Paint only: an interactive zero-size area would be an unnamed Tab stop.
            .interactable(false)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    Color32::from_black_alpha(if pal.dark { 150 } else { 70 }),
                );
            });
        egui::Window::new("Incoming call")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(
                egui::Frame::new()
                    .fill(pal.tile)
                    .stroke(egui::Stroke::new(1.0_f32, pal.separator))
                    .corner_radius(egui::CornerRadius::same(nothing::R_TILE))
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                // Never wider than the window minus a gutter, so the sheet keeps its rounded edges.
                let width = (screen.width() - 2.0 * nothing::S5 - 48.0).clamp(220.0, 340.0);
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    nothing::dot_text(ui, pal, "INCOMING", nothing::PITCH_TITLE, pal.accent, false);
                    ui.add_space(8.0);
                    ui.label(theme::semibold(row.caller_label(), 24.0));
                    if let (Some(name), Some(number)) = (&row.name, &row.number)
                        && name != number
                    {
                        ui.label(theme::secondary(pal, number.expose()));
                    }
                    ui.label(theme::caption(pal, &row.phone));
                    ui.add_space(16.0);
                    let mut ring = vec![sheet_id()];
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let width = (ui.available_width() - 12.0) / 2.0;
                        if row.reject.is_some() {
                            let r = theme::pill_button_sized(
                                ui,
                                pal,
                                "Decline  Esc",
                                ButtonKind::Primary,
                                true,
                                width,
                            );
                            ring.push(r.id);
                            if r.clicked()
                                && let Some(r) = request_for(&status.view, Decision::Decline)
                            {
                                feed.send(r);
                            }
                        }
                        if row.answer.is_some() {
                            let r = theme::pill_button_sized(
                                ui,
                                pal,
                                "Answer  Enter",
                                ButtonKind::Inverse,
                                true,
                                width,
                            );
                            ring.push(r.id);
                            if r.clicked()
                                && let Some(r) = request_for(&status.view, Decision::Answer)
                            {
                                feed.send(r);
                            }
                        }
                    });
                    self.popup_ring = ring;
                    if let Some(answer) = &row.answer {
                        ui.add_space(8.0);
                        let note = match answer.path {
                            ControlPath::Hfp => {
                                "Answers over Bluetooth; the voice comes to this computer.".to_owned()
                            }
                            ControlPath::Link => "Answers on the phone. The voice stays on the phone unless this computer is also its Bluetooth headset.".to_owned(),
                        };
                        ui.add(egui::Label::new(theme::caption(pal, note)).wrap());
                    }
                    if let Some(why) = &row.missing_control {
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(theme::secondary(pal, why)).wrap());
                    }
                    notice(ui, pal, &state);
                });
                // The sheet itself is focusable so a keyboard user lands on it, not on the window
                // behind; it has no click action, so Enter there still answers.
                let rect = ui.min_rect().expand(24.0);
                let sheet = ui.interact(rect, sheet_id(), egui::Sense::focusable_noninteractive());
                sheet.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        format!("Incoming call from {}", row.caller_label()),
                    )
                });
                crate::a11y::live_region(ctx, sheet.id, crate::a11y::Live::Assertive);
                #[cfg(feature = "accesskit")]
                crate::a11y::set_role(ctx, sheet.id, eframe::egui::accesskit::Role::AlertDialog);
                if sheet.has_focus() {
                    ui.painter()
                        .with_clip_rect(screen)
                        .rect_stroke(
                            rect.expand(1.0),
                            egui::CornerRadius::same(nothing::R_TILE),
                            egui::Stroke::new(2.0_f32, theme::focus_color(pal)),
                            egui::StrokeKind::Outside,
                        );
                }
            });
        self.trap_focus(ctx);
    }

    /// Focus lands on the popup when a call starts ringing, and Tab / Shift+Tab stay inside it.
    fn trap_focus(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.focus_sheet) {
            ctx.memory_mut(|m| m.request_focus(sheet_id()));
        }
        let Some(at) = ctx
            .memory(|m| m.focused())
            .and_then(|f| self.popup_ring.iter().position(|id| *id == f))
        else {
            return;
        };
        let focused = self.popup_ring[at];
        // Hand Tab to us instead of letting it walk out to the window behind.
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                focused,
                egui::EventFilter {
                    tab: true,
                    escape: false,
                    ..Default::default()
                },
            )
        });
        let (tab, back) = ctx.input(|i| (i.key_pressed(Key::Tab), i.modifiers.shift));
        if tab {
            let n = self.popup_ring.len();
            let next = if back { (at + n - 1) % n } else { (at + 1) % n };
            ctx.memory_mut(|m| m.request_focus(self.popup_ring[next]));
        }
    }
}

fn notice(ui: &mut egui::Ui, pal: &Palette, state: &FeedState) {
    let text = state
        .problem
        .as_deref()
        .or(state.status.as_ref().and_then(|s| s.notice.as_deref()));
    if let Some(t) = text {
        ui.add_space(8.0);
        ui.add(egui::Label::new(RichText::new(t).size(12.5).color(pal.err)).wrap());
    }
}

fn ongoing(
    ui: &mut egui::Ui,
    pal: &Palette,
    feed: &CallFeed,
    state: &FeedState,
    row: &CallRow,
    secs: u64,
    armed: bool,
) {
    let view = &state.status.as_ref().expect("checked by caller").view;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(theme::semibold(row.caller_label(), 16.0));
            match row.phase {
                CallPhase::Held => ui.label(theme::secondary(pal, "On hold")),
                CallPhase::Dialing | CallPhase::Alerting => {
                    ui.label(theme::secondary(pal, "Calling…"))
                }
                _ => {
                    nothing::dot_text(
                        ui,
                        pal,
                        &clock(secs),
                        nothing::PITCH_LABEL,
                        pal.accent,
                        false,
                    );
                    ui.label(theme::caption(pal, "In call"))
                }
            };
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if row.hangup.is_some() {
                let label = if armed {
                    "Hang up now  H"
                } else {
                    "Hang up  H"
                };
                if theme::pill_button(ui, pal, label, ButtonKind::Primary, true).clicked()
                    && let Some(r) = request_for(view, Decision::HangUp)
                {
                    feed.send(r);
                }
                if armed {
                    ui.label(theme::caption(
                        pal,
                        "Press H or Enter to end the call, Esc to keep it",
                    ));
                }
            }
            if let Some(m) = &view.mute {
                let label = if m.muted == Some(true) {
                    "Unmute  M"
                } else {
                    "Mute  M"
                };
                if theme::pill_button(ui, pal, label, ButtonKind::Plain, true).clicked()
                    && let Some(r) = request_for(view, Decision::ToggleMute)
                {
                    feed.send(r);
                }
            }
        });
    });
    let route = route_text(&view.audio);
    if !route.is_empty() {
        ui.add(egui::Label::new(theme::caption(pal, route)).wrap());
    }
    if view.audio.connect.is_some()
        && ui.small_button("Use computer audio").clicked()
        && let Some(r) = request_for(view, Decision::UseComputerAudio)
    {
        feed.send(r);
    }
    if let Some(why) = &row.missing_control {
        ui.add(egui::Label::new(theme::secondary(pal, why)).wrap());
    }
    notice(ui, pal, state);
}

/// What the keyboard asked for this frame.
#[derive(Debug, Default, PartialEq, Eq)]
struct Keys {
    decisions: Vec<Decision>,
    /// First `H` during a call: ask for confirmation instead of hanging up.
    arm_hangup: bool,
    cancel_hangup: bool,
}

/// Keyboard decisions pressed this frame that the view could act on. Enter and Escape always count
/// while a call rings, unless one of the popup's own buttons has focus (then Enter activates that
/// button, so Tab to Decline + Enter declines); letters only when no text field has the keyboard, so
/// typing a number is safe. Hanging up is destructive, so `H` only arms; a second `H` or Enter
/// confirms and Esc backs out.
fn pressed(ctx: &egui::Context, view: &CallsView, armed: bool, popup_focused: bool) -> Keys {
    let typing = ctx.wants_keyboard_input();
    ctx.input_mut(|i| {
        let mut hit = |key: Key| i.consume_key(egui::Modifiers::NONE, key);
        keys_for(view, armed, popup_focused, typing, &mut hit)
    })
}

fn keys_for(
    view: &CallsView,
    armed: bool,
    popup_focused: bool,
    typing: bool,
    hit: &mut dyn FnMut(Key) -> bool,
) -> Keys {
    let mut out = Keys::default();
    if view.ringing().is_some() {
        if (!popup_focused && hit(Key::Enter)) || (!typing && hit(Key::A)) {
            out.decisions.push(Decision::Answer);
        }
        if hit(Key::Escape) || (!typing && hit(Key::D)) {
            out.decisions.push(Decision::Decline);
        }
    }
    if view.live().is_some() {
        if armed {
            // Enter in the dialer's text field is the field's, not a hang-up.
            if !typing && (hit(Key::Enter) || hit(Key::H)) {
                out.decisions.push(Decision::HangUp);
            } else if hit(Key::Escape) {
                out.cancel_hangup = true;
            }
        } else if !typing && hit(Key::H) {
            out.arm_hangup = true;
        }
        if !typing && hit(Key::M) {
            out.decisions.push(Decision::ToggleMute);
        }
    }
    out
}

/// Why calls can or cannot be answered from this computer, one line per source, for the Recents tab.
pub fn sources(ui: &mut egui::Ui, pal: &Palette, state: &FeedState) {
    ui.label(theme::overline(pal, "ANSWERING FROM THIS COMPUTER"));
    match &state.status {
        None => {
            let why = state
                .problem
                .as_deref()
                .unwrap_or("Waiting for the GeekBridge service…");
            ui.add(egui::Label::new(theme::secondary(pal, why)).wrap());
        }
        Some(status) => {
            for line in &status.sources {
                ui.add(egui::Label::new(theme::secondary(pal, line)).wrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::calls::{
        AudioCodec, CallCapabilities, CallDirection, CallEvent, CallId, CallParty, CallState,
        LinkState, Sensitive,
    };
    use bridge_core::calls_link::{LinkCallReport, PhoneState};
    use bridge_core::calls_view::{self, SourceSnapshot};

    fn link_view(state: PhoneState, can: bool) -> CallsView {
        let r = LinkCallReport {
            state,
            number: Some(Sensitive::new("+15550100")),
            name: Some(Sensitive::new("Ada")),
            can_answer: can,
            can_end: can,
        };
        let mut s = CallState::default();
        r.apply(&mut s);
        calls_view::build(&[SourceSnapshot {
            key: "link:d".into(),
            label: "Pixel".into(),
            caps: r.capabilities(),
            state: s,
        }])
    }

    #[test]
    fn buttons_exist_only_when_the_service_offered_them() {
        let ringing = link_view(PhoneState::Ringing, true);
        assert!(request_for(&ringing, Decision::Answer).is_some());
        assert!(request_for(&ringing, Decision::Decline).is_some());
        assert!(request_for(&ringing, Decision::HangUp).is_none());
        assert!(request_for(&ringing, Decision::ToggleMute).is_none());

        let refused = link_view(PhoneState::Ringing, false);
        assert!(request_for(&refused, Decision::Answer).is_none());
        assert!(request_for(&refused, Decision::Decline).is_none());

        let live = link_view(PhoneState::OffHook, true);
        assert!(request_for(&live, Decision::Answer).is_none());
        assert!(request_for(&live, Decision::HangUp).is_some());
        assert!(request_for(&live, Decision::UseComputerAudio).is_none());

        assert!(request_for(&CallsView::empty(), Decision::Answer).is_none());
    }

    /// Run `keys_for` with exactly the listed keys pressed.
    fn keys(
        view: &CallsView,
        armed: bool,
        popup_focused: bool,
        typing: bool,
        down: &[Key],
    ) -> Keys {
        keys_for(view, armed, popup_focused, typing, &mut |k| {
            down.contains(&k)
        })
    }

    #[test]
    fn ringing_enter_answers_escape_declines_without_a_mouse() {
        let ring = link_view(PhoneState::Ringing, true);
        assert_eq!(
            keys(&ring, false, false, false, &[Key::Enter]).decisions,
            [Decision::Answer]
        );
        assert_eq!(
            keys(&ring, false, false, false, &[Key::Escape]).decisions,
            [Decision::Decline]
        );
        // Enter and Escape still work while a text field has focus; letters do not.
        assert_eq!(
            keys(&ring, false, false, true, &[Key::Enter]).decisions,
            [Decision::Answer]
        );
        assert!(
            keys(&ring, false, false, true, &[Key::A, Key::D])
                .decisions
                .is_empty()
        );
    }

    #[test]
    fn enter_belongs_to_a_focused_popup_button() {
        let ring = link_view(PhoneState::Ringing, true);
        // Tab to "Decline", press Enter: the button handles it, the shortcut must not answer.
        assert!(
            keys(&ring, false, true, false, &[Key::Enter])
                .decisions
                .is_empty()
        );
        // Escape always declines.
        assert_eq!(
            keys(&ring, false, true, false, &[Key::Escape]).decisions,
            [Decision::Decline]
        );
    }

    #[test]
    fn hang_up_needs_a_second_deliberate_key() {
        let live = link_view(PhoneState::OffHook, true);
        let first = keys(&live, false, false, false, &[Key::H]);
        assert!(first.arm_hangup && first.decisions.is_empty());
        // Enter alone, before arming, does nothing during a call.
        assert_eq!(
            keys(&live, false, false, false, &[Key::Enter]),
            Keys::default()
        );
        assert_eq!(
            keys(&live, true, false, false, &[Key::H]).decisions,
            [Decision::HangUp]
        );
        assert_eq!(
            keys(&live, true, false, false, &[Key::Enter]).decisions,
            [Decision::HangUp]
        );
        let back = keys(&live, true, false, false, &[Key::Escape]);
        assert!(back.cancel_hangup && back.decisions.is_empty());
        // Typing a number never hangs up, armed or not.
        assert_eq!(keys(&live, false, false, true, &[Key::H]), Keys::default());
        assert!(
            keys(&live, true, false, true, &[Key::Enter])
                .decisions
                .is_empty()
        );
    }

    #[test]
    fn mute_toggles_from_the_reported_state() {
        use bridge_core::calls::{HfpEvidence, HfpHost, MuteScope};
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::Windows,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                host_call_control_api: true,
                audio_codec: Some(AudioCodec::Msbc),
                ..Default::default()
            },
        );
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        state.apply(CallEvent::CallUpdated {
            id: CallId::new("c"),
            direction: CallDirection::Incoming,
            phase: CallPhase::Active,
            party: CallParty::default(),
        });
        state.apply(CallEvent::Audio {
            state: bridge_core::calls::AudioLinkState::Connected {
                codec: AudioCodec::Msbc,
            },
        });
        let mk = |state: &CallState| {
            calls_view::build(&[SourceSnapshot {
                key: "hfp:w".into(),
                label: "Phone".into(),
                caps: caps.clone(),
                state: state.clone(),
            }])
        };
        let Some(Request::CallMute { muted, .. }) = request_for(&mk(&state), Decision::ToggleMute)
        else {
            panic!("mute should be offered");
        };
        assert!(muted);
        state.apply(CallEvent::MuteChanged {
            scope: MuteScope::HostMicrophone,
            muted: true,
        });
        let Some(Request::CallMute { muted, .. }) = request_for(&mk(&state), Decision::ToggleMute)
        else {
            panic!("unmute should be offered");
        };
        assert!(!muted);
    }

    #[test]
    fn route_text_is_honest_about_where_the_voice_is() {
        let live = link_view(PhoneState::OffHook, true);
        let t = route_text(&live.audio);
        assert!(t.starts_with("Audio on the phone"));
        assert!(t.contains("Answered on the phone"));
        assert_eq!(route_text(&CallsView::empty().audio), "");
        assert!(
            route_text(&AudioRouteView {
                route: AudioRoute::Computer {
                    codec: AudioCodec::Msbc
                },
                hint: None,
                connect: None
            })
            .contains("this computer")
        );
    }

    #[test]
    fn timer_and_labels() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(187), "03:07");
        assert_eq!(via_label(ControlPath::Link), "phone link");
    }

    /// The ringing sheet is announced: an alert dialog with the caller's name, assertive live
    /// region, and Decline / Answer are named buttons.
    #[cfg(feature = "accesskit")]
    #[test]
    fn incoming_call_sheet_is_an_announced_alert() {
        use eframe::egui::accesskit::{Live, Role};
        let status = bridge_core::ipc::CallsStatus {
            view: link_view(PhoneState::Ringing, true),
            notice: None,
            sources: Vec::new(),
        };
        let feed = crate::call_feed::CallFeed::spawn_with(
            Duration::from_millis(20),
            move || Ok(status.clone()),
            |_| Ok(()),
            || {},
        );
        for _ in 0..200 {
            if feed.state().status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut panel = CallPanel::default();
        let tree = crate::a11y_tests::capture(egui::vec2(700.0, 500.0), |ctx| {
            crate::theme::apply(ctx, bridge_core::AppKind::Calls);
            let pal = crate::theme::current(ctx, bridge_core::AppKind::Calls);
            panel.popup(ctx, &pal, &feed);
        });
        let sheet = tree
            .find("Incoming call from Ada")
            .unwrap_or_else(|| panic!("{}", tree.outline()));
        assert_eq!(sheet.role(), Role::AlertDialog);
        assert_eq!(sheet.live(), Some(Live::Assertive));
        for button in ["Answer", "Decline"] {
            assert!(
                tree.has_name_starting(button),
                "{button}\n{}",
                tree.outline()
            );
        }
        assert_eq!(tree.unnamed_controls(), Vec::<String>::new());
    }
}
