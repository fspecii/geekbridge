// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray's pure model: what the service knows ([`TrayInput`]), the one icon state it maps to,
//! and the menu ([`menu`]). No I/O and no platform types, so every backend (StatusNotifierItem,
//! Shell_NotifyIcon, NSStatusItem) draws the same thing and the mapping is unit-tested once.
//!
//! Privacy: a tray icon sits on screen all day, so nothing here carries a caller's name or number,
//! message text or clipboard content. Phone names are the user's own labels and are sanitised.
use crate::notify_text::plain;
use bridge_i18n::{tr, tr_args, tr_n};

/// The five states the icon can show, most urgent first in [`TrayInput::icon_state`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconState {
    Disconnected,
    Connected,
    Ringing,
    UpdateAvailable,
    Transfer,
}

impl IconState {
    pub const ALL: [IconState; 5] = [
        IconState::Disconnected,
        IconState::Connected,
        IconState::Ringing,
        IconState::UpdateAvailable,
        IconState::Transfer,
    ];

    /// Short, stable file-name fragment (`assets/tray/icon-<slug>-<size>.png`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connected => "connected",
            Self::Ringing => "ringing",
            Self::UpdateAvailable => "update",
            Self::Transfer => "transfer",
        }
    }
}

/// What the tooltip says about hearing calls through this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallAudioHint {
    /// Not known or not applicable on this system: no line.
    #[default]
    Unknown,
    /// This computer has no Bluetooth adapter.
    NoBluetooth,
    /// An adapter exists and a phone is set up for call audio.
    Ready,
}

/// What the service reports, reduced to what the tray shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayInput {
    /// Names of phones with an authenticated link right now.
    pub connected: Vec<String>,
    /// Phones paired at all.
    pub paired: usize,
    /// A call is ringing and can be answered or declined from here.
    pub ringing: bool,
    pub can_answer: bool,
    pub can_decline: bool,
    /// Transfers that are waiting for an answer or moving bytes.
    pub transfers_live: usize,
    /// Version of a newer release the user has not skipped.
    pub update: Option<String>,
    /// Clipboard sync was paused from this menu (the consents are restored by "Resume sync").
    pub paused: bool,
    /// A connected phone has the clipboard feature on, so "paste from phone" can work.
    pub can_paste: bool,
    /// Why the first paired phone without a link is not connected.
    pub why: Option<bridge_core::why_disconnected::WhyDisconnected>,
    /// Battery of the first connected phone that reports one (its `battery` consent is on).
    pub battery: Option<bridge_core::ipc::BatteryStatus>,
    /// A connected phone offers notification mirroring (declared the stock packet), so the toggle can work.
    pub mirror_available: bool,
    /// Mirroring is on for at least one connected phone.
    pub mirror_on: bool,
    /// A connected phone offers recent photos (the Share window's Photos page exists).
    pub photos_available: bool,
    /// One extra tooltip line about call audio.
    pub call_audio: CallAudioHint,
}

impl TrayInput {
    pub fn icon_state(&self) -> IconState {
        if self.ringing {
            IconState::Ringing
        } else if self.transfers_live > 0 {
            IconState::Transfer
        } else if self.update.is_some() {
            IconState::UpdateAvailable
        } else if !self.connected.is_empty() {
            IconState::Connected
        } else {
            IconState::Disconnected
        }
    }

    /// First menu line: the phone, or the product name when there is none.
    pub fn headline(&self) -> String {
        match self.connected.as_slice() {
            [] => "GeekBridge".to_owned(), // i18n-ok: product name
            [one] => plain(one, 40),
            [first, rest @ ..] => tr_args(
                "tray.more_phones",
                &[("first", &plain(first, 32)), ("rest", &rest.len())],
            ),
        }
    }

    /// Second menu line (and the tooltip body): one plain sentence about the most urgent thing.
    pub fn status_line(&self) -> String {
        let n = self.connected.len();
        if self.ringing {
            tr("tray.incoming_call").to_owned()
        } else if self.transfers_live > 0 {
            tr_n("tray.transfers", self.transfers_live as u64, &[])
        } else if n > 0 {
            let line = tr(if self.paused {
                "tray.connected_paused"
            } else {
                "tray.connected"
            });
            match &self.battery {
                Some(b) => tr_args(
                    "tray.with_battery",
                    &[("status", &line), ("battery", &b.text())],
                ),
                None => line.to_owned(),
            }
        } else if self.paired == 0 {
            tr("tray.no_phone_paired").to_owned()
        } else if let Some(w) = &self.why {
            bridge_core::why_disconnected::one_line(w)
        } else {
            tr("tray.no_phone_connected").to_owned()
        }
    }

    /// Update notice, shown as its own menu line when there is one.
    pub fn update_line(&self) -> Option<String> {
        self.update
            .as_ref()
            .map(|v| tr_args("tray.update_available", &[("version", &plain(v, 24))]))
    }

    /// Tooltip title and body.
    pub fn tooltip(&self) -> (String, String) {
        let mut body = self.status_line();
        if self.connected.is_empty()
            && let Some(w) = &self.why
        {
            body.push('\n');
            body.push_str(&tr_args("tray.try", &[("fix", &w.fix)]));
        }
        let call_audio = match self.call_audio {
            CallAudioHint::Unknown => None,
            CallAudioHint::NoBluetooth => Some(tr("tray.call_audio_none")),
            CallAudioHint::Ready => Some(tr("tray.call_audio_ready")),
        };
        if let Some(line) = call_audio {
            body.push('\n');
            body.push_str(line);
        }
        (self.headline(), body)
    }

    /// The StatusNotifierItem `Status`: only a ringing call asks for attention. A disconnected
    /// icon stays `Active` (not `Passive`) because many bars hide passive items, and a hidden
    /// icon could not open the app.
    pub fn sni_status(&self) -> &'static str {
        if self.ringing {
            "NeedsAttention" // i18n-ok: SNI protocol value
        } else {
            "Active" // i18n-ok: SNI protocol value
        }
    }
}

/// A tooltip as the fixed 128-unit UTF-16 buffer of a Windows notification icon (`szTip`): cut at
/// a whole character, always ending in a zero. Platform-independent so it is tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn tip_units(text: &str) -> [u16; 128] {
    let mut out = [0_u16; 128];
    let mut at = 0;
    for c in text.chars() {
        let mut buf = [0_u16; 2];
        let units = c.encode_utf16(&mut buf);
        if at + units.len() > 127 {
            break;
        }
        out[at..at + units.len()].copy_from_slice(units);
        at += units.len();
    }
    out
}

/// What a click does. The runner turns each into a service request or a helper window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayAction {
    Open,
    PasteFromPhone,
    SendFile,
    Photos,
    Answer,
    Decline,
    TogglePause,
    ToggleMirror,
    CheckUpdates,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Label,
    Separator,
    Action(TrayAction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Stable for the life of the process: dbusmenu clients cache by id.
    pub id: i32,
    pub kind: ItemKind,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
}

impl MenuItem {
    fn label(id: i32, label: String, visible: bool) -> Self {
        Self {
            id,
            kind: ItemKind::Label,
            label,
            enabled: false,
            visible,
        }
    }
    fn separator(id: i32) -> Self {
        Self {
            id,
            kind: ItemKind::Separator,
            label: String::new(),
            enabled: false,
            visible: true,
        }
    }
    fn action(id: i32, action: TrayAction, label: &str, enabled: bool, visible: bool) -> Self {
        Self {
            id,
            kind: ItemKind::Action(action),
            label: label.to_owned(),
            enabled,
            visible,
        }
    }

    pub fn action_of(&self) -> Option<TrayAction> {
        match self.kind {
            ItemKind::Action(a) if self.enabled && self.visible => Some(a),
            _ => None,
        }
    }
}

/// The whole menu. The ids and the order never change; items that do not apply right now are
/// hidden or disabled rather than removed, so a menu that is open does not jump.
pub fn menu(input: &TrayInput) -> Vec<MenuItem> {
    let connected = !input.connected.is_empty();
    let update_line = input.update_line();
    vec![
        MenuItem::label(1, input.headline(), true),
        MenuItem::label(2, input.status_line(), true),
        MenuItem::label(
            3,
            update_line.clone().unwrap_or_default(),
            update_line.is_some(),
        ),
        MenuItem::separator(4),
        MenuItem::action(
            5,
            TrayAction::Answer,
            tr("tray.answer"),
            input.can_answer,
            input.ringing,
        ),
        MenuItem::action(
            6,
            TrayAction::Decline,
            tr("tray.decline"),
            input.can_decline,
            input.ringing,
        ),
        MenuItem::separator(7),
        MenuItem::action(8, TrayAction::Open, tr("tray.open"), true, true),
        MenuItem::action(
            9,
            TrayAction::PasteFromPhone,
            tr("tray.paste"),
            input.can_paste && !input.paused,
            true,
        ),
        MenuItem::action(
            10,
            TrayAction::SendFile,
            tr("tray.send_file"),
            connected,
            true,
        ),
        MenuItem::action(
            17,
            TrayAction::Photos,
            tr("tray.photos"),
            input.photos_available,
            input.photos_available,
        ),
        MenuItem::separator(11),
        MenuItem::action(
            12,
            TrayAction::TogglePause,
            if input.paused {
                tr("tray.resume_sync")
            } else {
                tr("tray.pause_sync")
            },
            input.paused || input.can_paste,
            true,
        ),
        MenuItem::action(
            16,
            TrayAction::ToggleMirror,
            if input.mirror_on {
                tr("tray.mirror_stop")
            } else {
                tr("tray.mirror_start")
            },
            input.mirror_on || input.mirror_available,
            input.mirror_on || input.mirror_available,
        ),
        MenuItem::action(
            13,
            TrayAction::CheckUpdates,
            tr("tray.check_updates"),
            true,
            true,
        ),
        MenuItem::separator(14),
        MenuItem::action(15, TrayAction::Quit, tr("tray.quit"), true, true),
    ]
}

/// The action behind a menu id, if that item can be clicked right now.
pub fn action_for(input: &TrayInput, id: i32) -> Option<TrayAction> {
    menu(input)
        .into_iter()
        .find(|i| i.id == id)
        .and_then(|i| i.action_of())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone() -> TrayInput {
        TrayInput {
            connected: vec!["Pixel 8".into()],
            paired: 1,
            can_paste: true,
            ..TrayInput::default()
        }
    }

    #[test]
    fn the_battery_shows_in_the_status_line_only_when_connected_and_known() {
        let mut i = phone();
        assert_eq!(i.status_line(), "Connected");
        i.battery = Some(bridge_core::ipc::BatteryStatus {
            percent: 80,
            charging: true,
            low: false,
        });
        assert_eq!(i.status_line(), "Connected \u{b7} Battery 80% (charging)");
        assert_eq!(i.tooltip().1, i.status_line());
        i.connected.clear();
        assert_eq!(i.status_line(), "No phone connected");
    }

    #[test]
    fn every_call_audio_line_fits_the_windows_tooltip_buffer_and_the_mac_tooltip_is_plain_text() {
        for hint in [
            CallAudioHint::Unknown,
            CallAudioHint::NoBluetooth,
            CallAudioHint::Ready,
        ] {
            for name in [
                "Pixel",
                "A very long phone name \u{1F4F1}\u{1F4F1}\u{1F4F1} that goes on and on and on",
            ] {
                let mut i = phone();
                i.connected = vec![name.to_owned()];
                i.call_audio = hint;
                let (title, body) = i.tooltip();
                let units = tip_units(&format!("GeekBridge \u{2013} {title}: {body}"));
                assert_eq!(units[127], 0, "always zero-terminated");
                let end = units.iter().position(|u| *u == 0).unwrap();
                let back = String::from_utf16(&units[..end]).expect("never half a surrogate pair");
                assert!(back.starts_with("GeekBridge"));
                // macOS sets the tooltip from the same two strings, as one line break separated text.
                let mac = format!("{title}: {body}");
                assert!(!mac.contains('\0'));
            }
        }
    }

    #[test]
    fn a_cut_never_splits_a_surrogate_pair() {
        let text = format!("{}\u{1F4F1}tail", "a".repeat(126));
        let units = tip_units(&text);
        assert_eq!(String::from_utf16(&units[..126]).unwrap(), "a".repeat(126));
        assert_eq!((units[126], units[127]), (0, 0));
    }

    #[test]
    fn the_tooltip_says_whether_calls_can_carry_voice() {
        let mut i = phone();
        assert_eq!(i.tooltip().1, i.status_line(), "no line when unknown");
        i.call_audio = CallAudioHint::NoBluetooth;
        assert_eq!(
            i.tooltip().1,
            format!("{}\nNo Bluetooth: calls can't carry voice", i.status_line())
        );
        i.call_audio = CallAudioHint::Ready;
        assert!(i.tooltip().1.ends_with("\nCall audio ready"));
        // The menu line stays about the phone.
        assert!(!i.status_line().contains("Call audio"));
    }

    #[test]
    fn icon_state_priority_is_ringing_transfer_update_connected_disconnected() {
        let mut i = TrayInput::default();
        assert_eq!(i.icon_state(), IconState::Disconnected);
        i = phone();
        assert_eq!(i.icon_state(), IconState::Connected);
        i.update = Some("0.2.0".into());
        assert_eq!(i.icon_state(), IconState::UpdateAvailable);
        i.transfers_live = 2;
        assert_eq!(i.icon_state(), IconState::Transfer);
        i.ringing = true;
        assert_eq!(i.icon_state(), IconState::Ringing);
        // An update with no phone still shows, the tooltip says there is no phone.
        let lone = TrayInput {
            update: Some("0.2.0".into()),
            ..TrayInput::default()
        };
        assert_eq!(lone.icon_state(), IconState::UpdateAvailable);
        assert_eq!(lone.status_line(), "No phone paired yet");
    }

    #[test]
    fn a_disconnected_phone_shows_why_in_the_line_and_how_to_fix_it_in_the_tooltip() {
        use bridge_core::why_disconnected::{Facts, explain};
        let i = TrayInput {
            paired: 1,
            why: Some(explain(
                "p",
                &Facts {
                    computer_online: false,
                    ..Facts::default()
                },
            )),
            ..TrayInput::default()
        };
        assert_eq!(i.status_line(), "This computer has no network connection.");
        let (_, body) = i.tooltip();
        assert!(body.contains("\nTry: Connect this computer"), "{body}");
        // Without a reason the old calm line stays.
        let plain = TrayInput {
            paired: 1,
            ..TrayInput::default()
        };
        assert_eq!(plain.status_line(), "No phone connected");
    }

    #[test]
    fn status_text_is_plain_and_has_no_caller() {
        let mut i = phone();
        assert_eq!(
            (i.headline(), i.status_line()),
            ("Pixel 8".into(), "Connected".into())
        );
        i.connected.push("Tablet".into());
        assert_eq!(i.headline(), "Pixel 8 +1");
        i.paused = true;
        assert!(i.status_line().contains("paused"));
        i.ringing = true;
        assert_eq!(i.status_line(), "Incoming call");
        let none = TrayInput {
            paired: 2,
            ..TrayInput::default()
        };
        assert_eq!(
            (none.headline(), none.status_line()),
            ("GeekBridge".into(), "No phone connected".into())
        );
    }

    #[test]
    fn phone_names_are_sanitised() {
        let i = TrayInput {
            connected: vec!["Ev\u{202e}il\n<b>x</b>".into()],
            ..TrayInput::default()
        };
        let h = i.headline();
        assert!(!h.contains(['\n', '\u{202e}']), "{h:?}");
    }

    #[test]
    fn sni_status_only_asks_for_attention_when_ringing() {
        let mut i = TrayInput::default();
        assert_eq!(i.sni_status(), "Active");
        i.ringing = true;
        assert_eq!(i.sni_status(), "NeedsAttention");
    }

    #[test]
    fn menu_ids_and_order_never_change() {
        let ids = |i: &TrayInput| menu(i).iter().map(|m| m.id).collect::<Vec<_>>();
        let a = ids(&TrayInput::default());
        let b = ids(&TrayInput {
            ringing: true,
            can_answer: true,
            ..phone()
        });
        assert_eq!(a, b);
        let mut expected: Vec<i32> = (1..=12).collect();
        expected.extend([16, 13, 14, 15]);
        expected.insert(10, 17);
        assert_eq!(a, expected);
    }

    #[test]
    fn the_photos_entry_follows_what_the_phone_offers_and_labels_stay_short() {
        let item = |i: &TrayInput| menu(i).into_iter().find(|m| m.id == 17).unwrap();
        assert!(!item(&phone()).visible && action_for(&phone(), 17).is_none());
        let on = TrayInput {
            photos_available: true,
            ..phone()
        };
        assert!(item(&on).visible && item(&on).enabled);
        assert_eq!(action_for(&on, 17), Some(TrayAction::Photos));
        // Longest text of every line a shell may draw in a menu: fixed labels, and the worst-case phone-named lines.
        let worst = TrayInput {
            connected: vec!["P".repeat(100), "Q".into(), "R".into()],
            paired: 3,
            paused: true,
            update: Some("9".repeat(100)),
            battery: Some(bridge_core::ipc::BatteryStatus {
                percent: 100,
                charging: true,
                low: false,
            }),
            photos_available: true,
            mirror_available: true,
            ..TrayInput::default()
        };
        for m in menu(&worst) {
            assert!(m.label.chars().count() <= 64, "{:?}", m.label);
        }
    }

    #[test]
    fn menu_enables_what_can_work() {
        let idle = TrayInput::default();
        assert_eq!(action_for(&idle, 8), Some(TrayAction::Open));
        assert_eq!(action_for(&idle, 9), None, "nothing to paste from");
        assert_eq!(action_for(&idle, 10), None, "nobody to send to");
        assert_eq!(action_for(&idle, 5), None, "no call");
        assert_eq!(action_for(&idle, 12), None, "nothing to pause");
        assert_eq!(action_for(&idle, 13), Some(TrayAction::CheckUpdates));
        assert_eq!(action_for(&idle, 15), Some(TrayAction::Quit));
        let on = phone();
        assert_eq!(action_for(&on, 9), Some(TrayAction::PasteFromPhone));
        assert_eq!(action_for(&on, 10), Some(TrayAction::SendFile));
        assert_eq!(action_for(&on, 12), Some(TrayAction::TogglePause));
        // Labels and headers are never actions.
        assert!(
            menu(&on)
                .iter()
                .filter(|m| m.kind == ItemKind::Label)
                .all(|m| m.action_of().is_none())
        );
    }

    #[test]
    fn answer_and_decline_appear_only_while_ringing_and_follow_the_offered_controls() {
        let mut i = phone();
        let shown = |i: &TrayInput| {
            menu(i)
                .iter()
                .filter(|m| {
                    matches!(
                        m.kind,
                        ItemKind::Action(TrayAction::Answer | TrayAction::Decline)
                    )
                })
                .map(|m| (m.visible, m.enabled))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&i), vec![(false, false), (false, false)]);
        i.ringing = true;
        i.can_decline = true; // the backend offers decline only
        assert_eq!(shown(&i), vec![(true, false), (true, true)]);
        assert_eq!(action_for(&i, 5), None);
        assert_eq!(action_for(&i, 6), Some(TrayAction::Decline));
    }

    #[test]
    fn pause_toggles_label_and_blocks_paste() {
        let mut i = phone();
        assert_eq!(menu(&i)[12].label, "Pause sync");
        i.paused = true;
        assert_eq!(menu(&i)[12].label, "Resume sync");
        assert_eq!(action_for(&i, 9), None);
        // Resume must stay possible even though nothing is enabled while paused.
        i.can_paste = false;
        assert_eq!(action_for(&i, 12), Some(TrayAction::TogglePause));
    }

    #[test]
    fn update_line_is_a_hidden_item_until_there_is_one() {
        let mut i = phone();
        assert!(!menu(&i)[2].visible);
        i.update = Some("1.4.0".into());
        let m = &menu(&i)[2];
        assert!(m.visible && m.label == "Update 1.4.0 available" && !m.enabled);
    }

    #[test]
    fn the_mirror_toggle_follows_what_the_phone_offers_and_never_turns_itself_on() {
        let mut i = phone();
        assert_eq!(menu(&i)[13].label, "Show phone notifications");
        assert_eq!(action_for(&i, 16), None, "the phone did not offer it");
        i.mirror_available = true;
        assert_eq!(action_for(&i, 16), Some(TrayAction::ToggleMirror));
        i.mirror_on = true;
        i.mirror_available = false; // it can still be turned off
        assert_eq!(menu(&i)[13].label, "Stop showing phone notifications");
        assert_eq!(action_for(&i, 16), Some(TrayAction::ToggleMirror));
        let ids: Vec<i32> = menu(&i).iter().map(|m| m.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }
}
