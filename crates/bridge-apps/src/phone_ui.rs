//! Small pieces shared by the Agenda and Messages windows for phone data: what to tell the user
//! when nothing is shown, and the explicit "Allow ... from <phone>" buttons. Consent is never
//! switched on by anything but a click on one of these (or `geekbridge-ctl consent`).
use crate::{
    shell::Env,
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    ipc::{Feature, Request},
    phone_view::{DataDevice, DataState},
};
use eframe::egui;

/// Plain-language line for a feature that is not showing data yet.
pub fn state_line(state: DataState, noun: &str) -> Option<String> {
    Some(match state {
        DataState::Ready => return None,
        DataState::ConsentOff => format!("{noun} is not shared from this phone."),
        DataState::Offline => "The phone is not connected right now.".to_owned(),
        DataState::NotOffered => format!(
            "The phone is connected but does not offer {}. Turn it on in the GeekBridge app on the phone (Share with computer) and grant the Android permission.",
            noun.to_lowercase()
        ),
        DataState::Waiting => format!("Asking the phone for {} ...", noun.to_lowercase()),
    })
}

/// One button per paired phone that has not allowed `feature`. Returns the request to run, if clicked.
pub fn allow_buttons(
    ui: &mut egui::Ui,
    pal: &Palette,
    env: &Env,
    feature: Feature,
    noun: &str,
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
            let label = format!("Allow {noun} from {}", phone.name);
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
        parts.push(format!("{} contacts", d.contacts_count));
    }
    if d.calendar != DataState::ConsentOff {
        parts.push(format!("{} events", d.events_count));
    }
    format!("{}: {}", d.name, parts.join(", "))
}

pub fn age_label(now_ms: u64, then_ms: u64) -> String {
    let s = now_ms.saturating_sub(then_ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_explain_each_state_and_stay_quiet_when_ready() {
        assert!(state_line(DataState::Ready, "Contacts").is_none());
        assert!(
            state_line(DataState::ConsentOff, "Contacts")
                .unwrap()
                .contains("not shared")
        );
        assert!(
            state_line(DataState::NotOffered, "Messages")
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
