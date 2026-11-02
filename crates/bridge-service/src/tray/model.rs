// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray's pure model: what the service knows ([`TrayInput`]), the one icon state it maps to,
//! and the menu ([`menu`]). No I/O and no platform types, so every backend (StatusNotifierItem,
//! Shell_NotifyIcon, NSStatusItem) draws the same thing and the mapping is unit-tested once.
//!
//! Privacy: a tray icon sits on screen all day, so nothing here carries a caller's name or number,
//! message text or clipboard content. Phone names are the user's own labels and are sanitised.
use crate::notify_text::plain;

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
    /// Battery of the first connected phone that reports one (its `battery` consent is on).
    pub battery: Option<bridge_core::ipc::BatteryStatus>,
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
            [] => "GeekBridge".to_owned(),
            [one] => plain(one, 40),
            [first, rest @ ..] => format!("{} +{}", plain(first, 32), rest.len()),
        }
    }

    /// Second menu line (and the tooltip body): one plain sentence about the most urgent thing.
    pub fn status_line(&self) -> String {
        let n = self.connected.len();
        if self.ringing {
            "Incoming call".to_owned()
        } else if self.transfers_live > 0 {
            match self.transfers_live {
                1 => "1 file transfer in progress".to_owned(),
                k => format!("{k} file transfers in progress"),
            }
        } else if n > 0 {
            let mut line = if self.paused {
                "Connected \u{b7} clipboard sync paused".to_owned()
            } else {
                "Connected".to_owned()
            };
            if let Some(b) = &self.battery {
                line.push_str(" \u{b7} ");
                line.push_str(&b.text());
            }
            line
        } else if self.paired == 0 {
            "No phone paired yet".to_owned()
        } else {
            "No phone connected".to_owned()
        }
    }

    /// Update notice, shown as its own menu line when there is one.
    pub fn update_line(&self) -> Option<String> {
        self.update
            .as_ref()
            .map(|v| format!("Update {} available", plain(v, 24)))
    }

    /// Tooltip title and body.
    pub fn tooltip(&self) -> (String, String) {
        (self.headline(), self.status_line())
    }

    /// The StatusNotifierItem `Status`: only a ringing call asks for attention. A disconnected
    /// icon stays `Active` (not `Passive`) because many bars hide passive items, and a hidden
    /// icon could not open the app.
    pub fn sni_status(&self) -> &'static str {
        if self.ringing {
            "NeedsAttention"
        } else {
            "Active"
        }
    }
}

/// What a click does. The runner turns each into a service request or a helper window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayAction {
    Open,
    PasteFromPhone,
    SendFile,
    Answer,
    Decline,
    TogglePause,
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
            "Answer",
            input.can_answer,
            input.ringing,
        ),
        MenuItem::action(
            6,
            TrayAction::Decline,
            "Decline",
            input.can_decline,
            input.ringing,
        ),
        MenuItem::separator(7),
        MenuItem::action(8, TrayAction::Open, "Open GeekBridge", true, true),
        MenuItem::action(
            9,
            TrayAction::PasteFromPhone,
            "Paste clipboard from phone",
            input.can_paste && !input.paused,
            true,
        ),
        MenuItem::action(
            10,
            TrayAction::SendFile,
            "Send file\u{2026}",
            connected,
            true,
        ),
        MenuItem::separator(11),
        MenuItem::action(
            12,
            TrayAction::TogglePause,
            if input.paused {
                "Resume sync"
            } else {
                "Pause sync"
            },
            input.paused || input.can_paste,
            true,
        ),
        MenuItem::action(
            13,
            TrayAction::CheckUpdates,
            "Check for updates",
            true,
            true,
        ),
        MenuItem::separator(14),
        MenuItem::action(15, TrayAction::Quit, "Quit", true, true),
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
        assert_eq!(a, (1..=15).collect::<Vec<_>>());
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
        assert_eq!(menu(&i)[11].label, "Pause sync");
        i.paused = true;
        assert_eq!(menu(&i)[11].label, "Resume sync");
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
}
