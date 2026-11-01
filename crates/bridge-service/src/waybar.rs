//! `geekbridge-ctl status --waybar`: one line of JSON for a waybar `custom` module (or any bar that
//! reads the same `text` / `tooltip` / `class` / `alt` shape).
//!
//! The line carries no phone numbers, message text or caller names: a bar sits on screen all day.
//! Phone names are shown (they are the user's own labels) after the same sanitising that
//! notifications get, because a waybar tooltip is Pango markup.
use crate::notify_text::plain;
use serde_json::json;

/// Nerd Font glyphs; waybar falls back to the text if the font lacks them.
const PHONE: &str = "\u{f10b}";
const MAIL: &str = "\u{f0e0}";
const RING: &str = "\u{f095}";
const UPDATE: &str = "\u{f019}";

/// What the bar shows, collected from the service. `None` for a number means "not known".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BarState {
    /// The service answered.
    pub service_up: bool,
    pub paired: usize,
    /// Names of phones with an authenticated link right now.
    pub connected: Vec<String>,
    /// Calls that are ringing (their callers are deliberately not carried here).
    pub ringing: usize,
    /// Unread SMS threads' message count across phones, when SMS is shared.
    pub unread_sms: Option<u32>,
    /// A newer release exists and was not skipped.
    pub update: Option<String>,
}

/// Highest-priority state, used for `alt` and as the first `class`.
fn primary(s: &BarState) -> &'static str {
    if !s.service_up {
        "offline"
    } else if s.ringing > 0 {
        "ringing"
    } else if s.connected.is_empty() {
        if s.paired == 0 {
            "unpaired"
        } else {
            "disconnected"
        }
    } else {
        "connected"
    }
}

pub fn render(s: &BarState) -> String {
    let state = primary(s);
    let mut text = String::new();
    let mut tip: Vec<String> = Vec::new();
    match state {
        "offline" => {
            text.push_str(PHONE);
            tip.push("GeekBridge service is not running".into());
        }
        "unpaired" => {
            text.push_str(PHONE);
            tip.push("No phone paired yet. Click to open GeekBridge.".into());
        }
        "disconnected" => {
            text.push_str(PHONE);
            tip.push("Phone not connected".into());
        }
        _ => {
            let first = s.connected.first().map_or(String::new(), |n| plain(n, 24));
            if state == "ringing" {
                text.push_str(&format!("{RING} Incoming call"));
                tip.push(if s.ringing > 1 {
                    format!("{} calls ringing", s.ringing)
                } else {
                    "Call ringing".into()
                });
            } else {
                text.push_str(&format!("{PHONE} {first}"));
            }
            tip.extend(
                s.connected
                    .iter()
                    .map(|n| format!("Connected: {}", plain(n, 40))),
            );
        }
    }
    let mut classes = vec![state];
    if let Some(n) = s.unread_sms.filter(|n| *n > 0) {
        text.push_str(&format!(" {MAIL} {n}"));
        tip.push(format!(
            "{n} unread message{}",
            if n == 1 { "" } else { "s" }
        ));
        classes.push("unread");
    }
    if let Some(v) = &s.update {
        text.push_str(&format!(" {UPDATE}"));
        tip.push(format!("Update available: {}", plain(v, 24)));
        classes.push("update");
    }
    json!({
        "text": text.trim(),
        "tooltip": tip.join("\n"),
        "class": classes,
        "alt": state,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn offline_and_unpaired() {
        let v = parse(&render(&BarState::default()));
        assert_eq!(v["alt"], "offline");
        assert_eq!(v["class"][0], "offline");
        let v = parse(&render(&BarState {
            service_up: true,
            ..Default::default()
        }));
        assert_eq!(v["alt"], "unpaired");
        let v = parse(&render(&BarState {
            service_up: true,
            paired: 1,
            ..Default::default()
        }));
        assert_eq!(v["alt"], "disconnected");
    }

    #[test]
    fn connected_with_unread_and_update() {
        let v = parse(&render(&BarState {
            service_up: true,
            paired: 1,
            connected: vec!["Pixel <b>9</b>".into()],
            unread_sms: Some(3),
            update: Some("0.2.0".into()),
            ..Default::default()
        }));
        assert_eq!(v["alt"], "connected");
        assert_eq!(
            v["class"],
            serde_json::json!(["connected", "unread", "update"])
        );
        let text = v["text"].as_str().unwrap();
        assert!(text.contains("Pixel") && text.contains(" 3") && !text.contains('<'));
        let tip = v["tooltip"].as_str().unwrap();
        assert!(tip.contains("3 unread messages") && tip.contains("Update available: 0.2.0"));
        assert!(!tip.contains("<b>"));
    }

    #[test]
    fn ringing_wins_and_hides_the_caller() {
        let v = parse(&render(&BarState {
            service_up: true,
            paired: 1,
            connected: vec!["Pixel".into()],
            ringing: 1,
            unread_sms: Some(0),
            ..Default::default()
        }));
        assert_eq!(v["alt"], "ringing");
        assert!(v["text"].as_str().unwrap().contains("Incoming call"));
        assert!(!v["class"].as_array().unwrap().iter().any(|c| c == "unread"));
    }
}
