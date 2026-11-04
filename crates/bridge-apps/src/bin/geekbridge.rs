// SPDX-License-Identifier: GPL-3.0-or-later
// Release builds on Windows are GUI-subsystem programs: no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `geekbridge [--theme light|dark|system] [--api-port P]`: the single entry window. Shows phone
//! pairing, clipboard sync and its shortcut, a drop target for sending files, the update banner,
//! and launchers for the individual apps.
use bridge_apps::{cli, home_app};
use std::process::ExitCode;

const HELP: &str = "geekbridge [--theme light|dark|system] [--api-port PORT]

Opens the GeekBridge window: pair a phone, set the clipboard shortcut, drop files to send them,
see update notices. The background service (geekbridge-service) is started at login by the
installer; this window only talks to it.
";

fn main() -> ExitCode {
    match cli::parse_args(std::env::args().skip(1)) {
        Err(message) => {
            eprintln!("geekbridge: {message}\nTry 'geekbridge --help'.");
            ExitCode::from(2)
        }
        Ok(cli::Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Status { .. }) => {
            eprintln!("geekbridge: --status is not supported here; use geekbridge-ctl status");
            ExitCode::from(2)
        }
        Ok(cli::Command::Overlay { .. }) => {
            eprintln!("geekbridge: --overlay belongs to geekbridge-calls");
            ExitCode::from(2)
        }
        Ok(cli::Command::Run { api_port, theme }) => match home_app::run(api_port, theme) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("geekbridge: cannot open window: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
