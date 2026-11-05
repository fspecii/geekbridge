// SPDX-License-Identifier: GPL-3.0-or-later
//! The display-privacy choices (`bridge_core::privacy`): stored in `<state dir>/privacy`, changed live
//! through `PrivacySet`, and still forced by the older switches. Nothing here sees message text or
//! numbers; it holds three booleans.
//!
//! Resolution per key: a forcing source (`GEEKBRIDGE_SMS_NOTIFY_CONTENT=show` or the
//! `sms-notify-content` marker for the two text keys; `GEEKBRIDGE_CALL_OVERLAY` or the
//! `call-overlay` file for the caller number) wins, then the stored choice, then the default.
use bridge_core::{
    call_overlay::Mode,
    privacy::{self, ForcedBy, PrivacyItem, PrivacyKey, PrivacyView},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const FILE: &str = "privacy";
pub const MARKER_FILE: &str = "sms-notify-content";
pub const ENV_SMS: &str = "GEEKBRIDGE_SMS_NOTIFY_CONTENT";
pub const ENV_OVERLAY: &str = "GEEKBRIDGE_CALL_OVERLAY";

type Env = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct Privacy {
    dir: Option<PathBuf>,
    env: Env,
    chosen: Mutex<Vec<(PrivacyKey, bool)>>,
    /// Serialises writers; `chosen` is held only to copy and to publish, never across the file write, so a slow
    /// fsync cannot freeze the readers (`Daemon::status`, the notification paths).
    writer: Mutex<()>,
    /// `std::env::consts::OS`; a field so the Windows and macOS answers are tested on Linux.
    os: &'static str,
}

impl Privacy {
    /// `dir` is the state folder; `None` keeps the choices in memory only (tests, no state folder).
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self::with_env(dir, Arc::new(|k| std::env::var(k).ok()))
    }

    pub fn with_env(dir: Option<PathBuf>, env: Env) -> Self {
        let chosen = dir.as_deref().map(stored).unwrap_or_default();
        Self {
            dir,
            env,
            chosen: Mutex::new(chosen),
            writer: Mutex::new(()),
            os: std::env::consts::OS,
        }
    }

    #[cfg(test)]
    fn on_os(mut self, os: &'static str) -> Self {
        self.os = os;
        self
    }

    fn forced(&self, key: PrivacyKey) -> Option<(bool, ForcedBy)> {
        let file = |name: &str| {
            self.dir
                .as_ref()
                .and_then(|d| std::fs::read_to_string(d.join(name)).ok())
        };
        match key {
            PrivacyKey::MessageText | PrivacyKey::PhoneNotificationText => {
                if (self.env)(ENV_SMS).is_some_and(|v| v == "show") {
                    Some((true, ForcedBy::Environment))
                } else if self
                    .dir
                    .as_ref()
                    .is_some_and(|d| d.join(MARKER_FILE).exists())
                {
                    Some((true, ForcedBy::File))
                } else {
                    None
                }
            }
            PrivacyKey::RespectDnd => None,
            PrivacyKey::CallerNumber => {
                let word = |t: Option<String>| t.filter(|t| !t.trim().is_empty());
                let by_env = word((self.env)(ENV_OVERLAY)).and_then(|w| Mode::parse(&w));
                let (mode, by) = match by_env {
                    Some(m) => (m, ForcedBy::Environment),
                    None => (
                        word(file(crate::call_overlay::SETTINGS_FILE))
                            .and_then(|w| Mode::parse(&w))?,
                        ForcedBy::File,
                    ),
                };
                match mode {
                    Mode::On => Some((true, by)),
                    Mode::NameOnly => Some((false, by)),
                    Mode::Off => None,
                }
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(PrivacyKey, bool)>> {
        self.chosen.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn item(&self, key: PrivacyKey) -> PrivacyItem {
        let chosen = self.lock().iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let mut item = privacy::resolve(key, self.forced(key), chosen);
        // A choice this system cannot honour is never reported as on, whatever an old file says.
        item.on &= key.supported_on(self.os);
        item
    }

    pub fn view(&self) -> PrivacyView {
        PrivacyView {
            items: PrivacyKey::ALL.into_iter().map(|k| self.item(k)).collect(),
        }
    }

    pub fn on(&self, key: PrivacyKey) -> bool {
        self.item(key).on
    }

    /// Store a choice (and write it). The caller applies the new view to the live parts. Does file I/O with an
    /// fsync: call it from `spawn_blocking`, not on an async worker.
    pub fn set(&self, key: PrivacyKey, on: bool) -> Result<(), String> {
        self.set_with(key, on, |text| match &self.dir {
            Some(dir) => bridge_kde::fsutil::atomic_write_private(&dir.join(FILE), text.as_bytes())
                .map_err(|_| "the choice could not be saved".to_owned()),
            None => Ok(()),
        })
    }

    fn set_with(
        &self,
        key: PrivacyKey,
        on: bool,
        write: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        if on && !key.supported_on(self.os) {
            return Err(format!("{}: {}", key.name(), privacy::NOT_SUPPORTED));
        }
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = self.lock().clone();
        next.retain(|(k, _)| *k != key);
        next.push((key, on));
        write(&privacy::format_choices(&next))?;
        *self.lock() = next;
        Ok(())
    }
}

/// The stored choices in `<dir>/privacy`.
pub fn stored(dir: &Path) -> Vec<(PrivacyKey, bool)> {
    std::fs::read_to_string(dir.join(FILE))
        .map(|t| privacy::parse_choices(&t))
        .unwrap_or_default()
}

/// The stored choice for one key, for the call card's launcher (a separate process reads the file).
pub fn stored_choice(dir: &Path, key: PrivacyKey) -> Option<bool> {
    stored(dir)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> Env {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Arc::new(move |k| m.get(k).cloned())
    }

    #[test]
    fn a_slow_write_does_not_block_readers() {
        let p = Arc::new(Privacy::with_env(None, env(&[])));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let writer = {
            let p = p.clone();
            std::thread::spawn(move || {
                p.set_with(PrivacyKey::MessageText, true, |_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap(); // the fsync that takes seconds on a busy disk
                    Ok(())
                })
            })
        };
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reader = {
            let p = p.clone();
            std::thread::spawn(move || {
                let _ = (p.view(), p.on(PrivacyKey::MessageText));
                done_tx.send(()).unwrap();
            })
        };
        let answered = done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        assert!(answered, "a reader waited for the writer's file I/O");
        assert!(
            p.on(PrivacyKey::MessageText),
            "the choice is published afterwards"
        );
    }

    #[test]
    fn do_not_disturb_cannot_be_switched_on_where_it_cannot_be_read() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        bridge_kde::fsutil::ensure_private_dir(&state).unwrap();
        for os in ["windows", "macos"] {
            // An old or copied file that says on does not make it on.
            bridge_kde::fsutil::atomic_write_private(&state.join(FILE), b"respect_dnd=on\r\n")
                .unwrap();
            let p = Privacy::with_env(Some(state.clone()), env(&[])).on_os(os);
            assert!(!p.on(PrivacyKey::RespectDnd), "{os}");
            let e = p.set(PrivacyKey::RespectDnd, true).unwrap_err();
            assert!(e.contains("not supported on this system"), "{e}");
            assert!(!p.on(PrivacyKey::RespectDnd));
            p.set(PrivacyKey::RespectDnd, false).unwrap();
            // The other choices are unaffected.
            p.set(PrivacyKey::MessageText, true).unwrap();
            assert!(p.on(PrivacyKey::MessageText));
        }
        let p = Privacy::with_env(Some(state), env(&[])).on_os("linux");
        p.set(PrivacyKey::RespectDnd, true).unwrap();
        assert!(p.on(PrivacyKey::RespectDnd));
    }

    #[test]
    fn defaults_set_persist_and_reload() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let p = Privacy::with_env(Some(state.clone()), env(&[]));
        let v = p.view();
        assert!(!v.get(PrivacyKey::MessageText).unwrap().on);
        assert!(!v.get(PrivacyKey::PhoneNotificationText).unwrap().on);
        assert!(v.get(PrivacyKey::CallerNumber).unwrap().on);
        assert!(
            !v.get(PrivacyKey::RespectDnd).unwrap().on,
            "Do Not Disturb is opt-in"
        );
        p.set(PrivacyKey::MessageText, true).unwrap();
        p.set(PrivacyKey::CallerNumber, false).unwrap();
        assert!(p.on(PrivacyKey::MessageText) && !p.on(PrivacyKey::CallerNumber));
        let again = Privacy::with_env(Some(state.clone()), env(&[]));
        assert!(again.on(PrivacyKey::MessageText) && !again.on(PrivacyKey::CallerNumber));
        assert!(
            !again.on(PrivacyKey::PhoneNotificationText),
            "keys are independent"
        );
        assert_eq!(stored_choice(&state, PrivacyKey::CallerNumber), Some(false));
        p.set(PrivacyKey::RespectDnd, true).unwrap();
        assert_eq!(stored_choice(&state, PrivacyKey::RespectDnd), Some(true));
    }

    #[test]
    fn the_old_marker_and_variable_still_force_the_text_choices_on() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let p = Privacy::with_env(Some(state.clone()), env(&[]));
        p.set(PrivacyKey::MessageText, false).unwrap();
        std::fs::write(state.join(MARKER_FILE), b"").unwrap();
        for k in [PrivacyKey::MessageText, PrivacyKey::PhoneNotificationText] {
            let i = p.item(k);
            assert!(i.on && i.forced == Some(ForcedBy::File), "{k:?}");
        }
        assert!(p.item(PrivacyKey::CallerNumber).forced.is_none());
        std::fs::remove_file(state.join(MARKER_FILE)).unwrap();
        assert!(
            !p.on(PrivacyKey::MessageText),
            "the stored off applies again"
        );
        let e = Privacy::with_env(None, env(&[(ENV_SMS, "show")]));
        let i = e.item(PrivacyKey::PhoneNotificationText);
        assert!(i.on && i.forced == Some(ForcedBy::Environment));
        let other = Privacy::with_env(None, env(&[(ENV_SMS, "yes")]));
        assert!(
            !other.on(PrivacyKey::MessageText),
            "only `show` forced it before"
        );
    }

    #[test]
    fn the_call_overlay_word_forces_the_caller_number() {
        let d = tempfile::tempdir().unwrap();
        let p = Privacy::with_env(Some(d.path().to_owned()), env(&[]));
        std::fs::write(d.path().join(crate::call_overlay::SETTINGS_FILE), "name\n").unwrap();
        let i = p.item(PrivacyKey::CallerNumber);
        assert!(!i.on && i.forced == Some(ForcedBy::File));
        let e = Privacy::with_env(Some(d.path().to_owned()), env(&[(ENV_OVERLAY, "on")]));
        let i = e.item(PrivacyKey::CallerNumber);
        assert!(
            i.on && i.forced == Some(ForcedBy::Environment),
            "the variable wins over the file"
        );
        // `off` disables the card; it does not say anything about the number.
        std::fs::write(d.path().join(crate::call_overlay::SETTINGS_FILE), "off\n").unwrap();
        assert!(p.item(PrivacyKey::CallerNumber).forced.is_none());
        std::fs::write(d.path().join(crate::call_overlay::SETTINGS_FILE), "\n").unwrap();
        assert!(
            p.item(PrivacyKey::CallerNumber).forced.is_none(),
            "an empty file forces nothing"
        );
    }
}
