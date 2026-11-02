//! Headless developer client for the GeekBridge control API.
//!
//! Prints the service's JSON answers. It never prints the control token and never approves a
//! pairing on its own: `approve` needs the verification code the *phone* shows and compares it
//! with the challenge before sending anything.

use bridge_core::ipc::{
    self, Endpoint, Feature, IpcError, PairMode, Request, Response, Token, UpdateState,
    UpdateStatus,
};
use std::{ffi::OsString, path::PathBuf, process::ExitCode};

const USAGE: &str = "geekbridge-ctl [--state-dir DIR] [--control-port PORT] COMMAND

Commands:
  status                              service identity, paired devices, links, pending challenges
  events [AFTER]                      events newer than AFTER (default 0)
  connect DEVICE_ID [IP PORT]         dial a device (endpoint from discovery if omitted)
  pair DEVICE_ID [request|await]      start pairing on an unpaired link (default: request)
  pair-qr                             one-time pairing link for the QR code (valid 2 minutes, single use);
                                      the desktop apps draw it as a QR code the phone scans
  test-connection [DEVICE_ID|IP PORT] check why a phone and this computer can't talk, in plain words
  approve CHALLENGE_ID CODE           approve; CODE is the verification key shown on the phone
  deny CHALLENGE_ID
  disconnect DEVICE_ID                close the link, keep the pairing
  unpair DEVICE_ID                    forget the device
  rename NAME                         set this computer's name (shown on phones from their next connection)
  consent DEVICE_ID FEATURE on|off    sms, contacts, calendar, clipboard, notifications, telephony
  ping DEVICE_ID                      interoperability check on a paired link
  agenda [QUERY] [YYYY-MM-DD] [DAYS]         contacts matching QUERY and that day's events, from phones whose
                                      Contacts/Calendar consent is on (prints personal data; memory only)
  calendar-add DEVICE_ID CALENDAR_ID TITLE START_MS MINUTES [LOCATION]
                                      create an event on the phone (needs the calendar_edit consent)
  calendar-delete DEVICE_ID EVENT_ID  delete a one-off event on the phone (calendar_edit consent)
  calendar-retry DEVICE_ID OP_ID      send an uncertain edit again under the same operation id
  agenda-refresh [DEVICE_ID]          ask the phone(s) for fresh contacts and calendar data
  messages [DEVICE_ID THREAD_ID]      SMS conversations from phones whose Sms consent is on, and one thread
  messages-refresh DEVICE_ID [THREAD] ask for the conversation list, or older history of a thread
  messages-send DEVICE_ID NUMBER TEXT [SUB_ID] send one SMS (SUB_ID: a SIM the phone listed); the send id is not a receipt
  calling                             read-only host calling backend summary
  calls [--reveal]                    ringing and live calls, the controls that really exist,
                                      the audio route and why a control is missing (numbers masked)
  call answer|reject|hangup           act on the ringing (answer, reject) or live (hangup) call
  call mute on|off | call audio       mute the computer microphone / move the audio here (only where offered)
  hotkey install|uninstall|print|status
                                      bind paste-from-phone and float the call/share windows on Hyprland
                                      (dry run unless --apply), or print steps for GNOME/KDE/sway;
                                      `geekbridge-ctl hotkey --help` for options
  status --waybar                     one JSON line for a waybar custom module (docs/omarchy.md)
  clipboard status                    clipboard bridge: backend, per-device counters, hotkey, window
  clipboard paste-from-phone [DEVICE_ID] [--notify]
                                      pull the phone's latest eligible item (copied within the
                                      freshness window, phone unlocked) onto this computer's
                                      clipboard; bind this to a key on Wayland
  clipboard push-to-phone [DEVICE_ID] send this computer's clipboard (image/HTML/URL/text) to a phone
  clipboard window SECONDS            how recently a phone item must have been copied (5-600)
  clipboard hotkey [COMBO]            show or set the paste-from-phone hotkey (default Ctrl+Shift+V)
  clipboard image-limit MIB           largest image accepted or sent (1-32)
  update [status|check|download|skip VERSION|enable|disable]
                                      look for a newer GitHub release, download and SHA-256-verify
                                      it (never installs anything); default: status
  send-file DEVICE_ID PATH...         send files to a connected, paired device
  send-text DEVICE_ID TEXT            send text (at most 8 KiB)
  send-url DEVICE_ID URL              send an http(s) link
  share [status]                      transfers, save folder and per-device share permissions
  share accept|decline TRANSFER_ID    answer an incoming file offer
  share cancel|dismiss TRANSFER_ID    stop a transfer / forget a finished one
  share allow DEVICE_ID on|off        let the device send us files, links and text (default off)
  share auto-accept DEVICE_ID on|off  accept files from the device without asking (default off)
  diagnose                            redacted support bundle (versions, capabilities, counters, last
                                      errors); no names, addresses, keys or content
  autostart [status|enable|disable]   start the service at login (XDG autostart, LaunchAgent or the
                                      per-user Run key); default: status
  tray [status|enable|disable]        the tray / menu-bar icon: show it or hide it for good (takes effect
                                      the next time the service starts); default: status
  firewall                            the exact command to allow GeekBridge through this computer's firewall
  token-path                          where the control token lives (never its contents)";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn text(arg: Option<&OsString>, what: &str) -> Result<String, String> {
    arg.and_then(|a| a.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {what}\n\n{USAGE}"))
}

fn run(mut args: Vec<OsString>) -> Result<(), String> {
    let mut state_dir: Option<PathBuf> = None;
    let mut port = ipc::control_port_from_env();
    while let Some(first) = args.first().and_then(|a| a.to_str()) {
        match first {
            "--state-dir" => {
                let value = args.get(1).ok_or("--state-dir expects a directory")?;
                state_dir = Some(PathBuf::from(value));
                args.drain(..2);
            }
            "--control-port" => {
                port = text(args.get(1), "port")?
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or("--control-port expects a port")?;
                args.drain(..2);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("geekbridge-ctl {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => break,
        }
    }
    let command = text(args.first(), "command")?;
    let state_dir = state_dir
        .or_else(ipc::default_state_dir)
        .ok_or("cannot determine the state directory; pass --state-dir")?;
    if command == "firewall" {
        println!("{}", bridge_service::autostart::firewall_help());
        return Ok(());
    }
    if command == "token-path" {
        println!("{}", ipc::token_path(&state_dir).display());
        return Ok(());
    }
    if command == "tray" {
        return tray_command(&args[1..], &state_dir);
    }
    if command == "autostart" {
        return autostart_command(&args[1..]);
    }
    if command == "hotkey" {
        return hotkey_command(&args[1..], port, &state_dir);
    }
    if command == "status" && args.iter().skip(1).any(|a| a == "--waybar") {
        println!("{}", waybar_line(port, &state_dir));
        return Ok(());
    }
    if matches!(
        command.as_str(),
        "send-file" | "send-text" | "send-url" | "share"
    ) {
        return share_command(&command, &args[1..], port, &state_dir);
    }
    if matches!(command.as_str(), "pair-qr" | "test-connection") {
        return pairing_command(&command, &args[1..], port, &state_dir);
    }
    if command == "diagnose" {
        return diagnose_command(port, &state_dir);
    }
    if command == "update" {
        return update_command(&args[1..], port, &state_dir);
    }
    if command == "clipboard" {
        return clipboard_command(&args[1..], port, &state_dir);
    }
    if matches!(command.as_str(), "calls" | "call") {
        let words: Vec<String> = args
            .iter()
            .filter_map(|a| a.to_str().map(str::to_owned))
            .collect();
        return bridge_service::call_cli::run(&words, port, &load_token(&state_dir)?);
    }
    let arg = |i: usize, what: &str| text(args.get(i), what);
    let request = match command.as_str() {
        "status" => Request::Status,
        "events" => Request::Events {
            after: args
                .get(1)
                .and_then(|a| a.to_str())
                .map_or(Ok(0), |a| a.parse().map_err(|_| "AFTER must be a number"))?,
        },
        "connect" => Request::Connect {
            device_id: arg(1, "DEVICE_ID")?,
            endpoint: match args.get(2) {
                None => None,
                Some(_) => Some(Endpoint {
                    address: arg(2, "IP")?.parse().map_err(|_| "IP is not an address")?,
                    port: arg(3, "PORT")?
                        .parse()
                        .map_err(|_| "PORT is not a number")?,
                }),
            },
        },
        "pair" => Request::Pair {
            device_id: arg(1, "DEVICE_ID")?,
            mode: match args.get(2).and_then(|a| a.to_str()).unwrap_or("request") {
                "request" => PairMode::Request,
                "await" => PairMode::Await,
                _ => return Err("mode must be request or await".into()),
            },
        },
        "approve" => {
            let challenge_id = arg(1, "CHALLENGE_ID")?;
            let typed = arg(2, "CODE shown on the phone")?;
            let token = load_token(&state_dir)?;
            let status = ipc::status(port, &token).map_err(|e| e.to_string())?;
            let challenge = status
                .challenges
                .iter()
                .find(|c| c.challenge_id == challenge_id)
                .ok_or("no such pending challenge")?;
            if !challenge
                .verification_key
                .eq_ignore_ascii_case(typed.trim())
            {
                return Err("the code does not match the challenge; not approving".into());
            }
            Request::Approve { challenge_id }
        }
        "deny" => Request::Deny {
            challenge_id: arg(1, "CHALLENGE_ID")?,
        },
        "disconnect" => Request::Disconnect {
            device_id: arg(1, "DEVICE_ID")?,
        },
        "rename" => Request::SetDeviceName {
            name: arg(1, "NAME")?,
        },
        "unpair" => Request::Unpair {
            device_id: arg(1, "DEVICE_ID")?,
        },
        "consent" => Request::SetConsent {
            device_id: arg(1, "DEVICE_ID")?,
            feature: serde_json::from_value::<Feature>(serde_json::Value::String(arg(
                2, "FEATURE",
            )?))
            .map_err(|_| "unknown feature")?,
            enabled: match arg(3, "on|off")?.as_str() {
                "on" => true,
                "off" => false,
                _ => return Err("expected on or off".into()),
            },
        },
        "ping" => Request::Ping {
            device_id: arg(1, "DEVICE_ID")?,
        },
        "calling" => Request::CallingStatus,
        "agenda" => {
            let day = match arg(2, "DAY") {
                Ok(d) => d,
                Err(_) => {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let d = bridge_core::agenda::Date::from_epoch_day((secs / 86_400) as i64)
                        .map_err(|e| e.to_string())?;
                    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
                }
            };
            Request::AgendaView {
                query: arg(1, "QUERY").unwrap_or_default(),
                day,
                utc_offset_minutes: 0,
                days: arg(3, "DAYS")
                    .ok()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(1),
                people_limit: 0,
            }
        }
        "calendar-add" => {
            let start: i64 = arg(4, "START_MS")?
                .parse()
                .map_err(|_| "START_MS must be epoch milliseconds")?;
            let minutes: i64 = arg(5, "MINUTES")?
                .parse()
                .map_err(|_| "MINUTES must be a number")?;
            let op_id = op_id();
            eprintln!("operation id: {op_id}");
            Request::CalendarEdit {
                device_id: arg(1, "DEVICE_ID")?,
                op_id,
                action: bridge_core::phone_view::CalendarEditAction::Create,
                calendar_id: Some(arg(2, "CALENDAR_ID")?),
                event_id: None,
                draft: Some(Box::new(bridge_core::phone_view::EventDraft {
                    title: arg(3, "TITLE")?,
                    all_day: false,
                    start_ms: start,
                    end_ms: start + minutes * 60_000,
                    tz: "UTC".into(),
                    location: arg(6, "LOCATION").ok(),
                    description: None,
                    reminder_min: Some(10),
                })),
            }
        }
        "calendar-delete" => Request::CalendarEdit {
            device_id: arg(1, "DEVICE_ID")?,
            op_id: op_id(),
            action: bridge_core::phone_view::CalendarEditAction::Delete,
            calendar_id: None,
            event_id: Some(arg(2, "EVENT_ID")?),
            draft: None,
        },
        "calendar-retry" => Request::CalendarEditRetry {
            device_id: arg(1, "DEVICE_ID")?,
            op_id: arg(2, "OP_ID")?,
        },
        "agenda-refresh" => Request::AgendaRefresh {
            device_id: arg(1, "DEVICE_ID").ok(),
        },
        "messages" => Request::MessagesView {
            device_id: arg(1, "DEVICE_ID").ok(),
            thread_id: match arg(2, "THREAD_ID") {
                Ok(t) => Some(t.parse().map_err(|_| "THREAD_ID must be a number")?),
                Err(_) => None,
            },
            query: None,
        },
        "messages-refresh" => Request::MessagesRefresh {
            device_id: arg(1, "DEVICE_ID")?,
            thread_id: match arg(2, "THREAD") {
                Ok(t) => Some(t.parse().map_err(|_| "THREAD must be a number")?),
                Err(_) => None,
            },
        },
        "messages-send" => Request::MessagesSend {
            device_id: arg(1, "DEVICE_ID")?,
            recipient: arg(2, "NUMBER")?,
            text: arg(3, "TEXT")?,
            sub_id: arg(4, "SUB_ID").ok().and_then(|v| v.parse().ok()),
            submission_id: format!(
                "ctl{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis())
            ),
        },
        other => return Err(format!("unknown command: {other}\n\n{USAGE}")),
    };
    let token = load_token(&state_dir)?;
    let response = ipc::call(port, &token, &request).map_err(|e| match e {
        IpcError::Unauthorized => {
            "the service rejected the token in this state directory (is --state-dir the service's?)"
                .into()
        }
        other => other.to_string(),
    })?;
    match response {
        Response::Ack => println!("ok"),
        other => println!(
            "{}",
            serde_json::to_string_pretty(&other).map_err(|e| e.to_string())?
        ),
    }
    Ok(())
}

fn op_id() -> String {
    format!(
        "ctl{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    )
}

fn pairing_command(
    command: &str,
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let token = load_token(state_dir)?;
    let words: Vec<String> = args
        .iter()
        .filter_map(|a| a.to_str().map(str::to_owned))
        .collect();
    let request = if command == "pair-qr" {
        Request::PairQr
    } else {
        let endpoint = match (words.first(), words.get(1)) {
            (Some(ip), Some(p)) if ip.parse::<std::net::IpAddr>().is_ok() => Some(Endpoint {
                address: ip.parse().map_err(|_| "invalid IP address")?,
                port: p.parse().map_err(|_| "invalid port")?,
            }),
            _ => None,
        };
        let device_id = if endpoint.is_none() {
            words.first().cloned()
        } else {
            None
        };
        Request::TestConnection {
            device_id,
            endpoint,
        }
    };
    match clipboard_call(port, &token, &request)? {
        Response::PairQr(q) => {
            println!("{}", q.payload);
            eprintln!(
                "Valid for {} s, single use. Addresses: {}",
                q.expires_in_ms / 1000,
                q.addresses.join(", ")
            );
        }
        Response::ConnectionTest(t) => {
            println!("{}", t.headline);
            for step in &t.steps {
                println!(
                    "  [{}] {}: {}",
                    if step.ok { "ok" } else { "!!" },
                    step.label,
                    step.detail
                );
            }
            for line in &t.advice {
                println!("  - {line}");
            }
        }
        _ => return Err("unexpected reply from the service".into()),
    }
    Ok(())
}

fn clipboard_call(port: u16, token: &Token, request: &Request) -> Result<Response, String> {
    match ipc::call(port, token, request).map_err(|e| e.to_string())? {
        Response::Error(body) => Err(body.message),
        other => Ok(other),
    }
}

fn clipboard_status(port: u16, token: &Token) -> Result<ipc::ClipboardStatus, String> {
    match clipboard_call(port, token, &Request::ClipboardStatus)? {
        Response::ClipboardStatus(s) => Ok(*s),
        _ => Err("unexpected reply from the service".into()),
    }
}

/// One-line explanation of a pull outcome for a person, not a log.
fn describe_pull(p: &ipc::ClipboardPullStatus) -> String {
    match (p.state.as_str(), p.reason.as_deref()) {
        ("delivered", _) => format!(
            "Pasted from phone ({}).",
            p.kind.as_deref().unwrap_or("item")
        ),
        ("waiting", _) => "Waiting for the phone...".into(),
        (_, Some("locked")) => "Phone is locked: unlock it and copy again.".into(),
        (_, Some("expired")) => {
            "Nothing recent on the phone: copy again, then paste within the time window.".into()
        }
        (_, Some("empty")) => "The phone's clipboard is empty.".into(),
        (_, Some("needs_gesture")) => {
            "Tap the GeekBridge notification on the phone to send its clipboard.".into()
        }
        (_, Some("disabled")) => "Clipboard sync is off in the phone app.".into(),
        (_, Some("too_large")) => "That item is too large to send.".into(),
        (_, Some("sensitive")) => {
            "The phone marked that item as sensitive; it was not sent.".into()
        }
        (_, Some("timeout")) => "The phone did not answer in time.".into(),
        (_, Some(other)) => format!("No item from the phone ({other})."),
        _ => "No item from the phone.".into(),
    }
}

fn clipboard_command(
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let word = |i: usize, what: &str| text(args.get(i), what);
    let sub = word(0, "clipboard subcommand")?;
    let token = load_token(state_dir)?;
    let notify = args.iter().any(|a| a == "--notify");
    let device = args
        .iter()
        .skip(1)
        .filter_map(|a| a.to_str())
        .find(|a| !a.starts_with("--"))
        .map(str::to_owned);
    match sub.as_str() {
        "status" => {
            let s = clipboard_status(port, &token)?;
            println!(
                "backend:  {} ({}){}",
                s.backend,
                s.observe,
                s.problem
                    .as_ref()
                    .map_or(String::new(), |p| format!(", problem: {p}"))
            );
            println!(
                "watching: {}",
                if s.watching {
                    "yes"
                } else {
                    "no (no phone enabled)"
                }
            );
            println!("window:   {} s", s.window_ms / 1000);
            println!("hotkey:   {} ({})", s.hotkey, s.hotkey_binding);
            println!("images:   up to {} MiB", s.max_image_bytes / (1024 * 1024));
            if let Some(p) = &s.last_pull {
                println!("last pull: {}", describe_pull(p));
            }
            for d in &s.devices {
                println!(
                    "{} {}: {}, {}, sent {}, received {}, skipped {}{}",
                    d.device_id,
                    d.name,
                    if d.enabled { "enabled" } else { "off" },
                    if d.connected {
                        "connected"
                    } else {
                        "not connected"
                    },
                    d.sent,
                    d.received,
                    d.skipped,
                    d.last_skip
                        .as_ref()
                        .map_or(String::new(), |r| format!(" (last: {r})")),
                );
            }
            Ok(())
        }
        "paste-from-phone" => {
            let outcome = (|| {
                clipboard_call(port, &token, &Request::ClipboardPull { device_id: device })?;
                // The phone may need a tap, so wait for the answer.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let s = clipboard_status(port, &token)?;
                    match s.last_pull {
                        Some(p) if p.state != "waiting" => return Ok(p),
                        _ if std::time::Instant::now() > deadline => {
                            return Err("the phone did not answer in time".to_string());
                        }
                        _ => {}
                    }
                }
            })();
            let (line, ok) = match &outcome {
                Ok(p) => (describe_pull(p), p.state == "delivered"),
                Err(e) => (e.clone(), false),
            };
            println!("{line}");
            if notify {
                let _ = std::process::Command::new("notify-send")
                    .args(["-a", "GeekBridge", "-t", "3000", "Clipboard", &line])
                    .status();
            }
            if ok { Ok(()) } else { Err(line) }
        }
        "push-to-phone" => {
            clipboard_call(port, &token, &Request::ClipboardPush { device_id: device })?;
            println!("ok");
            Ok(())
        }
        "window" => {
            let secs: u64 = word(1, "SECONDS")?
                .parse()
                .map_err(|_| "SECONDS must be a number")?;
            clipboard_call(
                port,
                &token,
                &Request::ClipboardSettings {
                    window_ms: Some(secs.saturating_mul(1000)),
                    hotkey: None,
                    max_image_bytes: None,
                },
            )?;
            println!("ok (clamped to 5-600 seconds)");
            Ok(())
        }
        "image-limit" => {
            let mib: u64 = word(1, "MIB")?
                .parse()
                .map_err(|_| "MIB must be a number")?;
            clipboard_call(
                port,
                &token,
                &Request::ClipboardSettings {
                    window_ms: None,
                    hotkey: None,
                    max_image_bytes: Some(mib.saturating_mul(1024 * 1024)),
                },
            )?;
            println!("ok (clamped to 1-32 MiB)");
            Ok(())
        }
        "hotkey" => {
            match args.get(1).and_then(|a| a.to_str()) {
                None => {
                    let s = clipboard_status(port, &token)?;
                    println!("{} ({})", s.hotkey, s.hotkey_binding);
                }
                Some(combo) => {
                    clipboard_call(
                        port,
                        &token,
                        &Request::ClipboardSettings {
                            window_ms: None,
                            hotkey: Some(combo.to_owned()),
                            max_image_bytes: None,
                        },
                    )?;
                    println!("ok");
                }
            }
            Ok(())
        }
        other => Err(format!("unknown clipboard command: {other}\n\n{USAGE}")),
    }
}

fn tray_command(args: &[OsString], state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_service::tray;
    match args.first().and_then(|a| a.to_str()) {
        None | Some("status") => {}
        Some(on @ ("enable" | "disable")) => {
            tray::set_enabled(state_dir, on == "enable")
                .map_err(|e| format!("cannot change the tray setting: {e}"))?;
        }
        Some(other) => return Err(format!("unknown tray command: {other}\n\n{USAGE}")),
    }
    println!(
        "tray icon: {} (a running service picks the change up at its next start)",
        if tray::enabled(state_dir) {
            "enabled"
        } else {
            "disabled"
        }
    );
    Ok(())
}

fn autostart_command(args: &[OsString]) -> Result<(), String> {
    use bridge_service::autostart::{self, Action};
    let action = match args.first().and_then(|a| a.to_str()) {
        None | Some("status") => Action::Status,
        Some("enable") => Action::Enable,
        Some("disable") => Action::Disable,
        Some(other) => return Err(format!("unknown autostart command: {other}\n\n{USAGE}")),
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let service = exe.with_file_name(autostart::service_file_name());
    println!("{}", autostart::run(action, &service)?);
    Ok(())
}

fn hotkey_command(args: &[OsString], port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_service::hotkey_cli::{Env, run};
    let words: Vec<String> = args
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or("arguments must be text")
        })
        .collect::<Result<_, _>>()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))
        .ok_or("cannot find your config directory ($HOME is not set)")?;
    // Omarchy keeps its shipped defaults read-only under $OMARCHY_PATH, /usr/share/omarchy or
    // ~/.local/share/omarchy; they are only read, to warn about a shortcut they already use.
    let roots = [
        std::env::var_os("OMARCHY_PATH").map(PathBuf::from),
        Some(PathBuf::from("/usr/share/omarchy")),
        home.map(|h| h.join(".local/share/omarchy")),
    ];
    let default_binding_files = roots
        .into_iter()
        .flatten()
        .filter_map(|r| std::fs::read_dir(r.join("default/hypr/bindings")).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    let configured = load_token(state_dir)
        .ok()
        .and_then(|t| clipboard_status(port, &t).ok())
        .map(|s| s.hotkey);
    let env = Env {
        hypr_dir: config_home.join("hypr"),
        stamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .to_string(),
        default_binding_files,
        ctl: std::env::current_exe()
            .ok()
            .filter(|p| p.file_name().is_some_and(|n| n == "geekbridge-ctl"))
            .map(|p| p.to_string_lossy().into_owned()),
        configured,
    };
    let report = run(&words, &env)?;
    print!("{}", report.text);
    // Hyprland reloads on save; ask it to now and show any error so a mistake is seen at once.
    if report.wrote && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let hypr = |a: &[&str]| {
            std::process::Command::new("hyprctl")
                .args(a)
                .stdin(std::process::Stdio::null())
                .output()
                .ok()
        };
        if hypr(&["reload"]).is_some() {
            let errors = hypr(&["configerrors"])
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            if errors.is_empty() {
                println!("Hyprland reloaded; `hyprctl configerrors` is clean.");
            } else {
                println!(
                    "Hyprland reports config errors:\n{errors}\nRestore the backup or run `geekbridge-ctl hotkey uninstall --apply`."
                );
            }
        }
    }
    Ok(())
}

/// The waybar JSON line. Every failure degrades to a state, never to an error exit: a bar that
/// prints nothing shows a blank gap.
fn waybar_line(port: u16, state_dir: &std::path::Path) -> String {
    use bridge_service::waybar::{BarState, render};
    let mut bar = BarState::default();
    if let Ok(token) = load_token(state_dir)
        && let Ok(status) = ipc::status(port, &token)
    {
        bar.service_up = true;
        bar.paired = status.paired.len();
        bar.connected = status
            .links
            .iter()
            .filter(|l| l.phase == ipc::LinkPhase::Connected)
            .map(|l| l.name.clone())
            .collect();
        if let Ok(Response::Calls(c)) = ipc::call(port, &token, &Request::Calls) {
            bar.ringing = c.view.rows.iter().filter(|r| r.ringing()).count();
        }
        if !bar.connected.is_empty()
            && let Ok(Response::Messages(m)) = ipc::call(
                port,
                &token,
                &Request::MessagesView {
                    device_id: None,
                    thread_id: None,
                    query: None,
                },
            )
            && m.devices
                .iter()
                .any(|d| d.sms == bridge_core::phone_view::DataState::Ready)
        {
            bar.unread_sms = Some(m.conversations.iter().map(|c| c.unread).sum());
        }
        if let Ok(Response::Update(u)) = ipc::call(port, &token, &Request::UpdateStatus)
            && u.newer
            && !u.skipped
        {
            bar.update = u.latest.map(|r| r.version);
        }
    }
    render(&bar)
}

fn load_token(state_dir: &std::path::Path) -> Result<Token, String> {
    let path = ipc::token_path(state_dir);
    let bytes = bridge_kde::fsutil::read_private_file(&path)
        .map_err(|e| format!("cannot read the control token safely: {e}"))?
        .ok_or("no control token yet; start geekbridge-service with the same --state-dir first")?;
    Token::parse(&bytes).map_err(|e| e.to_string())
}

fn diagnose_command(port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_service::diagnose::{Inputs, render};
    let token = load_token(state_dir)?;
    let mut notes = Vec::new();
    let status = match ipc::call(port, &token, &Request::Status).map_err(|e| e.to_string())? {
        Response::Status(s) => *s,
        _ => return Err("unexpected reply from the service".into()),
    };
    let mut ask = |request: Request, what: &str| match ipc::call(port, &token, &request) {
        Ok(Response::Error(e)) => {
            notes.push(format!("{what}: {}", e.message));
            None
        }
        Ok(r) => Some(r),
        Err(e) => {
            notes.push(format!("{what}: {e}"));
            None
        }
    };
    let events = match ask(Request::Events { after: 0 }, "events") {
        Some(Response::Events(p)) => Some(p),
        _ => None,
    };
    let calling = match ask(Request::CallingStatus, "calling") {
        Some(Response::CallingStatus(c)) => Some(c),
        _ => None,
    };
    let clipboard = match ask(Request::ClipboardStatus, "clipboard") {
        Some(Response::ClipboardStatus(c)) => Some(*c),
        _ => None,
    };
    let update = match ask(Request::UpdateStatus, "update") {
        Some(Response::Update(u)) => Some(*u),
        _ => None,
    };
    let host = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    print!(
        "{}",
        render(
            &Inputs {
                status,
                events,
                calling,
                clipboard,
                update,
                notes
            },
            &host
        )
    );
    Ok(())
}

fn update_call(port: u16, token: &Token, request: &Request) -> Result<UpdateStatus, String> {
    match ipc::call(port, token, request).map_err(|e| e.to_string())? {
        Response::Update(status) => Ok(*status),
        Response::Error(body) => Err(body.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

fn describe_update(s: &UpdateStatus) {
    println!("running:  v{}", s.current_version);
    println!(
        "checking: automatic {} (startup, then about daily)",
        if s.enabled { "on" } else { "off" }
    );
    match (&s.latest, s.newer) {
        (Some(latest), true) => {
            println!(
                "latest:   v{} - newer{}",
                latest.version,
                if s.skipped { " (skipped)" } else { "" }
            );
            println!("release:  {}", latest.release_url);
            match (&latest.asset_name, &latest.sha256) {
                (Some(name), Some(sha)) => println!("installer: {name} (SHA-256 {sha})"),
                (Some(name), None) => {
                    println!(
                        "installer: {name} - no published SHA-256, so it will not be downloaded"
                    );
                }
                _ => println!("installer: none published for this system"),
            }
        }
        (Some(latest), false) => println!("latest:   v{} - you are up to date", latest.version),
        (None, _) if s.state == UpdateState::NoPublicRelease => {
            println!("latest:   {}", ipc::UPDATE_NO_PUBLIC_RELEASE);
        }
        (None, _) => println!("latest:   unknown (no check has completed)"),
    }
    if let Some(path) = &s.downloaded_path {
        println!("verified download: {path}");
        println!("Open it yourself to install; GeekBridge never runs or replaces anything.");
    }
    if let Some(error) = &s.error {
        println!("problem:  {error}");
    }
}

fn update_command(args: &[OsString], port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    let sub = args.first().and_then(|a| a.to_str()).unwrap_or("status");
    let token = load_token(state_dir)?;
    let (request, wait_for) = match sub {
        "status" => (Request::UpdateStatus, None),
        "check" => (Request::UpdateCheck, Some(UpdateState::Checking)),
        "download" => (Request::UpdateDownload, Some(UpdateState::Downloading)),
        "skip" => (
            Request::UpdateSkip {
                version: text(args.get(1), "VERSION")?,
            },
            None,
        ),
        "enable" => (Request::UpdateSetEnabled { enabled: true }, None),
        "disable" => (Request::UpdateSetEnabled { enabled: false }, None),
        other => return Err(format!("unknown update command: {other}\n\n{USAGE}")),
    };
    let mut status = update_call(port, &token, &request)?;
    if let Some(busy) = wait_for {
        // The service works in the background; follow it for up to a minute (a check) or half an
        // hour (a download), then report whatever the state is.
        let limit = if busy == UpdateState::Checking {
            60
        } else {
            1800
        };
        for _ in 0..limit * 2 {
            if status.state != busy {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            status = update_call(port, &token, &Request::UpdateStatus)?;
        }
    }
    describe_update(&status);
    if status.error.is_some() && wait_for.is_some() {
        return Err("the update operation failed".into());
    }
    Ok(())
}

fn share_call(port: u16, token: &Token, request: &Request) -> Result<Response, String> {
    ipc::call(port, token, request).map_err(|e| e.to_string())
}

fn onoff(arg: Option<&OsString>) -> Result<bool, String> {
    match text(arg, "on|off")?.as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err("expected on or off".into()),
    }
}

fn describe_share(s: &bridge_core::share::ShareStatus) {
    use bridge_core::share::{TransferDirection, TransferKind};
    println!("save folder: {}", s.save_dir);
    for d in &s.devices {
        println!(
            "device {}: receive {}, auto-accept {}",
            d.device_id,
            if d.receive { "on" } else { "off" },
            if d.auto_accept { "on" } else { "off" }
        );
    }
    for t in &s.transfers {
        let arrow = match t.direction {
            TransferDirection::Incoming => "<-",
            TransferDirection::Outgoing => "->",
        };
        let what = match t.kind {
            TransferKind::File => t.name.clone(),
            TransferKind::Text => format!("text: {}", t.text.as_deref().unwrap_or("")),
            TransferKind::Url => format!("link: {}", t.text.as_deref().unwrap_or("")),
        };
        let size = t.size.map_or(String::new(), |n| format!("/{n}"));
        println!(
            "#{} {arrow} {} {:?} {}{size} bytes {what}{}",
            t.transfer_id,
            t.device_name,
            t.state,
            t.bytes,
            t.code
                .as_deref()
                .map_or(String::new(), |c| format!(" ({c})")),
        );
    }
}

fn share_command(
    command: &str,
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    use bridge_core::share::ShareTextKind;
    let arg = |i: usize, what: &str| text(args.get(i), what);
    let id = |i: usize| -> Result<u64, String> {
        arg(i, "TRANSFER_ID")?
            .parse()
            .map_err(|_| "TRANSFER_ID must be a number".to_owned())
    };
    let token = load_token(state_dir)?;
    match command {
        "send-file" => {
            let device_id = arg(0, "DEVICE_ID")?;
            if args.len() < 2 {
                return Err(format!("missing PATH\n\n{USAGE}"));
            }
            let mut failed = 0;
            for path in &args[1..] {
                // The service wants an absolute path; resolve it here, relative to the caller.
                let shown = path.to_string_lossy().into_owned();
                let absolute = std::fs::canonicalize(path)
                    .map_err(|e| format!("{shown}: {e}"))
                    .and_then(|p| {
                        p.to_str()
                            .map(str::to_owned)
                            .ok_or_else(|| format!("{shown}: path is not valid UTF-8"))
                    });
                let result = absolute.and_then(|path| {
                    share_call(
                        port,
                        &token,
                        &Request::ShareSendFile {
                            device_id: device_id.clone(),
                            path,
                        },
                    )
                });
                match result {
                    Ok(Response::TransferStarted { transfer_id }) => {
                        println!("{shown}: queued as transfer #{transfer_id}");
                    }
                    Ok(Response::Error(e)) => {
                        failed += 1;
                        eprintln!("{shown}: {}", e.message);
                    }
                    Ok(_) => println!("{shown}: ok"),
                    Err(e) => {
                        failed += 1;
                        eprintln!("{shown}: {e}");
                    }
                }
            }
            if failed > 0 {
                return Err(format!("{failed} file(s) were not sent"));
            }
            println!("Watch progress with: geekbridge-ctl share");
            Ok(())
        }
        "send-text" | "send-url" => {
            let request = Request::ShareSendText {
                device_id: arg(0, "DEVICE_ID")?,
                text_kind: if command == "send-url" {
                    ShareTextKind::Url
                } else {
                    ShareTextKind::Text
                },
                text: arg(1, "TEXT or URL")?,
            };
            match share_call(port, &token, &request)? {
                Response::Error(e) => Err(e.message),
                _ => {
                    println!("ok");
                    Ok(())
                }
            }
        }
        _ => {
            let sub = args.first().and_then(|a| a.to_str()).unwrap_or("status");
            let request = match sub {
                "status" => Request::ShareStatus,
                "accept" => Request::ShareDecide {
                    transfer_id: id(1)?,
                    accept: true,
                },
                "decline" => Request::ShareDecide {
                    transfer_id: id(1)?,
                    accept: false,
                },
                "cancel" => Request::ShareCancel {
                    transfer_id: id(1)?,
                },
                "dismiss" => Request::ShareDismiss {
                    transfer_id: id(1)?,
                },
                "allow" => Request::ShareSettings {
                    device_id: arg(1, "DEVICE_ID")?,
                    receive: Some(onoff(args.get(2))?),
                    auto_accept: None,
                },
                "auto-accept" => Request::ShareSettings {
                    device_id: arg(1, "DEVICE_ID")?,
                    receive: None,
                    auto_accept: Some(onoff(args.get(2))?),
                },
                other => return Err(format!("unknown share command: {other}\n\n{USAGE}")),
            };
            match share_call(port, &token, &request)? {
                Response::Share(s) => describe_share(&s),
                Response::Error(e) => return Err(e.message),
                _ => println!("ok"),
            }
            Ok(())
        }
    }
}
