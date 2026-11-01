//! Command line of the `geekbridge-macos-calls` helper.
//!
//! The default action (no arguments) is the read-only probe: it lists paired phones and
//! capabilities and exits. `serve` runs the long-lived JSON-lines helper. Nothing connects to a
//! phone or changes audio defaults unless a `connect` / `route_apply` request is received on
//! stdin in `serve` mode.
use crate::controller::Config;
use std::process::ExitCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Probe,
    Serve,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cli {
    pub action: Action,
    pub config: Config,
}

pub const USAGE: &str = "\
geekbridge-macos-calls [probe | serve] [options]

  probe   (default) read-only: authorization, paired phones, capabilities, then exit.
  serve   JSON-lines helper on stdin/stdout (protocol v1); see `docs/architecture.md`.

options:
  --allow-permission-prompt   let macOS show its Bluetooth permission prompt if undecided
  --allow-unadvertised        allow connecting to paired devices whose cached SDP lacks the
                              Hands-Free Audio Gateway role
  -h, --help
";

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut action = None;
    let mut config = Config::default();
    for arg in args {
        match arg.as_str() {
            "probe" | "serve" if action.is_some() => return Err("more than one action".into()),
            "probe" => action = Some(Action::Probe),
            "serve" => action = Some(Action::Serve),
            "-h" | "--help" => action = Some(Action::Help),
            "--allow-permission-prompt" => config.allow_permission_prompt = true,
            "--allow-unadvertised" => config.allow_unadvertised = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Cli {
        action: action.unwrap_or(Action::Probe),
        config,
    })
}

/// Exit code of a probe: 0 normal, 4 when the probe could not enumerate (permission, native error).
pub fn exit_for_probe(blocked: bool) -> ExitCode {
    ExitCode::from(if blocked { 4 } else { 0 })
}

/// JSON printed on hosts without a macOS Bluetooth stack.
pub fn unsupported_json() -> String {
    serde_json::json!({
        "protocol": crate::model::PROTOCOL_VERSION,
        "supported": false,
        "os": std::env::consts::OS,
        "error": "unsupported_host",
        "capabilities": crate::caps::unsupported_host(),
    })
    .to_string()
}

pub fn run(args: impl IntoIterator<Item = String>) -> ExitCode {
    let cli = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("geekbridge-macos-calls: {e}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    if cli.action == Action::Help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    run_host(cli)
}

#[cfg(target_os = "macos")]
fn run_host(cli: Cli) -> ExitCode {
    match cli.action {
        Action::Serve => crate::apple::serve(cli.config),
        Action::Probe | Action::Help => crate::apple::probe(cli.config),
    }
}

#[cfg(not(target_os = "macos"))]
fn run_host(_cli: Cli) -> ExitCode {
    println!("{}", unsupported_json());
    ExitCode::from(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn default_action_is_the_read_only_probe() {
        let c = p(&[]).unwrap();
        assert_eq!(c.action, Action::Probe);
        assert!(!c.config.allow_permission_prompt && !c.config.allow_unadvertised);
    }

    #[test]
    fn options_are_opt_in_and_unknown_arguments_fail() {
        let c = p(&["serve", "--allow-permission-prompt"]).unwrap();
        assert_eq!(c.action, Action::Serve);
        assert!(c.config.allow_permission_prompt);
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["probe", "serve"]).is_err());
        assert_eq!(p(&["--help"]).unwrap().action, Action::Help);
    }

    #[test]
    fn unsupported_json_is_explicit() {
        let v: serde_json::Value = serde_json::from_str(&unsupported_json()).unwrap();
        assert_eq!(v["supported"], false);
        assert_eq!(v["error"], "unsupported_host");
    }
}
