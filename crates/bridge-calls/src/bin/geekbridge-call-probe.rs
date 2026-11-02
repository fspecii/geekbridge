// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-call-probe`: read-only inventory of the host's Bluetooth call backend.
//!
//! Prints structured evidence (JSON by default) about the telephony service, Bluetooth
//! controllers, phones, calls and declared capabilities. It never invokes a control method; see
//! `docs/call-control.md`.

use bridge_calls::probe::{self, BusChoice, ProbeOptions};

const USAGE: &str = "\
geekbridge-call-probe [--text] [--show-private] [--session-address ADDR] [--system-address ADDR]
                      [--no-session] [--no-system]

Read-only. Lists the call backend, Bluetooth controllers, phones, calls and the capabilities the
backend *declares*. Nothing is dialed, answered or changed.

  --text                  human-readable output instead of JSON
  --show-private          do not mask numbers, names and Bluetooth addresses
  --session-address ADDR  probe this D-Bus address instead of the session bus
  --system-address ADDR   probe this D-Bus address instead of the system bus
  --no-session/--no-system  skip a bus
";

fn main() {
    let mut options = ProbeOptions::default();
    let mut text = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--text" => text = true,
            "--show-private" => options.show_private = true,
            "--no-session" => options.session = BusChoice::Skip,
            "--no-system" => options.system = BusChoice::Skip,
            "--session-address" | "--system-address" => {
                let Some(addr) = args.next() else {
                    eprintln!("{arg} needs a value\n{USAGE}");
                    std::process::exit(2);
                };
                if arg == "--session-address" {
                    options.session = BusChoice::Address(addr);
                } else {
                    options.system = BusChoice::Address(addr);
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument {other:?}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let report = probe::run(&options);
    if text {
        print!("{}", probe::render_text(&report));
    } else {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("could not serialize report: {e}");
                std::process::exit(1);
            }
        }
    }
}
