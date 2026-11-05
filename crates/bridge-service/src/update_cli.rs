// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl update install | rollback | restart | support`: the self-update commands for a
//! per-user Linux install. The rules live in `bridge_update::install` and
//! `docs/design/desktop-self-update.md`; this is only argument handling, prompts and wording.
use bridge_core::ipc::{Request, Token, UpdateStatus};
use bridge_update::install::{self, Support};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

// Only the Linux commands read these; the other platforms print a fixed message.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Default)]
struct Flags {
    yes: bool,
    dry_run: bool,
    no_restart: bool,
    force: bool,
    prefix: Option<PathBuf>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn flags(args: &[OsString]) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str().unwrap_or("") {
            "--yes" | "-y" => f.yes = true,
            "--dry-run" => f.dry_run = true,
            "--no-restart" => f.no_restart = true,
            "--force" => f.force = true,
            "--prefix" => {
                let p = PathBuf::from(it.next().ok_or("--prefix expects a directory")?);
                if !p.is_absolute() || p == Path::new("/") {
                    return Err("--prefix must be an absolute directory other than /".into());
                }
                f.prefix = Some(p);
            }
            other => return Err(format!("unknown option for update: {other}")),
        }
    }
    Ok(f)
}

fn support(f: &Flags) -> Support {
    match &f.prefix {
        Some(p) => install::support_for(p, None),
        None => install::support_here(),
    }
}

pub fn describe_support() -> String {
    support(&Flags::default()).message()
}

/// `update support`: one machine-readable word for the Settings button, then the sentence.
pub fn support_command() -> Result<(), String> {
    let s = support(&Flags::default());
    let word = match &s {
        Support::UserInstall(_) => "user-install",
        Support::PackageManaged(_) => "package-managed",
        Support::DownloadOnly(_) => "download-only",
    };
    println!("{word}\n{}", s.message());
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use bridge_core::ipc::{UpdateChannel, UpdateState};
    use install::{Error, IpcProbe, Restarted};
    use std::time::Duration;

    fn prefix_of(s: Support) -> Result<PathBuf, String> {
        match s {
            Support::UserInstall(p) => Ok(p),
            other => Err(other.message()),
        }
    }

    fn state_file(state_dir: &Path) -> PathBuf {
        state_dir.join("update-state.json")
    }

    fn restart_flow(
        f: &Flags,
        probe: &IpcProbe,
        prefix: &Path,
        version: &str,
        auto_rollback: bool,
        state_dir: &Path,
    ) -> Result<(), String> {
        if f.no_restart {
            println!(
                "The service keeps running the old version until it is restarted: geekbridge-ctl update restart (or systemctl --user restart geekbridge-service)."
            );
            return Ok(());
        }
        use bridge_update::install::Probe;
        if probe.running_version().as_deref() == Some(version) {
            println!("The service already runs v{version}.");
            return Ok(());
        }
        if !f.yes
            && !ask(&format!(
                "Restart the GeekBridge service now to run v{version}?"
            ))
        {
            println!("Not restarted. When you are ready: geekbridge-ctl update restart");
            return Ok(());
        }
        match install::restart(
            probe,
            prefix,
            version,
            Duration::from_secs(20),
            auto_rollback,
            Some(&state_file(state_dir)),
        ) {
            Ok(Restarted::Restarted) => println!("The service was restarted and runs v{version}."),
            Ok(Restarted::NotRunning) => {
                println!("The service is not running; v{version} starts the next time it does.")
            }
            // The files are in place; only the restart waits.
            Err(e @ Error::CallInProgress(_)) => {
                println!("{e}\nTry again with: geekbridge-ctl update restart")
            }
            // Not started by the systemd unit (run-desktop.sh, a shell): the service is left alone.
            Err(Error::Unsupported(why)) => {
                println!(
                    "Restart manually: {why}. v{version} runs after you restart geekbridge-service."
                )
            }
            Err(e) => return Err(e.to_string()),
        }
        Ok(())
    }

    pub fn install_command(
        args: &[OsString],
        port: u16,
        token: &Token,
        state_dir: &Path,
        call: &dyn Fn(&Request) -> Result<UpdateStatus, String>,
    ) -> Result<(), String> {
        let f = flags(args)?;
        let prefix = prefix_of(support(&f))?;
        let mut status = call(&Request::UpdateStatus)?;
        if status.channel == UpdateChannel::Dev {
            return Err("The dev channel is download only: GeekBridge verifies the dev build but does not install it. Switch to the stable channel (geekbridge-ctl update channel stable) or unpack it yourself.".into());
        }
        if status.state != UpdateState::Downloaded {
            if !status.downloadable {
                return Err(match &status.latest {
                    Some(l) if status.newer => format!(
                        "v{} is available but has no verified installer for this system: {}",
                        l.version, l.release_url
                    ),
                    _ => format!(
                        "GeekBridge v{} is up to date; there is nothing to install.",
                        status.current_version
                    ),
                });
            }
            println!("Downloading and verifying the update...");
            status = call(&Request::UpdateDownload)?;
            for _ in 0..3600 {
                if status.state != UpdateState::Downloading {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
                status = call(&Request::UpdateStatus)?;
            }
            if let Some(e) = &status.error {
                return Err(format!("download failed: {e}"));
            }
        }
        let (Some(latest), Some(path)) = (status.latest.clone(), status.downloaded_path.clone())
        else {
            return Err(
                "no verified download is available; run: geekbridge-ctl update download".into(),
            );
        };
        let (Some(sha), Some(size)) = (latest.sha256.clone(), latest.asset_size) else {
            return Err("the release publishes no checksum for this installer".into());
        };
        if !f.yes
            && !f.dry_run
            && !ask(&format!(
                "Install GeekBridge v{} over v{} in {}?",
                latest.version,
                status.current_version,
                prefix.display()
            ))
        {
            return Err("not installed (pass --yes to confirm without a prompt)".into());
        }
        let state = state_file(state_dir);
        let report = install::install(&install::Request {
            prefix: &prefix,
            archive: Path::new(&path),
            sha256: &sha,
            size,
            tag: &latest.tag,
            current_version: &status.current_version,
            dry_run: f.dry_run,
            state_file: Some(&state),
            strict_binaries: true,
        })
        .map_err(|e| {
            // Failures are worth remembering next to the successes.
            e.to_string()
        })?;
        if report.dry_run {
            println!(
                "Dry run: v{} passed every check (checksum, archive contents, executables, version). Nothing was changed.",
                report.version
            );
            return Ok(());
        }
        println!(
            "Installed v{} in {}{}.",
            report.version,
            prefix.display(),
            report
                .previous
                .as_deref()
                .map_or(String::new(), |p| format!(
                    "; v{p} is kept for `geekbridge-ctl update rollback`"
                ))
        );
        if report.migrated {
            println!(
                "(First self-update: the files install.sh wrote are now symbolic links into {}/lib/geekbridge.)",
                prefix.display()
            );
        }
        let probe = IpcProbe {
            port,
            token: token.clone(),
            force: f.force,
        };
        restart_flow(&f, &probe, &prefix, &report.version, true, state_dir)
    }

    pub fn rollback_command(
        args: &[OsString],
        port: u16,
        token: &Token,
        state_dir: &Path,
    ) -> Result<(), String> {
        let f = flags(args)?;
        let prefix = prefix_of(support(&f))?;
        let report =
            install::rollback(&prefix, Some(&state_file(state_dir))).map_err(|e| e.to_string())?;
        println!(
            "Rolled back: v{} is current again (v{} is kept).",
            report.version,
            report.previous.unwrap_or_default()
        );
        let probe = IpcProbe {
            port,
            token: token.clone(),
            force: f.force,
        };
        restart_flow(&f, &probe, &prefix, &report.version, false, state_dir)
    }

    pub fn restart_command(
        args: &[OsString],
        port: u16,
        token: &Token,
        state_dir: &Path,
    ) -> Result<(), String> {
        let f = flags(args)?;
        let prefix = prefix_of(support(&f))?;
        let version = install::installed_version(&prefix).ok_or(
            "this installation has not been through `update install`; nothing to restart onto",
        )?;
        let probe = IpcProbe {
            port,
            token: token.clone(),
            force: f.force,
        };
        restart_flow(
            &Flags { yes: true, ..f },
            &probe,
            &prefix,
            &version,
            false,
            state_dir,
        )
    }

    pub fn last_outcome(state_dir: &Path) -> Option<String> {
        install::last_outcome(&state_file(state_dir))
    }
}

/// What `update install | rollback | restart` print where GeekBridge does not install updates.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn refusal(os: &str, support: &str) -> String {
    let system = match os {
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    };
    format!(
        "GeekBridge does not install or roll back updates itself on {system}; nothing was changed. \
         It downloads the new version and checks its SHA-256, then you install it yourself \
         (geekbridge-ctl update download, or the Download button in Settings or Home).\n{support}"
    )
}

#[cfg(target_os = "linux")]
pub use imp::*;

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    fn download_only() -> Result<(), String> {
        Err(refusal(std::env::consts::OS, &describe_support()))
    }
    pub fn install_command(
        _: &[OsString],
        _: u16,
        _: &Token,
        _: &Path,
        _: &dyn Fn(&Request) -> Result<UpdateStatus, String>,
    ) -> Result<(), String> {
        download_only()
    }
    pub fn rollback_command(_: &[OsString], _: u16, _: &Token, _: &Path) -> Result<(), String> {
        download_only()
    }
    pub fn restart_command(_: &[OsString], _: u16, _: &Token, _: &Path) -> Result<(), String> {
        download_only()
    }
    pub fn last_outcome(_: &Path) -> Option<String> {
        None
    }
}
#[cfg(not(target_os = "linux"))]
pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_names_the_system_says_nothing_changed_and_points_at_download() {
        for (os, name) in [("windows", "Windows"), ("macos", "macOS")] {
            let r = refusal(os, "Download only: x.");
            assert!(r.contains(&format!("on {name}")), "{r}");
            assert!(r.contains("nothing was changed"), "{r}");
            assert!(r.contains("update download"), "{r}");
            assert!(r.ends_with("Download only: x."), "{r}");
        }
    }
}
