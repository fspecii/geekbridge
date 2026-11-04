// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl callwifi`: EXPERIMENTAL call audio over Wi-Fi from the command line (off by default;
//! Bluetooth hands-free stays the supported path). The text is built here from the service's view so it is tested
//! without a service. Nothing here carries call content, only counts.
use bridge_core::call_wifi_view::{CallWifiAction, CallWifiView};
use bridge_core::ipc::{self, Request, Response, Token};
use std::fmt::Write as _;

pub const USAGE: &str =
    "usage: geekbridge-ctl callwifi [status | listen [DEVICE_ID] | stop | speak on|off] [--json]";

/// The state as plain lines.
pub fn describe(v: &CallWifiView) -> String {
    let mut out = String::from(
        "Wi-Fi call audio (EXPERIMENTAL, off by default; Bluetooth is the supported way)\n",
    );
    if !v.supported {
        let _ = writeln!(
            out,
            "  not available on this computer: {}",
            v.problem.as_deref().unwrap_or("unsupported system")
        );
        return out;
    }
    if let (Some(o), i) = (&v.output, &v.input) {
        let _ = writeln!(
            out,
            "  plays on: {o}{}",
            i.as_ref()
                .map(|i| format!("; microphone: {i}"))
                .unwrap_or_default()
        );
        let _ = writeln!(
            out,
            "  (the same headphones and microphone as Bluetooth calls: geekbridge-ctl call audio-devices)"
        );
    }
    if let Some(s) = &v.session {
        let _ = writeln!(
            out,
            "  {} with {}: {} frames, {} lost, {} ms buffered, {} s{}",
            s.state.to_uppercase(),
            s.name,
            s.frames,
            s.lost,
            s.depth_ms,
            s.seconds,
            if s.speaking {
                "; YOUR MICROPHONE IS ON"
            } else {
                ""
            }
        );
        let _ = writeln!(out, "  stop it: geekbridge-ctl callwifi stop");
    }
    if v.phones.is_empty() {
        let _ = writeln!(
            out,
            "  no connected phone offers it (its app needs the experimental feature)"
        );
    }
    for p in &v.phones {
        let state = if !p.allowed {
            format!(
                "off here (allow: geekbridge-ctl consent {} call_audio_wifi on)",
                p.device_id
            )
        } else if !p.ready {
            format!(
                "allowed; the phone's helper is not running (self-test: {})",
                p.test
            )
        } else if !p.listen {
            format!(
                "allowed; self-test says \"{}\": cannot be heard here",
                p.test
            )
        } else if p.speak {
            "ready to listen, and to speak".to_owned()
        } else {
            "ready to listen".to_owned()
        };
        let _ = writeln!(
            out,
            "  {} [{}]: {state}{}",
            p.name,
            p.device_id,
            if p.in_call { "; a call is live" } else { "" }
        );
    }
    out
}

/// Which phone `listen` means when none is named: the only one that can listen right now.
pub fn pick_phone(v: &CallWifiView, named: Option<&str>) -> Result<String, String> {
    if let Some(n) = named {
        return Ok(n.to_owned());
    }
    let ready: Vec<_> = v.phones.iter().filter(|p| p.listen && p.in_call).collect();
    match ready.as_slice() {
        [one] => Ok(one.device_id.clone()),
        [] => Err("no phone can be listened to right now (it needs: allowed here, helper ready, a live call); see geekbridge-ctl callwifi".into()),
        _ => Err("more than one phone can be listened to; name one: geekbridge-ctl callwifi listen DEVICE_ID".into()),
    }
}

fn ask(port: u16, token: &Token, action: CallWifiAction) -> Result<CallWifiView, String> {
    match ipc::call(port, token, &Request::CallWifi { action }).map_err(|e| e.to_string())? {
        Response::CallWifi(v) => Ok(*v),
        Response::Error(e) => Err(e.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

pub fn run(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let view = match words.as_slice() {
        [] | ["status"] => ask(port, token, CallWifiAction::Status)?,
        ["listen", rest @ ..] if rest.len() <= 1 => {
            let status = ask(port, token, CallWifiAction::Status)?;
            let id = pick_phone(&status, rest.first().copied())?;
            ask(port, token, CallWifiAction::Listen { device_id: id })?
        }
        ["stop"] => ask(port, token, CallWifiAction::Stop)?,
        ["speak", "on"] => ask(port, token, CallWifiAction::Speak { on: true })?,
        ["speak", "off"] => ask(port, token, CallWifiAction::Speak { on: false })?,
        _ => return Err(USAGE.into()),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?);
    } else {
        print!("{}", describe(&view));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_wifi_view::{CallWifiPhone, CallWifiSession};

    fn phone(id: &str) -> CallWifiPhone {
        CallWifiPhone {
            device_id: id.into(),
            name: format!("Phone {id}"),
            connected: true,
            test: "not_run".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_text_says_what_is_off_what_is_missing_and_how_to_stop() {
        let mut v = CallWifiView {
            supported: true,
            ..Default::default()
        };
        assert!(describe(&v).contains("no connected phone offers it"));
        v.phones.push(phone("a"));
        assert!(describe(&v).contains("consent a call_audio_wifi on"));
        v.phones[0].allowed = true;
        assert!(describe(&v).contains("helper is not running"));
        v.phones[0].ready = true;
        v.phones[0].test = "silent".into();
        assert!(describe(&v).contains("cannot be heard here"));
        v.phones[0].listen = true;
        v.phones[0].in_call = true;
        assert!(describe(&v).contains("ready to listen; a call is live"));
        v.session = Some(CallWifiSession {
            name: "Phone a".into(),
            state: "live".into(),
            speaking: true,
            frames: 10,
            ..Default::default()
        });
        let t = describe(&v);
        assert!(t.contains("LIVE with Phone a") && t.contains("YOUR MICROPHONE IS ON"));
        assert!(t.contains("callwifi stop"));
        v.supported = false;
        v.problem = Some("no pw-cat".into());
        assert!(describe(&v).contains("not available on this computer: no pw-cat"));
    }

    #[test]
    fn listen_without_a_name_needs_exactly_one_phone_that_can() {
        let mut v = CallWifiView {
            supported: true,
            ..Default::default()
        };
        assert!(pick_phone(&v, None).is_err());
        let mut a = phone("a");
        a.listen = true;
        a.in_call = true;
        v.phones.push(a.clone());
        assert_eq!(pick_phone(&v, None).unwrap(), "a");
        let mut b = a;
        b.device_id = "b".into();
        v.phones.push(b);
        assert!(pick_phone(&v, None).unwrap_err().contains("name one"));
        assert_eq!(pick_phone(&v, Some("b")).unwrap(), "b");
    }
}
