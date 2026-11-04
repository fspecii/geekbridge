// SPDX-License-Identifier: GPL-3.0-or-later
//! The two small files call audio keeps in the state directory.
//!
//! * `call-audio.json`: the user's pick of output and microphone for calls (`null` = system
//!   default). Written by the service when the user picks, read at every call.
//! * `call-audio-restore.json`: what GeekBridge changed for the current call and must undo (a
//!   card profile, a microphone mute, the phone's RejectSCO flag). Written before each change,
//!   removed after the undo, and replayed at the next start if the process died in between.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CHOICE_FILE: &str = "call-audio.json";
pub const RESTORE_FILE: &str = "call-audio-restore.json";

/// Longest device name accepted from a user or file.
pub const MAX_NAME: usize = 255;

/// A device name as the audio server reports it. Rejects control characters and empty strings
/// so a hand-edited file cannot smuggle anything into a server request.
pub fn valid_device_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= MAX_NAME && !n.chars().any(char::is_control)
}

/// The user's pick for calls. `None` = follow the system default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
}

impl Choice {
    fn sanitized(mut self) -> Self {
        self.output = self.output.filter(|n| valid_device_name(n));
        self.input = self.input.filter(|n| valid_device_name(n));
        self
    }
}

/// A card profile GeekBridge switched for a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRestore {
    pub card: String,
    pub previous: String,
    pub set_to: String,
}

/// A microphone GeekBridge muted or unmuted for a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteRestore {
    pub source: String,
    pub was_muted: bool,
    /// What GeekBridge last set; only undone while the source still shows it.
    pub set_to: bool,
}

/// The phone's `RejectSCO` flag GeekBridge changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectRestore {
    /// `AA:BB:CC:DD:EE:FF`; the file is private to the user like the rest of the state dir.
    pub address: String,
    pub previous: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restore {
    #[serde(default)]
    pub cards: Vec<CardRestore>,
    #[serde(default)]
    pub mutes: Vec<MuteRestore>,
    #[serde(default)]
    pub reject: Vec<RejectRestore>,
}

impl Restore {
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty() && self.mutes.is_empty() && self.reject.is_empty()
    }
}

/// Where the files live.
#[derive(Clone, Debug)]
pub struct Paths {
    pub choice: PathBuf,
    pub restore: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            choice: dir.join(CHOICE_FILE),
            restore: dir.join(RESTORE_FILE),
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// A missing or damaged file is the default pick.
pub fn load_choice(path: &Path) -> Choice {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Choice>(&b).ok())
        .map(Choice::sanitized)
        .unwrap_or_default()
}

pub fn save_choice(path: &Path, choice: &Choice) -> std::io::Result<()> {
    for n in [&choice.output, &choice.input].into_iter().flatten() {
        if !valid_device_name(n) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a device name",
            ));
        }
    }
    let json = serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?;
    write_atomic(path, &json)
}

pub fn load_restore(path: &Path) -> Restore {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write the pending undo list; an empty list removes the file.
pub fn save_restore(path: &Path, r: &Restore) -> std::io::Result<()> {
    if r.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let json = serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?;
    write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_round_trips_and_damage_means_default() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::in_dir(d.path());
        assert_eq!(load_choice(&p.choice), Choice::default());
        let c = Choice {
            output: Some("alsa_output.usb-x".into()),
            input: None,
        };
        save_choice(&p.choice, &c).unwrap();
        assert_eq!(load_choice(&p.choice), c);
        std::fs::write(&p.choice, b"{not json").unwrap();
        assert_eq!(load_choice(&p.choice), Choice::default());
        std::fs::write(&p.choice, br#"{"output":"a\nb","input":"ok"}"#).unwrap();
        assert_eq!(
            load_choice(&p.choice),
            Choice {
                output: None,
                input: Some("ok".into())
            }
        );
        assert!(
            save_choice(
                &p.choice,
                &Choice {
                    output: Some(String::new()),
                    input: None
                }
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_restore_list_removes_the_file() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::in_dir(d.path());
        let r = Restore {
            cards: vec![CardRestore {
                card: "bluez_card.X".into(),
                previous: "off".into(),
                set_to: "audio-gateway".into(),
            }],
            ..Restore::default()
        };
        save_restore(&p.restore, &r).unwrap();
        assert_eq!(load_restore(&p.restore), r);
        save_restore(&p.restore, &Restore::default()).unwrap();
        assert!(!p.restore.exists());
        // Removing twice is fine.
        save_restore(&p.restore, &Restore::default()).unwrap();
    }
}
