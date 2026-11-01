//! The incoming-call popup and the ongoing-call panel of the Calls window.
//!
//! Everything shown here comes from the service's merged view (`bridge_core::calls_view`): a button
//! exists only if the service offered a target for it, so **Answer is never shown unless something
//! can really answer**. The decision logic ([`request_for`], [`route_text`], [`clock`]) is plain data
//! and tested without a window.
use crate::{
    call_feed::{CallFeed, FeedState},
    theme::{self, Palette},
};
use bridge_core::{
    calls::CallPhase,
    calls_view::{AudioRoute, AudioRouteView, CallRow, CallsView, ControlPath},
    ipc::{CallAction, Request},
};
use eframe::egui::{self, Align2, Color32, Key, Margin, RichText, vec2};
use std::{collections::BTreeMap, time::Instant};

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
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        for d in pressed(ctx, view) {
            if let Some(request) = request_for(view, d) {
                feed.send(request);
            }
        }

        if let Some(row) = view.live() {
            let secs = self.seconds(&row.key);
            egui::TopBottomPanel::top("ongoing-call")
                .frame(
                    egui::Frame::new()
                        .fill(pal.surface)
                        .inner_margin(Margin::symmetric(14, 10)),
                )
                .show(ctx, |ui| ongoing(ui, pal, feed, &state, row, secs));
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
        egui::Window::new("Incoming call")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(pal.surface)
                    .inner_margin(Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(260.0);
                ui.vertical_centered(|ui| {
                    ui.label(theme::overline(pal, "INCOMING CALL"));
                    ui.add_space(6.0);
                    ui.label(theme::semibold(row.caller_label(), 24.0));
                    if let (Some(name), Some(number)) = (&row.name, &row.number)
                        && name != number
                    {
                        ui.label(theme::secondary(pal, number.expose()));
                    }
                    ui.label(theme::caption(pal, &row.phone));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let width = (ui.available_width() - 12.0) / 2.0;
                        if row.reject.is_some() {
                            let b = ui.add_sized(
                                [width, 40.0],
                                egui::Button::new(
                                    RichText::new("Decline   Esc").color(Color32::WHITE),
                                )
                                .fill(pal.err),
                            );
                            if b.clicked()
                                && let Some(r) = request_for(&status.view, Decision::Decline)
                            {
                                feed.send(r);
                            }
                        }
                        if row.answer.is_some() {
                            let b = ui.add_sized(
                                [width, 40.0],
                                egui::Button::new(
                                    RichText::new("Answer   Enter").color(Color32::WHITE),
                                )
                                .fill(pal.ok),
                            );
                            if b.clicked()
                                && let Some(r) = request_for(&status.view, Decision::Answer)
                            {
                                feed.send(r);
                            }
                        }
                    });
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
            });
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
) {
    let view = &state.status.as_ref().expect("checked by caller").view;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(theme::semibold(row.caller_label(), 16.0));
            let phase = match row.phase {
                CallPhase::Held => "On hold".to_owned(),
                CallPhase::Dialing | CallPhase::Alerting => "Calling…".to_owned(),
                _ => format!("In call · {}", clock(secs)),
            };
            ui.label(theme::secondary(pal, phase));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if row.hangup.is_some()
                && ui
                    .add(
                        egui::Button::new(RichText::new("Hang up  H").color(Color32::WHITE))
                            .fill(pal.err)
                            .min_size(vec2(96.0, 30.0)),
                    )
                    .clicked()
                && let Some(r) = request_for(view, Decision::HangUp)
            {
                feed.send(r);
            }
            if let Some(m) = &view.mute {
                let label = if m.muted == Some(true) {
                    "Unmute  M"
                } else {
                    "Mute  M"
                };
                if ui
                    .add(egui::Button::new(label).min_size(vec2(84.0, 30.0)))
                    .clicked()
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

/// Keyboard decisions pressed this frame that the view could act on. Enter and Escape always count
/// while a call rings; letters only when no text field has the keyboard, so typing a number is safe.
fn pressed(ctx: &egui::Context, view: &CallsView) -> Vec<Decision> {
    let mut out = Vec::new();
    let typing = ctx.wants_keyboard_input();
    ctx.input_mut(|i| {
        let mut hit = |key: Key| i.consume_key(egui::Modifiers::NONE, key);
        if view.ringing().is_some() {
            if hit(Key::Enter) || (!typing && hit(Key::A)) {
                out.push(Decision::Answer);
            }
            if hit(Key::Escape) || (!typing && hit(Key::D)) {
                out.push(Decision::Decline);
            }
        }
        if !typing && view.live().is_some() {
            if hit(Key::H) {
                out.push(Decision::HangUp);
            }
            if hit(Key::M) {
                out.push(Decision::ToggleMute);
            }
        }
    });
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
}
