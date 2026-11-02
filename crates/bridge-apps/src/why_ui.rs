// SPDX-License-Identifier: GPL-3.0-or-later
//! Words for "why is my phone not connected": the service sends a stable reason code (plus an English
//! fallback); the catalogue turns the code into the user's language.
use crate::i18n::{tr, tr_args};
use bridge_core::why_disconnected::{ReasonCode, WhyDisconnected};

/// Catalogue keys for a code: (sentence, fix).
fn keys(code: ReasonCode) -> (&'static str, &'static str) {
    match code {
        ReasonCode::ComputerOffline => ("why.computer_offline", "why.computer_offline.fix"),
        ReasonCode::NotListening => ("why.not_listening", "why.not_listening.fix"),
        ReasonCode::PausedByUser => ("why.paused_by_user", "why.paused_by_user.fix"),
        ReasonCode::PairingLost => ("why.pairing_lost", "why.pairing_lost.fix"),
        ReasonCode::FirewallBlocking => ("why.firewall_blocking", "why.firewall_blocking.fix"),
        ReasonCode::Reconnecting => ("why.reconnecting", "why.reconnecting.fix"),
        ReasonCode::GaveUp => ("why.gave_up", "why.gave_up.fix"),
        ReasonCode::LinkTimedOut => ("why.link_timed_out", "why.link_timed_out.fix"),
        ReasonCode::DifferentNetwork => ("why.different_network", "why.different_network.fix"),
        ReasonCode::PhoneSilent => ("why.phone_silent", "why.phone_silent.fix"),
        ReasonCode::NotSeenYet => ("why.not_seen_yet", "why.not_seen_yet.fix"),
        ReasonCode::Unknown => ("why.unknown", "why.unknown.fix"),
    }
}

/// The sentence in the user's language.
pub fn sentence(w: &WhyDisconnected) -> String {
    tr(keys(w.code).0).to_owned()
}

/// The suggested fix in the user's language.
pub fn fix(w: &WhyDisconnected) -> String {
    tr(keys(w.code).1).to_owned()
}

/// "Try: ..." line for the details card.
pub fn try_line(w: &WhyDisconnected) -> String {
    tr_args("why.try", &[("fix", &fix(w))])
}

/// "Last heard 12 min ago" when the phone was heard in this run of the service.
pub fn last_heard(w: &WhyDisconnected) -> Option<String> {
    let s = w.last_seen_ago_s?;
    Some(tr_args(
        "why.last_heard",
        &[("age", &crate::phone_ui::age_label(s * 1000, 0))],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::why_disconnected::{Facts, explain};

    #[test]
    fn every_code_has_catalogue_text_in_english() {
        for code in ReasonCode::ALL {
            let (s, f) = keys(code);
            assert_eq!(tr(s), code.sentence(), "{code:?}");
            assert_eq!(tr(f), code.fix(), "{code:?}");
        }
    }

    #[test]
    fn last_heard_only_when_known() {
        let mut w = explain("p", &Facts::default());
        assert_eq!(last_heard(&w), None);
        w.last_seen_ago_s = Some(720);
        assert_eq!(last_heard(&w).as_deref(), Some("Last heard 12 min ago"));
        assert!(try_line(&w).starts_with("Try: "));
    }
}
