// SPDX-License-Identifier: GPL-3.0-or-later
//! Small pieces shared by the Agenda and Messages windows for phone data: what to tell the user
//! when nothing is shown, and the explicit "Allow ... from <phone>" buttons. Consent is never
//! switched on by anything but a click on one of these (or `geekbridge-ctl consent`).
use crate::{
    i18n::{tr, tr_args, tr_n},
    shell::Env,
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    phone_view::{DataDevice, DataState},
};
use eframe::egui;

/// What a phone-data line is about; the names come from the catalogue so each language inflects them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Noun {
    Contacts,
    Messages,
    Calendar,
    CalendarEditing,
}

impl Noun {
    fn key(self) -> &'static str {
        match self {
            Noun::Contacts => "phone.noun.contacts",
            Noun::Messages => "phone.noun.messages",
            Noun::Calendar => "phone.noun.calendar",
            Noun::CalendarEditing => "phone.noun.calendar_editing",
        }
    }

    fn title(self) -> &'static str {
        tr(self.key())
    }

    fn lower(self) -> String {
        tr_args(&format!("{}.lower", self.key()), &[])
    }
}

/// Plain-language line for a feature that is not showing data yet.
pub fn state_line(state: DataState, noun: Noun) -> Option<String> {
    Some(match state {
        DataState::Ready => return None,
        DataState::ConsentOff => tr_args("phone.state.consent_off", &[("noun", &noun.title())]),
        DataState::Offline => tr("phone.state.offline").to_owned(),
        DataState::NotOffered => {
            tr_args("phone.state.not_offered", &[("noun_lower", &noun.lower())])
        }
        DataState::Waiting => tr_args("phone.state.waiting", &[("noun_lower", &noun.lower())]),
    })
}

/// One button per paired phone that has not allowed `feature`. Returns the request to run, if clicked.
pub fn allow_buttons(
    ui: &mut egui::Ui,
    pal: &Palette,
    env: &Env,
    feature: Feature,
    noun: Noun,
) -> Option<Request> {
    let status = env.control.status.as_ref()?;
    let mut out = None;
    for phone in &status.paired {
        let off = phone
            .consents
            .iter()
            .any(|c| c.feature == feature && !c.enabled);
        if off {
            ui.add_space(6.0);
            let label = tr_args(
                "phone.allow",
                &[("noun", &noun.title()), ("phone", &phone.name)],
            );
            if theme::pill_button(ui, pal, &label, ButtonKind::Primary, true).clicked() {
                out = Some(Request::SetConsent {
                    device_id: phone.device_id.clone(),
                    feature,
                    enabled: true,
                });
            }
        }
    }
    out
}

/// Caption for the devices list: "Pixel: contacts 5, calendar 6 events".
pub fn device_summary(d: &DataDevice) -> String {
    let mut parts = Vec::new();
    if d.contacts != DataState::ConsentOff {
        parts.push(tr_n("phone.contacts", d.contacts_count as u64, &[]));
    }
    if d.calendar != DataState::ConsentOff {
        parts.push(tr_n("phone.events", d.events_count as u64, &[]));
    }
    tr_args(
        "phone.device_summary",
        &[("name", &d.name), ("parts", &parts.join(", "))],
    )
}

pub fn age_label(now_ms: u64, then_ms: u64) -> String {
    let s = now_ms.saturating_sub(then_ms) / 1000;
    match s {
        0..=59 => tr("phone.age.now").to_owned(),
        60..=3599 => tr_args("phone.age.minutes", &[("n", &(s / 60))]),
        3600..=86_399 => tr_args("phone.age.hours", &[("n", &(s / 3600))]),
        _ => tr_args("phone.age.days", &[("n", &(s / 86_400))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_explain_each_state_and_stay_quiet_when_ready() {
        assert!(state_line(DataState::Ready, Noun::Contacts).is_none());
        assert!(
            state_line(DataState::ConsentOff, Noun::Contacts)
                .unwrap()
                .contains("not shared")
        );
        assert!(
            state_line(DataState::NotOffered, Noun::Messages)
                .unwrap()
                .contains("Share with computer")
        );
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_label(100_000, 99_000), "just now");
        assert_eq!(age_label(10 * 60_000, 0), "10 min ago");
        assert_eq!(age_label(5 * 3_600_000, 0), "5 h ago");
        assert_eq!(age_label(3 * 86_400_000, 0), "3 d ago");
    }
}
