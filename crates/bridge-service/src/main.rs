// SPDX-License-Identifier: GPL-3.0-or-later
use bridge_service::config::{self, Parsed};

/// Workers for the async runtime. The service is I/O bound (a handful of sockets and small JSON
/// frames), so one worker per core only costs threads, stacks and idle wakeups. Blocking work uses
/// tokio's separate, lazily grown blocking pool.
const WORKER_THREADS: usize = 2;

fn main() -> std::io::Result<()> {
    // Started from a systemd user unit before the compositor exported its environment? Find the
    // session's Wayland socket / Hyprland instance / bus ourselves (docs/omarchy.md).
    bridge_service::session_env::adopt_session_env();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> std::io::Result<()> {
    match config::parse(std::env::args_os().skip(1)).map_err(std::io::Error::other)? {
        Parsed::Help => {
            println!("{}", config::USAGE);
            Ok(())
        }
        Parsed::PrintTokenPath(path) => {
            println!("{}", path.display());
            Ok(())
        }
        Parsed::Run(config) => {
            bridge_service::memtrim::after_startup();
            bridge_service::run(config).await
        }
    }
}
