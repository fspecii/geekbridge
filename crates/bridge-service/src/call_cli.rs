// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl calls` and `geekbridge-ctl call ...`: the same call controls the Calls window
//! has, for scripts and the emulated end-to-end checks. Numbers are masked unless `--reveal`.
use bridge_core::calls::{CallPhase, Sensitive};
use bridge_core::calls_view::{CallRow, ControlPath};
use bridge_core::ipc::{self, CallAction, CallsStatus, Request, Response, Token};
use std::io::{BufRead, IsTerminal, Write};

pub const USAGE: &str = "  calls [--reveal]                    ringing and live calls, the controls that really exist,
                                      the audio route and why a control is missing (numbers masked)
  call answer|reject|hangup           act on the ringing (answer, reject) or live (hangup) call
  call dial NUMBER [--yes]            place a call from the phone (digits, optional leading +); asks to
                                      confirm unless --yes. Needs Telephony consent and the phone's permission
  call mute on|off                    mute or unmute the microphone the call uses on this computer
  call audio [computer] | audio phone move the live call's voice to this computer / back to the phone
  call setup [--phone NAME]           set up call audio: power Bluetooth on, pair this computer with the phone
                                      as its hands-free device (compare the six-digit code), trust, connect
  call setup --fix-config             fix the WirePlumber settings call audio needs (user file, no sudo)
  call setup --confirm yes|no | --cancel   answer the pairing code from a script / stop the setup
  call audio-devices [--output NAME] [--input NAME]
                                      list speakers/headphones and microphones for calls; with a flag,
                                      pick one (NAME from the list, or default = follow the system)";

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
    if let Some(m) = &s.view.mute {
        out.push_str(&format!(
            "microphone: {}\n",
            match m.muted {
                Some(true) => "muted",
                Some(false) => "on",
                None => "unknown",
            }
        ));
    }
    if let Some(n) = &s.audio_note {
        out.push_str(&format!("audio note: {n}\n"));
    }
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

/// Why `call dial` has nothing to dial with, from the service's own words.
pub fn dial_unavailable(s: &CallsStatus) -> String {
    let mut out = String::from("dialing is not available right now; nothing was dialed.");
    if !s.view.rows.is_empty() {
        out.push_str(" A call is already going on.");
    }
    for line in &s.sources {
        out.push_str("\n  ");
        out.push_str(line);
    }
    out.push_str(
        "\nDialing needs: the phone connected, Telephony allowed for it (geekbridge-ctl consent DEVICE telephony on) and \"make phone calls\" allowed in the phone app.",
    );
    out
}

/// Ask on a terminal; without one only `--yes` confirms. Never dials by default.
fn confirmed(yes: bool, phone: &str, number: &str) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "refusing to dial without confirmation: run it in a terminal or add --yes".into(),
        );
    }
    eprint!("Place a call to {} with {phone}? [y/N] ", mask(number));
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if matches!(line.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err("cancelled; nothing was dialed".into())
    }
}

/// `call audio-devices [--output NAME] [--input NAME]`.
fn audio_devices(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    let mut output = None;
    let mut input = None;
    let mut it = args.iter().skip_while(|a| *a != "audio-devices").skip(1);
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--output" => &mut output,
            "--input" => &mut input,
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        };
        *slot = Some(
            it.next()
                .ok_or(format!("{a} needs a device name or default"))?
                .clone(),
        );
    }
    let set = output.is_some() || input.is_some();
    if set {
        // Keep the other half of the pick as it is.
        let current = match ipc::call(
            port,
            token,
            &Request::CallAudioDevices {
                set: false,
                output: None,
                input: None,
            },
        )
        .map_err(|e| e.to_string())?
        {
            Response::CallAudioDevices(d) => *d,
            Response::Error(e) => return Err(e.message),
            _ => return Err("unexpected reply from the service".into()),
        };
        output = output.or(current.chosen_output);
        input = input.or(current.chosen_input);
    }
    match ipc::call(
        port,
        token,
        &Request::CallAudioDevices { set, output, input },
    )
    .map_err(|e| e.to_string())?
    {
        Response::CallAudioDevices(d) => {
            print!("{}", crate::call_audio_host::describe(&d));
            Ok(())
        }
        Response::Error(e) => Err(e.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

fn hear(port: u16, token: &Token, req: Request) -> Result<bridge_core::ipc::HearCalls, String> {
    match ipc::call(port, token, &req).map_err(|e| e.to_string())? {
        Response::HearCalls(h) => Ok(*h),
        Response::Error(e) => Err(e.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

/// `call setup ...`: drive the service's guided setup and follow it until it ends.
fn setup(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    use bridge_core::ipc::CallAudioSetupAction as A;
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    let action = if has("--fix-config") {
        A::FixConfig
    } else if has("--cancel") {
        A::Cancel
    } else if let Some(c) = flag("--confirm") {
        A::Confirm {
            accept: match c.as_str() {
                "yes" | "y" => true,
                "no" | "n" => false,
                _ => return Err("--confirm takes yes or no".into()),
            },
        }
    } else {
        A::Start {
            phone: flag("--phone"),
        }
    };
    let follow = matches!(action, A::Start { .. });
    let h = hear(port, token, Request::CallAudioPair { action })?;
    if !follow {
        println!("{}", h.setup.message);
        return Ok(());
    }
    let mut shown = 0;
    let started = std::time::Instant::now();
    loop {
        let h = hear(port, token, Request::HearCalls)?;
        for l in h.setup.log.iter().skip(shown) {
            println!("  {l}");
        }
        shown = h.setup.log.len();
        match h.setup.phase.as_str() {
            "confirm" => {
                let code = h.setup.passkey.clone().unwrap_or_default();
                if !std::io::stdin().is_terminal() {
                    println!(
                        "Pairing code {code}: answer with geekbridge-ctl call setup --confirm yes (or no)"
                    );
                    return Ok(());
                }
                eprint!("Does the phone show {code}? Accept there too. [y/N] ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .map_err(|e| e.to_string())?;
                let accept = matches!(line.trim(), "y" | "Y" | "yes");
                hear(
                    port,
                    token,
                    Request::CallAudioPair {
                        action: A::Confirm { accept },
                    },
                )?;
            }
            "done" => {
                println!(
                    "Next: geekbridge-ctl calling (every step should be ok), then answer a test call here."
                );
                return Ok(());
            }
            "failed" => return Err("call-audio setup did not finish (see above)".into()),
            _ if started.elapsed() > std::time::Duration::from_secs(240) => {
                return Err(
                    "the setup is taking too long; geekbridge-ctl call setup --cancel".into(),
                );
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(400)),
        }
    }
}

pub fn run(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    if args.get(1).map(String::as_str) == Some("setup")
        && args.first().map(String::as_str) == Some("call")
    {
        return setup(args, port, token);
    }
    if args.get(1).map(String::as_str) == Some("audio-devices")
        && args.first().map(String::as_str) == Some("call")
    {
        return audio_devices(args, port, token);
    }
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
                ["dial", number] => {
                    if !bridge_core::calls_link::valid_dial_number(number) {
                        return Err(
                            "that is not a number this computer can dial (digits, optional leading +)"
                                .into(),
                        );
                    }
                    let dial = s.view.dial.clone().ok_or_else(|| dial_unavailable(&s))?;
                    confirmed(args.iter().any(|a| a == "--yes"), &dial.phone, number)?;
                    Request::CallDial {
                        source: dial.source,
                        number: Sensitive::new(*number),
                    }
                }
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
                ["audio"] | ["audio", "computer"] => Request::CallAudio {
                    source: s
                        .view
                        .audio
                        .connect
                        .clone()
                        .ok_or("moving the audio here is not available (no call, or it is already here; see geekbridge-ctl calling)")?,
                },
                ["audio", "phone"] => Request::CallAudioPhone {
                    source: live
                        .and_then(|r| r.hangup.as_ref())
                        .filter(|t| t.path == ControlPath::Hfp)
                        .map(|t| t.source.clone())
                        .ok_or("no call's audio is on this computer")?,
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

    // ---- test-gaps-4

    fn row(phase: &str, extra: serde_json::Value) -> CallRow {
        let mut v = serde_json::json!({
            "key": "k", "phone": "Pixel", "direction": "incoming", "phase": phase,
            "number": "+15550142", "name": "Ada", "answer": null, "reject": null, "hangup": null,
            "missing_control": null
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn target(path: &str) -> serde_json::Value {
        serde_json::json!({"source": "s", "call": "c1", "path": path})
    }

    fn status(rows: Vec<CallRow>, sources: &[&str], notice: Option<&str>) -> CallsStatus {
        let mut v = serde_json::json!({
            "view": {
                "rows": [], "audio": {"route": {"route": "no_call"}, "hint": null, "connect": null},
                "mute": null, "sources_up": 0, "dial": null
            },
            "notice": notice, "sources": sources, "audio_note": null
        });
        v["view"]["rows"] = serde_json::to_value(&rows).unwrap();
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn mask_counts_characters_not_bytes_and_never_shows_more_than_two() {
        assert_eq!(mask("1"), "*");
        assert_eq!(mask("123"), "*23");
        assert_eq!(mask("\u{661}\u{662}\u{663}\u{664}"), "**\u{663}\u{664}");
        assert_eq!(
            mask("+4915112345678").chars().filter(|c| *c == '*').count(),
            12
        );
        assert!(!mask("+15550142").contains("5550"));
    }

    #[test]
    fn a_row_names_the_phase_the_masked_number_and_which_path_each_control_takes() {
        let r = row(
            "incoming",
            serde_json::json!({"answer": target("hfp"), "reject": target("link")}),
        );
        let line = describe_row(&r, false);
        assert_eq!(
            line,
            "ringing | number: *******42 | name: known | answer: bluetooth reject: phone-link hangup: -"
        );
        assert!(
            !line.contains("Ada"),
            "the name is never printed, only that it is known"
        );
        assert!(describe_row(&r, true).contains("number: +15550142"));
        let waiting = row("waiting", serde_json::json!({"number": null, "name": null}));
        assert_eq!(
            describe_row(&waiting, true),
            "ringing | number: - | name: - | answer: - reject: - hangup: -"
        );
        for (phase, word) in [
            ("dialing", "dialing"),
            ("alerting", "dialing"),
            ("active", "active"),
            ("held", "held"),
            ("waiting", "ringing"),
        ] {
            assert!(
                describe_row(&row(phase, serde_json::json!({})), false)
                    .starts_with(&format!("{word} |")),
                "{phase}"
            );
        }
        let missing = row(
            "active",
            serde_json::json!({"hangup": target("link"), "missing_control": "no answer button"}),
        );
        assert!(describe_row(&missing, false).ends_with("hangup: phone-link | no answer button"));
    }

    #[test]
    fn the_overview_says_no_calls_the_audio_route_the_notice_and_every_source() {
        let s = status(
            vec![],
            &[
                "Phone link (Pixel): ready.",
                "Bluetooth hands-free calling is not available on this computer.",
            ],
            Some("The phone could not do that."),
        );
        let text = describe(&s, false);
        assert!(text.starts_with("no calls\n"));
        assert!(text.contains("audio: NoCall\n"));
        assert!(text.contains("notice: The phone could not do that.\n"));
        assert_eq!(text.matches("source: ").count(), 2);
        // With calls there is no "no calls" line, and no notice line without a notice.
        let s = status(vec![row("active", serde_json::json!({}))], &[], None);
        let text = describe(&s, false);
        assert!(
            !text.contains("no calls") && !text.contains("notice:") && !text.contains("source:")
        );
        assert!(text.starts_with("active |"));
        assert!(!text.contains("5550142"), "masked by default");
        assert!(describe(&s, true).contains("+15550142"));
        // The audio hint shows in brackets.
        let mut s = s;
        s.view.audio.hint = Some("Answered on the phone".into());
        assert!(describe(&s, false).contains("audio: NoCall (Answered on the phone)\n"));
    }

    #[test]
    fn dial_unavailable_lists_what_the_service_said_and_what_dialing_needs() {
        let idle = status(
            vec![],
            &[
                "Phone link (Pixel): ready to answer; to dial from here, allow GeekBridge to make phone calls in the phone app's setup.",
            ],
            None,
        );
        let t = dial_unavailable(&idle);
        assert!(t.starts_with("dialing is not available right now; nothing was dialed."));
        assert!(!t.contains("already going on"));
        assert!(t.contains("\n  Phone link (Pixel)"));
        assert!(
            t.contains("geekbridge-ctl consent DEVICE telephony on")
                && t.contains("make phone calls")
        );
        let busy = status(vec![row("active", serde_json::json!({}))], &[], None);
        let t = dial_unavailable(&busy);
        assert!(t.contains("A call is already going on."));
        assert!(!t.contains("5550142"));
        // No sources: still a usable message.
        assert!(dial_unavailable(&status(vec![], &[], None)).contains("Dialing needs:"));
    }

    #[test]
    fn unknown_commands_fail_before_any_network_use_and_print_the_usage() {
        let token = Token::from_bytes([7; 32]);
        for args in [
            vec![],
            vec!["calls", "extra"],
            vec!["hello"],
            vec!["calls", "--reveal", "x"],
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            let e = run(&args, 1, &token).unwrap_err();
            assert!(e.starts_with("unknown calls command"), "{args:?}: {e}");
            assert!(e.contains("call dial NUMBER [--yes]"));
        }
        // A reachable-service failure is reported, not a panic (port 1 refuses).
        let args = vec!["calls".to_owned()];
        assert!(run(&args, 1, &token).is_err());
        let args = vec!["call".into(), "dial".into(), "+1555".into(), "--yes".into()];
        assert!(run(&args, 1, &token).is_err());
        assert!(
            USAGE.contains("call dial")
                && USAGE.contains("--reveal")
                && USAGE.contains("call mute")
        );
    }

    #[test]
    fn path_labels_are_the_three_documented_words() {
        use bridge_core::calls_view::ActionTarget;
        let t = |p| ActionTarget {
            source: "s".into(),
            call: bridge_core::calls::CallId::new("c"),
            path: p,
        };
        assert_eq!(path(None), "-");
        assert_eq!(path(Some(&t(ControlPath::Hfp))), "bluetooth");
        assert_eq!(path(Some(&t(ControlPath::Link))), "phone-link");
    }
}
