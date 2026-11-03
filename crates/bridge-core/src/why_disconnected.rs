// SPDX-License-Identifier: GPL-3.0-or-later
//! "Why is my phone not connected?" in plain words.
//!
//! A pure mapping from facts the service already has (the redial book, discovery sightings, the last
//! link event, the listener, the computer's own network) to one reason code, one sentence and one
//! suggested fix. The service fills [`Facts`] and puts the result into `Status::disconnected`; the
//! desktop app, `geekbridge-ctl`, the tray and the waybar line only show it. The codes are stable
//! strings, so the desktop catalogue can translate `why.<code>` and `why.<code>.fix`; the English
//! `sentence` and `fix` here are the fallback for a code an older client does not know.
use serde::{Deserialize, Serialize};

/// Why a paired phone has no link. Most specific cause first; see [`classify`] for the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// This computer has no usable network.
    ComputerOffline,
    /// The service is not listening for phones at all.
    NotListening,
    /// The user disconnected this phone by hand.
    PausedByUser,
    /// The phone removed this computer from its paired list (or the pairing no longer matches).
    PairingLost,
    /// The firewall on this computer is blocking phones (set by `geekbridge-ctl` from `doctor`).
    FirewallBlocking,
    /// The computer is dialing the phone again right now.
    Reconnecting,
    /// The phone is announcing itself but every connection attempt failed.
    GaveUp,
    /// A link existed and stopped answering (heartbeat or socket timeout).
    LinkTimedOut,
    /// The phone was last seen on another network than this computer's.
    DifferentNetwork,
    /// The phone is not announcing itself (asleep, off Wi-Fi, app stopped).
    PhoneSilent,
    /// Nothing is known yet about this phone in this run of the service.
    NotSeenYet,
    /// A code from a newer service.
    #[serde(other)]
    Unknown,
}

impl ReasonCode {
    /// Stable name, the same as the serde name and the catalogue key suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ComputerOffline => "computer_offline",
            Self::NotListening => "not_listening",
            Self::PausedByUser => "paused_by_user",
            Self::PairingLost => "pairing_lost",
            Self::FirewallBlocking => "firewall_blocking",
            Self::Reconnecting => "reconnecting",
            Self::GaveUp => "gave_up",
            Self::LinkTimedOut => "link_timed_out",
            Self::DifferentNetwork => "different_network",
            Self::PhoneSilent => "phone_silent",
            Self::NotSeenYet => "not_seen_yet",
            Self::Unknown => "unknown",
        }
    }

    pub const ALL: [ReasonCode; 12] = [
        Self::ComputerOffline,
        Self::NotListening,
        Self::PausedByUser,
        Self::PairingLost,
        Self::FirewallBlocking,
        Self::Reconnecting,
        Self::GaveUp,
        Self::LinkTimedOut,
        Self::DifferentNetwork,
        Self::PhoneSilent,
        Self::NotSeenYet,
        Self::Unknown,
    ];

    /// One calm English sentence; no numbers, addresses or names.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::ComputerOffline => "This computer has no network connection.",
            Self::NotListening => "GeekBridge is not listening for phones on this computer.",
            Self::PausedByUser => "You disconnected this phone.",
            Self::PairingLost => "The phone no longer trusts this computer.",
            Self::FirewallBlocking => "This computer's firewall is blocking the phone.",
            Self::Reconnecting => "Trying to reach the phone again.",
            Self::GaveUp => "The phone is nearby but would not connect.",
            Self::LinkTimedOut => "The connection stopped answering.",
            Self::DifferentNetwork => "The phone was last seen on a different network.",
            Self::PhoneSilent => "The phone is not announcing itself.",
            Self::NotSeenYet => "This computer has not heard from the phone yet.",
            Self::Unknown => "The phone is not connected.",
        }
    }

    /// One suggested next step in English.
    pub fn fix(self) -> &'static str {
        match self {
            Self::ComputerOffline => "Connect this computer to Wi-Fi or Ethernet.",
            Self::NotListening => {
                "Quit KDE Connect or GSConnect if it is running, then restart GeekBridge."
            }
            Self::PausedByUser => "Open the GeekBridge app on the phone, or connect it from here.",
            Self::PairingLost => "Pair the phone with this computer again.",
            Self::FirewallBlocking => {
                "Allow TCP and UDP 1716-1764 on your LAN (run geekbridge-ctl firewall)."
            }
            Self::Reconnecting => "Keep the GeekBridge app open on the phone, on the same Wi-Fi.",
            Self::GaveUp => {
                "Check the firewall (geekbridge-ctl doctor), or open the app on the phone to retry."
            }
            Self::LinkTimedOut => {
                "Wake the phone and open the GeekBridge app; allow it to run in the background."
            }
            Self::DifferentNetwork => "Put the phone and this computer on the same Wi-Fi.",
            Self::PhoneSilent => {
                "Wake the phone, join the same Wi-Fi and open the GeekBridge app (Stay connected on)."
            }
            Self::NotSeenYet => "Open the GeekBridge app on the phone, on the same Wi-Fi.",
            Self::Unknown => {
                "Open the GeekBridge app on the phone, then run geekbridge-ctl doctor."
            }
        }
    }
}

/// The answer for one paired phone that has no link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhyDisconnected {
    pub device_id: String,
    pub code: ReasonCode,
    /// English sentence (fallback for the catalogue).
    pub sentence: String,
    /// English suggested fix (fallback for the catalogue).
    pub fix: String,
    /// Seconds since the phone was last heard announcing itself, when known.
    #[serde(default)]
    pub last_seen_ago_s: Option<u64>,
    /// Redial attempts made since the link dropped.
    #[serde(default)]
    pub tries: u32,
}

/// How the last link ended, from the service's event log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEnd {
    /// `LinkLost` with the service's short reason.
    Lost(String),
    /// `ConnectFailed` with the service's short reason.
    ConnectFailed(String),
    /// The phone unpaired us, or we unpaired it.
    Unpaired,
    /// The user disconnected it.
    Disconnected,
}

/// Everything [`classify`] looks at. `Default` is "nothing known", which maps to `NotSeenYet`.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// A usable, up network interface exists on this computer.
    pub computer_online: bool,
    /// The service has an inbound listener.
    pub listening: bool,
    /// The redial book is paused for this phone (user disconnected it).
    pub paused: bool,
    /// The phone announced itself within the last few seconds.
    pub announcing: bool,
    /// Redial attempts so far, and the cap after which the service gives up until the phone moves.
    pub tries: u32,
    pub max_tries: u32,
    /// Seconds since the phone was last heard, if ever in this run.
    pub last_seen_ago_s: Option<u64>,
    /// Did the last-seen address belong to one of this computer's networks? `None` when unknown.
    pub on_our_network: Option<bool>,
    pub last_end: Option<LinkEnd>,
}

fn looks_like_timeout(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    [
        "timed out",
        "timeout",
        "silent",
        "heartbeat",
        "reset",
        "broken pipe",
    ]
    .iter()
    .any(|k| r.contains(k))
}

fn looks_like_trust_failure(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    [
        "not paired",
        "unpaired",
        "untrusted",
        "not trusted",
        "certificate",
        "fingerprint",
    ]
    .iter()
    .any(|k| r.contains(k))
}

/// The one reason for a phone that has no link. Order matters: the most specific cause wins, and a
/// cause that is about this computer outranks one about the phone.
pub fn classify(f: &Facts) -> ReasonCode {
    if !f.computer_online {
        return ReasonCode::ComputerOffline;
    }
    if !f.listening {
        return ReasonCode::NotListening;
    }
    if f.paused {
        return ReasonCode::PausedByUser;
    }
    match &f.last_end {
        Some(LinkEnd::Unpaired) => return ReasonCode::PairingLost,
        Some(LinkEnd::ConnectFailed(r)) if looks_like_trust_failure(r) => {
            return ReasonCode::PairingLost;
        }
        _ => {}
    }
    if f.announcing {
        return if f.max_tries > 0 && f.tries >= f.max_tries {
            ReasonCode::GaveUp
        } else {
            ReasonCode::Reconnecting
        };
    }
    if f.on_our_network == Some(false) {
        return ReasonCode::DifferentNetwork;
    }
    match &f.last_end {
        Some(LinkEnd::Lost(r) | LinkEnd::ConnectFailed(r)) if looks_like_timeout(r) => {
            return ReasonCode::LinkTimedOut;
        }
        _ => {}
    }
    if f.last_seen_ago_s.is_some() || f.last_end.is_some() {
        ReasonCode::PhoneSilent
    } else {
        ReasonCode::NotSeenYet
    }
}

/// Build the full answer for `device_id`.
pub fn explain(device_id: &str, f: &Facts) -> WhyDisconnected {
    let code = classify(f);
    WhyDisconnected {
        device_id: device_id.to_owned(),
        code,
        sentence: code.sentence().to_owned(),
        fix: code.fix().to_owned(),
        last_seen_ago_s: f.last_seen_ago_s,
        tries: f.tries,
    }
}

/// A firewall that is known to block phones turns the two "the phone is out there but we cannot talk"
/// answers into the firewall answer. `geekbridge-ctl` knows the firewall; the service does not.
pub fn with_firewall_blocking(w: &mut WhyDisconnected, blocking: bool) {
    if blocking
        && matches!(
            w.code,
            ReasonCode::GaveUp
                | ReasonCode::Reconnecting
                | ReasonCode::PhoneSilent
                | ReasonCode::LinkTimedOut
        )
    {
        w.code = ReasonCode::FirewallBlocking;
        w.sentence = w.code.sentence().to_owned();
        w.fix = w.code.fix().to_owned();
    }
}

/// "12 min" style age for a plain line; `None` under a few seconds.
pub fn age_words(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// The sentence plus the last-seen age, for the command line, the tray and the bar.
pub fn one_line(w: &WhyDisconnected) -> String {
    match w.last_seen_ago_s {
        Some(s) if w.code == ReasonCode::PhoneSilent || w.code == ReasonCode::LinkTimedOut => {
            format!("{} Last heard {} ago.", w.sentence, age_words(s))
        }
        _ => w.sentence.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Facts {
        Facts {
            computer_online: true,
            listening: true,
            max_tries: 12,
            ..Facts::default()
        }
    }

    #[test]
    fn computer_offline_beats_everything() {
        let f = Facts {
            computer_online: false,
            announcing: true,
            paused: true,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::ComputerOffline);
    }

    #[test]
    fn no_listener() {
        let f = Facts {
            listening: false,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::NotListening);
    }

    #[test]
    fn disconnected_by_hand() {
        let f = Facts {
            paused: true,
            last_end: Some(LinkEnd::Disconnected),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PausedByUser);
    }

    #[test]
    fn pairing_lost_from_unpair_or_trust_failure() {
        let f = Facts {
            last_end: Some(LinkEnd::Unpaired),
            announcing: true,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PairingLost);
        let f = Facts {
            last_end: Some(LinkEnd::ConnectFailed(
                "peer certificate not trusted".into(),
            )),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PairingLost);
    }

    #[test]
    fn redialing_then_giving_up() {
        let f = Facts {
            announcing: true,
            tries: 3,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::Reconnecting);
        let f = Facts {
            announcing: true,
            tries: 12,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::GaveUp);
    }

    #[test]
    fn link_timed_out_when_silent_after_a_timeout() {
        let f = Facts {
            last_seen_ago_s: Some(300),
            last_end: Some(LinkEnd::Lost("connection timed out".into())),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::LinkTimedOut);
    }

    #[test]
    fn different_network_when_last_seen_elsewhere() {
        let f = Facts {
            last_seen_ago_s: Some(900),
            on_our_network: Some(false),
            last_end: Some(LinkEnd::Lost("connection timed out".into())),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::DifferentNetwork);
    }

    #[test]
    fn phone_silent_after_a_clean_close_and_not_seen_yet_with_no_history() {
        let f = Facts {
            last_seen_ago_s: Some(120),
            on_our_network: Some(true),
            last_end: Some(LinkEnd::Lost("peer closed the link".into())),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PhoneSilent);
        assert_eq!(classify(&base()), ReasonCode::NotSeenYet);
    }

    #[test]
    fn firewall_overrides_only_the_unreachable_answers() {
        let mut w = explain(
            "p",
            &Facts {
                announcing: true,
                tries: 12,
                ..base()
            },
        );
        with_firewall_blocking(&mut w, true);
        assert_eq!(w.code, ReasonCode::FirewallBlocking);
        assert!(w.fix.contains("firewall"));
        let mut w = explain(
            "p",
            &Facts {
                paused: true,
                ..base()
            },
        );
        with_firewall_blocking(&mut w, true);
        assert_eq!(w.code, ReasonCode::PausedByUser);
        let mut w = explain("p", &base());
        with_firewall_blocking(&mut w, false);
        assert_eq!(w.code, ReasonCode::NotSeenYet);
    }

    #[test]
    fn every_code_has_text_a_stable_name_and_round_trips() {
        for c in ReasonCode::ALL {
            assert!(!c.sentence().is_empty() && !c.fix().is_empty());
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
            assert_eq!(serde_json::from_str::<ReasonCode>(&json).unwrap(), c);
        }
        // A code from a newer service degrades to Unknown instead of failing the whole status.
        assert_eq!(
            serde_json::from_str::<ReasonCode>("\"battery_restricted\"").unwrap(),
            ReasonCode::Unknown
        );
    }

    #[test]
    fn one_line_adds_the_age_only_where_it_helps() {
        let w = explain(
            "p",
            &Facts {
                last_seen_ago_s: Some(720),
                ..base()
            },
        );
        assert_eq!(
            one_line(&w),
            "The phone is not announcing itself. Last heard 12 min ago."
        );
        let w = explain(
            "p",
            &Facts {
                paused: true,
                last_seen_ago_s: Some(5),
                ..base()
            },
        );
        assert_eq!(one_line(&w), "You disconnected this phone.");
        assert_eq!(age_words(7200), "2 h");
    }

    // ---- test-gaps-4: precedence, keyword and boundary branches

    fn lost(r: &str) -> Option<LinkEnd> {
        Some(LinkEnd::Lost(r.into()))
    }

    #[test]
    fn the_documented_precedence_holds_pairwise() {
        // computer > listener > user > unpaired > announcing > network > timeout > silent
        let all = Facts {
            computer_online: true,
            listening: true,
            paused: true,
            announcing: true,
            tries: 99,
            max_tries: 12,
            last_seen_ago_s: Some(1),
            on_our_network: Some(false),
            last_end: Some(LinkEnd::Unpaired),
        };
        assert_eq!(classify(&all), ReasonCode::PausedByUser);
        let f = Facts {
            paused: false,
            ..all.clone()
        };
        assert_eq!(
            classify(&f),
            ReasonCode::PairingLost,
            "unpaired beats announcing"
        );
        let f = Facts {
            last_end: lost("timed out"),
            ..f
        };
        assert_eq!(
            classify(&f),
            ReasonCode::GaveUp,
            "announcing beats the network and the timeout"
        );
        let f = Facts {
            announcing: false,
            ..f
        };
        assert_eq!(
            classify(&f),
            ReasonCode::DifferentNetwork,
            "network beats the timeout"
        );
        let f = Facts {
            on_our_network: Some(true),
            ..f
        };
        assert_eq!(classify(&f), ReasonCode::LinkTimedOut);
        let f = Facts {
            on_our_network: None,
            ..f
        };
        assert_eq!(
            classify(&f),
            ReasonCode::LinkTimedOut,
            "unknown network is not a different network"
        );
        let f = Facts {
            listening: false,
            ..f
        };
        assert_eq!(classify(&f), ReasonCode::NotListening);
        let f = Facts {
            computer_online: false,
            ..f
        };
        assert_eq!(classify(&f), ReasonCode::ComputerOffline);
    }

    #[test]
    fn a_zero_retry_cap_never_means_gave_up() {
        for tries in [0, 1, 50] {
            let f = Facts {
                announcing: true,
                tries,
                max_tries: 0,
                ..base()
            };
            assert_eq!(classify(&f), ReasonCode::Reconnecting, "tries {tries}");
        }
        let f = Facts {
            announcing: true,
            tries: 11,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::Reconnecting);
        let f = Facts {
            announcing: true,
            tries: 13,
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::GaveUp);
    }

    #[test]
    fn every_trust_failure_keyword_is_found_in_any_case_but_only_for_connect_failures() {
        for r in [
            "peer NOT PAIRED",
            "Unpaired by phone",
            "untrusted certificate",
            "not trusted",
            "bad Certificate",
            "Fingerprint mismatch",
        ] {
            let f = Facts {
                last_end: Some(LinkEnd::ConnectFailed(r.into())),
                ..base()
            };
            assert_eq!(classify(&f), ReasonCode::PairingLost, "{r}");
        }
        // A link that was lost mentioning a certificate is not evidence of a lost pairing.
        let f = Facts {
            last_end: lost("certificate expired"),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PhoneSilent);
        // An ordinary connect failure is not a trust failure.
        let f = Facts {
            last_end: Some(LinkEnd::ConnectFailed("connection refused".into())),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::PhoneSilent);
    }

    #[test]
    fn every_timeout_keyword_is_found_for_lost_links_and_failed_connects() {
        for r in [
            "timed out",
            "TIMEOUT",
            "peer went silent",
            "no heartbeat",
            "Connection reset by peer",
            "Broken pipe",
        ] {
            for end in [LinkEnd::Lost(r.into()), LinkEnd::ConnectFailed(r.into())] {
                let f = Facts {
                    last_end: Some(end.clone()),
                    ..base()
                };
                assert_eq!(classify(&f), ReasonCode::LinkTimedOut, "{end:?}");
            }
        }
        for end in [LinkEnd::Disconnected, LinkEnd::Lost(String::new())] {
            let f = Facts {
                last_end: Some(end),
                last_seen_ago_s: Some(5),
                ..base()
            };
            assert_eq!(classify(&f), ReasonCode::PhoneSilent);
        }
    }

    #[test]
    fn any_history_at_all_means_silent_and_none_means_not_seen_yet() {
        let f = Facts {
            last_end: Some(LinkEnd::Disconnected),
            ..base()
        };
        assert_eq!(
            classify(&f),
            ReasonCode::PhoneSilent,
            "an end without a sighting"
        );
        let f = Facts {
            last_seen_ago_s: Some(0),
            ..base()
        };
        assert_eq!(
            classify(&f),
            ReasonCode::PhoneSilent,
            "a sighting without an end"
        );
        let f = Facts {
            on_our_network: Some(true),
            ..base()
        };
        assert_eq!(classify(&f), ReasonCode::NotSeenYet);
        assert_eq!(
            classify(&Facts::default()),
            ReasonCode::ComputerOffline,
            "nothing known is not online"
        );
    }

    #[test]
    fn explain_copies_the_facts_and_the_texts_of_its_code() {
        let f = Facts {
            announcing: true,
            tries: 4,
            last_seen_ago_s: Some(9),
            ..base()
        };
        let w = explain("dev", &f);
        assert_eq!(
            (w.device_id.as_str(), w.tries, w.last_seen_ago_s),
            ("dev", 4, Some(9))
        );
        assert_eq!(w.code, ReasonCode::Reconnecting);
        assert_eq!(
            (w.sentence.as_str(), w.fix.as_str()),
            (w.code.sentence(), w.code.fix())
        );
        // Round trip, and an older status without the optional fields still parses.
        assert_eq!(
            serde_json::from_str::<WhyDisconnected>(&serde_json::to_string(&w).unwrap()).unwrap(),
            w
        );
        let old: WhyDisconnected =
            serde_json::from_str(r#"{"device_id":"d","code":"gave_up","sentence":"s","fix":"f"}"#)
                .unwrap();
        assert_eq!((old.last_seen_ago_s, old.tries), (None, 0));
        let future: WhyDisconnected = serde_json::from_str(
            r#"{"device_id":"d","code":"from_the_future","sentence":"s","fix":"f"}"#,
        )
        .unwrap();
        assert_eq!(future.code, ReasonCode::Unknown);
    }

    #[test]
    fn the_firewall_rewrites_exactly_four_codes() {
        for code in ReasonCode::ALL {
            let mut w = WhyDisconnected {
                device_id: "p".into(),
                code,
                sentence: "keep".into(),
                fix: "keep".into(),
                last_seen_ago_s: Some(1),
                tries: 2,
            };
            with_firewall_blocking(&mut w, false);
            assert_eq!(
                (w.code, w.sentence.as_str()),
                (code, "keep"),
                "{code:?} not blocking"
            );
            with_firewall_blocking(&mut w, true);
            let rewritten = matches!(
                code,
                ReasonCode::GaveUp
                    | ReasonCode::Reconnecting
                    | ReasonCode::PhoneSilent
                    | ReasonCode::LinkTimedOut
            );
            if rewritten {
                assert_eq!(w.code, ReasonCode::FirewallBlocking, "{code:?}");
                assert_eq!(w.sentence, ReasonCode::FirewallBlocking.sentence());
                assert_eq!(w.fix, ReasonCode::FirewallBlocking.fix());
            } else {
                assert_eq!((w.code, w.sentence.as_str()), (code, "keep"), "{code:?}");
            }
            assert_eq!((w.last_seen_ago_s, w.tries), (Some(1), 2), "facts survive");
        }
    }

    #[test]
    fn age_words_switch_unit_exactly_at_the_boundaries() {
        for (secs, text) in [
            (0, "0 s"),
            (59, "59 s"),
            (60, "1 min"),
            (119, "1 min"),
            (3599, "59 min"),
            (3600, "1 h"),
            (86_399, "23 h"),
            (86_400, "1 d"),
            (172_800, "2 d"),
            (u64::MAX, &format!("{} d", u64::MAX / 86_400)),
        ] {
            assert_eq!(age_words(secs), text, "{secs}");
        }
    }

    #[test]
    fn one_line_adds_the_age_for_silent_and_timed_out_only() {
        for code in ReasonCode::ALL {
            let w = WhyDisconnected {
                device_id: "p".into(),
                code,
                sentence: code.sentence().into(),
                fix: code.fix().into(),
                last_seen_ago_s: Some(3700),
                tries: 0,
            };
            let line = one_line(&w);
            if matches!(code, ReasonCode::PhoneSilent | ReasonCode::LinkTimedOut) {
                assert_eq!(line, format!("{} Last heard 1 h ago.", code.sentence()));
            } else {
                assert_eq!(line, code.sentence(), "{code:?}");
            }
            let unknown_age = WhyDisconnected {
                last_seen_ago_s: None,
                ..w
            };
            assert_eq!(one_line(&unknown_age), code.sentence());
        }
    }

    #[test]
    fn texts_are_calm_plain_and_free_of_private_detail() {
        for c in ReasonCode::ALL {
            for t in [c.sentence(), c.fix()] {
                assert!(t.ends_with('.'), "{t}");
                assert!(!t.contains('\n') && t.len() < 120, "{t}");
                assert!(
                    !t.chars().any(|ch| ch.is_ascii_digit()) || c == ReasonCode::FirewallBlocking,
                    "{t}"
                );
            }
        }
        let mut seen = std::collections::HashSet::new();
        assert!(
            ReasonCode::ALL.iter().all(|c| seen.insert(c.as_str())),
            "names are unique"
        );
        assert_eq!(ReasonCode::ALL.len(), 12);
    }
}
