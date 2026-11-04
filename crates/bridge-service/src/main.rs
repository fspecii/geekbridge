// SPDX-License-Identifier: GPL-3.0-or-later
use bridge_service::config::{self, Parsed};

/// Workers for the async runtime. The service is I/O bound (a handful of sockets and small JSON
/// frames), so one worker per core only costs threads, stacks and idle wakeups. Blocking work uses
/// tokio's separate, lazily grown blocking pool.
const WORKER_THREADS: usize = 2;

/// True when the console this process is attached to belongs to it alone: it was started by Explorer,
/// the `Run` registry key or the Task Scheduler (Windows gives a console program a fresh console then),
/// not from a shell, which would be attached as well.
#[cfg(any(windows, test))]
fn owns_its_console(attached_processes: u32) -> bool {
    attached_processes == 1
}

/// `geekbridge-service.exe` is a console program (its `--help` and logs go to a terminal), so started at
/// login it would leave a console window open for as long as the service runs, and closing that window
/// would kill the service. Give the console up when nobody else shares it. Redirected output (CI, a log
/// file) is not a console handle and is unaffected.
#[cfg(windows)]
fn release_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0_u32; 4];
    // SAFETY: both calls only read or change this process's console attachment; `pids` is a local
    // buffer whose length is passed along.
    unsafe {
        if owns_its_console(GetConsoleProcessList(&mut pids)) {
            let _ = FreeConsole();
        }
    }
}

fn main() -> std::io::Result<()> {
    #[cfg(windows)]
    release_own_console();
    bridge_service::memtrim::single_arena();
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

#[cfg(test)]
mod tests {
    use super::owns_its_console;

    #[test]
    fn a_console_shared_with_a_shell_is_kept() {
        assert!(owns_its_console(1));
        assert!(
            !owns_its_console(2),
            "cmd.exe or PowerShell is attached too"
        );
        assert!(
            !owns_its_console(0),
            "the call failed or there is no console"
        );
    }
}
