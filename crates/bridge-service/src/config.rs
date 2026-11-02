// SPDX-License-Identifier: GPL-3.0-or-later
//! Service configuration: command-line flags with environment fallbacks for the state directory
//! and control port (the same lookups `bridge_core::ipc` offers to clients).

use bridge_core::ipc;
use std::{ffi::OsString, net::SocketAddr, path::PathBuf};

pub const USAGE: &str = "geekbridge-service [OPTIONS]

Runs the GeekBridge helper: passive KDE Connect discovery (public, read-only API) plus an
authenticated local control API for pairing and linking explicitly chosen phones.

Options:
  --state-dir DIR          private directory for identity, trust, consent and control token
                           (default: $GEEKBRIDGE_STATE_DIR or the per-user data directory)
  --api-port PORT          public read-only discovery API (default 17380)
  --control-port PORT      authenticated control API on 127.0.0.1 (default 17381,
                           or $GEEKBRIDGE_CONTROL_PORT)
  --discovery-port PORT    UDP port for passive discovery (default 1716)
  --name NAME              device name shown to phones (default GeekBridge)
  --listen-inbound ADDR    accept TCP connections from phones on ADDR (default 0.0.0.0:1716; when that
                           port is busy the next free one up to 1764 is used)
  --no-listen-inbound      do not accept connections from phones (outbound dialling only)
  --no-announce            do not announce this computer on the LAN (phones then cannot find it)
  --no-calling-probe       never touch the system bus for the calling status request
  --tray / --no-tray       show / hide the tray or menu-bar icon (default: shown, except with a private
                           --state-dir or $GEEKBRIDGE_STATE_DIR; $GEEKBRIDGE_NO_TRAY=1 hides it;
                           `geekbridge-ctl tray disable` hides it for good)
  --dev-loopback           developer only: allow loopback peers and any TCP port
  --print-token-path       print where the control token lives (never the token) and exit
  -h, --help               show this help

The control token is created on first start with owner-only permissions. If the state directory
cannot be opened privately the control API reports it as unavailable; it never falls back to
weaker storage.";

#[derive(Clone, Debug)]
pub struct Config {
    pub api_port: u16,
    pub discovery_port: u16,
    pub control_port: u16,
    pub state_dir: PathBuf,
    pub device_name: String,
    pub inbound: Option<SocketAddr>,
    /// `inbound` is the built-in default, so a busy port may fall back to the next one.
    pub inbound_auto: bool,
    /// Broadcast this computer's identity so phones list it by name.
    pub announce: bool,
    pub calling_probe: bool,
    pub dev_loopback: bool,
    /// Show a tray / menu-bar icon. On by default only for the default state directory, so test
    /// and demo services with a private `--state-dir` never put an icon on the user's bar;
    /// `--tray` forces it, `--no-tray` / `$GEEKBRIDGE_NO_TRAY` / `geekbridge-ctl tray disable` stop it.
    pub tray: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Config),
    Help,
    PrintTokenPath(PathBuf),
}

fn port(value: Option<OsString>, flag: &str) -> Result<u16, String> {
    value
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| format!("{flag} expects a port between 1 and 65535"))
}

impl PartialEq for Config {
    fn eq(&self, other: &Self) -> bool {
        self.api_port == other.api_port
            && self.discovery_port == other.discovery_port
            && self.control_port == other.control_port
            && self.state_dir == other.state_dir
            && self.device_name == other.device_name
            && self.inbound == other.inbound
            && self.inbound_auto == other.inbound_auto
            && self.announce == other.announce
            && self.calling_probe == other.calling_probe
            && self.dev_loopback == other.dev_loopback
            && self.tray == other.tray
    }
}
impl Eq for Config {}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut api_port = bridge_core::DEFAULT_API_PORT;
    let mut discovery_port = 1716;
    let mut control_port = ipc::control_port_from_env();
    let mut state_dir = None;
    let mut device_name = "GeekBridge".to_owned();
    let mut inbound = Some(SocketAddr::from(([0, 0, 0, 0], 1716)));
    let mut inbound_auto = true;
    let mut announce = true;
    let mut calling_probe = true;
    let mut dev_loopback = false;
    let mut tray = std::env::var_os("GEEKBRIDGE_NO_TRAY")
        .is_some_and(|v| !v.is_empty() && v != "0")
        .then_some(false);
    let mut print_token = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str().unwrap_or("") {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--api-port" => api_port = port(args.next(), "--api-port")?,
            "--discovery-port" => discovery_port = port(args.next(), "--discovery-port")?,
            "--control-port" => control_port = port(args.next(), "--control-port")?,
            "--state-dir" => {
                let dir = PathBuf::from(args.next().ok_or("--state-dir expects a directory")?);
                if !dir.is_absolute() {
                    return Err("--state-dir must be an absolute path".into());
                }
                state_dir = Some(dir);
            }
            "--name" => {
                device_name = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("--name expects a non-empty name")?;
            }
            "--no-listen-inbound" => {
                inbound = None;
                inbound_auto = false;
            }
            "--no-announce" => announce = false,
            "--listen-inbound" => {
                inbound_auto = false;
                inbound = Some(
                    args.next()
                        .and_then(|v| v.into_string().ok())
                        .and_then(|v| v.parse::<SocketAddr>().ok())
                        .ok_or("--listen-inbound expects ADDR:PORT")?,
                );
            }
            "--no-calling-probe" => calling_probe = false,
            "--dev-loopback" => dev_loopback = true,
            "--no-tray" => tray = Some(false),
            "--tray" => tray = Some(true),
            "--print-token-path" => print_token = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if api_port == control_port {
        return Err("--api-port and --control-port must differ".into());
    }
    let private_state = state_dir.is_some() || std::env::var_os(ipc::ENV_STATE_DIR).is_some();
    let state_dir = state_dir
        .or_else(ipc::default_state_dir)
        .ok_or("cannot determine a state directory; pass --state-dir")?;
    if print_token {
        return Ok(Parsed::PrintTokenPath(ipc::token_path(&state_dir)));
    }
    Ok(Parsed::Run(Config {
        api_port,
        discovery_port,
        control_port,
        state_dir,
        device_name,
        inbound,
        inbound_auto,
        announce,
        calling_probe,
        dev_loopback,
        tray: tray.unwrap_or(!private_state),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_flags_and_rejects_bad_ones() {
        let Parsed::Run(c) = parse(args(&[
            "--state-dir",
            "/tmp/gb",
            "--control-port",
            "4000",
            "--listen-inbound",
            "127.0.0.1:1716",
        ]))
        .unwrap() else {
            panic!()
        };
        assert_eq!(c.state_dir, PathBuf::from("/tmp/gb"));
        assert_eq!(c.control_port, 4000);
        assert!(c.calling_probe && !c.dev_loopback);
        assert_eq!(c.inbound, Some("127.0.0.1:1716".parse().unwrap()));
        for bad in [
            &["--control-port", "0"][..],
            &["--api-port", "x"],
            &["--state-dir", "relative"],
            &["--bogus"],
            &["--api-port", "5000", "--control-port", "5000"],
            &["--listen-inbound", "nope"],
        ] {
            assert!(parse(args(bad)).is_err(), "{bad:?}");
        }
        assert!(!c.inbound_auto && c.announce);
        assert_eq!(parse(args(&["-h"])).unwrap(), Parsed::Help);
        assert_eq!(
            parse(args(&["--state-dir", "/tmp/gb", "--print-token-path"])).unwrap(),
            Parsed::PrintTokenPath(PathBuf::from("/tmp/gb/ipc-token"))
        );
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    fn run(list: &[&str]) -> Config {
        let list: Vec<OsString> = list.iter().map(OsString::from).collect();
        match parse(list).unwrap() {
            Parsed::Run(c) => c,
            _ => panic!(),
        }
    }

    #[test]
    fn phones_can_find_and_dial_this_computer_by_default() {
        let c = run(&["--state-dir", "/tmp/gb"]);
        assert_eq!(c.inbound, Some("0.0.0.0:1716".parse().unwrap()));
        assert!(c.inbound_auto && c.announce);
    }

    #[test]
    fn listening_and_announcing_can_be_switched_off() {
        let c = run(&[
            "--state-dir",
            "/tmp/gb",
            "--no-listen-inbound",
            "--no-announce",
        ]);
        assert_eq!(c.inbound, None);
        assert!(!c.announce);
    }
}
