// SPDX-License-Identifier: GPL-3.0-or-later
//! EXPERIMENTAL: the Wi-Fi call audio part of the live-call card ("Listen here (Wi-Fi, experimental)").
//!
//! Off by default and absent unless the service reports a connected phone that is ready (its helper runs and its
//! self-test found the caller's voice) with this computer's consent. While audio plays on this computer a clear
//! indicator with a Stop button is always shown, even if the call card itself is gone. "Speak (experimental)" is
//! a separate button that exists only when the phone reported a way to speak into the call. Bluetooth hands-free
//! stays the primary, supported path.
use crate::{
    call_feed::CallFeed,
    i18n::{tr, tr_args},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    call_wifi_view::{CallWifiAction, CallWifiView},
    calls_view::CallRow,
    ipc::{CallsStatus, Request},
};
use eframe::egui::{self, RichText};

/// What the card shows about Wi-Fi call audio. Plain data, tested without a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiCard {
    /// A phone could be listened to: offer the button.
    Offer { device_id: String, phone: String },
    /// Audio is, or is about to be, playing on this computer.
    Running {
        /// `starting` or `live`.
        live: bool,
        phone: String,
        seconds: u64,
        lost: u64,
        speaking: bool,
        /// The phone reported a way to speak and the session is live: show "Speak (experimental)".
        can_speak: bool,
    },
}

fn live_phone<'a>(
    w: &'a CallWifiView,
    row: Option<&CallRow>,
) -> Option<&'a bridge_core::call_wifi_view::CallWifiPhone> {
    let mut can = w
        .phones
        .iter()
        .filter(|p| p.allowed && p.listen && p.in_call);
    let first = can.next()?;
    match (can.next(), row) {
        (None, _) => Some(first),
        // Several phones could be listened to: only the one this card is about.
        (Some(_), Some(row)) => w
            .phones
            .iter()
            .find(|p| p.allowed && p.listen && p.in_call && p.name == row.phone),
        (Some(_), None) => None,
    }
}

/// The card for this status. `row` is the live call the card is drawn for, if any.
pub fn card(status: &CallsStatus, row: Option<&CallRow>) -> Option<WifiCard> {
    let w = status.wifi.as_ref().filter(|w| w.supported)?;
    if let Some(s) = &w.session {
        let phone = w.phones.iter().find(|p| p.device_id == s.device_id);
        let live = s.state == "live";
        return Some(WifiCard::Running {
            live,
            phone: s.name.clone(),
            seconds: s.seconds,
            lost: s.lost,
            speaking: s.speaking,
            can_speak: live && phone.is_some_and(|p| p.speak),
        });
    }
    let row = row?;
    let p = live_phone(w, Some(row))?;
    Some(WifiCard::Offer {
        device_id: p.device_id.clone(),
        phone: p.name.clone(),
    })
}

/// Whether the safety indicator must be on screen even without a live call row.
pub fn running(status: &CallsStatus) -> bool {
    status.wifi.as_ref().is_some_and(|w| w.session.is_some())
}

fn ask(feed: &CallFeed, action: CallWifiAction) {
    feed.send(Request::CallWifi { action });
}

/// Draw the section inside the live-call card.
pub fn show(ui: &mut egui::Ui, pal: &Palette, feed: &CallFeed, card: &WifiCard) {
    ui.add_space(8.0);
    match card {
        WifiCard::Offer { device_id, phone } => {
            ui.horizontal(|ui| {
                let r = theme::pill_button(ui, pal, tr("callwifi.listen"), ButtonKind::Plain, true);
                let name = tr_args("callwifi.listen.name", &[("phone", phone)]);
                r.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone())
                });
                if r.clicked() {
                    ask(
                        feed,
                        CallWifiAction::Listen {
                            device_id: device_id.clone(),
                        },
                    );
                }
            });
            ui.add(egui::Label::new(theme::caption(pal, tr("callwifi.listen.hint"))).wrap());
        }
        WifiCard::Running {
            live,
            seconds,
            lost,
            speaking,
            can_speak,
            ..
        } => {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0_f32, pal.err))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let line = if *live {
                            tr("callwifi.listening")
                        } else {
                            tr("callwifi.starting")
                        };
                        ui.label(RichText::new("\u{25CF}").color(pal.err));
                        ui.add(
                            egui::Label::new(RichText::new(line).strong().color(pal.err)).wrap(),
                        );
                    });
                    if *live {
                        ui.label(theme::caption(
                            pal,
                            tr_args("callwifi.numbers", &[("seconds", seconds), ("lost", lost)]),
                        ));
                    }
                    if *speaking {
                        ui.add(
                            egui::Label::new(
                                RichText::new(tr("callwifi.speaking"))
                                    .strong()
                                    .color(pal.err),
                            )
                            .wrap(),
                        );
                    }
                    ui.horizontal(|ui| {
                        if theme::pill_button(
                            ui,
                            pal,
                            tr("callwifi.stop"),
                            ButtonKind::Primary,
                            true,
                        )
                        .clicked()
                        {
                            ask(feed, CallWifiAction::Stop);
                        }
                        if *can_speak {
                            let (label, kind) = if *speaking {
                                (tr("callwifi.speak.stop"), ButtonKind::Primary)
                            } else {
                                (tr("callwifi.speak"), ButtonKind::Plain)
                            };
                            if theme::pill_button(ui, pal, label, kind, true).clicked() {
                                ask(feed, CallWifiAction::Speak { on: !*speaking });
                            }
                        }
                    });
                    if *can_speak && !*speaking {
                        ui.add(
                            egui::Label::new(theme::caption(pal, tr("callwifi.speak.hint"))).wrap(),
                        );
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_wifi_view::{CallWifiPhone, CallWifiSession};
    use bridge_core::calls_view::CallsView;

    fn phone(id: &str, name: &str) -> CallWifiPhone {
        CallWifiPhone {
            device_id: id.into(),
            name: name.into(),
            connected: true,
            allowed: true,
            ready: true,
            listen: true,
            in_call: true,
            test: "works".into(),
            ..Default::default()
        }
    }

    fn status(w: Option<CallWifiView>) -> CallsStatus {
        CallsStatus {
            view: CallsView::empty(),
            notice: None,
            sources: vec![],
            audio_note: None,
            wifi: w,
        }
    }

    fn view(phones: Vec<CallWifiPhone>) -> CallWifiView {
        CallWifiView {
            supported: true,
            phones,
            ..Default::default()
        }
    }

    fn row(phone: &str) -> CallRow {
        use bridge_core::calls::{CallDirection, CallPhase};
        CallRow {
            key: "k".into(),
            phone: phone.into(),
            direction: CallDirection::Incoming,
            phase: CallPhase::Active,
            number: None,
            name: None,
            answer: None,
            reject: None,
            hangup: None,
            missing_control: None,
            parts: Vec::new(),
        }
    }

    #[test]
    fn nothing_is_offered_unless_a_phone_is_allowed_ready_able_and_in_a_call() {
        let r = row("Fold");
        assert_eq!(card(&status(None), Some(&r)), None);
        let mut w = view(vec![phone("a", "Fold")]);
        assert!(matches!(
            card(&status(Some(w.clone())), Some(&r)),
            Some(WifiCard::Offer { .. })
        ));
        // No live row on this card: no offer.
        assert_eq!(card(&status(Some(w.clone())), None), None);
        for off in [
            |p: &mut CallWifiPhone| p.allowed = false,
            |p: &mut CallWifiPhone| p.listen = false,
            |p: &mut CallWifiPhone| p.in_call = false,
        ] {
            let mut x = w.clone();
            off(&mut x.phones[0]);
            assert_eq!(card(&status(Some(x)), Some(&r)), None);
        }
        w.supported = false;
        assert_eq!(card(&status(Some(w)), Some(&r)), None);
    }

    #[test]
    fn with_two_phones_the_card_offers_the_one_it_is_about() {
        let w = view(vec![phone("a", "Fold"), phone("b", "Pixel")]);
        let s = status(Some(w));
        assert_eq!(card(&s, None), None);
        assert_eq!(
            card(&s, Some(&row("Pixel"))),
            Some(WifiCard::Offer {
                device_id: "b".into(),
                phone: "Pixel".into()
            })
        );
        assert_eq!(card(&s, Some(&row("Other"))), None);
    }

    #[test]
    fn speak_is_hidden_unless_the_phone_reported_an_uplink_and_the_session_is_live() {
        let mut w = view(vec![phone("a", "Fold")]);
        w.session = Some(CallWifiSession {
            device_id: "a".into(),
            name: "Fold".into(),
            state: "live".into(),
            ..Default::default()
        });
        let c = card(&status(Some(w.clone())), None).unwrap();
        assert!(matches!(
            c,
            WifiCard::Running {
                live: true,
                can_speak: false,
                ..
            }
        ));
        w.phones[0].speak = true;
        assert!(matches!(
            card(&status(Some(w.clone())), None).unwrap(),
            WifiCard::Running {
                can_speak: true,
                ..
            }
        ));
        w.session.as_mut().unwrap().state = "starting".into();
        assert!(matches!(
            card(&status(Some(w.clone())), None).unwrap(),
            WifiCard::Running {
                live: false,
                can_speak: false,
                ..
            }
        ));
        // The indicator must show even when no call row is on screen.
        assert!(running(&status(Some(w))));
        assert!(!running(&status(None)));
    }

    #[test]
    fn the_section_draws_for_every_state_and_clicks_send_the_right_requests() {
        use crate::call_feed::CallFeed;
        use std::sync::{Arc, Mutex};
        let sent: Arc<Mutex<Vec<Request>>> = Arc::default();
        let s2 = sent.clone();
        let feed = CallFeed::spawn_with(
            std::time::Duration::from_secs(3600),
            || Ok(status(None)),
            move |r| {
                s2.lock().unwrap().push(r.clone());
                Ok(())
            },
            || {},
        );
        let rig = crate::test_support::Rig::new(bridge_core::AppKind::Calls, 420.0, 640.0);
        let ctx = rig.ctx.clone();
        let pal = theme::current(&ctx, bridge_core::AppKind::Calls);
        for c in [
            WifiCard::Offer {
                device_id: "a".into(),
                phone: "Fold".into(),
            },
            WifiCard::Running {
                live: false,
                phone: "Fold".into(),
                seconds: 0,
                lost: 0,
                speaking: false,
                can_speak: false,
            },
            WifiCard::Running {
                live: true,
                phone: "Fold".into(),
                seconds: 12,
                lost: 1,
                speaking: true,
                can_speak: true,
            },
        ] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &pal, &feed, &c));
            });
        }
        assert!(
            sent.lock().unwrap().is_empty(),
            "drawing alone sends nothing"
        );
        ask(&feed, CallWifiAction::Stop);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(matches!(
            sent.lock().unwrap().as_slice(),
            [Request::CallWifi {
                action: CallWifiAction::Stop
            }]
        ));
    }
}
