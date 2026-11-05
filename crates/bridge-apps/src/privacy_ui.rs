// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings -> Privacy: what a banner or the incoming-call card may show. The service owns the
//! values (`bridge_core::privacy`); this module reads them once in a while, forwards a click as one
//! request and shows what the service says is in effect, including a value an older switch (the
//! `sms-notify-content` file, an environment variable) forces.
use crate::{
    a11y,
    connection::Config,
    i18n::{tr, tr_args},
    nothing::S1,
    theme::{self, Palette},
};
use bridge_core::{
    ipc::{Request, Response},
    privacy::{ForcedBy, PrivacyKey, PrivacyView},
};
use eframe::egui;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivacyState {
    pub view: Option<PrivacyView>,
    pub error: Option<String>,
    /// Why the last change was refused; kept until the next change is accepted.
    pub refused: Option<String>,
    /// Counts answered changes, so a shown-but-not-yet-confirmed click can be dropped.
    pub answered: u64,
}

/// One line of the page, with every word resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: PrivacyKey,
    pub title: String,
    pub detail: String,
    pub on: bool,
    /// Why the switch cannot be moved here, when an older switch decides the value.
    pub forced_note: Option<String>,
}

fn title_key(key: PrivacyKey) -> &'static str {
    match key {
        PrivacyKey::MessageText => "privacy.message_text",
        PrivacyKey::CallerNumber => "privacy.caller_number",
        PrivacyKey::PhoneNotificationText => "privacy.phone_notification_text",
        PrivacyKey::RespectDnd => "privacy.respect_dnd",
    }
}

fn detail_key(key: PrivacyKey) -> &'static str {
    match key {
        PrivacyKey::MessageText => "privacy.message_text.detail",
        PrivacyKey::CallerNumber => "privacy.caller_number.detail",
        PrivacyKey::PhoneNotificationText => "privacy.phone_notification_text.detail",
        PrivacyKey::RespectDnd if cfg!(target_os = "linux") => "privacy.respect_dnd.detail",
        PrivacyKey::RespectDnd => "privacy.respect_dnd.detail_other",
    }
}

fn forced_key(key: PrivacyKey, by: ForcedBy) -> &'static str {
    match (key, by) {
        (PrivacyKey::CallerNumber, ForcedBy::Environment) => "privacy.forced.overlay_env",
        (PrivacyKey::CallerNumber, ForcedBy::File) => "privacy.forced.overlay_file",
        (_, ForcedBy::Environment) => "privacy.forced.sms_env",
        (_, ForcedBy::File) => "privacy.forced.sms_file",
    }
}

/// The rows for a view; `pending` holds clicks the service has not answered yet.
pub fn rows(view: &PrivacyView, pending: &HashMap<PrivacyKey, bool>) -> Vec<Row> {
    view.items
        .iter()
        .map(|i| Row {
            key: i.key,
            title: tr(title_key(i.key)).to_owned(),
            detail: tr(detail_key(i.key)).to_owned(),
            on: if i.forced.is_some() {
                i.on
            } else {
                pending.get(&i.key).copied().unwrap_or(i.on)
            },
            forced_note: i.forced.map(|by| tr(forced_key(i.key, by)).to_owned()),
        })
        .collect()
}

pub struct PrivacyLink {
    commands: mpsc::SyncSender<(PrivacyKey, bool)>,
    latest: Arc<Mutex<Option<PrivacyState>>>,
    stop: Arc<AtomicBool>,
    pub state: PrivacyState,
    pending: HashMap<PrivacyKey, bool>,
}

impl PrivacyLink {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let config = Config::from_environment();
        Self::spawn_with(move |r| config.request(r), wake)
    }

    pub fn spawn_with(
        call: impl Fn(&Request) -> Result<Response, String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let (commands, rx) = mpsc::sync_channel::<(PrivacyKey, bool)>(8);
        let latest = Arc::new(Mutex::new(None));
        let output = latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = stop.clone();
        std::thread::Builder::new()
            .name("geekbridge-privacy-link".into())
            .spawn(move || {
                let mut state = PrivacyState::default();
                let mut previous: Option<PrivacyState> = None;
                while !quitting.load(Ordering::Acquire) {
                    match call(&Request::PrivacyStatus) {
                        Ok(Response::Privacy(v)) => {
                            state.view = Some(v);
                            state.error = None;
                        }
                        Ok(_) => state.error = Some(tr("privacy.unexpected").to_owned()),
                        Err(e) => {
                            state.view = None;
                            state.error = Some(e);
                        }
                    }
                    if previous.as_ref() != Some(&state) {
                        previous = Some(state.clone());
                        *output.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
                        wake();
                    }
                    match rx.recv_timeout(crate::pace::scaled(Duration::from_secs(3))) {
                        Ok((key, on)) if !quitting.load(Ordering::Acquire) => {
                            state.answered += 1;
                            state.refused = match call(&Request::PrivacySet { key, on }) {
                                Ok(Response::Error(b)) => {
                                    Some(crate::state::display_text(&b.message))
                                }
                                Ok(_) => None,
                                Err(e) => Some(e),
                            };
                        }
                        Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn privacy link worker");
        Self {
            commands,
            latest,
            stop,
            state: PrivacyState::default(),
            pending: HashMap::new(),
        }
    }

    pub fn update(&mut self) {
        if let Some(s) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if s.answered != self.state.answered {
                self.pending.clear();
            }
            self.state = s;
        }
    }

    pub fn set(&mut self, key: PrivacyKey, on: bool) {
        if self.commands.try_send((key, on)).is_ok() {
            self.pending.insert(key, on);
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.state
            .view
            .as_ref()
            .map(|v| rows(v, &self.pending))
            .unwrap_or_default()
    }
}

impl Drop for PrivacyLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// The Settings card.
pub fn show(ui: &mut egui::Ui, pal: &Palette, link: &mut PrivacyLink) {
    let rows = link.rows();
    if rows.is_empty() {
        let text = link
            .state
            .error
            .clone()
            .unwrap_or_else(|| tr("privacy.checking").to_owned());
        ui.add(egui::Label::new(theme::secondary(pal, text)).wrap());
        return;
    }
    ui.add(egui::Label::new(theme::caption(pal, tr("privacy.intro"))).wrap());
    for row in rows {
        ui.add_space(S1);
        let mut on = row.on;
        let enabled = row.forced_note.is_none();
        let response = ui
            .add_enabled_ui(enabled, |ui| ui.checkbox(&mut on, row.title.as_str()))
            .inner;
        a11y::describe(
            ui.ctx(),
            response.id,
            &row.title,
            Some(&tr_args(
                "privacy.a11y.state",
                &[(
                    "state",
                    &tr(if row.on { "privacy.on" } else { "privacy.off" }),
                )],
            )),
        );
        if enabled && response.changed() {
            link.set(row.key, on);
        }
        ui.add(egui::Label::new(theme::caption(pal, row.detail.as_str())).wrap());
        if let Some(note) = &row.forced_note {
            ui.add(egui::Label::new(theme::caption(pal, note.as_str())).wrap());
        }
    }
    if let Some(e) = link.state.refused.as_ref().or(link.state.error.as_ref()) {
        ui.add_space(S1);
        ui.add(egui::Label::new(theme::secondary(pal, e.as_str())).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::privacy::PrivacyItem;

    fn view(forced: Option<ForcedBy>) -> PrivacyView {
        PrivacyView {
            items: vec![
                PrivacyItem {
                    key: PrivacyKey::MessageText,
                    on: forced.is_some(),
                    forced,
                },
                PrivacyItem {
                    key: PrivacyKey::CallerNumber,
                    on: true,
                    forced: None,
                },
                PrivacyItem {
                    key: PrivacyKey::PhoneNotificationText,
                    on: false,
                    forced: None,
                },
            ],
        }
    }

    #[test]
    fn rows_show_the_current_state_and_why_a_forced_one_is_fixed() {
        let r = rows(&view(None), &HashMap::new());
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].title, "Show message text in notifications");
        assert!(!r[0].on && r[0].forced_note.is_none());
        assert_eq!(r[1].title, "Show caller number on the call card");
        assert!(r[1].on);
        assert_eq!(r[2].title, "Show phone notification text");
        let forced = rows(&view(Some(ForcedBy::File)), &HashMap::new());
        assert!(forced[0].on);
        assert!(
            forced[0]
                .forced_note
                .as_deref()
                .unwrap()
                .contains("sms-notify-content")
        );
        // A click in flight shows at once, but never over a forced value.
        let pending = HashMap::from([(PrivacyKey::MessageText, true)]);
        assert!(rows(&view(None), &pending)[0].on);
        let pending = HashMap::from([(PrivacyKey::MessageText, false)]);
        assert!(rows(&view(Some(ForcedBy::Environment)), &pending)[0].on);
    }

    #[test]
    fn a_click_becomes_one_request_and_the_new_state_replaces_the_pending_one() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let log = seen.clone();
        let on = Arc::new(AtomicBool::new(false));
        let live = on.clone();
        let mut link = PrivacyLink::spawn_with(
            move |r| {
                log.lock().unwrap().push(r.clone());
                match r {
                    Request::PrivacySet { on, .. } => {
                        live.store(*on, Ordering::SeqCst);
                        Ok(Response::Ack)
                    }
                    _ => Ok(Response::Privacy(PrivacyView {
                        items: vec![PrivacyItem {
                            key: PrivacyKey::MessageText,
                            on: live.load(Ordering::SeqCst),
                            forced: None,
                        }],
                    })),
                }
            },
            || {},
        );
        let wait = |link: &mut PrivacyLink, f: &dyn Fn(&PrivacyLink) -> bool| {
            for _ in 0..200 {
                link.update();
                if f(link) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        };
        assert!(wait(&mut link, &|l| !l.rows().is_empty()));
        assert!(!link.rows()[0].on);
        link.set(PrivacyKey::MessageText, true);
        assert!(link.rows()[0].on, "shown at once");
        assert!(wait(&mut link, &|l| l.pending.is_empty() && l.rows()[0].on));
        let sets = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Request::PrivacySet {
                        key: PrivacyKey::MessageText,
                        on: true
                    }
                )
            })
            .count();
        assert_eq!(sets, 1);
    }

    #[test]
    fn a_refusal_is_shown_and_the_switch_snaps_back() {
        let mut link = PrivacyLink::spawn_with(
            |r| match r {
                Request::PrivacySet { .. } => Err("the choice could not be saved".into()),
                _ => Ok(Response::Privacy(view(None))),
            },
            || {},
        );
        for _ in 0..200 {
            link.update();
            if !link.rows().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        link.set(PrivacyKey::MessageText, true);
        for _ in 0..200 {
            link.update();
            if link.state.refused.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            link.state.refused.as_deref(),
            Some("the choice could not be saved")
        );
        assert!(link.pending.is_empty());
        assert!(!link.rows()[0].on, "the service never accepted it");
    }

    #[test]
    fn every_privacy_word_exists() {
        for k in PrivacyKey::ALL {
            assert!(!tr(title_key(k)).starts_with("privacy."));
            assert!(!tr(detail_key(k)).starts_with("privacy."));
            for by in [ForcedBy::Environment, ForcedBy::File] {
                assert!(!tr(forced_key(k, by)).starts_with("privacy."));
            }
        }
    }
}
