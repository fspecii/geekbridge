// SPDX-License-Identifier: GPL-3.0-or-later
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
  about                               version, build commit, update channel, licence and where the licence text and
                                      third-party notices are (also --version)
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
  consent DEVICE_ID FEATURE on|off    sms, contacts, calendar, calendar_edit, calendar_alerts,
                                      clipboard, notifications, telephony, debug, battery, photos, call_audio_wifi
  notifications [DEVICE_ID] [--json]  phone notifications mirrored here (needs `consent ID notifications on`; prints
                                      personal data; memory only, newest first)
  notifications dismiss DEVICE_ID ID  dismiss one here and on the phone
  notifications action DEVICE_ID ID TITLE
                                      press the phone notification's button titled TITLE
  photos [DEVICE_ID] [--json]         the phone's recent photos held here (needs `consent ID photos on`; names only,
                                      memory only, newest first)
  photos refresh DEVICE_ID            ask the phone for its newest photos (at most 24, thumbnails follow)
  photos thumb DEVICE_ID PHOTO_ID FILE
                                      write the held thumbnail (a small JPEG) to FILE
  photos save DEVICE_ID PHOTO_ID      ask the phone to send that photo in full to the save folder
  photos copy DEVICE_ID PHOTO_ID      same, to a scratch file that is emptied soon (for the clipboard)
  phone-log DEVICE_ID [-f] [-n N]     last N (default 200) lines of a debug-build phone's log; -f follows
  phone-state DEVICE_ID               ask a debug-build phone for its redacted state dump and print it
  ping DEVICE_ID                      interoperability check on a paired link
  agenda [QUERY] [YYYY-MM-DD] [DAYS]         contacts matching QUERY and that day's events, from phones whose
                                      Contacts/Calendar consent is on (prints personal data; memory only)
  calendar-add DEVICE_ID CALENDAR_ID TITLE START_MS MINUTES [LOCATION]
                                      create an event on the phone (needs the calendar_edit consent)
  calendar-update DEVICE_ID EVENT_ID TITLE START_MS MINUTES [LOCATION]
                                      change a one-off event on the phone; refused as changed if the phone's copy
                                      moved on since this computer last synced it (calendar_edit consent)
  calendar-delete DEVICE_ID EVENT_ID  delete a one-off event on the phone (calendar_edit consent)
  calendar-retry DEVICE_ID OP_ID      send an uncertain edit again under the same operation id
  agenda-refresh [DEVICE_ID]          ask the phone(s) for fresh contacts and calendar data
  messages [DEVICE_ID THREAD_ID]      SMS conversations from phones whose Sms consent is on, and one thread
  messages-refresh DEVICE_ID [THREAD] ask for the conversation list, or older history of a thread
  messages-send DEVICE_ID NUMBER TEXT [SUB_ID] send one SMS (SUB_ID: a SIM the phone listed); the send id is not a receipt
  messages-resend DEVICE_ID SEND_ID   send a failed or unconfirmed text again as a NEW message (it may arrive twice)
  messages-read DEVICE_ID THREAD_ID   mark a conversation read on this computer only (the phone keeps its own unread state)
  privacy [show|set KEY on|off]       what a banner or the call card may show: message_text (default off),
                                      caller_number (default on), phone_notification_text, respect_dnd (default off);
                                      live, no restart; the sms-notify-content file and the older
                                      environment variables still force a value
  calling                             Bluetooth calling summary and the 'Hear calls on this computer' checklist (fix per step)
  call-audio [test | use [--output NAME] [--input NAME]]
                                      hear and talk through this computer: the checklist (Bluetooth adapter,
                                      phone paired, hands-free, headphones), a five-second tone and microphone
                                      test, or choose the headphones and microphone (no name = system default)
  callwifi [status | listen [DEVICE_ID] | stop | speak on|off | echo auto|on|off] [--json]
                                      EXPERIMENTAL, off by default: hear a phone call on this computer over Wi-Fi
                                      and, where the phone allows it, speak into it (needs `consent ID
                                      call_audio_wifi on` and the phone's own switch; plays on the headphones
                                      chosen for calls; Bluetooth stays the supported way)
  callwifi setup [--serial S] [--pair HOST:PORT CODE] [--connect HOST:PORT] [--yes] | setup --status|--stop
                                      one-time guided start of the phone's helper over Wireless debugging (adb)
  calls [--reveal]                    ringing and live calls, the controls that really exist,
                                      the audio route and why a control is missing (numbers masked)
  call answer|reject|hangup           act on the ringing (answer, reject) or live (hangup) call
  call dial NUMBER [--yes]            place a call from the phone (digits, optional leading +); asks to confirm
  call tone KEYS                      press keypad keys (0-9 * #) in the live call (phone menus)
  call mute on|off | call audio       mute the computer microphone / move the audio here (only where offered)
  call setup [--phone NAME]           pair this computer with the phone as its hands-free device for call audio
                                      (--fix-config fixes the WirePlumber settings; no sudo)
  call audio-devices [--output NAME] [--input NAME]
                                      list or pick the headphones and microphone calls use
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
  update [status|check|download|skip VERSION|channel stable|dev|enable|disable]
                                      look for a newer GitHub release, download and SHA-256-verify
                                      it; default: status
  update install [--yes] [--dry-run] [--no-restart] [--prefix DIR]
                                      install the verified update into the per-user install
                                      (~/.local from install.sh): unpack beside the old version,
                                      check it, switch atomically, keep the old one. Asks before
                                      it restarts the service and never does during a call.
                                      --dry-run checks everything and changes nothing. Package
                                      installs and the dev channel are download only.
  update rollback [--yes] [--no-restart]  go back to the previous version (again: forward)
  update restart                      restart the service onto the installed version (not during a call)
  update support                      can this copy update itself? (user-install, package-managed,
                                      download-only)
  whats-new                           what changed in this version (the same text as the card Home shows
                                      once after an update), and where the full release notes are
  send-file DEVICE_ID PATH...         send files to a connected, paired device
  send-text DEVICE_ID TEXT            send text (at most 8 KiB)
  send-url DEVICE_ID URL              send an http(s) link
  share [status]                      transfers, save folder and per-device share permissions
  share accept|decline TRANSFER_ID    answer an incoming file offer
  share cancel|dismiss TRANSFER_ID    stop a transfer / forget a finished one
  share allow DEVICE_ID on|off        let the device send us files, links and text (default off)
  share auto-accept DEVICE_ID on|off  accept files from the device without asking (default off)
  call-overlay-test                   show the incoming-call card with a fake call for 10 seconds (no phone)
  doctor                              why something does not work, in plain words, with the fix: service,
                                      firewall (ufw/firewalld ports 1716-1764), desktop session, paired
                                      phones, clipboard, calls, Messages; exits 1 on a failed check
  diagnose                            redacted support bundle (versions, capabilities, counters, last
                                      errors); no names, addresses, keys or content
  report [--preview|--open|--copy]    a prefilled bug report for the public tracker: shows exactly what
                                      it carries (the redacted diagnose output), --open opens it in your
                                      browser, --copy copies it. Nothing is sent by this program.
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
            if let Some(hint) = bridge_service::doctor::hint_for_error(&message) {
                eprintln!("hint: {hint}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Version, licence and where the documents are: the same lines as "About GeekBridge" in the
/// desktop Settings. The update channel comes from the running service; offline it is left out.
/// `--version` keeps `geekbridge-ctl VERSION` as its first line for scripts.
fn about_command(port: u16, state_dir: &std::path::Path, version_flag: bool) {
    let mut about = bridge_core::about::About {
        commit: bridge_update::Config::github(bridge_core::about::VERSION).build_commit,
        channel: None,
    };
    if let Ok(token) = load_token(state_dir)
        && let Ok(status) = update_call(port, &token, &Request::UpdateStatus)
    {
        about.commit = status.current_commit.or(about.commit);
        about.channel = Some(status.channel.as_str().to_owned());
    }
    if version_flag {
        println!("geekbridge-ctl {}", bridge_core::about::VERSION);
    }
    let lines = bridge_core::about::lines(
        &about,
        &|key| bridge_i18n::tr(key).to_owned(),
        &|key, args| {
            let args: Vec<(&str, &dyn std::fmt::Display)> = args
                .iter()
                .map(|(k, v)| (*k, v as &dyn std::fmt::Display))
                .collect();
            bridge_i18n::tr_args(key, &args)
        },
    );
    for line in lines {
        println!("{line}");
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
                match state_dir.clone().or_else(ipc::default_state_dir) {
                    Some(dir) => about_command(port, &dir, true),
                    None => about_command(port, std::path::Path::new(""), true),
                }
                return Ok(());
            }
            _ => break,
        }
    }
    let command = text(args.first(), "command")?;
    let state_dir = state_dir
        .or_else(ipc::default_state_dir)
        .ok_or("cannot determine the state directory; pass --state-dir")?;
    if command == "about" {
        about_command(port, &state_dir, false);
        return Ok(());
    }
    if command == "firewall" {
        println!("{}", bridge_service::autostart::firewall_help());
        return Ok(());
    }
    if command == "whats-new" {
        let notes = bridge_core::whats_new::embedded();
        print!("{}", notes.render());
        println!(
            "Full release notes: {}",
            bridge_core::whats_new::release_url(&notes.version)
        );
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
    if command == "phone-log" {
        return phone_log_command(&args[1..], &state_dir);
    }
    if command == "notifications" {
        return notifications_command(&args[1..], port, &state_dir);
    }
    if command == "photos" {
        return photos_command(&args[1..], port, &state_dir);
    }
    if command == "phone-state" {
        return phone_state_command(&args[1..], port, &state_dir);
    }
    if command == "call-overlay-test" {
        println!(
            "Showing a test call card in the top-right corner for 10 seconds (nobody is calling)..."
        );
        return match bridge_service::call_overlay::test(
            &state_dir,
            bridge_core::DEFAULT_API_PORT,
            port,
        ) {
            Ok(()) => {
                println!("Done. If you saw the card, incoming calls will show it too.");
                Ok(())
            }
            Err(c) => Err(format!(
                "incoming-call overlay cannot start: {} ({})",
                c.code(),
                c.explain()
            )),
        };
    }
    if command == "doctor" {
        return doctor_command(port, &state_dir);
    }
    if command == "calling" {
        return calling_command(port, &state_dir);
    }
    if command == "diagnose" {
        return diagnose_command(port, &state_dir);
    }
    if command == "report" {
        return report_command(&args[1..], port, &state_dir);
    }
    if command == "update" {
        return update_command(&args[1..], port, &state_dir);
    }
    if command == "privacy" {
        return privacy_command(&args[1..], port, &state_dir);
    }
    if command == "clipboard" {
        return clipboard_command(&args[1..], port, &state_dir);
    }
    if command == "callwifi" {
        let words: Vec<String> = args[1..]
            .iter()
            .filter_map(|a| a.to_str().map(str::to_owned))
            .collect();
        if words.first().map(String::as_str) == Some("setup") {
            // The guided helper start also works before the service runs (it then skips the consent step).
            let rest: Vec<&str> = words[1..].iter().map(String::as_str).collect();
            return bridge_service::callwifi_setup::run(&rest, port, load_token(&state_dir).ok());
        }
        return bridge_service::callwifi_cli::run(&words, port, &load_token(&state_dir)?);
    }
    if command == "call-audio" {
        let words: Vec<String> = args
            .iter()
            .filter_map(|a| a.to_str().map(str::to_owned))
            .collect();
        return bridge_service::call_audio_cli::run(&words, port, &load_token(&state_dir)?);
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
        "calendar-update" => {
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
                action: bridge_core::phone_view::CalendarEditAction::Update,
                calendar_id: None,
                event_id: Some(arg(2, "EVENT_ID")?),
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
            message_limit: 0,
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
        "messages-resend" => Request::MessagesResend {
            device_id: arg(1, "DEVICE_ID")?,
            send_id: arg(2, "SEND_ID")?
                .parse()
                .map_err(|_| "SEND_ID must be a number")?,
            submission_id: format!(
                "ctlr{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis())
            ),
        },
        "messages-read" => Request::MessagesMarkRead {
            device_id: arg(1, "DEVICE_ID")?,
            thread_id: arg(2, "THREAD_ID")?
                .parse()
                .map_err(|_| "THREAD_ID must be a number")?,
        },
        other => return Err(format!("unknown command: {other}\n\n{USAGE}")),
    };
    let token = load_token(&state_dir)?;
    let (response, body) = ipc::call_with_body(port, &token, &request).map_err(|e| match e {
        IpcError::Unauthorized => {
            "the service rejected the token in this state directory (is --state-dir the service's?)"
                .into()
        }
        other => other.to_string(),
    })?;
    match response {
        Response::Ack => println!("ok"),
        Response::Status(status) => {
            println!("{}", ipc::pretty_json(&body));
            // On stderr so the JSON above stays machine-readable.
            if let Some(note) = status.port_note() {
                eprintln!("{note}");
            }
            let fw_blocks = bridge_service::doctor::firewall_blocks(&read_firewall());
            for p in &status.paired {
                if let Some(w) = bridge_service::doctor::why_for(&status, &p.device_id, fw_blocks) {
                    eprintln!(
                        "{} is not connected: {}\n  Try: {}",
                        bridge_service::notify_text::plain(&p.name, 40),
                        bridge_core::why_disconnected::one_line(&w),
                        w.fix
                    );
                }
            }
        }
        _ => println!("{}", ipc::pretty_json(&body)),
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
        (_, Some("needs_gesture")) => "Android needs a tap on the phone: tap the GeekBridge notification, the Send clipboard \
             tile, or open the app and tap Send clipboard. No notification? Allow GeekBridge notifications on the phone."
            .into(),
        (_, Some("disabled")) => "Clipboard sync is off in the phone app.".into(),
        (_, Some("too_large")) => "That item is too large to send.".into(),
        (_, Some("sensitive")) => {
            "The phone marked that item as sensitive; it was not sent.".into()
        }
        (_, Some("timeout")) => "The phone did not answer in time: it may be asleep or offline, or its \
             notifications are blocked. Open GeekBridge on the phone, copy, and tap the Send clipboard tile."
            .into(),
        (_, Some(other)) => format!("No item from the phone ({other})."),
        _ => "No item from the phone.".into(),
    }
}

/// One short desktop notification for the hotkey command, which has no terminal to print to.
fn notify_clipboard(line: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "GeekBridge", "-t", "3000", "--", "Clipboard", line])
        .status();
}

fn privacy_command(
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    use bridge_core::privacy::{ForcedBy, PrivacyKey, parse_switch};
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    let token = load_token(state_dir)?;
    let request = match words.as_slice() {
        [] | ["show"] => None,
        ["set", key, value] => {
            let key = PrivacyKey::parse(key).ok_or_else(|| {
                format!(
                    "unknown setting {key}; use one of: {}",
                    PrivacyKey::ALL.map(PrivacyKey::name).join(", ")
                )
            })?;
            let on = parse_switch(value).ok_or("the value must be on or off")?;
            Some(Request::PrivacySet { key, on })
        }
        _ => {
            return Err(format!(
                "usage: geekbridge-ctl privacy [show|set KEY on|off]\n\n{USAGE}"
            ));
        }
    };
    if let Some(request) = request {
        clipboard_call(port, &token, &request)?;
    }
    match clipboard_call(port, &token, &Request::PrivacyStatus)? {
        Response::Privacy(view) => {
            for i in &view.items {
                let forced = match i.forced {
                    None => String::new(),
                    Some(ForcedBy::Environment) => " (forced by an environment variable)".into(),
                    Some(ForcedBy::File) => " (forced by a file in the state folder)".into(),
                };
                println!(
                    "{:<24} {}{forced}",
                    i.key.name(),
                    if i.on { "on" } else { "off" }
                );
            }
            Ok(())
        }
        _ => Err("unexpected reply from the service".into()),
    }
}

fn clipboard_command(
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let word = |i: usize, what: &str| text(args.get(i), what);
    let sub = word(0, "clipboard subcommand")?;
    let notify = args.iter().any(|a| a == "--notify");
    let token = load_token(state_dir).inspect_err(|e| {
        // Started from a hotkey there is no terminal: say that the service is not running.
        if notify && sub == "paste-from-phone" {
            notify_clipboard("GeekBridge is not running; start it, then try again.");
            eprintln!("{e}");
        }
    })?;
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
            if s.other_sync_app {
                println!(
                    "note:     KDE Connect is also running here; if both sync the clipboard to the same phone, text can echo. Turn one off."
                );
            }
            println!(
                "hotkey:   {} {}",
                s.hotkey,
                hotkey_binding_text(&s.hotkey_binding)
            );
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
                            return Err(describe_pull(&ipc::ClipboardPullStatus {
                                device_id: String::new(),
                                state: "none".into(),
                                reason: Some("timeout".into()),
                                kind: None,
                                at_unix_ms: 0,
                            }));
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
                notify_clipboard(&line);
            }
            if ok { Ok(()) } else { Err(line) }
        }
        "push-to-phone" => {
            clipboard_call(port, &token, &Request::ClipboardPush { device_id: device })?;
            println!(
                "Sent this computer's clipboard to the phone (it works while the phone is locked or in the background)."
            );
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
                    println!("{} {}", s.hotkey, hotkey_binding_text(&s.hotkey_binding));
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
        service_env: std::env::var_os(ipc::ENV_STATE_DIR)
            .is_some()
            .then(|| (state_dir.to_string_lossy().into_owned(), port)),
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
        bar.battery = status
            .links
            .iter()
            .filter(|l| l.phase == ipc::LinkPhase::Connected)
            .find_map(|l| l.battery);
        if bar.connected.is_empty() {
            bar.why = status.disconnected.first().cloned().map(|mut w| {
                bridge_core::why_disconnected::with_firewall_blocking(
                    &mut w,
                    bridge_service::doctor::firewall_blocks(&read_firewall()),
                );
                w
            });
        }
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
                    message_limit: 0,
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

/// What a first-time user sees when nothing has run yet (the phrase "no control token yet" is matched by `doctor`).
const NO_SERVICE_YET: &str =
    "no control token yet: the background service is not running; run `geekbridge-ctl doctor`";

/// Plain words for the hotkey binding state; the raw word `unsupported` meant "this desktop cannot grab keys".
fn hotkey_binding_text(binding: &str) -> String {
    const BIND: &str = "bind `geekbridge-ctl clipboard paste-from-phone --notify` to a key";
    match binding {
        "bound" => "(works anywhere)".into(),
        "not_bound" => format!("(not registered by this desktop: {BIND})"),
        "unsupported" => format!("(works in the GeekBridge window only; for anywhere, {BIND})"),
        other => format!("({other})"),
    }
}

fn load_token(state_dir: &std::path::Path) -> Result<Token, String> {
    let path = ipc::token_path(state_dir);
    let bytes = bridge_kde::fsutil::read_private_file(&path)
        .map_err(|e| format!("cannot read the control token safely: {e}"))?
        .ok_or(NO_SERVICE_YET)?;
    Token::parse(&bytes).map_err(|e| e.to_string())
}

/// What can be read about the firewall without root.
fn read_firewall() -> bridge_service::doctor::Firewall {
    bridge_core::firewall::read()
}

/// `doctor`: run every check, print what to do about each problem, fail if one check failed.
/// The "Hear calls on this computer" checklist, read by the service (it has the telephony and
/// sound-server clients; ctl stays small). `None` when the service does not answer.
fn hear_calls(port: u16, state_dir: &std::path::Path) -> Option<bridge_core::ipc::HearCalls> {
    let t = load_token(state_dir).ok()?;
    match ipc::call(port, &t, &Request::HearCalls).ok()? {
        Response::HearCalls(h) if !h.steps.is_empty() => Some(*h),
        _ => None,
    }
}

/// Bluetooth controllers the kernel lists, for when the service is not running.
fn bluetooth_controllers() -> usize {
    std::fs::read_dir(
        std::env::var_os("GEEKBRIDGE_BT_SYSFS").unwrap_or_else(|| "/sys/class/bluetooth".into()),
    )
    .map(|d| {
        d.flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.strip_prefix("hci")
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            })
            .count()
    })
    .unwrap_or(0)
}

/// `geekbridge-ctl calling`: the service's Bluetooth calling summary, then the step-by-step
/// "Hear calls on this computer" checklist with a fix for every open step.
fn calling_command(port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    match load_token(state_dir)
        .map_err(|e| e.to_string())
        .and_then(|t| ipc::call(port, &t, &Request::CallingStatus).map_err(|e| e.to_string()))
    {
        Ok(Response::CallingStatus(c)) if !cfg!(target_os = "linux") => {
            println!(
                "calling: {}",
                bridge_service::doctor::native_calls_line(&c.platform)
            );
        }
        Ok(Response::CallingStatus(c)) => {
            println!(
                "calling: {} ({}), telephony service: {}, Bluetooth controllers: {}, phones: {}, calls: {}",
                c.verdict,
                c.backend,
                if c.telephony_service_present {
                    "present"
                } else {
                    "absent"
                },
                c.bluetooth_controllers
                    .map_or("unknown".to_owned(), |n| n.to_string()),
                c.phones,
                c.calls
            );
        }
        Ok(Response::Error(e)) => println!("calling: the service answered: {}", e.message),
        Ok(_) => println!("calling: unexpected reply from the service"),
        Err(e) => {
            println!("calling: the service is not reachable ({e}); checking this computer anyway")
        }
    }
    let hear = hear_calls(port, state_dir);
    if cfg!(target_os = "linux") {
        match &hear {
            Some(h) => println!("{}", h.banner()),
            None => println!(
                "{}",
                bridge_core::ipc::banner_line(bluetooth_controllers() == 0, false, None)
            ),
        }
    }
    match hear {
        _ if !cfg!(target_os = "linux") => print!(
            "{}",
            bridge_service::call_audio_host::platform_calling_text(
                std::env::consts::OS,
                hear.as_ref()
            )
        ),
        Some(h) => print!("{}", h.render()),
        None => {
            println!("Hear calls on this computer:");
            if bluetooth_controllers() == 0 {
                println!("  {}", bridge_core::ipc::NO_BLUETOOTH_WARNING);
            }
            println!(
                "  The full step-by-step check needs the GeekBridge service: systemctl --user start geekbridge-service, then run this again."
            );
        }
    }
    Ok(())
}

fn doctor_command(port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_core::phone_view::MessagesView;
    use bridge_service::doctor::{self, Inputs, SessionEnv};
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let firewall = read_firewall();
    let connected = load_token(state_dir)
        .map_err(|e| e.to_string())
        .and_then(|token| {
            let status = ipc::status(port, &token).map_err(|e| e.to_string())?;
            Ok((token, status))
        });
    let (status, clipboard, calling, data, callwifi) = match connected {
        Err(e) => (Err(e), None, None, Vec::new(), None),
        Ok((token, status)) => {
            let ask = |r: Request| ipc::call(port, &token, &r).ok();
            let clipboard = match ask(Request::ClipboardStatus) {
                Some(Response::ClipboardStatus(c)) => Some(*c),
                _ => None,
            };
            let calling = match ask(Request::CallingStatus) {
                Some(Response::CallingStatus(c)) => Some(c),
                _ => None,
            };
            let callwifi = match ask(Request::CallWifi {
                action: bridge_core::call_wifi_view::CallWifiAction::Status,
            }) {
                Some(Response::CallWifi(v)) => Some(*v),
                _ => None,
            };
            let data = match ask(Request::MessagesView {
                device_id: None,
                thread_id: None,
                query: None,
                message_limit: 1,
            }) {
                Some(Response::Messages(v)) => {
                    let MessagesView { devices, .. } = *v;
                    devices
                }
                _ => Vec::new(),
            };
            (Ok(status), clipboard, calling, data, callwifi)
        }
    };
    let mut checks = doctor::run(&Inputs {
        hear_calls: hear_calls(port, state_dir),
        status,
        clipboard,
        calling,
        data,
        callwifi,
        session: SessionEnv {
            wayland_display: set("WAYLAND_DISPLAY"),
            display: set("DISPLAY"),
            xdg_runtime_dir: set("XDG_RUNTIME_DIR"),
            hyprland_signature: set("HYPRLAND_INSTANCE_SIGNATURE"),
            desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        },
        firewall,
        systemd_unit: ["/usr/lib/systemd/user", "/etc/systemd/user"]
            .iter()
            .map(std::path::PathBuf::from)
            .chain(
                std::env::var_os("HOME")
                    .map(|h| std::path::Path::new(&h).join(".config/systemd/user")),
            )
            .any(|d| d.join("geekbridge-service.service").exists()),
    });
    let (ok, line) = bridge_service::call_overlay::doctor_line(state_dir);
    checks.push(doctor::Check {
        level: if ok {
            doctor::Level::Ok
        } else {
            doctor::Level::Warn
        },
        title: "Incoming-call overlay".into(),
        detail: line,
        fix: None,
    });
    print!("{}", doctor::render(&checks));
    match doctor::failures(&checks) {
        0 => Ok(()),
        n => Err(format!("doctor found {n} failed check(s)")),
    }
}

fn diagnose_command(port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    print!("{}", diagnose_text(port, state_dir)?);
    Ok(())
}

/// `report [--open|--copy]`: a prefilled bug report for the public tracker. Shows exactly what the
/// link carries; nothing is sent by this program (the browser does it, only with `--open`).
fn report_command(args: &[OsString], port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_core::report;
    let mode = args.first().and_then(|a| a.to_str()).unwrap_or("--preview");
    if !matches!(mode, "--preview" | "--open" | "--copy") || args.len() > 1 {
        return Err("usage: geekbridge-ctl report [--preview|--open|--copy]".into());
    }
    // A stopped service is itself worth reporting: fall back to a report that says so.
    let text = diagnose_text(port, state_dir).unwrap_or_else(|e| {
        format!(
            "GeekBridge support report (redacted)\nservice not reachable: {}\nhost: {} {}\n",
            bridge_bin_scrub(&e),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    });
    let link = report::desktop_link(&report::repo_from_env(), &text, env!("CARGO_PKG_VERSION"));
    println!("{}", link.preview());
    if link.truncated {
        println!("(Shortened to fit a link; `geekbridge-ctl diagnose` prints everything.)\n");
    }
    match mode {
        "--open" => {
            if report::open_in_browser(&link.url) {
                println!("Opened your browser. Review the form there before you submit it.");
            } else {
                let copied = report::copy_to_clipboard(&link.preview());
                println!(
                    "No browser could be opened.{} Open this link yourself:\n{}",
                    if copied {
                        " The report above is on your clipboard."
                    } else {
                        ""
                    },
                    link.url
                );
            }
        }
        "--copy" => {
            if report::copy_to_clipboard(&link.preview()) {
                println!("Copied the report above to your clipboard.");
            } else {
                return Err("no clipboard tool found (wl-copy, xclip, xsel, pbcopy, clip)".into());
            }
        }
        _ => {
            println!("Link: {}\n", link.url);
            println!("Nothing has been sent. `geekbridge-ctl report --open` opens this in your");
            println!("browser; GitHub receives it only if you press Submit there.");
        }
    }
    Ok(())
}

fn bridge_bin_scrub(text: &str) -> String {
    bridge_service::diagnose::scrub(text)
}

fn diagnose_text(port: u16, state_dir: &std::path::Path) -> Result<String, String> {
    use bridge_service::diagnose::{Inputs, render};
    let token = load_token(state_dir)?;
    let mut notes = Vec::new();
    let status = match ipc::call(port, &token, &Request::Status).map_err(|e| e.to_string())? {
        Response::Status(s) => *s,
        _ => return Err("unexpected reply from the service".into()),
    };
    let status_ids: Vec<String> = status.paired.iter().map(|d| d.device_id.clone()).collect();
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
    let call_trace = match ask(Request::CallTrace { clear: false }, "call trace") {
        Some(Response::CallTrace { events }) => Some(events),
        _ => None,
    };
    let host = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    Ok(render(
        &Inputs {
            status,
            events,
            calling,
            clipboard,
            update,
            phone_logs: phone_log_sizes(state_dir, &status_ids),
            call_trace,
            notes,
        },
        &host,
    ))
}

/// Sizes of the captured debug logs of the paired phones (their content is never read here).
fn phone_log_sizes(state_dir: &std::path::Path, ids: &[String]) -> Vec<(String, u64)> {
    let dir = bridge_service::phone_debug::log_dir(state_dir);
    ids.iter()
        .filter_map(|id| {
            let bytes = std::fs::metadata(dir.join(format!("{id}.log"))).ok()?.len();
            Some((id.clone(), bytes))
        })
        .collect()
}

fn phone_device_id(args: &[OsString]) -> Result<String, String> {
    let id = text(args.first(), "DEVICE_ID")?;
    // Also what keeps the id from naming a path.
    bridge_kde::DeviceId::parse(&id).map_err(|_| "DEVICE_ID is not a valid device id")?;
    Ok(id)
}

fn phone_log_command(args: &[OsString], state_dir: &std::path::Path) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let id = phone_device_id(args)?;
    let mut follow = false;
    let mut count = 200usize;
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.to_str() {
            Some("-f") | Some("--follow") => follow = true,
            Some("-n") => {
                count = rest
                    .next()
                    .and_then(|v| v.to_str())
                    .and_then(|v| v.parse().ok())
                    .ok_or("-n expects a number of lines")?;
            }
            _ => return Err(format!("unexpected argument\n\n{USAGE}")),
        }
    }
    let path = bridge_service::phone_debug::log_dir(state_dir).join(format!("{id}.log"));
    let mut file = std::fs::File::open(&path).map_err(|_| {
        format!(
            "no log captured yet for this phone ({}). Is it a debug build, connected, and is the debug consent on? \
             (geekbridge-ctl consent {id} debug on)",
            path.display()
        )
    })?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).map_err(|e| e.to_string())?;
    let all = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = all.lines().collect();
    let out = std::io::stdout();
    let mut out = out.lock();
    for l in &lines[lines.len().saturating_sub(count)..] {
        let _ = writeln!(out, "{l}");
    }
    let _ = out.flush();
    if !follow {
        return Ok(());
    }
    let mut pos = data.len() as u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() < pos {
            // Rotated: continue with the new live file from its start.
            pos = 0;
            file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
        }
        if meta.len() > pos {
            file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
            let mut chunk = Vec::new();
            file.read_to_end(&mut chunk).map_err(|e| e.to_string())?;
            pos += chunk.len() as u64;
            let _ = out.write_all(String::from_utf8_lossy(&chunk).as_bytes());
            let _ = out.flush();
        }
    }
}

fn notifications_command(
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let words: Vec<String> = args
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or("arguments must be text")
        })
        .collect::<Result<_, _>>()?;
    let token = load_token(state_dir)?;
    let send = |request: Request| match ipc::call(port, &token, &request) {
        Ok(Response::Error(e)) => Err(e.message),
        Ok(r) => Ok(r),
        Err(IpcError::Service(e)) => Err(e.message),
        Err(e) => Err(e.to_string()),
    };
    match words.first().map(String::as_str) {
        Some("dismiss") => {
            let (Some(device_id), Some(id)) = (words.get(1), words.get(2)) else {
                return Err("usage: notifications dismiss DEVICE_ID ID".into());
            };
            send(Request::DismissNotification {
                device_id: device_id.clone(),
                id: id.clone(),
            })?;
            println!("dismissed");
            Ok(())
        }
        Some("action") => {
            let (Some(device_id), Some(id), Some(action)) =
                (words.get(1), words.get(2), words.get(3))
            else {
                return Err("usage: notifications action DEVICE_ID ID TITLE".into());
            };
            send(Request::NotificationAction {
                device_id: device_id.clone(),
                id: id.clone(),
                action: action.clone(),
            })?;
            println!("sent");
            Ok(())
        }
        _ => {
            let json = words.iter().any(|w| w == "--json");
            let device_id = words.iter().find(|w| !w.starts_with("--")).cloned();
            let Response::Notifications(view) = send(Request::Notifications { device_id })? else {
                return Err("unexpected reply from the service".into());
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&*view).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            if view.enabled_devices.is_empty() {
                eprintln!(
                    "mirroring is off for every connected phone: geekbridge-ctl consent DEVICE_ID notifications on"
                );
            }
            for n in &view.items {
                println!(
                    "{}  {}  [{}] {}: {}{}",
                    n.device_id,
                    n.id,
                    n.device_name,
                    n.app,
                    n.title,
                    if n.text.is_empty() {
                        String::new()
                    } else {
                        format!(" - {}", n.text)
                    },
                );
                if !n.actions.is_empty() {
                    println!("    buttons: {}", n.actions.join(" | "));
                }
            }
            if view.items.is_empty() {
                println!("no mirrored notifications");
            }
            Ok(())
        }
    }
}

fn photos_command(args: &[OsString], port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    use bridge_core::photos_view::FetchMode;
    let words: Vec<String> = args
        .iter()
        .map(|a| {
            a.to_str()
                .map(str::to_owned)
                .ok_or("arguments must be text")
        })
        .collect::<Result<_, _>>()?;
    let token = load_token(state_dir)?;
    let send = |request: Request| match ipc::call(port, &token, &request) {
        Ok(Response::Error(e)) => Err(e.message),
        Ok(r) => Ok(r),
        Err(IpcError::Service(e)) => Err(e.message),
        Err(e) => Err(e.to_string()),
    };
    match words.first().map(String::as_str) {
        Some("refresh") => {
            let Some(device_id) = words.get(1) else {
                return Err("usage: photos refresh DEVICE_ID".into());
            };
            send(Request::PhotosRefresh {
                device_id: device_id.clone(),
            })?;
            println!("asked; run `geekbridge-ctl photos` in a moment");
            Ok(())
        }
        Some("thumb") => {
            use base64::Engine as _;
            let (Some(device_id), Some(id), Some(file)) =
                (words.get(1), words.get(2), words.get(3))
            else {
                return Err("usage: photos thumb DEVICE_ID PHOTO_ID FILE".into());
            };
            let Response::PhotoThumb(t) = send(Request::PhotoThumb {
                device_id: device_id.clone(),
                id: id.clone(),
            })?
            else {
                return Err("unexpected reply from the service".into());
            };
            let jpeg = base64::engine::general_purpose::STANDARD
                .decode(t.jpeg_base64)
                .map_err(|e| e.to_string())?;
            std::fs::write(file, &jpeg).map_err(|e| format!("cannot write {file}: {e}"))?;
            println!("{}x{} JPEG, {} bytes", t.width, t.height, jpeg.len());
            Ok(())
        }
        Some(mode @ ("save" | "copy")) => {
            let (Some(device_id), Some(id)) = (words.get(1), words.get(2)) else {
                return Err(format!("usage: photos {mode} DEVICE_ID PHOTO_ID"));
            };
            send(Request::PhotoFetch {
                device_id: device_id.clone(),
                id: id.clone(),
                mode: if mode == "save" {
                    FetchMode::Save
                } else {
                    FetchMode::Copy
                },
            })?;
            println!("asked the phone to send it; progress is in `geekbridge-ctl photos`");
            Ok(())
        }
        _ => {
            let json = words.iter().any(|w| w == "--json");
            let device_id = words
                .iter()
                .find(|w| !w.starts_with("--") && *w != "list")
                .cloned();
            let Response::Photos(view) = send(Request::Photos { device_id })? else {
                return Err("unexpected reply from the service".into());
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&*view).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            if view.devices.is_empty() {
                eprintln!(
                    "recent photos are off for every connected phone: geekbridge-ctl consent DEVICE_ID photos on"
                );
            }
            for d in &view.devices {
                if d.partial {
                    eprintln!(
                        "{}: the phone shows only the photos its user picked",
                        d.name
                    );
                }
            }
            for p in &view.items {
                println!(
                    "{}  {}  {}  {}x{}  {}{}",
                    p.device_id,
                    p.id,
                    p.name,
                    p.width,
                    p.height,
                    human_bytes(p.size),
                    if p.has_thumb {
                        ""
                    } else {
                        "  (no thumbnail yet)"
                    },
                );
            }
            for f in &view.fetches {
                println!(
                    "fetch {}  {}  {:?} {:?}{}",
                    f.fetch_id,
                    f.name,
                    f.mode,
                    f.state,
                    f.path
                        .as_deref()
                        .map(|p| format!("  {p}"))
                        .unwrap_or_default(),
                );
            }
            if view.items.is_empty() {
                println!("no photos held; run `geekbridge-ctl photos refresh DEVICE_ID`");
            }
            Ok(())
        }
    }
}

fn human_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / f64::from(1u32 << 20))
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
}

fn phone_state_command(
    args: &[OsString],
    port: u16,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let id = phone_device_id(args)?;
    let token = load_token(state_dir)?;
    match ipc::call(port, &token, &Request::PhoneDebugState { device_id: id }) {
        Ok(Response::PhoneDebugState { text }) => {
            println!("{}", text.trim_end());
            Ok(())
        }
        Ok(Response::Error(e)) => Err(e.message),
        Ok(_) => Err("unexpected reply from the service".into()),
        Err(IpcError::Service(e)) => Err(e.message),
        Err(e) => Err(e.to_string()),
    }
}

fn update_call(port: u16, token: &Token, request: &Request) -> Result<UpdateStatus, String> {
    match ipc::call(port, token, request).map_err(|e| e.to_string())? {
        Response::Update(status) => Ok(*status),
        Response::Error(body) => Err(body.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

fn describe_update(s: &UpdateStatus) {
    match &s.current_commit {
        Some(commit) => println!("running:  v{} ({commit})", s.current_version),
        None => println!("running:  v{}", s.current_version),
    }
    println!("channel:  {}", s.channel.as_str());
    println!(
        "checking: automatic {} (startup, then about daily)",
        if s.enabled { "on" } else { "off" }
    );
    match (&s.latest, s.newer) {
        (Some(latest), true) => {
            println!(
                "latest:   {}{} - newer{}",
                if s.channel == ipc::UpdateChannel::Dev {
                    ""
                } else {
                    "v"
                },
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
        (Some(latest), false) => println!(
            "latest:   {}{} - you are up to date",
            if s.channel == ipc::UpdateChannel::Dev {
                ""
            } else {
                "v"
            },
            latest.version
        ),
        (None, _) if s.state == UpdateState::NoPublicRelease => {
            println!("latest:   {}", ipc::UPDATE_NO_PUBLIC_RELEASE);
        }
        // A check finished but offered nothing (only drafts or prereleases exist): not "unknown".
        (None, _) if s.last_checked_unix.is_some() => {
            println!("latest:   no stable release published - you are up to date");
        }
        (None, _) => println!("latest:   unknown (no check has completed)"),
    }
    if let Some(path) = &s.downloaded_path {
        println!("verified download: {path}");
        if bridge_update::install::support_here().can_install() {
            println!("Install it with: geekbridge-ctl update install");
        } else {
            println!(
                "Install it yourself (see `geekbridge-ctl update status`); this copy does not replace its own files."
            );
        }
    }
    if let Some(error) = &s.error {
        println!("problem:  {error}");
    }
}

fn update_command(args: &[OsString], port: u16, state_dir: &std::path::Path) -> Result<(), String> {
    let sub = args.first().and_then(|a| a.to_str()).unwrap_or("status");
    if sub == "support" {
        return bridge_service::update_cli::support_command();
    }
    let token = load_token(state_dir)?;
    let (request, wait_for) = match sub {
        "install" => {
            return bridge_service::update_cli::install_command(
                &args[1..],
                port,
                &token,
                state_dir,
                &|r| update_call(port, &token, r),
            );
        }
        "rollback" => {
            return bridge_service::update_cli::rollback_command(
                &args[1..],
                port,
                &token,
                state_dir,
            );
        }
        "restart" => {
            return bridge_service::update_cli::restart_command(
                &args[1..],
                port,
                &token,
                state_dir,
            );
        }
        "status" => (Request::UpdateStatus, None),
        "check" => (Request::UpdateCheck, Some(UpdateState::Checking)),
        "download" => (Request::UpdateDownload, Some(UpdateState::Downloading)),
        "skip" => (
            Request::UpdateSkip {
                version: text(args.get(1), "VERSION")?,
            },
            None,
        ),
        "channel" => (
            Request::UpdateSetChannel {
                channel: ipc::UpdateChannel::parse(&text(args.get(1), "stable|dev")?)
                    .ok_or("expected stable or dev")?,
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
    if sub == "status" {
        println!(
            "install:  {}",
            bridge_service::update_cli::describe_support()
        );
        if let Some(last) = bridge_service::update_cli::last_outcome(state_dir) {
            println!("last install: {last}");
        }
    }
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

#[cfg(test)]
mod clipboard_words_tests {
    use super::*;

    fn pull(state: &str, reason: Option<&str>) -> ipc::ClipboardPullStatus {
        ipc::ClipboardPullStatus {
            device_id: "d".into(),
            state: state.into(),
            reason: reason.map(str::to_owned),
            kind: None,
            at_unix_ms: 0,
        }
    }

    #[test]
    fn the_hotkey_state_is_explained_not_printed_raw() {
        assert!(hotkey_binding_text("unsupported").contains("GeekBridge window"));
        assert!(!hotkey_binding_text("unsupported").contains("unsupported"));
        assert!(hotkey_binding_text("not_bound").contains("paste-from-phone"));
        assert!(hotkey_binding_text("bound").contains("anywhere"));
        assert!(!NO_SERVICE_YET.contains("state-dir"));
        assert!(NO_SERVICE_YET.contains("doctor"));
    }

    #[test]
    fn every_refusal_tells_the_person_what_to_do() {
        let locked = describe_pull(&pull("none", Some("locked")));
        assert_eq!(locked, "Phone is locked: unlock it and copy again.");
        let tap = describe_pull(&pull("none", Some("needs_gesture")));
        assert!(
            tap.contains("notification")
                && tap.contains("tile")
                && tap.contains("Allow GeekBridge notifications")
        );
        let late = describe_pull(&pull("none", Some("timeout")));
        assert!(late.contains("did not answer") && late.contains("tile"));
        assert!(describe_pull(&pull("none", Some("sensitive"))).contains("sensitive"));
        assert!(describe_pull(&pull("none", Some("disabled"))).contains("off in the phone app"));
        assert_eq!(
            describe_pull(&pull("waiting", None)),
            "Waiting for the phone..."
        );
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn help_lists_every_consent_feature() {
        for f in Feature::ALL {
            let name = serde_json::to_value(f).unwrap();
            let name = name.as_str().unwrap();
            let found = USAGE
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| w == name);
            assert!(
                found,
                "geekbridge-ctl --help does not list the consent feature {name}"
            );
        }
    }
}
