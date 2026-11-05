// SPDX-License-Identifier: GPL-3.0-or-later
//! The "Update available" banner and the update controls in the details panel.
//!
//! The service owns the schedule, the network and the SHA-256 check (`bridge-update`). This
//! module only shows its state and forwards explicit clicks. Installing is a click on "Install
//! update" in the Settings card, run by `geekbridge-ctl update install` (the same code and checks as
//! the command line; per-user Linux installs only). The banner still reveals the downloaded file,
//! and a package-managed or non-Linux copy says so instead of offering to install.
use crate::{
    i18n::{tr, tr_args},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::ipc::{self, Request, Response, Token, UpdateChannel, UpdateState, UpdateStatus};
use eframe::egui::{self, Frame, Margin, Stroke};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use zeroize::Zeroize;

/// Release pages open only on github.com; the repository is configurable (`GEEKBRIDGE_UPDATE_REPO`).
const RELEASE_PREFIX: &str = "https://github.com/";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Check,
    Download,
    Skip(String),
    SetEnabled(bool),
    SetChannel(UpdateChannel),
}

type Published = Option<Result<UpdateStatus, String>>;

fn call(request: &Request) -> Result<UpdateStatus, String> {
    let unreachable = || tr("update.unavailable").to_owned();
    let dir = ipc::default_state_dir().ok_or_else(unreachable)?;
    let mut bytes = bridge_kde::fsutil::read_private_file(&ipc::token_path(&dir))
        .ok()
        .flatten()
        .ok_or_else(unreachable)?;
    let token = Token::parse(&bytes).map_err(|_| unreachable());
    bytes.zeroize();
    match ipc::call(ipc::control_port_from_env(), &token?, request) {
        Ok(Response::Update(status)) => Ok(*status),
        Ok(Response::Error(body)) => Err(crate::state::display_text(&body.message)),
        _ => Err(unreachable()),
    }
}

/// Can this copy install updates itself? Answered by `geekbridge-ctl update support`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Support {
    User,
    PackageManaged,
    DownloadOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Install {
    Idle,
    Working,
    /// Installed; the service still runs the old version.
    Installed(String),
    Restarting,
    Done(String),
    Failed(String),
}

/// `geekbridge-ctl` next to this executable (same version), else the one on PATH.
fn ctl_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| Some(p.parent()?.join("geekbridge-ctl")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "geekbridge-ctl".into())
}

/// Run `geekbridge-ctl update <args>`; its first stdout line on success, its last stderr line on failure.
fn run_ctl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(ctl_path())
        .arg("update")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let last = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_owned()
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(crate::state::display_text(&last(&out.stderr)))
    }
}

pub struct Updates {
    wake: Arc<dyn Fn() + Send + Sync>,
    support: Arc<Mutex<Option<Support>>>,
    support_asked: bool,
    install: Arc<Mutex<Install>>,
    commands: mpsc::SyncSender<Command>,
    published: Arc<Mutex<Published>>,
    stop: Arc<AtomicBool>,
    status: Option<UpdateStatus>,
    problem: Option<String>,
    /// Version whose banner was closed with "Later" (until the app restarts).
    dismissed: Option<String>,
}

impl Updates {
    pub fn new(ctx: egui::Context) -> Self {
        Self::spawn(call, move || ctx.request_repaint())
    }

    fn spawn(
        mut call: impl FnMut(&Request) -> Result<UpdateStatus, String> + Send + 'static,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let worker_wake = wake.clone();
        let (commands, rx) = mpsc::sync_channel::<Command>(4);
        let published: Arc<Mutex<Published>> = Arc::default();
        let out = published.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = stop.clone();
        std::thread::Builder::new()
            .name("geekbridge-update".into())
            .spawn(move || {
                let mut previous: Published = None;
                let mut busy = false;
                while !quitting.load(Ordering::Acquire) {
                    let next = match rx.recv_timeout(if busy {
                        Duration::from_millis(700)
                    } else {
                        crate::pace::scaled(Duration::from_millis(4000))
                    }) {
                        Ok(Command::Check) => call(&Request::UpdateCheck),
                        Ok(Command::Download) => call(&Request::UpdateDownload),
                        Ok(Command::Skip(version)) => call(&Request::UpdateSkip { version }),
                        Ok(Command::SetEnabled(enabled)) => {
                            call(&Request::UpdateSetEnabled { enabled })
                        }
                        Ok(Command::SetChannel(channel)) => {
                            call(&Request::UpdateSetChannel { channel })
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => call(&Request::UpdateStatus),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    busy = matches!(&next, Ok(s) if matches!(s.state, UpdateState::Checking | UpdateState::Downloading));
                    let next = Some(next);
                    if next != previous {
                        previous = next.clone();
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = next;
                        worker_wake();
                    }
                }
            })
            .expect("spawn update status worker");
        Self {
            wake,
            support: Arc::default(),
            support_asked: false,
            install: Arc::new(Mutex::new(Install::Idle)),
            commands,
            published,
            stop,
            status: None,
            problem: None,
            dismissed: None,
        }
    }

    fn poll(&mut self) {
        let latest = self
            .published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match latest {
            Some(Ok(status)) => {
                self.status = Some(status);
                self.problem = None;
            }
            Some(Err(message)) => self.problem = Some(message),
            None => {}
        }
    }

    /// Ask `geekbridge-ctl update support` once, off the UI thread.
    fn support(&mut self) -> Option<Support> {
        if !self.support_asked {
            self.support_asked = true;
            let (slot, wake) = (self.support.clone(), self.wake.clone());
            std::thread::spawn(move || {
                let word = run_ctl(&["support"])
                    .ok()
                    .and_then(|o| o.lines().next().map(str::to_owned));
                let found = match word.as_deref() {
                    Some("user-install") => Support::User,
                    Some("package-managed") => Support::PackageManaged,
                    _ => Support::DownloadOnly,
                };
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
                wake();
            });
        }
        self.support
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Run one ctl step in the background and publish its outcome.
    fn run_install(
        &self,
        working: Install,
        args: &'static [&'static str],
        ok: impl Fn(String) -> Install + Send + 'static,
    ) {
        {
            let mut state = self.install.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*state, Install::Working | Install::Restarting) {
                return;
            }
            *state = working;
        }
        let (slot, wake) = (self.install.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let next = match run_ctl(args) {
                Ok(out) => ok(out),
                Err(e) => Install::Failed(tr_args("update.install_failed", &[("reason", &e)])),
            };
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = next;
            wake();
        });
    }

    fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }

    /// Non-intrusive banner above the app content; nothing is drawn unless an update is offered.
    pub fn banner(&mut self, ctx: &egui::Context, pal: &Palette) {
        self.poll();
        let Some(status) = self.status.clone() else {
            return;
        };
        let Some(version) = banner_version(&status, self.dismissed.as_deref()) else {
            return;
        };
        let mut later = false;
        let mut action = None;
        egui::TopBottomPanel::top("update_banner")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(pal.content)
                    .inner_margin(Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                Frame::new()
                    .fill(pal.tile)
                    .stroke(Stroke::new(1.0_f32, pal.separator))
                    .corner_radius(egui::CornerRadius::same(crate::nothing::R_CARD))
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            theme::dot(ui, pal.accent_fill, false);
                            let key = if status.channel == UpdateChannel::Dev {
                                "update.available_dev"
                            } else {
                                "update.available"
                            };
                            ui.label(theme::semibold(
                                tr_args(key, &[("version", &version)]),
                                13.0,
                            ));
                            ui.label(theme::caption(pal, banner_detail(&status)));
                            match status.state {
                                UpdateState::Downloading => {}
                                UpdateState::Downloaded => {
                                    if theme::pill_button(
                                        ui,
                                        pal,
                                        tr("update.show_file"),
                                        theme::ButtonKind::Primary,
                                        true,
                                    )
                                    .clicked()
                                    {
                                        action = Some(Action::Reveal);
                                    }
                                }
                                _ => {
                                    if status.downloadable
                                        && theme::pill_button(
                                            ui,
                                            pal,
                                            tr("update.download"),
                                            theme::ButtonKind::Primary,
                                            true,
                                        )
                                        .clicked()
                                    {
                                        action = Some(Action::Download);
                                    }
                                }
                            }
                            if theme::pill_button(
                                ui,
                                pal,
                                tr("update.release_notes"),
                                theme::ButtonKind::Plain,
                                true,
                            )
                            .clicked()
                            {
                                action = Some(Action::Notes);
                            }
                            if theme::pill_button(
                                ui,
                                pal,
                                tr("update.skip"),
                                theme::ButtonKind::Plain,
                                true,
                            )
                            .clicked()
                            {
                                action = Some(Action::Skip);
                            }
                            if theme::pill_button(
                                ui,
                                pal,
                                tr("update.later"),
                                theme::ButtonKind::Plain,
                                true,
                            )
                            .clicked()
                            {
                                later = true;
                            }
                        });
                    });
            });
        if later {
            self.dismissed = Some(version.clone());
        }
        match action {
            Some(Action::Download) => self.send(Command::Download),
            Some(Action::Skip) => self.send(Command::Skip(version)),
            Some(Action::Notes) => {
                if let Some(latest) = &status.latest
                    && release_url_ok(&latest.release_url)
                {
                    open_in_desktop(&latest.release_url);
                }
            }
            Some(Action::Reveal) => {
                if let Some(path) = &status.downloaded_path {
                    reveal(path);
                }
            }
            None => {}
        }
    }

    /// One line about the version and update state, once the service has answered.
    pub fn summary(&mut self) -> Option<String> {
        self.poll();
        self.status.as_ref().map(summary_line)
    }

    /// Commit and channel of the running build, once the service has answered.
    pub fn about(&mut self) -> bridge_core::about::About {
        self.poll();
        bridge_core::about::About {
            commit: self.status.as_ref().and_then(|s| s.current_commit.clone()),
            channel: self.status.as_ref().map(|s| s.channel.as_str().to_owned()),
        }
    }

    /// Details-panel section: manual check and the automatic-check switch.
    pub fn details(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        self.poll();
        ui.add_space(2.0);
        ui.label(theme::overline(pal, tr("update.section")));
        let Some(status) = self.status.clone() else {
            ui.label(theme::caption(
                pal,
                self.problem
                    .as_deref()
                    .unwrap_or_else(|| tr("update.checking_settings")),
            ));
            return;
        };
        ui.add(egui::Label::new(theme::secondary(pal, summary_line(&status))).wrap());
        self.install_controls(ui, pal, &status);
        ui.horizontal_wrapped(|ui| {
            let idle = !matches!(
                status.state,
                UpdateState::Checking | UpdateState::Downloading
            );
            if theme::pill_button(ui, pal, tr("update.check"), ButtonKind::Plain, idle).clicked() {
                self.dismissed = None;
                self.send(Command::Check);
            }
            let mut automatic = status.enabled;
            if ui
                .checkbox(&mut automatic, tr("update.automatic"))
                .changed()
            {
                self.send(Command::SetEnabled(automatic));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::caption(pal, tr("update.channel")));
            let mut channel = status.channel;
            for (value, key) in [
                (UpdateChannel::Stable, "update.channel_stable"),
                (UpdateChannel::Dev, "update.channel_dev"),
            ] {
                ui.selectable_value(&mut channel, value, tr(key));
            }
            if channel != status.channel {
                self.dismissed = None;
                self.send(Command::SetChannel(channel));
            }
        });
        if status.channel == UpdateChannel::Dev {
            ui.add(egui::Label::new(theme::caption(pal, tr("update.channel_dev_note"))).wrap());
        }
        ui.label(theme::caption(pal, tr("update.privacy")));
    }
}

impl Updates {
    /// "Install update" (per-user Linux installs), or the plain reason there is no such button.
    fn install_controls(&mut self, ui: &mut egui::Ui, pal: &Palette, status: &UpdateStatus) {
        let offered = status.newer && !status.skipped;
        let state = self
            .install
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match (&state, self.support()) {
            (Install::Installed(version), _) => {
                ui.add(
                    egui::Label::new(theme::caption(
                        pal,
                        tr_args("update.installed", &[("version", version)]),
                    ))
                    .wrap(),
                );
                if theme::pill_button(ui, pal, tr("update.restart_now"), ButtonKind::Primary, true)
                    .clicked()
                {
                    self.run_install(Install::Restarting, &["restart"], |_| {
                        Install::Done(tr("update.restarted").to_owned())
                    });
                }
            }
            (Install::Working, _) => {
                ui.add(egui::Label::new(theme::caption(pal, tr("update.installing"))).wrap());
            }
            (Install::Restarting, _) => {
                ui.add(egui::Label::new(theme::caption(pal, tr("update.restarting"))).wrap());
            }
            (Install::Done(text) | Install::Failed(text), _) => {
                ui.add(egui::Label::new(theme::caption(pal, text.clone())).wrap());
            }
            (Install::Idle, Some(Support::User))
                if offered && status.channel == UpdateChannel::Stable =>
            {
                if (status.downloadable || status.state == UpdateState::Downloaded)
                    && theme::pill_button(ui, pal, tr("update.install"), ButtonKind::Primary, true)
                        .clicked()
                {
                    let version = status
                        .latest
                        .as_ref()
                        .map(|l| l.version.clone())
                        .unwrap_or_default();
                    self.run_install(
                        Install::Working,
                        &["install", "--yes", "--no-restart"],
                        move |_| Install::Installed(version.clone()),
                    );
                }
            }
            (Install::Idle, Some(Support::PackageManaged)) if offered => {
                ui.add(egui::Label::new(theme::caption(pal, tr("update.package_managed"))).wrap());
            }
            (Install::Idle, Some(Support::DownloadOnly)) if offered => {
                ui.add(egui::Label::new(theme::caption(pal, tr("update.download_only"))).wrap());
            }
            _ => {}
        }
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

enum Action {
    Download,
    Reveal,
    Notes,
    Skip,
}

/// The version to announce, or `None` when there is nothing to show (not newer, skipped, or
/// closed with "Later").
fn banner_version(status: &UpdateStatus, dismissed: Option<&str>) -> Option<String> {
    let latest = status.latest.as_ref()?;
    (status.newer && !status.skipped && dismissed != Some(latest.version.as_str()))
        .then(|| latest.version.clone())
}

fn banner_detail(status: &UpdateStatus) -> String {
    match status.state {
        UpdateState::Downloading => status
            .downloaded_bytes
            .saturating_mul(100)
            .checked_div(status.download_total)
            .map_or(tr("update.downloading").to_owned(), |pct| {
                tr_args("update.downloading_pct", &[("pct", &pct.min(100))])
            }),
        UpdateState::Downloaded => tr("update.downloaded").to_owned(),
        _ => match (&status.error, status.downloadable) {
            (Some(error), _) => crate::state::display_text(error),
            (None, true) => tr_args("update.you_have", &[("version", &status.current_version)]),
            (None, false) => tr("update.no_installer").to_owned(),
        },
    }
}

fn summary_line(status: &UpdateStatus) -> String {
    let running = match &status.current_commit {
        Some(commit) => format!("v{} ({commit})", status.current_version),
        None => format!("v{}", status.current_version),
    };
    let line = match (&status.latest, status.newer) {
        _ if status.state == UpdateState::NoPublicRelease => {
            ipc::UPDATE_NO_PUBLIC_RELEASE.to_owned()
        }
        (Some(latest), true) if status.channel == UpdateChannel::Dev => tr_args(
            "update.summary_available_dev",
            &[("latest", &latest.version), ("version", &running)],
        ),
        (Some(_), false) | (None, _)
            if status.channel == UpdateChannel::Dev && status.last_checked_unix.is_some() =>
        {
            tr_args("update.summary_current_dev", &[("version", &running)])
        }
        (Some(latest), true) => tr_args(
            "update.summary_available",
            &[
                ("latest", &latest.version),
                ("version", &status.current_version),
            ],
        ),
        (Some(_), false) | (None, _) if status.last_checked_unix.is_some() => tr_args(
            "update.summary_current",
            &[("version", &status.current_version)],
        ),
        _ => tr_args(
            if status.last_checked_unix.is_some() {
                "update.summary_checked"
            } else {
                "update.summary_unchecked"
            },
            &[("version", &status.current_version)],
        ),
    };
    match &status.error {
        Some(error) => format!("{line} {}", crate::state::display_text(error)),
        None => line,
    }
}

/// Only a GitHub `<owner>/<repo>/releases...` page is ever opened (the service has already
/// restricted the URL to the configured repository).
fn release_url_ok(url: &str) -> bool {
    url.strip_prefix(RELEASE_PREFIX).is_some_and(|rest| {
        rest.split('/').nth(2) == Some("releases")
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'+'))
    })
}

pub(crate) fn open_in_desktop(target: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", windows)))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(target).spawn();
}

fn reveal(path: &str) {
    let path = std::path::Path::new(path);
    if !path.is_absolute() || !path.is_file() {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(windows)]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        let _ = std::process::Command::new("explorer").arg(arg).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    if let Some(dir) = path.parent() {
        open_in_desktop(&dir.display().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::ipc::UpdateRelease;

    fn status(newer: bool, skipped: bool) -> UpdateStatus {
        UpdateStatus {
            enabled: true,
            channel: UpdateChannel::Stable,
            current_commit: None,
            current_version: "0.1.0".into(),
            state: if newer {
                UpdateState::Available
            } else {
                UpdateState::UpToDate
            },
            latest: Some(UpdateRelease {
                version: "0.2.0".into(),
                tag: "v0.2.0".into(),
                notes: String::new(),
                release_url: format!("{RELEASE_PREFIX}fspecii/geekbridge/releases/tag/v0.2.0"),
                asset_name: Some("a.zip".into()),
                asset_size: Some(1),
                sha256: Some("0".repeat(64)),
            }),
            newer,
            skipped,
            downloadable: newer,
            last_checked_unix: Some(1),
            downloaded_bytes: 0,
            download_total: 0,
            downloaded_path: None,
            error: None,
        }
    }

    #[test]
    fn banner_only_for_newer_unskipped_undismissed() {
        assert_eq!(
            banner_version(&status(true, false), None).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(banner_version(&status(false, false), None), None);
        assert_eq!(banner_version(&status(true, true), None), None);
        assert_eq!(banner_version(&status(true, false), Some("0.2.0")), None);
        assert!(banner_version(&status(true, false), Some("0.1.9")).is_some());
    }

    #[test]
    fn only_our_release_pages_are_opened() {
        assert!(release_url_ok(&format!(
            "{RELEASE_PREFIX}fspecii/geekbridge/releases/tag/v0.2.0"
        )));
        for bad in [
            "https://github.com/evil/geekbridge/issues",
            "https://github.com/fspecii/geekbridge/",
            "http://github.com/fspecii/geekbridge/releases",
            "https://github.com/fspecii/geekbridge/releases;calc",
            "https://github.com/fspecii/geekbridge/a b",
            "file:///etc/passwd",
        ] {
            assert!(!release_url_ok(bad), "{bad}");
        }
    }

    #[test]
    fn no_public_release_is_worded_plainly() {
        let mut s = status(false, false);
        s.state = UpdateState::NoPublicRelease;
        s.latest = None;
        let line = summary_line(&s);
        assert!(line.contains("No public release found"), "{line}");
        assert!(line.contains("private or has no releases yet"));
        assert!(!line.contains("up to date"));
    }

    #[test]
    fn rc1_with_only_prereleases_published_reads_up_to_date_and_offers_no_banner() {
        // The state after a check on v0.1.0-rc.1 while the repository's only release is that rc.
        let mut s = status(false, false);
        s.current_version = "0.1.0-rc.1".into();
        s.latest = None;
        let line = summary_line(&s);
        assert_eq!(line, "GeekBridge v0.1.0-rc.1 is up to date.");
        assert_eq!(banner_version(&s, None), None);
    }

    #[test]
    fn downloaded_banner_says_how_to_install_and_errors_stay_visible() {
        let mut s = status(true, false);
        s.state = UpdateState::Downloaded;
        s.downloaded_path = Some("/tmp/x/geekbridge-linux-x86_64.tar.gz".into());
        let detail = banner_detail(&s);
        assert!(
            detail.contains("checked against the published SHA-256")
                && detail.contains("Open it to install"),
            "{detail}"
        );
        // An outage keeps the offer and appends the reason to the summary.
        let mut s = status(true, false);
        s.error = Some("Couldn't reach GitHub".into());
        assert!(
            summary_line(&s).contains("v0.2.0 is available") && summary_line(&s).contains("reach")
        );
        assert!(banner_detail(&s).contains("reach"));
    }

    #[test]
    fn summary_and_progress_text() {
        assert!(summary_line(&status(true, false)).contains("v0.2.0 is available"));
        assert!(summary_line(&status(false, false)).contains("up to date"));
        let mut s = status(true, false);
        s.state = UpdateState::Downloading;
        s.downloaded_bytes = 50;
        s.download_total = 200;
        assert_eq!(banner_detail(&s), "Downloading… 25%");
    }
}
