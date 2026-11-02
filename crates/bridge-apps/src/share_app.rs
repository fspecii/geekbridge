// SPDX-License-Identifier: GPL-3.0-or-later
//! The Share window: pick or drop files for a connected phone, answer incoming offers, watch
//! progress. It is a client of the authenticated control API like the other apps; it never moves
//! file bytes itself (the service does) and never touches the network on a GUI frame.

use crate::a11y::ScrollBars as _;
use crate::{
    cli::ThemeChoice,
    icons::Icon,
    theme::{self, ButtonKind},
};
use bridge_core::{
    AppKind,
    ipc::{self, LinkPhase, Request, Response, Status, Token},
    share::{
        ShareStatus, ShareTextKind, TransferDirection, TransferInfo, TransferKind, TransferState,
    },
};
use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, ThemePreference, vec2};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use zeroize::Zeroize;

/// The palette this window borrows (it has no accent of its own yet).
const LOOK: AppKind = AppKind::Clipboard;

// ---------------------------------------------------------------------- pure helpers

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (n as f64, 0);
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn progress_fraction(t: &TransferInfo) -> Option<f32> {
    match (t.state, t.size) {
        (TransferState::Done, _) => Some(1.0),
        (_, Some(size)) if size > 0 => Some((t.bytes as f32 / size as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// One line describing where a transfer stands, in plain words.
pub fn state_line(t: &TransferInfo) -> String {
    let who = &t.device_name;
    let size = t.size.map(human_size);
    match (t.direction, t.state) {
        (TransferDirection::Incoming, TransferState::Offered) => match size {
            Some(s) => format!("{who} wants to send {s}"),
            None => format!("{who} wants to send a file"),
        },
        (_, TransferState::Waiting) => format!("Waiting for {who} to take it…"),
        (_, TransferState::Active) => match (size, t.bytes) {
            (Some(s), b) => format!("{} of {s}", human_size(b)),
            (None, b) => human_size(b),
        },
        (TransferDirection::Incoming, TransferState::Done) => "Saved".into(),
        (TransferDirection::Outgoing, TransferState::Done) => format!("Sent to {who}"),
        (_, TransferState::Cancelled) => "Cancelled".into(),
        (_, TransferState::Declined) => "Declined".into(),
        (_, TransferState::Failed) => format!("Failed: {}", failure_words(t.code.as_deref())),
        (_, TransferState::Offered) => "Waiting".into(),
    }
}

pub fn failure_words(code: Option<&str>) -> &'static str {
    match code {
        Some("expired") => "nobody answered in time",
        Some("timeout") => "the phone stopped responding",
        Some("too_large") => "the file is too large",
        Some("truncated") => "the file ended early",
        Some("cancelled") => "cancelled",
        Some("pin_mismatch") | Some("tls_failed") => "the secure connection could not be verified",
        Some("unreadable") => "the file could not be read",
        Some("io_error") => "a disk or network error",
        _ => "the transfer did not finish",
    }
}

/// Files only: directories and non-UTF-8 names are skipped, the rest are made absolute.
pub fn sendable(paths: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .filter(|p| p.is_file())
        .filter_map(|p| p.to_str().map(str::to_owned))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub device_id: String,
    pub name: String,
    pub connected: bool,
}

/// Paired devices, connected ones first.
pub fn targets(status: &Status) -> Vec<Target> {
    let mut out: Vec<Target> = status
        .paired
        .iter()
        .map(|p| Target {
            device_id: p.device_id.clone(),
            name: p.name.clone(),
            connected: status
                .links
                .iter()
                .any(|l| l.device_id == p.device_id && l.phase == LinkPhase::Connected),
        })
        .collect();
    out.sort_by_key(|t| !t.connected);
    out
}

/// Commands for the platform file chooser, tried in order. Each prints the chosen paths.
pub fn picker_commands() -> Vec<(&'static str, Vec<String>)> {
    if cfg!(target_os = "windows") {
        vec![("powershell", vec![
            "-NoProfile".into(),
            "-Command".into(),
            "Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.OpenFileDialog; $d.Multiselect=$true; if($d.ShowDialog() -eq 'OK'){ $d.FileNames | ForEach-Object { $_ } }".into(),
        ])]
    } else if cfg!(target_os = "macos") {
        vec![(
            "osascript",
            vec![
                "-e".into(),
                "set fs to choose file with multiple selections allowed".into(),
                "-e".into(),
                "set out to \"\"".into(),
                "-e".into(),
                "repeat with f in fs\nset out to out & POSIX path of f & linefeed\nend repeat"
                    .into(),
                "-e".into(),
                "out".into(),
            ],
        )]
    } else {
        vec![
            (
                "zenity",
                vec![
                    "--file-selection".into(),
                    "--multiple".into(),
                    "--title=Send files".into(),
                    "--separator=\n".into(),
                ],
            ),
            (
                "kdialog",
                vec![
                    "--getopenfilename".into(),
                    ".".into(),
                    "--multiple".into(),
                    "--separate-output".into(),
                    "--title".into(),
                    "Send files".into(),
                ],
            ),
            (
                "yad",
                vec![
                    "--file".into(),
                    "--multiple".into(),
                    "--separator=\n".into(),
                ],
            ),
        ]
    }
}

pub fn parse_picker_output(out: &str) -> Vec<PathBuf> {
    out.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Extensions that run code (or a shortcut to code) when the system "opens" them. A received file with one of these is
/// only ever shown in the folder, never launched: the user can still start it from the file manager on purpose.
const RISKY_EXTENSIONS: &[&str] = &[
    "exe", "bat", "cmd", "com", "scr", "pif", "msi", "msp", "msix", "appx", "ps1", "psm1", "vbs",
    "vbe", "js", "jse", "wsf", "wsh", "hta", "cpl", "reg", "lnk", "url", "scf", "jar", "command",
    "app", "sh", "bash", "zsh", "csh", "ksh", "run", "bin", "desktop", "workflow", "action", "pkg",
    "dmg", "py", "pl", "rb", "dll", "gadget", "inf", "chm", "iso", "appimage",
];

/// True if opening this file could run it. Case-insensitive, and trailing dots/spaces (which Windows ignores) do not hide it.
pub fn is_risky_to_open(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase());
    let Some(name) = name else { return false };
    let name = name.trim_end_matches(['.', ' ']);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| RISKY_EXTENSIONS.contains(&ext))
}

fn open_command(path: &Path, reveal: bool) -> (&'static str, Vec<String>) {
    // "Open" on something that would execute becomes "show in folder".
    let reveal = reveal || is_risky_to_open(path);
    let p = path.display().to_string();
    if cfg!(target_os = "windows") {
        (
            "explorer",
            if reveal {
                vec![format!("/select,{p}")]
            } else {
                vec![p]
            },
        )
    } else if cfg!(target_os = "macos") {
        (
            "open",
            if reveal {
                vec!["-R".into(), p]
            } else {
                vec![p]
            },
        )
    } else {
        let target = if reveal {
            path.parent().map_or(p.clone(), |d| d.display().to_string())
        } else {
            p
        };
        ("xdg-open", vec![target])
    }
}

fn spawn_quiet(program: &str, args: &[String]) {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn open_in_system(path: &Path, reveal: bool) {
    let (prog, args) = open_command(path, reveal);
    spawn_quiet(prog, &args);
}

fn open_link(url: &str) {
    if bridge_kde::share::is_openable_url(url) {
        let (prog, args) = if cfg!(target_os = "windows") {
            ("explorer", vec![url.to_owned()])
        } else if cfg!(target_os = "macos") {
            ("open", vec![url.to_owned()])
        } else {
            ("xdg-open", vec![url.to_owned()])
        };
        spawn_quiet(prog, &args);
    }
}

fn choose_files() -> Vec<PathBuf> {
    for (prog, args) in picker_commands() {
        if let Ok(out) = crate::home_app::no_window(&mut Command::new(prog))
            .args(&args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            return parse_picker_output(&String::from_utf8_lossy(&out.stdout));
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------- worker

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub status: Option<Status>,
    pub share: Option<ShareStatus>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

enum Cmd {
    Call(Request),
    SendFiles {
        device_id: String,
        paths: Vec<String>,
    },
    Pick {
        device_id: String,
    },
}

struct Worker {
    tx: mpsc::SyncSender<Cmd>,
    latest: Arc<Mutex<Option<View>>>,
}

fn token() -> Result<Token, String> {
    let dir =
        ipc::default_state_dir().ok_or("This computer's private app folder couldn't be found.")?;
    let mut bytes = bridge_kde::fsutil::read_private_file(&ipc::token_path(&dir))
        .map_err(|_| "The service settings couldn't be read securely.".to_owned())?
        .ok_or("Start the GeekBridge service first.")?;
    let t = Token::parse(&bytes).map_err(|_| "The service settings are unreadable.".to_owned());
    bytes.zeroize();
    t
}

fn describe(e: ipc::IpcError) -> String {
    match e {
        ipc::IpcError::Io(_) => {
            "The GeekBridge service isn't reachable. Start it and try again.".into()
        }
        ipc::IpcError::Service(b) => b
            .message
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect(),
        _ => "The service couldn't be authenticated.".into(),
    }
}

impl Worker {
    fn spawn(wake: impl Fn() + Send + 'static, initial: Vec<String>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Cmd>(16);
        let latest = Arc::new(Mutex::new(None));
        let out = latest.clone();
        std::thread::Builder::new()
            .name("geekbridge-share".into())
            .spawn(move || {
                let port = ipc::control_port_from_env();
                let mut view = View::default();
                let mut pending_initial = initial;
                let mut last: Option<View> = None;
                loop {
                    match token() {
                        Err(e) => {
                            view.status = None;
                            view.share = None;
                            view.error = Some(e);
                        }
                        Ok(token) => {
                            match ipc::status(port, &token) {
                                Ok(s) => {
                                    view.status = Some(s);
                                    view.error = None;
                                }
                                Err(e) => {
                                    view.status = None;
                                    view.error = Some(describe(e));
                                }
                            }
                            if view.error.is_none() {
                                match ipc::call(port, &token, &Request::ShareStatus) {
                                    Ok(Response::Share(s)) => view.share = Some(*s),
                                    Ok(_) => {}
                                    Err(e) => view.error = Some(describe(e)),
                                }
                            }
                            // Files named on the command line go to the first connected phone.
                            if !pending_initial.is_empty()
                                && let Some(t) = view
                                    .status
                                    .as_ref()
                                    .and_then(|s| targets(s).into_iter().find(|t| t.connected))
                            {
                                let paths = std::mem::take(&mut pending_initial);
                                view.notice = Some(send_files(port, &token, &t.device_id, &paths));
                            }
                        }
                    }
                    if last.as_ref() != Some(&view) {
                        last = Some(view.clone());
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(view.clone());
                        wake();
                    }
                    match rx.recv_timeout(crate::pace::scaled(Duration::from_millis(500))) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Ok(Cmd::Call(req)) => {
                            view.notice = Some(match token().map(|t| ipc::call(port, &t, &req)) {
                                Ok(Ok(Response::Error(b))) => b.message,
                                Ok(Ok(_)) => String::new(),
                                Ok(Err(e)) => describe(e),
                                Err(e) => e,
                            })
                            .filter(|n| !n.is_empty());
                        }
                        Ok(Cmd::SendFiles { device_id, paths }) => {
                            view.notice = match token() {
                                Ok(t) => Some(send_files(port, &t, &device_id, &paths)),
                                Err(e) => Some(e),
                            };
                        }
                        Ok(Cmd::Pick { device_id }) => {
                            let paths = sendable(choose_files());
                            if !paths.is_empty() {
                                view.notice = match token() {
                                    Ok(t) => Some(send_files(port, &t, &device_id, &paths)),
                                    Err(e) => Some(e),
                                };
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn share worker");
        Self { tx, latest }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.try_send(cmd);
    }

    fn latest(&self) -> Option<View> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn send_files(port: u16, token: &Token, device_id: &str, paths: &[String]) -> String {
    let mut sent = 0;
    let mut first_error = None;
    for path in paths {
        match ipc::call(
            port,
            token,
            &Request::ShareSendFile {
                device_id: device_id.to_owned(),
                path: path.clone(),
            },
        ) {
            Ok(Response::TransferStarted { .. }) => sent += 1,
            Ok(Response::Error(b)) => first_error = first_error.or(Some(b.message)),
            Ok(_) => sent += 1,
            Err(e) => first_error = first_error.or(Some(describe(e))),
        }
    }
    match (sent, first_error) {
        (0, Some(e)) => e,
        (n, Some(e)) => format!("{n} sent; one or more were not: {e}"),
        (0, None) => "Nothing to send.".into(),
        (n, None) => format!("Sending {n} file{}…", if n == 1 { "" } else { "s" }),
    }
}

// ---------------------------------------------------------------------- window

struct ShareApp {
    worker: Worker,
    view: View,
    chosen: Option<String>,
    text: String,
}

impl ShareApp {
    fn target<'a>(&self, list: &'a [Target]) -> Option<&'a Target> {
        self.chosen
            .as_ref()
            .and_then(|id| list.iter().find(|t| &t.device_id == id))
            .or_else(|| list.first())
    }
}

impl eframe::App for ShareApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::memtrim::cap_atlas(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

impl ShareApp {
    /// One frame of the window. Separate from `eframe::App::update` so tests can drive it.
    pub fn frame(&mut self, ctx: &egui::Context) {
        crate::a11y::window_title(ctx, "GeekBridge Share");
        crate::pace::observe_focus(ctx);
        if let Some(v) = self.worker.latest() {
            self.view = v;
        }
        let pal = theme::current(ctx, LOOK);
        let list = self.view.status.as_ref().map(targets).unwrap_or_default();
        let target = self.target(&list).cloned();
        let ready = target.as_ref().is_some_and(|t| t.connected);

        // Dropped files go to the selected, connected phone.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !dropped.is_empty() {
            match (&target, ready) {
                (Some(t), true) => {
                    let paths = sendable(dropped);
                    if paths.is_empty() {
                        self.view.notice = Some("Only files can be sent, not folders.".into());
                    } else {
                        self.worker.send(Cmd::SendFiles {
                            device_id: t.device_id.clone(),
                            paths,
                        });
                    }
                }
                _ => self.view.notice = Some("Connect a phone first, then drop the files.".into()),
            }
        }

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal.content)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(e) = &self.view.error {
                            ui.add(egui::Label::new(RichText::new(e).color(pal.err)).wrap());
                            ui.add_space(8.0);
                        }
                        self.send_section(ui, &pal, &list, ready, hovering);
                        self.incoming_section(ui, &pal);
                        self.transfers_section(ui, &pal);
                        self.settings_section(ui, &pal, &list);
                    })
                    .bars(ui.ctx());
            });
        ctx.request_repaint_after(crate::pace::scaled(Duration::from_millis(500)));
    }
}

impl ShareApp {
    fn send_section(
        &mut self,
        ui: &mut egui::Ui,
        pal: &theme::Palette,
        list: &[Target],
        ready: bool,
        hovering: bool,
    ) {
        ui.label(theme::overline(pal, "Send to"));
        let target = self.target(list).cloned();
        match &target {
            None => {
                ui.add(egui::Label::new(theme::secondary(
                    pal,
                    "No phone is paired yet. Open the GeekBridge window and choose Pair with QR code.",
                )).wrap());
            }
            Some(t) => {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("share-target")
                        .selected_text(format!(
                            "{}{}",
                            t.name,
                            if t.connected { "" } else { " (not connected)" }
                        ))
                        .show_ui(ui, |ui| {
                            for o in list {
                                let label = format!(
                                    "{}{}",
                                    o.name,
                                    if o.connected { "" } else { " (not connected)" }
                                );
                                if ui
                                    .selectable_label(
                                        Some(&o.device_id) == target.as_ref().map(|t| &t.device_id),
                                        label,
                                    )
                                    .clicked()
                                {
                                    self.chosen = Some(o.device_id.clone());
                                }
                            }
                        });
                    if !t.connected
                        && theme::pill_button(ui, pal, "Connect", ButtonKind::Plain, true).clicked()
                    {
                        self.worker.send(Cmd::Call(Request::Connect {
                            device_id: t.device_id.clone(),
                            endpoint: None,
                        }));
                    }
                });
            }
        }
        ui.add_space(6.0);
        // The drop target.
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 120.0), egui::Sense::hover());
        let colour = if hovering && ready {
            pal.accent
        } else {
            pal.separator
        };
        ui.painter().rect_filled(rect, 10.0, pal.surface);
        ui.painter().rect_stroke(
            rect,
            10.0,
            Stroke::new(1.5_f32, colour),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center() - vec2(0.0, 12.0),
            egui::Align2::CENTER_CENTER,
            if ready {
                "Drop files here to send"
            } else {
                "Connect a phone to send files"
            },
            egui::FontId::new(15.0, theme::semibold_family()),
            if ready { pal.text } else { pal.text3 },
        );
        let button_rect =
            egui::Rect::from_center_size(rect.center() + vec2(0.0, 22.0), vec2(150.0, 30.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(button_rect), |ui| {
            if theme::pill_button(ui, pal, "Choose files…", ButtonKind::Primary, ready).clicked()
                && let Some(t) = &target
            {
                self.worker.send(Cmd::Pick {
                    device_id: t.device_id.clone(),
                });
            }
        });
        ui.add_space(22.0);
        ui.label(theme::overline(pal, "Link or text"));
        ui.horizontal(|ui| {
            crate::a11y::named(
                ui.add(
                    egui::TextEdit::singleline(&mut self.text)
                        .desired_width(ui.available_width() - 190.0)
                        .margin(Margin::symmetric(8, 4))
                        .hint_text("Paste a link or type a note"),
                ),
                "Link or note to send",
            );
            let trimmed = self.text.trim().to_owned();
            let is_url = bridge_kde::share::is_openable_url(&trimmed);
            let enabled = ready
                && !trimmed.is_empty()
                && trimmed.len() <= bridge_core::share::MAX_SEND_TEXT_BYTES;
            if theme::pill_button(
                ui,
                pal,
                if is_url { "Send link" } else { "Send text" },
                ButtonKind::Plain,
                enabled,
            )
            .clicked()
                && let Some(t) = &target
            {
                self.worker.send(Cmd::Call(Request::ShareSendText {
                    device_id: t.device_id.clone(),
                    text_kind: if is_url {
                        ShareTextKind::Url
                    } else {
                        ShareTextKind::Text
                    },
                    text: trimmed,
                }));
                self.text.clear();
            }
        });
        if let Some(n) = &self.view.notice {
            ui.add_space(4.0);
            ui.add(egui::Label::new(theme::caption(pal, n)).wrap());
        }
        ui.add_space(10.0);
    }

    fn incoming_section(&mut self, ui: &mut egui::Ui, pal: &theme::Palette) {
        let Some(share) = &self.view.share else {
            return;
        };
        let offers: Vec<&TransferInfo> = share
            .transfers
            .iter()
            .filter(|t| t.state == TransferState::Offered)
            .collect();
        if offers.is_empty() {
            return;
        }
        ui.label(theme::overline(pal, "Incoming"));
        for t in offers {
            theme::group(ui, pal, |ui| {
                ui.add(egui::Label::new(theme::semibold(&t.name, 14.0)).truncate());
                ui.label(theme::caption(pal, state_line(t)));
                ui.horizontal(|ui| {
                    if theme::pill_button(ui, pal, "Accept", ButtonKind::Primary, true).clicked() {
                        self.worker.send(Cmd::Call(Request::ShareDecide {
                            transfer_id: t.transfer_id,
                            accept: true,
                        }));
                    }
                    if theme::pill_button(ui, pal, "Decline", ButtonKind::Plain, true).clicked() {
                        self.worker.send(Cmd::Call(Request::ShareDecide {
                            transfer_id: t.transfer_id,
                            accept: false,
                        }));
                    }
                });
                ui.label(theme::caption(pal, format!("Saved to {}", share.save_dir)));
            });
        }
        ui.add_space(10.0);
    }

    fn transfers_section(&mut self, ui: &mut egui::Ui, pal: &theme::Palette) {
        let Some(share) = &self.view.share else {
            return;
        };
        let items: Vec<&TransferInfo> = share
            .transfers
            .iter()
            .rev()
            .filter(|t| t.state != TransferState::Offered)
            .collect();
        ui.label(theme::overline(pal, "Transfers"));
        if items.is_empty() {
            ui.label(theme::caption(pal, "Nothing yet."));
        }
        for t in items {
            theme::group(ui, pal, |ui| {
                match t.kind {
                    TransferKind::File => {
                        let arrow = if t.direction == TransferDirection::Incoming {
                            "From"
                        } else {
                            "To"
                        };
                        ui.add(egui::Label::new(theme::semibold(&t.name, 14.0)).truncate());
                        ui.label(theme::caption(
                            pal,
                            format!("{arrow} {}  ·  {}", t.device_name, state_line(t)),
                        ));
                        if let Some(f) = progress_fraction(t).filter(|_| t.state.is_live()) {
                            let bar = ui
                                .add(egui::ProgressBar::new(f).desired_width(ui.available_width()));
                            let name = format!("{} {}", t.name, state_line(t));
                            crate::a11y::describe(
                                ui.ctx(),
                                bar.id,
                                &name,
                                Some(&format!("{:.0}%", f * 100.0)),
                            );
                            crate::a11y::live_region(ui.ctx(), bar.id, crate::a11y::Live::Polite);
                        }
                    }
                    TransferKind::Text | TransferKind::Url => {
                        let what = if t.kind == TransferKind::Url {
                            "Link"
                        } else {
                            "Text"
                        };
                        ui.label(theme::caption(
                            pal,
                            format!("{what} from {}", t.device_name),
                        ));
                        let body = t.text.clone().unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(&body).color(pal.text)).wrap());
                        ui.horizontal(|ui| {
                            if theme::pill_button(ui, pal, "Copy", ButtonKind::Plain, true)
                                .clicked()
                            {
                                ui.ctx().copy_text(body.clone());
                            }
                            if t.kind == TransferKind::Url
                                && bridge_kde::share::is_openable_url(&body)
                                && theme::pill_button(
                                    ui,
                                    pal,
                                    "Open link",
                                    ButtonKind::Primary,
                                    true,
                                )
                                .clicked()
                            {
                                open_link(&body);
                            }
                        });
                    }
                }
                ui.horizontal(|ui| {
                    if t.state.is_live() {
                        if theme::pill_button(ui, pal, "Cancel", ButtonKind::Plain, true).clicked()
                        {
                            self.worker.send(Cmd::Call(Request::ShareCancel {
                                transfer_id: t.transfer_id,
                            }));
                        }
                    } else {
                        if t.direction == TransferDirection::Incoming
                            && t.state == TransferState::Done
                            && let Some(p) = t
                                .path
                                .as_deref()
                                .map(PathBuf::from)
                                .filter(|p| p.starts_with(&share.save_dir))
                        {
                            if theme::pill_button(ui, pal, "Open", ButtonKind::Primary, true)
                                .clicked()
                            {
                                open_in_system(&p, false);
                            }
                            if theme::pill_button(
                                ui,
                                pal,
                                "Show in folder",
                                ButtonKind::Plain,
                                true,
                            )
                            .clicked()
                            {
                                open_in_system(&p, true);
                            }
                        }
                        if theme::pill_button(ui, pal, "Dismiss", ButtonKind::Plain, true).clicked()
                        {
                            self.worker.send(Cmd::Call(Request::ShareDismiss {
                                transfer_id: t.transfer_id,
                            }));
                        }
                    }
                });
            });
        }
        ui.add_space(10.0);
    }

    fn settings_section(&mut self, ui: &mut egui::Ui, pal: &theme::Palette, list: &[Target]) {
        let Some(share) = &self.view.share else {
            return;
        };
        ui.label(theme::overline(pal, "Receiving"));
        theme::group(ui, pal, |ui| {
            if list.is_empty() {
                ui.label(theme::caption(pal, "Pair a phone to receive from it."));
            }
            for t in list {
                let cur = share.devices.iter().find(|d| d.device_id == t.device_id);
                let (mut receive, mut auto) =
                    cur.map_or((false, false), |d| (d.receive, d.auto_accept));
                ui.label(theme::semibold(&t.name, 13.0));
                if ui
                    .checkbox(
                        &mut receive,
                        "Allow this phone to send me files, links and text",
                    )
                    .changed()
                {
                    self.worker.send(Cmd::Call(Request::ShareSettings {
                        device_id: t.device_id.clone(),
                        receive: Some(receive),
                        auto_accept: None,
                    }));
                }
                if ui
                    .add_enabled(
                        receive,
                        egui::Checkbox::new(&mut auto, "Accept its files without asking"),
                    )
                    .changed()
                {
                    self.worker.send(Cmd::Call(Request::ShareSettings {
                        device_id: t.device_id.clone(),
                        receive: None,
                        auto_accept: Some(auto),
                    }));
                }
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.label(theme::caption(
                    pal,
                    format!("Files are saved to {}", share.save_dir),
                ));
                if theme::tool_button(
                    ui,
                    pal,
                    Icon::Folder,
                    Some("Open folder"),
                    true,
                    "Open the save folder",
                )
                .clicked()
                {
                    let dir = PathBuf::from(&share.save_dir);
                    let _ = std::fs::create_dir_all(&dir);
                    open_in_system(&dir, false);
                }
            });
        });
    }
}

/// Entry point of `geekbridge-share`.
pub fn run(theme_choice: ThemeChoice, files: Vec<String>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GeekBridge Share")
            .with_app_id("dev.geekbridge.Share")
            .with_inner_size([480.0, 640.0])
            .with_min_inner_size([380.0, 460.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    #[cfg(feature = "memstats")]
    crate::memstats::start();
    eframe::run_native(
        "GeekBridge Share",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, LOOK);
            cc.egui_ctx.set_theme(match theme_choice {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Light => ThemePreference::Light,
                ThemeChoice::Dark => ThemePreference::Dark,
            });
            let ctx = cc.egui_ctx.clone();
            let initial = sendable(files.into_iter().map(PathBuf::from));
            Ok(Box::new(ShareApp {
                worker: Worker::spawn(move || ctx.request_repaint(), initial),
                view: View::default(),
                chosen: None,
                text: String::new(),
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::ipc::{LinkRole, LinkStatus, PairedDevice};

    fn info(
        state: TransferState,
        direction: TransferDirection,
        size: Option<u64>,
        bytes: u64,
    ) -> TransferInfo {
        TransferInfo {
            transfer_id: 1,
            device_id: "d".into(),
            device_name: "Pixel".into(),
            direction,
            kind: TransferKind::File,
            name: "a.bin".into(),
            size,
            bytes,
            state,
            code: None,
            path: None,
            text: None,
            expires_in_ms: None,
            at_unix_ms: 0,
        }
    }

    #[test]
    fn sizes_and_progress() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
        let t = info(
            TransferState::Active,
            TransferDirection::Incoming,
            Some(200),
            50,
        );
        assert_eq!(progress_fraction(&t), Some(0.25));
        let t = info(TransferState::Active, TransferDirection::Incoming, None, 50);
        assert_eq!(progress_fraction(&t), None);
        let t = info(
            TransferState::Active,
            TransferDirection::Incoming,
            Some(10),
            99,
        );
        assert_eq!(progress_fraction(&t), Some(1.0));
    }

    #[test]
    fn plain_state_lines() {
        let offer = info(
            TransferState::Offered,
            TransferDirection::Incoming,
            Some(2048),
            0,
        );
        assert_eq!(state_line(&offer), "Pixel wants to send 2.0 KB");
        let mut failed = info(
            TransferState::Failed,
            TransferDirection::Outgoing,
            Some(1),
            0,
        );
        failed.code = Some("expired".into());
        assert_eq!(state_line(&failed), "Failed: nobody answered in time");
        let sent = info(TransferState::Done, TransferDirection::Outgoing, Some(1), 1);
        assert_eq!(state_line(&sent), "Sent to Pixel");
    }

    #[test]
    fn only_real_files_are_sendable() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, b"x").unwrap();
        let out = sendable([
            f.clone(),
            dir.path().to_path_buf(),
            dir.path().join("missing"),
        ]);
        assert_eq!(out.len(), 1);
        assert!(Path::new(&out[0]).is_absolute());
    }

    #[test]
    fn picker_output_and_commands() {
        assert_eq!(
            parse_picker_output("/a b/c.txt\n\n/d.png\r\n"),
            vec![PathBuf::from("/a b/c.txt"), PathBuf::from("/d.png")]
        );
        assert!(!picker_commands().is_empty());
        let (prog, args) = open_command(Path::new("/tmp/x/y.txt"), true);
        if cfg!(target_os = "linux") {
            assert_eq!((prog, args), ("xdg-open", vec!["/tmp/x".to_owned()]));
        }
    }

    #[test]
    fn opening_a_program_or_script_only_shows_it_in_its_folder() {
        for name in [
            "setup.exe",
            "a.BAT",
            "b.cmd",
            "c.com",
            "d.scr",
            "e.ps1",
            "f.lnk",
            "g.command",
            "h.app",
            "i.jar",
            "j.msi",
            "k.sh",
            "l.vbs",
            "m.js",
            "n.desktop",
            "invoice.pdf.exe",
            "hidden.exe.",
            "spaced.exe ",
        ] {
            assert!(is_risky_to_open(Path::new(name)), "{name}");
            let p = format!("/tmp/x/{name}");
            let (prog, args) = open_command(Path::new(&p), false);
            let (rprog, rargs) = open_command(Path::new(&p), true);
            assert_eq!(
                (prog, &args),
                (rprog, &rargs),
                "{name} must reveal, not open"
            );
        }
        for name in [
            "photo.jpg",
            "notes.txt",
            "report.pdf",
            "archive.zip",
            "noext",
            "exe",
            ".bashrc.txt",
        ] {
            assert!(!is_risky_to_open(Path::new(name)), "{name}");
        }
        if cfg!(target_os = "linux") {
            let (_, args) = open_command(Path::new("/tmp/x/run.sh"), false);
            assert_eq!(args, vec!["/tmp/x".to_owned()]);
            let (_, args) = open_command(Path::new("/tmp/x/pic.png"), false);
            assert_eq!(args, vec!["/tmp/x/pic.png".to_owned()]);
        }
    }

    #[test]
    fn connected_devices_come_first() {
        let paired = |id: &str, name: &str| PairedDevice {
            device_id: id.into(),
            name: name.into(),
            device_type: "phone".into(),
            cert_sha256: String::new(),
            paired_at_unix: 0,
            consents: vec![],
        };
        let link = |id: &str| LinkStatus {
            device_id: id.into(),
            name: String::new(),
            phase: LinkPhase::Connected,
            role: Some(LinkRole::Initiator),
            connection_id: None,
            peer_ip: None,
            peer_cert_sha256: None,
            negotiated: vec![],
            since_unix_ms: 0,
            pings_received: 0,
            last_ping_unix_ms: None,
            rejected_packets: 0,
        };
        let status = Status {
            ipc_version: 1,
            service_version: String::new(),
            device_id: String::new(),
            device_name: String::new(),
            own_cert_sha256: String::new(),
            paired: vec![paired("a", "Old"), paired("b", "New")],
            links: vec![link("b")],
            challenges: vec![],
            event_seq: 0,
            inbound_listener: None,
            reconnecting: vec![],
        };
        let t = targets(&status);
        assert_eq!(t[0].device_id, "b");
        assert!(t[0].connected && !t[1].connected);
    }
}

#[cfg(test)]
#[path = "share_app_tests.rs"]
mod render_tests;
