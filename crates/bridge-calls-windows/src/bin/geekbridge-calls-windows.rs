//! Developer CLI for the Windows calling adapter.
//!
//! The default command, `diagnose`, is read-only: it reports contract, package identity, privacy,
//! paired phones, registration, line, calls and audio-route state and changes nothing. Anything
//! that touches Windows or the phone (`request-access`, `register`, `connect`, `unregister`,
//! `dial`, `answer`, ...) is its own subcommand and additionally needs `--execute`; without it the
//! command only prints what it would do. Output is redacted unless `--reveal` is given. Input is
//! bounded: at most 24 arguments of at most 128 bytes, numbers and tones at most 32 characters.
use bridge_calls_windows::engine::*;
use bridge_calls_windows::model::*;
use bridge_calls_windows::report::*;
use bridge_calls_windows::service::*;
use bridge_core::calls::Sensitive;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// `println!` that ignores a closed pipe (`| head`) instead of panicking.
macro_rules! say {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

const MAX_ARGS: usize = 24;
const MAX_ARG_LEN: usize = 128;
const MAX_WATCH_SECONDS: u64 = 3600;

const USAGE: &str = "\
geekbridge-calls-windows [COMMAND] [OPTIONS]

Read-only (no changes to Windows or the phone):
  diagnose                 report environment, phones, lines, calls, audio route (default)
  watch [--seconds N]      print a report whenever Windows signals a change (N <= 3600, default 60)

Explicit actions (each also needs --execute; without it, only a dry-run plan is printed):
  request-access --device D        PhoneLineTransportDevice.RequestAccessAsync
  register --device D              request access, then RegisterApp for this app only
  connect --device D               ConnectAsync (the phone must be registered)
  unregister --device D            UnregisterApp for this app and this phone only
  dial --device D --number N [--route-local]
  answer | reject | end | hold | resume | mute | unmute  [--device D] [--call C]
  tones --tones 12#* [--device D] [--call C]
  route --to local|remote [--device D] [--call C]

Options:
  --device D        device index (from diagnose) or fingerprint like 1a2b3c4d
  --call C          call fingerprint; optional when exactly one call fits
  --json            machine-readable output
  --reveal          show numbers, names and addresses (default: redacted)
  --allow-unpackaged  do not require package identity (experiment; likely denied by Windows)
  --strict          diagnose exits 3 unless calling control is ready
  --execute         actually perform an explicit action

Exit codes: 0 ok, 1 failed or uncertain, 2 usage error / refused / dry run, 3 not ready (--strict).
";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Cmd {
    Diagnose,
    Watch { seconds: u64 },
    RequestAccess,
    Register,
    Connect,
    Unregister,
    Dial { number: String, route_local: bool },
    Call(CallVerb),
    Tones(String),
    Route(AudioDevice),
}

/// Which calls an action applies to, and how to build its command for the chosen call.
type Fits = fn(&CallView) -> bool;
type Build = fn(CallRef) -> WinCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallVerb {
    Answer,
    Reject,
    End,
    Hold,
    Resume,
    Mute,
    Unmute,
}

#[derive(Debug)]
struct Args {
    cmd: Cmd,
    device: Option<String>,
    call: Option<String>,
    json: bool,
    reveal: bool,
    allow_unpackaged: bool,
    strict: bool,
    execute: bool,
}

fn usage_err(msg: &str) -> String {
    format!("{msg}\n\n{USAGE}")
}

fn bounded(arg: &str) -> Result<&str, String> {
    if arg.len() > MAX_ARG_LEN {
        return Err(usage_err("argument too long"));
    }
    if arg.chars().any(char::is_control) {
        return Err(usage_err("control characters are not allowed in arguments"));
    }
    Ok(arg)
}

fn parse(raw: Vec<String>) -> Result<Option<Args>, String> {
    if raw.len() > MAX_ARGS {
        return Err(usage_err("too many arguments"));
    }
    let mut it = raw
        .iter()
        .map(|s| bounded(s))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter();
    let mut verb: Option<&str> = None;
    let mut args = Args {
        cmd: Cmd::Diagnose,
        device: None,
        call: None,
        json: false,
        reveal: false,
        allow_unpackaged: false,
        strict: false,
        execute: false,
    };
    let (mut seconds, mut number, mut tones, mut to) = (60u64, None, None, None);
    let mut route_local = false;
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .map(str::to_owned)
                .ok_or_else(|| usage_err(&format!("{name} needs a value")))
        };
        match a {
            "-h" | "--help" | "help" => return Ok(None),
            "--json" => args.json = true,
            "--reveal" => args.reveal = true,
            "--allow-unpackaged" => args.allow_unpackaged = true,
            "--strict" => args.strict = true,
            "--execute" => args.execute = true,
            "--route-local" => route_local = true,
            "--device" => args.device = Some(value("--device")?),
            "--call" => args.call = Some(value("--call")?),
            "--number" => number = Some(value("--number")?),
            "--tones" => tones = Some(value("--tones")?),
            "--to" => to = Some(value("--to")?),
            "--seconds" => {
                seconds = value("--seconds")?
                    .parse()
                    .map_err(|_| usage_err("--seconds must be a whole number"))?;
            }
            flag if flag.starts_with('-') => {
                return Err(usage_err(&format!("unknown option {flag}")));
            }
            word if verb.is_none() => verb = Some(word),
            _ => return Err(usage_err("unexpected extra argument")),
        }
    }
    args.cmd = match verb.unwrap_or("diagnose") {
        "diagnose" => Cmd::Diagnose,
        "watch" => {
            if seconds == 0 || seconds > MAX_WATCH_SECONDS {
                return Err(usage_err("--seconds must be between 1 and 3600"));
            }
            Cmd::Watch { seconds }
        }
        "request-access" => Cmd::RequestAccess,
        "register" => Cmd::Register,
        "connect" => Cmd::Connect,
        "unregister" => Cmd::Unregister,
        "dial" => Cmd::Dial {
            number: number.ok_or_else(|| usage_err("dial needs --number"))?,
            route_local,
        },
        "answer" => Cmd::Call(CallVerb::Answer),
        "reject" => Cmd::Call(CallVerb::Reject),
        "end" => Cmd::Call(CallVerb::End),
        "hold" => Cmd::Call(CallVerb::Hold),
        "resume" => Cmd::Call(CallVerb::Resume),
        "mute" => Cmd::Call(CallVerb::Mute),
        "unmute" => Cmd::Call(CallVerb::Unmute),
        "tones" => Cmd::Tones(tones.ok_or_else(|| usage_err("tones needs --tones"))?),
        "route" => match to.as_deref() {
            Some("local") => Cmd::Route(AudioDevice::Local),
            Some("remote") => Cmd::Route(AudioDevice::Remote),
            _ => return Err(usage_err("route needs --to local|remote")),
        },
        other => return Err(usage_err(&format!("unknown command {other}"))),
    };
    let needs_device = matches!(
        args.cmd,
        Cmd::RequestAccess | Cmd::Register | Cmd::Connect | Cmd::Unregister | Cmd::Dial { .. }
    );
    if needs_device && args.device.is_none() {
        return Err(usage_err("this action needs an explicit --device"));
    }
    Ok(Some(args))
}

fn is_action(cmd: &Cmd) -> bool {
    !matches!(cmd, Cmd::Diagnose | Cmd::Watch { .. })
}

fn describe(cmd: &Cmd) -> String {
    match cmd {
        Cmd::RequestAccess => "request transport access".into(),
        Cmd::Register => "request access and register this app for the phone".into(),
        Cmd::Connect => "ask Windows to connect to the phone".into(),
        Cmd::Unregister => "unregister this app from the phone".into(),
        Cmd::Dial { route_local, .. } => format!(
            "place a call{}",
            if *route_local {
                " and route its audio to this PC"
            } else {
                ""
            }
        ),
        Cmd::Call(v) => format!("{v:?}").to_lowercase() + " a call",
        Cmd::Tones(t) => format!("send {} DTMF tone(s)", t.chars().count()),
        Cmd::Route(d) => format!("route call audio to {d:?}").to_lowercase(),
        Cmd::Diagnose | Cmd::Watch { .. } => "read state".into(),
    }
}

/// Resolve `--device` by 1-based index or fingerprint.
fn pick_device(snap: &Snapshot, sel: Option<&str>) -> Result<TransportDeviceId, String> {
    match sel {
        Some(s) => {
            let by_index = s
                .parse::<usize>()
                .ok()
                .and_then(|i| snap.devices.get(i.wrapping_sub(1)));
            let by_fp = || {
                snap.devices
                    .iter()
                    .find(|d| d.id.fingerprint() == s.trim_start_matches('#'))
            };
            by_index
                .or_else(by_fp)
                .map(|d| d.id.clone())
                .ok_or_else(|| format!("no device matches '{s}'; run diagnose to list devices"))
        }
        None => match snap.devices.as_slice() {
            [only] => Ok(only.id.clone()),
            [] => Err("no phone transports found; run diagnose".into()),
            _ => Err("several phones found; choose one with --device".into()),
        },
    }
}

fn pick_call(
    dev: &DeviceView,
    sel: Option<&str>,
    fits: impl Fn(&CallView) -> bool,
) -> Result<CallRef, String> {
    if let Some(s) = sel {
        return dev
            .calls
            .iter()
            .find(|c| c.reference.fingerprint() == s.trim_start_matches('#'))
            .map(|c| c.reference.clone())
            .ok_or_else(|| format!("no call matches '{s}'"));
    }
    let candidates: Vec<&CallView> = dev.calls.iter().filter(|c| fits(c)).collect();
    match candidates.as_slice() {
        [one] => Ok(one.reference.clone()),
        [] => Err("no call is in a state this action applies to".into()),
        many => Err(format!(
            "{} calls fit; choose one with --call ({})",
            many.len(),
            many.iter()
                .map(|c| c.reference.fingerprint())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn status_of(c: &CallView) -> NativeCallStatus {
    c.record.status
}

fn print_value(args: &Args, value: serde_json::Value, text: impl FnOnce() -> String) {
    if args.json {
        say!("{value:#}");
    } else {
        say!("{}", text());
    }
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1).collect()) {
        Ok(Some(a)) => a,
        Ok(None) => {
            say!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let redactor = if args.reveal {
        Redactor::reveal()
    } else {
        Redactor::redacted()
    };
    let mut cfg = ServiceConfig::default();
    cfg.engine.policy.require_package_identity = !args.allow_unpackaged;
    let service = match bridge_calls_windows::spawn_default(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not start the calling service: {e}");
            return ExitCode::from(1);
        }
    };
    let handle = service.handle();

    let snapshot = match handle.refresh() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("refresh failed: {e}");
            return ExitCode::from(1);
        }
    };

    match &args.cmd {
        Cmd::Diagnose => {
            let ready = snapshot.devices.iter().any(|d| d.readiness.control_ready());
            print_value(
                &args,
                {
                    let mut v = snapshot_json(&snapshot, &redactor);
                    v["actions_performed"] = serde_json::json!([]);
                    v
                },
                || {
                    snapshot_text(&snapshot, &redactor)
                        + "No changes were made to Windows or the phone.\n"
                },
            );
            if args.strict && !ready {
                return ExitCode::from(3);
            }
            ExitCode::SUCCESS
        }
        Cmd::Watch { seconds } => watch(&args, &handle, *seconds, &redactor),
        cmd => act(&args, cmd, &handle, snapshot, &redactor),
    }
}

fn watch(args: &Args, handle: &CallsHandle, seconds: u64, r: &Redactor) -> ExitCode {
    let events = handle.subscribe();
    if let Err(e) = handle.watch(true) {
        eprintln!("watch failed: {e}");
        return ExitCode::from(1);
    }
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut last = String::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let Ok(ServiceEvent::Snapshot(s)) = events.recv_timeout(left.min(Duration::from_secs(1)))
        else {
            continue;
        };
        let rendered = if args.json {
            snapshot_json(&s, r).to_string()
        } else {
            snapshot_text(&s, r)
        };
        // Only print when something other than the refresh counter changed.
        let key = rendered.replace(&format!("refresh #{}", s.seq), "");
        if key != last {
            say!("{rendered}");
            last = key;
        }
    }
    let _ = handle.watch(false);
    ExitCode::SUCCESS
}

fn act(args: &Args, cmd: &Cmd, handle: &CallsHandle, snapshot: Snapshot, r: &Redactor) -> ExitCode {
    debug_assert!(is_action(cmd));
    let id = match pick_device(&snapshot, args.device.as_deref()) {
        Ok(id) => id,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    if !args.execute {
        say!(
            "dry run: would {} on device [{}]. Nothing was changed. Re-run with --execute to perform it.",
            describe(cmd),
            r.device(&id)
        );
        return ExitCode::from(2);
    }
    let snapshot = match handle.select(&id) {
        Ok(s) => s,
        Err(e) => return fail(args, &e),
    };
    let dev = snapshot
        .device(&id)
        .expect("selected device is in the snapshot");

    let command = match cmd {
        Cmd::RequestAccess => {
            return finish(
                args,
                handle.request_access().map(|a| format!("access: {a:?}")),
            );
        }
        Cmd::Register => {
            return finish(args, handle.register().map(|o| format!("register: {o:?}")));
        }
        Cmd::Connect => {
            return finish(
                args,
                handle.connect().map(|c| {
                    format!(
                        "connect attempt: {:?}; line visible afterwards: {}",
                        c.history, c.line_visible
                    )
                }),
            );
        }
        Cmd::Unregister => {
            return finish(
                args,
                handle.unregister().map(|o| format!("unregister: {o:?}")),
            );
        }
        Cmd::Dial {
            number,
            route_local,
        } => WinCommand::Dial {
            number: Sensitive::new(number.clone()),
            route_to_local: *route_local,
        },
        Cmd::Call(verb) => {
            let (fits, build): (Fits, Build) = match verb {
                CallVerb::Answer => (
                    |c| status_of(c) == NativeCallStatus::Incoming,
                    WinCommand::Answer,
                ),
                CallVerb::Reject => (
                    |c| status_of(c) == NativeCallStatus::Incoming,
                    WinCommand::Reject,
                ),
                CallVerb::End => (
                    |c| status_of(c) != NativeCallStatus::Incoming,
                    WinCommand::End,
                ),
                CallVerb::Hold => (
                    |c| status_of(c) == NativeCallStatus::Talking,
                    WinCommand::Hold,
                ),
                CallVerb::Resume => (
                    |c| status_of(c) == NativeCallStatus::Held,
                    WinCommand::Resume,
                ),
                CallVerb::Mute => (
                    |c| status_of(c) == NativeCallStatus::Talking,
                    |call| WinCommand::SetMute { call, muted: true },
                ),
                CallVerb::Unmute => (
                    |c| status_of(c) == NativeCallStatus::Talking,
                    |call| WinCommand::SetMute { call, muted: false },
                ),
            };
            match pick_call(dev, args.call.as_deref(), fits) {
                Ok(call) => build(call),
                Err(m) => {
                    eprintln!("{m}");
                    return ExitCode::from(2);
                }
            }
        }
        Cmd::Tones(tones) => match pick_call(dev, args.call.as_deref(), |c| {
            status_of(c) == NativeCallStatus::Talking
        }) {
            Ok(call) => WinCommand::SendTones {
                call,
                tones: Sensitive::new(tones.clone()),
            },
            Err(m) => {
                eprintln!("{m}");
                return ExitCode::from(2);
            }
        },
        Cmd::Route(to) => match pick_call(dev, args.call.as_deref(), |c| status_of(c).is_live()) {
            Ok(call) => WinCommand::RouteAudio { call, to: *to },
            Err(m) => {
                eprintln!("{m}");
                return ExitCode::from(2);
            }
        },
        Cmd::Diagnose | Cmd::Watch { .. } => unreachable!("read-only commands do not reach act"),
    };

    match handle.execute(command) {
        Ok(report) => {
            let ok = report.outcome.is_success();
            print_value(args, command_report_json(&report, r), || {
                format!(
                    "{}: {:?}{}",
                    report.action,
                    report.outcome,
                    report.after.as_ref().map_or(String::new(), |c| format!(
                        "; call now {:?}",
                        c.record.status
                    ))
                )
            });
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => fail(args, &e),
    }
}

fn finish(args: &Args, result: Result<String, ServiceError>) -> ExitCode {
    match result {
        Ok(text) => {
            print_value(args, serde_json::json!({ "result": text }), || text.clone());
            ExitCode::SUCCESS
        }
        Err(e) => fail(args, &e),
    }
}

fn fail(args: &Args, e: &ServiceError) -> ExitCode {
    if args.json {
        say!(
            "{}",
            serde_json::to_string_pretty(e).unwrap_or_else(|_| e.to_string())
        );
    } else {
        eprintln!("{e}");
    }
    let refused = matches!(
        e,
        ServiceError::Calls {
            error: CallsError::Refused { .. }
        }
    );
    ExitCode::from(if refused { 2 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Option<Args>, String> {
        parse(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn default_command_is_the_read_only_diagnose() {
        let a = p(&[]).unwrap().unwrap();
        assert_eq!(a.cmd, Cmd::Diagnose);
        assert!(!a.execute);
        assert!(!is_action(&a.cmd));
    }

    #[test]
    fn actions_are_separate_subcommands_and_need_a_device() {
        assert!(p(&["register"]).is_err());
        let a = p(&["register", "--device", "1"]).unwrap().unwrap();
        assert_eq!(a.cmd, Cmd::Register);
        assert!(is_action(&a.cmd));
        assert!(!a.execute, "--execute is never implied");
    }

    #[test]
    fn input_is_bounded_and_strict() {
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["diagnose", "extra"]).is_err());
        assert!(p(&[&"x".repeat(MAX_ARG_LEN + 1)]).is_err());
        assert!(p(&["dial\n"]).is_err());
        let many: Vec<&str> = vec!["--json"; MAX_ARGS + 1];
        assert!(p(&many).is_err());
        assert!(p(&["watch", "--seconds", "0"]).is_err());
        assert!(p(&["watch", "--seconds", "99999"]).is_err());
        assert!(p(&["route", "--to", "sideways"]).is_err());
        assert!(p(&["dial", "--device", "1"]).is_err());
    }

    #[test]
    fn help_prints_usage() {
        assert!(p(&["--help"]).unwrap().is_none());
    }
}
