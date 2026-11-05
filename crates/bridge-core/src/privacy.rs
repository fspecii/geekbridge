// SPDX-License-Identifier: GPL-3.0-or-later
//! The display-privacy choices and how they are resolved. Pure: no I/O, so the service, the
//! `geekbridge-ctl privacy` command and the Settings page agree on names, defaults and the rules for
//! "forced" values.
//!
//! * `message_text`: text of a new message in the desktop banner (default off: the sender only).
//! * `caller_number`: the caller's number on the incoming-call card (default on; off shows the
//!   contact name only and "Unknown caller" otherwise).
//! * `phone_notification_text`: text of a mirrored phone notification (default off).
//! * `respect_dnd`: a ringing call does not pop the card or sound a banner while the desktop is in Do Not
//!   Disturb (default off; the ring is still recorded in Recents and shown by the tray).
//!
//! The older switches (the `sms-notify-content` marker file or `GEEKBRIDGE_SMS_NOTIFY_CONTENT=show`
//! for the two text choices, `GEEKBRIDGE_CALL_OVERLAY` or the `call-overlay` file for the caller
//! number) still win: a value they set is reported as forced and cannot be changed from the UI.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyKey {
    MessageText,
    CallerNumber,
    PhoneNotificationText,
    RespectDnd,
}

impl PrivacyKey {
    pub const ALL: [PrivacyKey; 4] = [
        PrivacyKey::MessageText,
        PrivacyKey::CallerNumber,
        PrivacyKey::PhoneNotificationText,
        PrivacyKey::RespectDnd,
    ];

    /// The name used on the command line, in the file and on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Self::MessageText => "message_text",
            Self::CallerNumber => "caller_number",
            Self::PhoneNotificationText => "phone_notification_text",
            Self::RespectDnd => "respect_dnd",
        }
    }

    /// Accepts `message_text`, `message-text` and any letter case.
    pub fn parse(word: &str) -> Option<Self> {
        let w = word.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|k| k.name() == w)
    }

    /// The value when nothing was chosen and nothing forces one.
    pub fn default_on(self) -> bool {
        matches!(self, Self::CallerNumber)
    }
}

/// `on`/`off` (and the usual synonyms) for the command line.
pub fn parse_switch(word: &str) -> Option<bool> {
    match word.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" | "show" => Some(true),
        "off" | "no" | "false" | "0" | "hide" => Some(false),
        _ => None,
    }
}

/// What forces a value, so the page and the command can say how to undo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForcedBy {
    /// An environment variable (`GEEKBRIDGE_SMS_NOTIFY_CONTENT`, `GEEKBRIDGE_CALL_OVERLAY`).
    Environment,
    /// A marker or settings file in the state folder (`sms-notify-content`, `call-overlay`).
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyItem {
    pub key: PrivacyKey,
    /// The value in effect right now.
    pub on: bool,
    /// Set when an older switch decides the value; `PrivacySet` is stored but has no effect.
    pub forced: Option<ForcedBy>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyView {
    pub items: Vec<PrivacyItem>,
}

impl PrivacyView {
    pub fn get(&self, key: PrivacyKey) -> Option<&PrivacyItem> {
        self.items.iter().find(|i| i.key == key)
    }
}

/// The value in effect: a forcing source first, then the stored choice, then the default.
pub fn resolve(
    key: PrivacyKey,
    forced: Option<(bool, ForcedBy)>,
    chosen: Option<bool>,
) -> PrivacyItem {
    PrivacyItem {
        key,
        on: forced.map(|f| f.0).or(chosen).unwrap_or(key.default_on()),
        forced: forced.map(|f| f.1),
    }
}

/// The stored choices as text: one `name=on|off` line per chosen key.
pub fn format_choices(choices: &[(PrivacyKey, bool)]) -> String {
    choices
        .iter()
        .map(|(k, on)| format!("{}={}\n", k.name(), if *on { "on" } else { "off" }))
        .collect()
}

/// Reads [`format_choices`] text; unknown keys and unreadable lines are skipped.
pub fn parse_choices(text: &str) -> Vec<(PrivacyKey, bool)> {
    let mut out: Vec<(PrivacyKey, bool)> = Vec::new();
    for line in text.lines().take(16) {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if let (Some(k), Some(v)) = (PrivacyKey::parse(k), parse_switch(v)) {
            out.retain(|(o, _)| *o != k);
            out.push((k, v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_defaults_are_the_quiet_ones() {
        for k in PrivacyKey::ALL {
            assert_eq!(PrivacyKey::parse(k.name()), Some(k));
            assert_eq!(
                PrivacyKey::parse(&k.name().replace('_', "-").to_uppercase()),
                Some(k)
            );
        }
        assert_eq!(PrivacyKey::parse("nope"), None);
        assert!(!PrivacyKey::MessageText.default_on());
        assert!(!PrivacyKey::PhoneNotificationText.default_on());
        assert!(
            PrivacyKey::CallerNumber.default_on(),
            "today's behaviour is kept"
        );
    }

    #[test]
    fn a_forcing_source_beats_the_stored_choice_which_beats_the_default() {
        let k = PrivacyKey::MessageText;
        assert!(!resolve(k, None, None).on);
        assert!(resolve(k, None, Some(true)).on);
        let forced = resolve(k, Some((true, ForcedBy::File)), Some(false));
        assert!(forced.on);
        assert_eq!(forced.forced, Some(ForcedBy::File));
        assert!(
            !resolve(
                PrivacyKey::CallerNumber,
                Some((false, ForcedBy::Environment)),
                Some(true)
            )
            .on
        );
    }

    #[test]
    fn choices_round_trip_and_junk_is_skipped() {
        let c = vec![
            (PrivacyKey::MessageText, true),
            (PrivacyKey::CallerNumber, false),
        ];
        assert_eq!(parse_choices(&format_choices(&c)), c);
        let junk = "x\nmessage_text=maybe\nbogus=on\nmessage_text=on\nmessage_text=off\n";
        assert_eq!(
            parse_choices(junk),
            vec![(PrivacyKey::MessageText, false)],
            "the last line wins"
        );
        assert_eq!(parse_switch("Show"), Some(true));
        assert_eq!(parse_switch("x"), None);
    }
}
