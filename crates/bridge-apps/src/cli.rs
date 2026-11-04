// SPDX-License-Identifier: GPL-3.0-or-later
//! Argument handling shared by the four executables. Each binary parses its own arguments.
use crate::{identity::binary_name, state::DiscoveryModel};
use bridge_core::{AppKind, DEFAULT_API_PORT};

/// Appearance override for the window; follows the system unless told otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    /// Print one discovery status report and exit; no window.
    Status {
        api_port: u16,
    },
    Run {
        api_port: u16,
        theme: ThemeChoice,
    },
    /// `geekbridge-calls --overlay`: the small incoming-call card instead of the app window.
    Overlay {
        api_port: u16,
        theme: ThemeChoice,
        /// Show the caller's name only, never the number.
        name_only: bool,
        /// A fake ring for 10 seconds with disabled buttons (`geekbridge-ctl call-overlay-test`).
        test: bool,
    },
}

pub fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| format!("invalid port '{value}': expected a number from 1 to 65535"))
}

pub fn parse_theme(value: &str) -> Result<ThemeChoice, String> {
    match value {
        "system" => Ok(ThemeChoice::System),
        "light" => Ok(ThemeChoice::Light),
        "dark" => Ok(ThemeChoice::Dark),
        _ => Err(format!(
            "invalid theme '{value}': expected light, dark or system"
        )),
    }
}

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut args = args.into_iter();
    let (mut port, mut help, mut status) = (DEFAULT_API_PORT, false, false);
    let (mut overlay, mut name_only, mut test) = (false, false, false);
    let mut theme = ThemeChoice::System;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => help = true,
            "--status" => status = true,
            "--overlay" => overlay = true,
            "--name-only" => name_only = true,
            "--test" => test = true,
            "--api-port" => {
                let value = args.next().ok_or("--api-port needs a port number")?;
                port = parse_port(&value)?;
            }
            "--theme" => {
                let value = args.next().ok_or("--theme needs light, dark or system")?;
                theme = parse_theme(&value)?;
            }
            other => match (
                other.strip_prefix("--api-port="),
                other.strip_prefix("--theme="),
            ) {
                (Some(value), _) => port = parse_port(value)?,
                (_, Some(value)) => theme = parse_theme(value)?,
                _ => return Err(format!("unknown argument: {other}")),
            },
        }
    }
    Ok(if help {
        Command::Help
    } else if status {
        Command::Status { api_port: port }
    } else if overlay {
        Command::Overlay {
            api_port: port,
            theme,
            name_only,
            test,
        }
    } else {
        Command::Run {
            api_port: port,
            theme,
        }
    })
}

pub fn help_text(app: AppKind) -> String {
    let bin = binary_name(app);
    format!(
        "{bin} - GeekBridge {name}: {subtitle}\n\n\
         USAGE:\n    {bin} [--api-port PORT] [--theme light|dark|system] [--status]\n    {bin} --help\n\n\
         OPTIONS:\n    \
         --api-port PORT   Local geekbridge-service API port (default {DEFAULT_API_PORT})\n    \
         --theme MODE      Appearance: light, dark or system (default system)\n    \
         --status          Print service and device discovery status, then exit\n    \
         -h, --help        Show this help\n{extra}\n\
         This app opens its own window. It reads only public discovery data from the local\n\
         service; pairing is not implemented, so phone features are unavailable.\n",
        name = app.name(),
        subtitle = app.subtitle(),
        extra = if app == AppKind::Calls {
            "    --overlay         Show only the small incoming-call card (started by the service)\n    \
             --name-only       With --overlay: never show the caller's number\n    \
             --test            With --overlay: a fake ring for 10 seconds, buttons off\n"
        } else {
            ""
        }
    )
}

/// Plain-text report for `--status`. Never claims a device is paired or usable.
pub fn status_report(app: AppKind, api_port: u16, model: &DiscoveryModel) -> String {
    let mut out = format!("{} (127.0.0.1:{api_port})\n", binary_name(app));
    let headline = model.headline();
    out.push_str(&format!("  {}\n  {}\n", headline.title, headline.detail));
    for device in model.devices() {
        out.push_str(&format!(
            "  - {} [{}] {} - {}\n",
            device.name,
            device.kind,
            device.address,
            if device.trusted {
                "linked"
            } else {
                "discovered, not linked"
            }
        ));
    }
    out
}

/// Exit code for `--status`: 0 when the service answered, 1 otherwise.
pub fn status_exit_code(model: &DiscoveryModel) -> i32 {
    if model.service_reachable() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DiscoveryModel;
    fn parse(args: &[&str]) -> Result<Command, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn defaults_and_flags() {
        assert_eq!(
            parse(&[]),
            Ok(Command::Run {
                api_port: DEFAULT_API_PORT,
                theme: ThemeChoice::System
            })
        );
        assert_eq!(
            parse(&["--status"]),
            Ok(Command::Status {
                api_port: DEFAULT_API_PORT
            })
        );
        assert_eq!(
            parse(&["--api-port", "9000", "--status"]),
            Ok(Command::Status { api_port: 9000 })
        );
        assert_eq!(
            parse(&["--api-port=9001", "--theme", "dark"]),
            Ok(Command::Run {
                api_port: 9001,
                theme: ThemeChoice::Dark
            })
        );
        assert_eq!(
            parse(&["--theme=light"]),
            Ok(Command::Run {
                api_port: DEFAULT_API_PORT,
                theme: ThemeChoice::Light
            })
        );
        assert_eq!(parse(&["--status", "-h"]), Ok(Command::Help));
        assert_eq!(
            parse(&["--overlay", "--api-port", "9000", "--name-only"]),
            Ok(Command::Overlay {
                api_port: 9000,
                theme: ThemeChoice::System,
                name_only: true,
                test: false
            })
        );
        assert!(matches!(
            parse(&["--overlay"]),
            Ok(Command::Overlay {
                name_only: false,
                ..
            })
        ));
    }

    #[test]
    fn rejects_bad_input() {
        for args in [
            &["--api-port"][..],
            &["--api-port", "0"],
            &["--api-port", "65536"],
            &["--api-port=x"],
            &["--theme"],
            &["--theme=blue"],
            &["--bogus"],
            &["extra"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn help_is_per_app() {
        let text = help_text(AppKind::Clipboard);
        assert!(
            text.contains("geekbridge-clipboard")
                && text.contains("--api-port")
                && text.contains("--status")
        );
        assert!(!text.contains("geekbridge-calls"));
    }

    #[test]
    fn unreachable_status_is_honest() {
        let model = DiscoveryModel::service_unavailable("connection refused".into());
        let report = status_report(AppKind::Calls, 17380, &model);
        assert!(report.contains("geekbridge-calls") && report.contains("isn't running"));
        assert!(!report.contains("paired\n"));
        assert_eq!(status_exit_code(&model), 1);
    }
}
