// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app identity: every app is its own executable, window, and desktop app ID.
use bridge_core::AppKind;

pub fn binary_name(app: AppKind) -> &'static str {
    match app {
        AppKind::Calls => "geekbridge-calls",
        AppKind::Messages => "geekbridge-messages",
        // The Calendar app; the old name stays so installs and launchers keep working.
        AppKind::Agenda => "geekbridge-agenda",
        AppKind::Contacts => "geekbridge-contacts",
        AppKind::Clipboard => "geekbridge-clipboard",
    }
}

/// Used as the Wayland app_id / X11 WM_CLASS so each window groups separately.
pub fn app_id(app: AppKind) -> &'static str {
    match app {
        AppKind::Calls => "dev.geekbridge.Calls",
        AppKind::Messages => "dev.geekbridge.Messages",
        AppKind::Agenda => "dev.geekbridge.Agenda",
        AppKind::Contacts => "dev.geekbridge.Contacts",
        AppKind::Clipboard => "dev.geekbridge.Clipboard",
    }
}

pub fn window_title(app: AppKind) -> String {
    format!("GeekBridge {}", app.name())
}

pub fn initial_size(app: AppKind) -> [f32; 2] {
    match app {
        AppKind::Calls => [380.0, 680.0],
        AppKind::Messages => [900.0, 600.0],
        AppKind::Agenda => [1080.0, 720.0],
        AppKind::Contacts => [820.0, 600.0],
        AppKind::Clipboard => [460.0, 580.0],
    }
}

/// Smallest window each layout is designed for; nothing is clipped at or above this.
pub fn min_size(app: AppKind) -> [f32; 2] {
    match app {
        AppKind::Calls => [320.0, 560.0],
        AppKind::Messages => [640.0, 460.0],
        AppKind::Agenda => [760.0, 520.0],
        AppKind::Contacts => [560.0, 460.0],
        AppKind::Clipboard => [360.0, 440.0],
    }
}

/// Plain-language name of the phone feature an app uses, for the optional details.
pub fn feature_label(app: AppKind) -> Option<&'static str> {
    match app {
        AppKind::Calls => None,
        AppKind::Messages => Some("messaging"),
        AppKind::Agenda => Some("calendar"),
        AppKind::Contacts => Some("contacts"),
        AppKind::Clipboard => Some("clipboard"),
    }
}

/// What already works in this app without a phone, shown in the connection details.
pub fn works_without_phone(app: AppKind) -> &'static str {
    match app {
        AppKind::Calls => "You can type a number and copy it. Calling needs a connected phone.",
        AppKind::Messages => "You can write a draft and copy it. Sending needs a connected phone.",
        AppKind::Agenda => "You can browse dates here. Events need a connected phone.",
        AppKind::Contacts => "Contacts need a connected phone.",
        AppKind::Clipboard => {
            "You can type or paste text and copy it. Sending needs a connected phone."
        }
    }
}

/// Discovery hints for LAN features. Calling is negotiated by a separate HFP backend.
pub fn capability_prefix(app: AppKind) -> Option<&'static str> {
    match app {
        AppKind::Calls => None,
        AppKind::Messages => Some("kdeconnect.sms"),
        AppKind::Agenda => Some("geekbridge.calendar"),
        AppKind::Contacts => Some("kdeconnect.contacts"),
        AppKind::Clipboard => Some("kdeconnect.clipboard"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALL: [AppKind; 5] = [
        AppKind::Calls,
        AppKind::Messages,
        AppKind::Agenda,
        AppKind::Contacts,
        AppKind::Clipboard,
    ];

    #[test]
    fn identities_are_distinct() {
        for (i, a) in ALL.iter().enumerate() {
            let [w, h] = initial_size(*a);
            let [min_w, min_h] = min_size(*a);
            assert!(
                w >= min_w && h >= min_h,
                "{} opens below its minimum",
                binary_name(*a)
            );
            for b in &ALL[i + 1..] {
                assert_ne!(binary_name(*a), binary_name(*b));
                assert_ne!(app_id(*a), app_id(*b));
                assert_ne!(window_title(*a), window_title(*b));
                assert_ne!(capability_prefix(*a), capability_prefix(*b));
            }
        }
    }
}
