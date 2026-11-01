//! `geekbridge-ctl calls` and `geekbridge-ctl call ...`: the same call controls the Calls window
//! has, for scripts and the emulated end-to-end checks. Numbers are masked unless `--reveal`.
use bridge_core::calls::CallPhase;
use bridge_core::calls_view::{CallRow, ControlPath};
use bridge_core::ipc::{self, CallAction, CallsStatus, Request, Response, Token};

pub const USAGE: &str = "  calls [--reveal]                    ringing and live calls, the controls that really exist,
                                      the audio route and why a control is missing (numbers masked)
  call answer|reject|hangup           act on the ringing (answer, reject) or live (hangup) call
  call mute on|off | call audio       mute the computer microphone / move the audio here (only where offered)";

/// Keep the last two digits, like the redacted device reports.
pub fn mask(number: &str) -> String {
    let n = number.chars().count();
    if n <= 2 {
        return "*".repeat(n);
    }
    let tail: String = number.chars().skip(n - 2).collect();
    format!("{}{tail}", "*".repeat(n - 2))
}

fn path(p: Option<&bridge_core::calls_view::ActionTarget>) -> &'static str {
    match p.map(|t| t.path) {
        None => "-",
        Some(ControlPath::Hfp) => "bluetooth",
        Some(ControlPath::Link) => "phone-link",
    }
}

pub fn describe_row(r: &CallRow, reveal: bool) -> String {
    let show = |s: &bridge_core::calls::Sensitive| {
        if reveal {
            s.expose().to_owned()
        } else {
            mask(s.expose())
        }
    };
    let phase = match r.phase {
        CallPhase::Incoming | CallPhase::Waiting => "ringing",
        CallPhase::Dialing | CallPhase::Alerting => "dialing",
        CallPhase::Active => "active",
        CallPhase::Held => "held",
    };
    format!(
        "{phase} | number: {} | name: {} | answer: {} reject: {} hangup: {}{}",
        r.number.as_ref().map_or("-".to_owned(), show),
        r.name.as_ref().map_or("-", |_| "known"),
        path(r.answer.as_ref()),
        path(r.reject.as_ref()),
        path(r.hangup.as_ref()),
        r.missing_control
            .as_ref()
            .map_or(String::new(), |w| format!(" | {w}")),
    )
}

pub fn describe(s: &CallsStatus, reveal: bool) -> String {
    let mut out = String::new();
    if s.view.rows.is_empty() {
        out.push_str("no calls\n");
    }
    for r in &s.view.rows {
        out.push_str(&describe_row(r, reveal));
        out.push('\n');
    }
    out.push_str(&format!(
        "audio: {:?}{}\n",
        s.view.audio.route,
        s.view
            .audio
            .hint
            .as_ref()
            .map_or(String::new(), |h| format!(" ({h})"))
    ));
    if let Some(n) = &s.notice {
        out.push_str(&format!("notice: {n}\n"));
    }
    for line in &s.sources {
        out.push_str(&format!("source: {line}\n"));
    }
    out
}

fn status(port: u16, token: &Token) -> Result<CallsStatus, String> {
    match ipc::call(port, token, &Request::Calls).map_err(|e| e.to_string())? {
        Response::Calls(c) => Ok(*c),
        Response::Error(e) => Err(e.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

pub fn run(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    let reveal = args.iter().any(|a| a == "--reveal");
    let words: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    match words.as_slice() {
        ["calls"] => {
            print!("{}", describe(&status(port, token)?, reveal));
            Ok(())
        }
        ["call", what @ ..] => {
            let s = status(port, token)?;
            let (ring, live) = (s.view.ringing(), s.view.live());
            let request = match what {
                ["answer"] => Request::CallAction {
                    target: ring
                        .and_then(|r| r.answer.clone())
                        .ok_or("nothing can answer a call right now")?,
                    action: CallAction::Answer,
                },
                ["reject"] => Request::CallAction {
                    target: ring
                        .and_then(|r| r.reject.clone())
                        .ok_or("nothing can decline a call right now")?,
                    action: CallAction::Reject,
                },
                ["hangup"] => Request::CallAction {
                    target: live
                        .and_then(|r| r.hangup.clone())
                        .ok_or("nothing can end a call right now")?,
                    action: CallAction::Hangup,
                },
                ["mute", on] => Request::CallMute {
                    source: s
                        .view
                        .mute
                        .as_ref()
                        .ok_or("muting is not available")?
                        .source
                        .clone(),
                    muted: match *on {
                        "on" => true,
                        "off" => false,
                        _ => return Err("expected on or off".into()),
                    },
                },
                ["audio"] => Request::CallAudio {
                    source: s
                        .view
                        .audio
                        .connect
                        .clone()
                        .ok_or("moving the audio here is not available")?,
                },
                _ => return Err(format!("unknown call command\n\n{USAGE}")),
            };
            match ipc::call(port, token, &request).map_err(|e| e.to_string())? {
                Response::Ack => {
                    println!("ok");
                    Ok(())
                }
                Response::Error(e) => Err(e.message),
                _ => Err("unexpected reply from the service".into()),
            }
        }
        _ => Err(format!("unknown calls command\n\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_keep_only_the_last_two_digits() {
        assert_eq!(mask("+15550142"), "*******42");
        assert_eq!(mask("12"), "**");
        assert_eq!(mask(""), "");
    }
}
