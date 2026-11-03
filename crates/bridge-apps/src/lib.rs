// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared implementation for the four separate GeekBridge app executables.
pub mod a11y;
#[cfg(all(test, feature = "accesskit"))]
mod a11y_tests;
pub mod apps;
pub mod calendar_model;
pub mod call_feed;
pub mod call_ui;
pub mod cli;
pub mod clipboard_link;
pub mod connection;
pub mod dot_calendar;
pub mod dotmatrix;
pub mod home_app;
pub mod i18n;
pub mod icons;
pub mod identity;
pub mod input;
pub mod localtime;
#[cfg(feature = "memstats")]
pub mod memstats;
pub mod memtrim;
pub mod nothing;
pub mod onboarding;
pub mod pace;
pub mod phone_feed;
pub mod phone_ui;
pub mod poller;
pub mod prefill;
pub mod report_ui;
pub mod share_app;
pub mod shell;
pub mod state;
#[cfg(test)]
mod test_support;
pub mod theme;
pub mod update_ui;
pub mod why_ui;

use bridge_core::{AppKind, fetch_snapshot};
use std::process::ExitCode;

/// Entry point used by each binary: parses its own arguments, then shows one window.
pub fn main_for(app: AppKind) -> ExitCode {
    match cli::parse_args(std::env::args().skip(1)) {
        Err(message) => {
            eprintln!(
                "{}: {message}\nTry '{} --help'.",
                identity::binary_name(app),
                identity::binary_name(app)
            );
            ExitCode::from(2)
        }
        Ok(cli::Command::Help) => {
            print!("{}", cli::help_text(app));
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Status { api_port }) => {
            let model = state::DiscoveryModel::from_fetch(app, fetch_snapshot(api_port));
            print!("{}", cli::status_report(app, api_port, &model));
            ExitCode::from(cli::status_exit_code(&model) as u8)
        }
        Ok(cli::Command::Run { api_port, theme }) => {
            match shell::run(app, api_port, theme, apps::view_for(app)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("{}: cannot open window: {err}", identity::binary_name(app));
                    ExitCode::FAILURE
                }
            }
        }
    }
}
