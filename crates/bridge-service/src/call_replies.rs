// SPDX-License-Identifier: GPL-3.0-or-later
//! The short texts behind "Decline with message": stored in `<state dir>/call-replies` (one per
//! line, owner-only), changed through `CallRepliesSet`, and sent through the ordinary Messages path
//! (`PhoneData::send_text`, with its consent checks and per-minute limit). The file holds only what
//! the person typed; no number, no history of what was sent.
use crate::call_control::ReplyTexts;
use crate::phone_data::PhoneData;
use bridge_core::{
    call_replies::{self, ReplyError},
    ipc::ErrorBody,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const FILE: &str = "call-replies";

pub struct Replies {
    dir: Option<PathBuf>,
    saved: Mutex<Vec<String>>,
    /// One save at a time; the disk write happens under this, never under `saved`, so reading the
    /// texts (a ring, a status poll) never waits for an fsync.
    writing: Mutex<()>,
}

impl Replies {
    /// `dir` is the state folder; `None` keeps the list in memory only (tests).
    pub fn new(dir: Option<PathBuf>) -> Self {
        let saved = dir.as_deref().map(load).unwrap_or_default();
        Self {
            dir,
            saved: Mutex::new(saved),
            writing: Mutex::new(()),
        }
    }

    pub fn texts(&self) -> Vec<String> {
        call_replies::effective(&self.saved.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Save `texts` (cleaned; an empty list restores the defaults and removes the file).
    pub fn set(&self, texts: &[String]) -> Result<(), ReplyError> {
        let clean = call_replies::clean(texts)?;
        let _one_save = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = &self.dir {
            let path = dir.join(FILE);
            if clean.is_empty() {
                let _ = std::fs::remove_file(&path);
            } else {
                let body = clean.join("\n") + "\n";
                bridge_kde::fsutil::atomic_write_private(&path, body.as_bytes())
                    .map_err(|_| ReplyError::Save)?;
            }
        }
        *self.saved.lock().unwrap_or_else(|e| e.into_inner()) = clean;
        Ok(())
    }
}

/// The stored list; anything that does not pass the rules is ignored (a hand-edited file cannot
/// smuggle in a long or multi-line text).
fn load(dir: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    call_replies::clean(&lines).unwrap_or_default()
}

/// The hub's view of all this: the stored texts and the phone's Messages path.
pub struct HubReplies {
    pub store: Arc<Replies>,
    pub data: Arc<PhoneData>,
}

impl ReplyTexts for HubReplies {
    fn texts(&self) -> Vec<String> {
        self.store.texts()
    }
    fn can_send(&self, device_id: &str) -> bool {
        self.data.can_send_text(device_id)
    }
    fn send(
        &self,
        device_id: &str,
        number: &str,
        text: &str,
        submission: &str,
    ) -> Result<(), ErrorBody> {
        self.data
            .send_text(device_id, number, text, submission, None)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gb-replies-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        bridge_kde::fsutil::ensure_private_dir(&d).unwrap();
        d
    }
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn without_a_saved_list_the_defaults_are_offered() {
        let d = tmp("defaults");
        assert_eq!(
            Replies::new(Some(d.clone())).texts(),
            call_replies::defaults()
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_saved_list_survives_a_restart_and_an_empty_one_restores_the_defaults() {
        let d = tmp("save");
        let r = Replies::new(Some(d.clone()));
        r.set(&v(&[" Busy ", "", "Later"])).unwrap();
        assert_eq!(r.texts(), v(&["Busy", "Later"]));
        assert_eq!(Replies::new(Some(d.clone())).texts(), v(&["Busy", "Later"]));
        r.set(&[]).unwrap();
        assert_eq!(
            Replies::new(Some(d.clone())).texts(),
            call_replies::defaults()
        );
        assert!(!d.join(FILE).exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_refused_list_leaves_the_old_one() {
        let r = Replies::new(None);
        r.set(&v(&["Busy"])).unwrap();
        assert!(r.set(&v(&["a\nb"])).is_err());
        assert!(r.set(&v(&["1", "2", "3", "4"])).is_err());
        assert_eq!(r.texts(), v(&["Busy"]));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only_and_a_bad_hand_edit_is_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("mode");
        let r = Replies::new(Some(d.clone()));
        r.set(&v(&["Busy"])).unwrap();
        let mode = std::fs::metadata(d.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        std::fs::write(d.join(FILE), format!("{}\n", "x".repeat(500))).unwrap();
        assert_eq!(
            Replies::new(Some(d.clone())).texts(),
            call_replies::defaults()
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
