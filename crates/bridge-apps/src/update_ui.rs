//! The "Update available" banner and the update controls in the details panel.
//!
//! The service owns the schedule, the network and the SHA-256 check (`bridge-update`). This
//! module only shows its state and forwards explicit clicks. It never installs anything: after a
//! verified download it reveals the file and the user opens it.
use crate::theme::{self, ButtonKind, Palette};
use bridge_core::ipc::{self, Request, Response, Token, UpdateState, UpdateStatus};
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

const RELEASE_PREFIX: &str = "https://github.com/fspecii/geekbridge/";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Check,
    Download,
    Skip(String),
    SetEnabled(bool),
}

type Published = Option<Result<UpdateStatus, String>>;

fn call(request: &Request) -> Result<UpdateStatus, String> {
    let unreachable = || "The update status isn't available right now.".to_owned();
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

pub struct Updates {
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
        wake: impl Fn() + Send + 'static,
    ) -> Self {
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
                        Err(mpsc::RecvTimeoutError::Timeout) => call(&Request::UpdateStatus),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    busy = matches!(&next, Ok(s) if matches!(s.state, UpdateState::Checking | UpdateState::Downloading));
                    let next = Some(next);
                    if next != previous {
                        previous = next.clone();
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = next;
                        wake();
                    }
                }
            })
            .expect("spawn update status worker");
        Self {
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
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0_f32, pal.separator))
                    .inner_margin(Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    theme::dot(ui, pal.accent, false);
                    ui.label(theme::semibold(
                        format!("Update available: v{version}"),
                        13.0,
                    ));
                    ui.label(theme::caption(pal, banner_detail(&status)));
                    match status.state {
                        UpdateState::Downloading => {}
                        UpdateState::Downloaded => {
                            if ui.button("Show file").clicked() {
                                action = Some(Action::Reveal);
                            }
                        }
                        _ => {
                            if status.downloadable && ui.button("Download").clicked() {
                                action = Some(Action::Download);
                            }
                        }
                    }
                    if ui.button("Release notes").clicked() {
                        action = Some(Action::Notes);
                    }
                    if ui.button("Skip this version").clicked() {
                        action = Some(Action::Skip);
                    }
                    if ui.button("Later").clicked() {
                        later = true;
                    }
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

    /// Details-panel section: manual check and the automatic-check switch.
    pub fn details(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        self.poll();
        ui.add_space(2.0);
        ui.label(theme::overline(pal, "Updates"));
        let Some(status) = self.status.clone() else {
            ui.label(theme::caption(
                pal,
                self.problem
                    .as_deref()
                    .unwrap_or("Checking update settings…"),
            ));
            return;
        };
        ui.add(egui::Label::new(theme::secondary(pal, summary_line(&status))).wrap());
        ui.horizontal_wrapped(|ui| {
            let idle = !matches!(
                status.state,
                UpdateState::Checking | UpdateState::Downloading
            );
            if theme::pill_button(ui, pal, "Check for updates", ButtonKind::Plain, idle).clicked() {
                self.dismissed = None;
                self.send(Command::Check);
            }
            let mut automatic = status.enabled;
            if ui
                .checkbox(&mut automatic, "Check automatically (about daily)")
                .changed()
            {
                self.send(Command::SetEnabled(automatic));
            }
        });
        ui.label(theme::caption(
            pal,
            "Only GitHub's public release list is contacted. Nothing is installed automatically.",
        ));
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
            .map_or("Downloading…".to_owned(), |pct| {
                format!("Downloading… {}%", pct.min(100))
            }),
        UpdateState::Downloaded => {
            "Downloaded and checked against the published SHA-256. Open it to install.".into()
        }
        _ => match (&status.error, status.downloadable) {
            (Some(error), _) => crate::state::display_text(error),
            (None, true) => format!("You have v{}.", status.current_version),
            (None, false) => {
                "No verified installer is published for this system; see the release notes.".into()
            }
        },
    }
}

fn summary_line(status: &UpdateStatus) -> String {
    let when = status
        .last_checked_unix
        .map_or("not checked yet".to_owned(), |_| "checked".to_owned());
    let line = match (&status.latest, status.newer) {
        (Some(latest), true) => format!(
            "v{} is available (you have v{}).",
            latest.version, status.current_version
        ),
        (Some(_), false) | (None, _) if status.last_checked_unix.is_some() => {
            format!("GeekBridge v{} is up to date.", status.current_version)
        }
        _ => format!("GeekBridge v{}, {when}.", status.current_version),
    };
    match &status.error {
        Some(error) => format!("{line} {}", crate::state::display_text(error)),
        None => line,
    }
}

/// Only our own release pages are ever opened.
fn release_url_ok(url: &str) -> bool {
    url.strip_prefix(RELEASE_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'+'))
    })
}

fn open_in_desktop(target: &str) {
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
                release_url: format!("{RELEASE_PREFIX}releases/tag/v0.2.0"),
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
            "{RELEASE_PREFIX}releases/tag/v0.2.0"
        )));
        for bad in [
            "https://github.com/evil/geekbridge/releases",
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
