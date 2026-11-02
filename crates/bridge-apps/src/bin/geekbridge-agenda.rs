// SPDX-License-Identifier: GPL-3.0-or-later
// Release builds on Windows are GUI-subsystem programs: no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    bridge_apps::main_for(bridge_core::AppKind::Agenda)
}
