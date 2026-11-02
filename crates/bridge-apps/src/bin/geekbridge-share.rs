// Release builds on Windows are GUI-subsystem programs: no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `geekbridge-share [--theme light|dark|system] [FILE...]`: send files, links and text to a paired
//! phone and answer what it sends back. Files named on the command line go to the first
//! connected phone (handy for a file manager's "Open with").
use bridge_apps::{cli, share_app};
use std::process::ExitCode;

const HELP: &str = "geekbridge-share [--theme light|dark|system] [FILE...]

Opens the GeekBridge Share window. Drop files on it or press Choose files to send them to a
connected, paired phone. Files named here are sent to the first connected phone.
Incoming files wait for Accept unless you turn on auto-accept for that phone.
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut theme = cli::ThemeChoice::System;
    let mut files = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "--theme" => match args.next().as_deref().map(cli::parse_theme) {
                Some(Ok(t)) => theme = t,
                _ => {
                    eprintln!("--theme needs light, dark or system");
                    return ExitCode::from(2);
                }
            },
            other if other.starts_with("--") => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
            file => files.push(file.to_owned()),
        }
    }
    match share_app::run(theme, files) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("geekbridge-share: cannot open window: {e}");
            ExitCode::FAILURE
        }
    }
}
